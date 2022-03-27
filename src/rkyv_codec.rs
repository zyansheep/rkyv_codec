use futures::{ready, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, Sink};
use std::{
	borrow::BorrowMut,
	ops::Range,
	pin::Pin,
	task::{Context, Poll},
};

use bytecheck::CheckBytes;
use rkyv::{
	ser::{
		serializers::{
			AlignedSerializer, AllocScratch, CompositeSerializer, FallbackScratch, HeapScratch,
			SharedSerializeMap,
		},
		Serializer,
	},
	validation::validators::DefaultValidator,
	AlignedVec, Archive, Archived, Serialize,
};

use crate::{length_codec::LengthCodec, RkyvCodecError};

/// Rewrites a single buffer representing an Archive to an `AsyncWrite`
pub async fn archive_sink<'b, Inner: AsyncWrite + Unpin, L: LengthCodec>(
	inner: &mut Inner,
	archived: &[u8],
) -> Result<(), RkyvCodecError> {
	let length_buf = &mut L::Buffer::default();
	let length_buf = L::encode(archived.len(), length_buf);
	inner.write_all(length_buf).await?;
	inner.write_all(archived).await?;
	Ok(())
}
/// Reads a single `&Archived<Object>` from an `AsyncRead` without checking for correct byte formatting
/// # Safety
/// This will cause undefined behavior if the bytestream is not the correct format (i.e. not generated through `archive_sink`, `RkyvWriter`, or `RkyvCodec`)
pub async unsafe fn unsafe_archive_stream<
	'b,
	Inner: AsyncRead + Unpin,
	Packet: Archive + 'b,
	L: LengthCodec,
>(
	inner: &mut Inner,
	buffer: &'b mut AlignedVec,
) -> Result<&'b Archived<Packet>, RkyvCodecError> {
	buffer.clear();

	// Read Length
	let mut length_buf = L::Buffer::default();
	let length_buf = L::as_slice(&mut length_buf);
	inner.read_exact(&mut *length_buf).await?;
	let (archive_len, remaining) =
		L::decode(length_buf).map_err(|_| RkyvCodecError::ReadLengthError)?;

	// Reserve buffer
	buffer.reserve(archive_len - buffer.len()); // Reserve at least the amount of bytes needed
											// Safety: Already reserved the required space
	unsafe {
		buffer.set_len(archive_len);
	}

	buffer[0..remaining.len()].copy_from_slice(remaining); // Copy unread length_buf bytes to buffer

	inner.read_exact(&mut buffer[remaining.len()..]).await?;
	unsafe { Ok(rkyv::archived_root::<Packet>(buffer)) }
}

/// Reads a single `&Archived<Object>` from an `AsyncRead` using the passed buffer.
///
/// Until streaming iterators (and streaming futures) are implemented in rust, this currently the fastest method I could come up with that requires no recurring heap allocations.
///
/// Requires rkyv validation feature & CheckBytes
pub async fn archive_stream<'b, Inner: AsyncRead + Unpin, Packet, L: LengthCodec>(
	inner: &mut Inner,
	buffer: &'b mut AlignedVec,
) -> Result<&'b Archived<Packet>, RkyvCodecError>
where
	Packet: rkyv::Archive<Archived: CheckBytes<DefaultValidator<'b>> + 'b>,
{
	buffer.clear();

	// Read length
	let mut length_buf = L::Buffer::default();
	let length_buf = L::as_slice(&mut length_buf);
	inner.read_exact(length_buf).await?;
	let (archive_len, remaining) =
		L::decode(length_buf).map_err(|_| RkyvCodecError::ReadLengthError)?;

	// Reserve buffer
	buffer.reserve(archive_len - buffer.len()); // Reserve at least the amount of bytes needed for packet
											// Safety: Already reserved the required space
	unsafe {
		buffer.set_len(archive_len);
	}

	// Read into aligned buffer
	buffer[0..remaining.len()].copy_from_slice(remaining); // Copy unread length_buf bytes to buffer
	inner.read_exact(&mut buffer[remaining.len()..]).await?; // Read into buffer after the appended bytes

	let archive = rkyv::check_archived_root::<'b, Packet>(buffer)
		.map_err(|_| RkyvCodecError::CheckArchiveError)?;
	Ok(archive)
}

/// Wraps an `AsyncWrite` and implements `Sink` to serialize `Archive` objects.
#[pin_project]
pub struct RkyvWriter<Writer: AsyncWrite, L: LengthCodec> {
	#[pin]
	writer: Writer,
	buffer: AlignedVec,
	length_buffer: L::Buffer,
	len_state: Range<usize>, // How much of the length buffer has been written
	buf_state: usize, // Whether or not the aligned buf is being written and if so, how much so far
	scratch: Option<FallbackScratch<HeapScratch<256>, AllocScratch>>,
	shared: Option<SharedSerializeMap>,
}
impl<Writer: AsyncWrite, L: LengthCodec> RkyvWriter<Writer, L> {
	pub fn new(writer: Writer) -> Self {
		Self {
			writer,
			buffer: AlignedVec::new(),
			length_buffer: L::Buffer::default(),
			len_state: Default::default(),
			buf_state: 0,
			scratch: Some(FallbackScratch::new(
				HeapScratch::new(),
				AllocScratch::default(),
			)),
			shared: Some(SharedSerializeMap::new()),
		}
	}
	pub fn inner(self) -> Writer {
		self.writer
	}
}

impl<Writer: AsyncWrite, Packet, L: LengthCodec> Sink<&Packet> for RkyvWriter<Writer, L>
where
	Packet: Archive
		+ for<'b> Serialize<
			CompositeSerializer<
				AlignedSerializer<&'b mut AlignedVec>,
				FallbackScratch<HeapScratch<256>, AllocScratch>,
				SharedSerializeMap,
			>,
		>,
{
	type Error = RkyvCodecError;

	fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		self.project()
			.writer
			.poll_flush(cx)
			.map_err(RkyvCodecError::IoError)
	}

	fn start_send(self: Pin<&mut Self>, item: &Packet) -> Result<(), Self::Error> {
		let this = self.project();
		this.buffer.clear();

		// Construct Serializer
		let mut serializer = CompositeSerializer::new(
			AlignedSerializer::new(this.buffer.borrow_mut()),
			this.scratch.take().unwrap(),
			this.shared.take().unwrap(),
		);

		serializer
			.serialize_value(item)
			.map_err(|_| RkyvCodecError::SerializeError)?;

		// Deconstruct composite serializer
		let (_, scratch, shared) = serializer.into_components();
		*this.scratch = Some(scratch);
		*this.shared = Some(shared);

		*this.len_state = 0..L::encode(this.buffer.len(), this.length_buffer).len();
		*this.buf_state = 0;
		Ok(())
	}

	fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		let mut this = self.project();

		let len_state = this.len_state;
		if len_state.start <= len_state.end {
			let length_buffer = L::as_slice(this.length_buffer);
			let length_buffer = &mut length_buffer[len_state.clone()];

			let written = ready!(Pin::new(&mut this.writer).poll_write(cx, length_buffer)?);
			len_state.start += written;
		}

		while *this.buf_state < this.buffer.len() {
			let buffer_left = &this.buffer[*this.buf_state..this.buffer.len()];
			let bytes_written = ready!(Pin::new(&mut this.writer).poll_write(cx, buffer_left))?;
			if bytes_written == 0 {
				return Poll::Ready(Err(RkyvCodecError::EOFError));
			}
			*this.buf_state += bytes_written;
		}

		ready!(this.writer.poll_flush(cx)?);
		Poll::Ready(Ok(()))
	}

	fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		self.project()
			.writer
			.poll_close(cx)
			.map_err(RkyvCodecError::IoError)
	}
}

#[cfg(test)]
mod tests {
	use async_std::task::block_on;
	use bytecheck::CheckBytes;
	use futures::{io::Cursor, AsyncRead, AsyncWrite, SinkExt, StreamExt, TryStreamExt};
	use futures_codec::{CborCodec, Framed};
	use rkyv::{to_bytes, AlignedVec, Archive, Archived, Deserialize, Infallible, Serialize};

	use crate::{
		archive_sink, archive_stream,
		futures_stream::RkyvCodec,
		length_codec::{LengthCodec, U64Length, VarintLength},
		RkyvWriter,
	};

	#[derive(
		Archive,
		Deserialize,
		Serialize,
		Debug,
		PartialEq,
		Clone,
		serde::Serialize,
		serde::Deserialize,
	)]
	// This will generate a PartialEq impl between our unarchived and archived types
	#[archive(compare(PartialEq))]
	// To use the safe API, you have to derive CheckBytes for the archived type
	#[archive_attr(derive(CheckBytes, Debug))]
	struct Test {
		int: u8,
		string: String,
		option: Option<Vec<i32>>,
	}

	lazy_static::lazy_static! {
		static ref TEST: Test = Test {
			int: 42,
			string: "hello world".to_string(),
			option: Some(vec![1, 2, 3, 4]),
		};
		static ref TEST_BYTES: &'static [u8] = {
			let vec = rkyv::to_bytes::<_, 256>(&*TEST).unwrap();
			Box::leak(vec.into_boxed_slice())
		};
		static ref TEST_ARCHIVED: &'static Archived<Test> = unsafe { rkyv::archived_root::<Test>(*TEST_BYTES) };
	}

	#[inline]
	async fn gen_amount<W: AsyncWrite + Unpin, L: LengthCodec>(writer: &mut W, count: usize) {
		for _ in 0..count {
			archive_sink::<_, L>(writer, *TEST_BYTES).await.unwrap();
		}
	}
	#[inline]
	async fn consume_amount<R: AsyncRead + Unpin, L: LengthCodec>(mut reader: R, count: usize) {
		let mut buffer = AlignedVec::new();
		for _ in 0..count {
			let value = archive_stream::<_, Test, L>(&mut reader, &mut buffer)
				.await
				.unwrap();
			assert_eq!(*TEST, *value);
		}
	}

	#[async_std::test]
	async fn functions() {
		let mut writer = Vec::new();
		archive_sink::<_, VarintLength>(&mut writer, *TEST_BYTES)
			.await
			.unwrap();

		let mut reader = &writer[..];

		let mut buffer = AlignedVec::with_capacity(256);
		let data: &Archived<Test> =
			archive_stream::<_, Test, VarintLength>(&mut reader, &mut buffer)
				.await
				.unwrap();

		let value_sent: Test = data.deserialize(&mut Infallible).unwrap();

		assert_eq!(*TEST, value_sent);
	}

	#[async_std::test]
	async fn rkyv_writer() {
		let mut writer = Vec::new();
		let mut sink = RkyvWriter::<_, VarintLength>::new(&mut writer);
		sink.send(&*TEST).await.unwrap();

		let mut reader = &writer[..];

		let mut buffer = AlignedVec::with_capacity(256);
		let data: &Archived<Test> =
			archive_stream::<_, Test, VarintLength>(&mut reader, &mut buffer)
				.await
				.unwrap();

		assert_eq!(*TEST, *data);
	}

	#[async_std::test]
	async fn futures_ser_de() {
		let codec = RkyvCodec::<Test, VarintLength>::default();
		let mut buffer = vec![0u8; 256];
		let mut framed = futures_codec::Framed::new(Cursor::new(&mut buffer), codec);
		framed.send(TEST.clone()).await.unwrap();

		let (_, codec) = framed.release();

		let mut framed = futures_codec::Framed::new(Cursor::new(&mut buffer), codec);
		let received_value = framed.next().await.unwrap().unwrap();

		assert_eq!(*TEST, received_value);
	}
	#[async_std::test]
	async fn futures_cbor_ser_de() {
		let codec = CborCodec::<Test, Test>::new();

		let mut buffer = vec![0u8; 256];
		let mut framed = Framed::new(Cursor::new(&mut buffer), codec);

		framed.send(TEST.clone()).await.unwrap();

		let (_, codec) = framed.release();

		let mut framed = futures_codec::Framed::new(Cursor::new(&mut buffer), codec);
		let received_value = framed.next().await.unwrap().unwrap();

		assert_eq!(*TEST, received_value);
	}

	use test::Bencher;

	#[bench]
	fn bench_varint_length_encoding(b: &mut Bencher) {
		let mut buffer = Vec::with_capacity(1024);
		b.iter(|| {
			block_on(async {
				buffer.clear();
				gen_amount::<_, VarintLength>(&mut buffer, 50).await;
				consume_amount::<_, VarintLength>(&mut &buffer[..], 50).await;
			})
		})
	}
	#[bench]
	fn bench_u64_length_encoding(b: &mut Bencher) {
		let mut buffer = Vec::with_capacity(1024);

		b.iter(|| {
			block_on(async {
				buffer.clear();
				gen_amount::<_, U64Length>(&mut buffer, 50).await;
				consume_amount::<_, U64Length>(&mut &buffer[..], 50).await;
			})
		})
	}
	#[bench]
	fn bench_archive_sink_prearchived_50(b: &mut Bencher) {
		let mut buffer = Vec::with_capacity(1024);
		b.iter(|| {
			block_on(async {
				buffer.clear();
				for _ in 0..50 {
					archive_sink::<_, VarintLength>(&mut buffer, &*TEST_BYTES)
						.await
						.unwrap()
				}
			})
		});
		block_on(consume_amount::<_, VarintLength>(&mut &buffer[..], 50));
	}
	#[bench]
	fn bench_archive_sink_50(b: &mut Bencher) {
		let mut buffer = Vec::with_capacity(1024);
		b.iter(|| {
			block_on(async {
				buffer.clear();

				for _ in 0..50 {
					let bytes = to_bytes::<_, 256>(&*TEST).unwrap(); // This makes it very slow
					archive_sink::<_, VarintLength>(&mut buffer, &bytes)
						.await
						.unwrap();
				}
			})
		});
		block_on(consume_amount::<_, VarintLength>(&mut &buffer[..], 50))
	}
	#[bench]
	fn bench_rkyv_writer_50(b: &mut Bencher) {
		let mut buffer = Vec::with_capacity(1024);
		b.iter(|| {
			block_on(async {
				buffer.clear();
				let mut sink = RkyvWriter::<_, VarintLength>::new(&mut buffer);
				for _ in 0..50 {
					sink.send(&*TEST).await.unwrap();
				}
			})
		});
		block_on(consume_amount::<_, VarintLength>(&mut &buffer[..], 50))
	}
	#[bench]
	fn bench_archive_stream_50(b: &mut Bencher) {
		let mut buffer = Vec::with_capacity(1024);

		block_on(gen_amount::<_, VarintLength>(&mut buffer, 50));

		b.iter(|| {
			block_on(async {
				consume_amount::<_, VarintLength>(&mut &buffer[..], 50).await;
			})
		});
	}
	#[bench]
	fn bench_rkyv_futures_codec_sink_50(b: &mut Bencher) {
		let mut buffer = Vec::with_capacity(1024);
		b.iter(|| {
			block_on(async {
				buffer.clear();
				let codec = RkyvCodec::<Test, VarintLength>::default();
				let mut framed = futures_codec::Framed::new(Cursor::new(&mut buffer), codec);
				for _ in 0..50 {
					framed.send(TEST.clone()).await.unwrap();
				}
			})
		});
		block_on(consume_amount::<_, VarintLength>(&mut &buffer[..], 50));
	}
	#[bench]
	fn bench_rkyv_futures_codec_stream_50(b: &mut Bencher) {
		let mut buffer = Vec::with_capacity(1024);

		block_on(gen_amount::<_, VarintLength>(&mut buffer, 50));

		let codec = RkyvCodec::<Test, VarintLength>::default();
		let mut framed = futures_codec::Framed::new(Cursor::new(&mut buffer), codec);
		b.iter(|| {
			block_on(async {
				framed.set_position(0);
				while let Some(_value) = framed.try_next().await.unwrap() {}
			})
		});
	}

	#[bench]
	fn bench_futures_cbor_sink_50(b: &mut Bencher) {
		let mut buffer = vec![0u8; 256];

		b.iter(|| {
			block_on(async {
				buffer.clear();
				let codec = CborCodec::<Test, Test>::new();
				let mut framed = Framed::new(Cursor::new(&mut buffer), codec);
				framed.set_position(0);
				for _ in 0..50 {
					framed.send(TEST.clone()).await.unwrap();
				}
			})
		});
	}
	#[bench]
	fn bench_futures_cbor_stream_50(b: &mut Bencher) {
		let codec = CborCodec::<Test, Test>::new();

		let mut buffer = vec![0u8; 256];
		let mut framed = Framed::new(Cursor::new(&mut buffer), codec);

		block_on(async {
			for _ in 0..50 {
				framed.send(TEST.clone()).await.unwrap();
			}
		});

		let (_, codec) = framed.release();
		let mut framed = futures_codec::Framed::new(Cursor::new(&mut buffer), codec);

		b.iter(|| {
			block_on(async {
				framed.set_position(0);
				while let Some(value) = framed.next().await {
					test::black_box(value.unwrap());
				}
			})
		});
	}
}
