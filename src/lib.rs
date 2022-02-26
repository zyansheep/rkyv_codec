#![feature(associated_type_bounds)]
#![feature(test)]

//! Simple usage example:
//! ```rust
//! # use rkyv::{Infallible, Archived, AlignedVec, Archive, Serialize, Deserialize};
//! # use rkyv_codec::{archive_stream, RkyvWriter};
//! # use bytecheck::CheckBytes;
//! # use futures::SinkExt;
//! # async_std::task::block_on(async {
//! #[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
//! #[archive_attr(derive(CheckBytes, Debug))] // Checkbytes is required
//! struct Test {
//!     int: u8,
//!     string: String,
//!     option: Option<Vec<i32>>,
//! }
//! let value = Test {
//!     int: 42,
//!     string: "hello world".to_string(),
//!     option: Some(vec![1, 2, 3, 4]),
//! };
//!
//! // Writing
//! let writer = Vec::new();
//! let mut codec = RkyvWriter::new(writer);
//! codec.send(value.clone()).await.unwrap();
//!
//! // Reading
//! let mut reader = &codec.inner()[..];
//! let mut buffer = AlignedVec::new(); // Aligned streaming buffer for re-use
//! let data: &Archived<Test> = archive_stream::<_, Test>(&mut reader, &mut buffer).await.unwrap(); // This returns a reference into the passed buffer
//! let value_received: Test = data.deserialize(&mut Infallible).unwrap();
//!
//! assert_eq!(value, value_received);
//! # })
//! ```

extern crate test;

use std::{
	ops::Range,
	pin::Pin,
	task::{Context, Poll},
};

#[macro_use]
extern crate thiserror;
#[macro_use]
extern crate pin_project;

use bytecheck::CheckBytes;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, Sink};
use rkyv::{
	ser::{
		serializers::{
			AllocScratch, CompositeSerializer, FallbackScratch, HeapScratch, SharedSerializeMap,
			WriteSerializer,
		},
		Serializer,
	},
	validation::validators::DefaultValidator,
	AlignedVec, Archive, Archived, Serialize,
};

/// Error type for rkyv_codec
#[derive(Debug, Error)]
pub enum RkyvCodecError {
	#[error(transparent)]
	IoError(#[from] futures::io::Error),
	#[error("Packet not correctly archived")]
	CheckArchiveError,
	#[error("Failed to Serialize")]
	SerializeError,
	#[error("Failed to parse length")]
	ReadLengthError,
	#[error("Premature End of File Error")]
	EOFError,

	#[cfg(feature = "futures_stream")]
	#[error("Deserialize Error")]
	DeserializeError,
}

macro_rules! ready {
	($e:expr $(,)?) => {
		match $e {
			$crate::Poll::Ready(t) => t,
			$crate::Poll::Pending => return $crate::Poll::Pending,
		}
	};
}

/// Reads a single `&Archived<Object>` from an `AsyncRead` using the passed buffer.
///
/// Until streaming iterators (and streaming futures) are implemented in rust, this currently the fastest method I could come up with that requires no recurring heap allocations.
///
/// Requires rkyv validation feature & CheckBytes
pub async fn archive_stream<'b, Inner: AsyncRead + Unpin, Packet>(
	mut inner: &mut Inner,
	buffer: &'b mut AlignedVec,
) -> Result<&'b Archived<Packet>, RkyvCodecError>
where
	Packet: rkyv::Archive<Archived: CheckBytes<DefaultValidator<'b>> + 'b>,
{
	let archive_len = unsigned_varint::aio::read_usize(&mut inner).await.map_err(|_|RkyvCodecError::ReadLengthError)?;
	buffer.reserve(archive_len - buffer.len()); // Reserve at least the amount of bytes needed
	unsafe {
		buffer.set_len(archive_len);
	} // Safety: Already reserved the required space

	inner.read_exact(buffer).await?;
	let archive = rkyv::check_archived_root::<'b, Packet>(buffer)
		.map_err(|_| RkyvCodecError::CheckArchiveError)?;
	Ok(archive)
}

pub async fn archive_sink<'b, Inner: AsyncWrite + Unpin, Packet: Archive> (inner: &mut Inner, archived: &[u8]) -> Result<(), RkyvCodecError> {
	let length_buf = &mut unsigned_varint::encode::usize_buffer();
	let length_buf = unsigned_varint::encode::usize(archived.len(), length_buf);
	inner.write(length_buf).await?;
	inner.write(archived).await?;
	Ok(())
}

/// Wraps an `AsyncWrite` and implements `Sink` to serialize `Archive` objects.
#[pin_project]
pub struct RkyvWriter<Writer: AsyncWrite> {
	#[pin]
	writer: Writer,
	buffer: AlignedVec,
	length_buffer: [u8; 10],
	len_state: Range<usize>, // How much of the length buffer has been written
	buf_state: usize, // Whether or not the aligned buf is being written and if so, how much so far
}
impl<Writer: AsyncWrite> RkyvWriter<Writer> {
	pub fn new(writer: Writer) -> Self {
		Self {
			writer,
			buffer: AlignedVec::new(),
			length_buffer: [0u8; 10],
			len_state: Default::default(),
			buf_state: 0,
		}
	}
	pub fn inner(self) -> Writer {
		self.writer
	}
}

impl<Writer: AsyncWrite, Packet> Sink<Packet> for RkyvWriter<Writer>
where
	Packet: Archive
		+ for<'b> Serialize<
			CompositeSerializer<
				WriteSerializer<&'b mut AlignedVec>,
				FallbackScratch<HeapScratch<0>, AllocScratch>,
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

	fn start_send(self: Pin<&mut Self>, item: Packet) -> Result<(), Self::Error> {
		let this = self.project();
		this.buffer.clear();
		let serializer = WriteSerializer::new(this.buffer);
		let mut serializer = CompositeSerializer::new(
			serializer,
			FallbackScratch::default(),
			SharedSerializeMap::default(),
		);
		let _bytes_written = serializer
			.serialize_value(&item)
			.map_err(|_| RkyvCodecError::SerializeError)?;

		let bytes_written = serializer.into_serializer().into_inner().len();
		*this.len_state =
			0..unsigned_varint::encode::usize(bytes_written, this.length_buffer).len();
		*this.buf_state = 0;
		Ok(())
	}

	fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		let mut this = self.project();

		let len_state = this.len_state;
		if len_state.start <= len_state.end {
			let length_buffer = &this.length_buffer[len_state.clone()];

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
	use bytecheck::CheckBytes;
	use futures::{AsyncRead, AsyncWrite, SinkExt, StreamExt, io::Cursor};
	use futures_codec::{CborCodec, Framed};
use rkyv::{AlignedVec, Archive, Archived, Deserialize, Infallible, Serialize};

	use crate::{RkyvWriter, archive_sink, archive_stream, futures_stream::RkyvCodec};

	#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone, serde::Serialize, serde::Deserialize)]
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
	}

	#[inline]
	async fn sink_amount<W: AsyncWrite + Unpin>(mut writer: W, count: usize) {
		let archive = rkyv::to_bytes::<_, 256>(&*TEST).unwrap();
		for _ in 0..count {
			archive_sink::<_, Test>(&mut writer, &archive).await.unwrap();
		}
	}
	#[inline]
	async fn stream_amount<R: AsyncRead + Unpin>(mut reader: R, count: usize, check: &Test) {
		let mut buffer = AlignedVec::new();
		for _ in 0..count {
			let value = archive_stream::<_, Test>(&mut reader, &mut buffer).await.unwrap();
			assert_eq!(value, check);
		}
		
	}

	#[async_std::test]
	async fn ser_de() {
		let writer = Vec::new();
		let mut codec = RkyvWriter::new(writer);
		codec.send(TEST.clone()).await.unwrap();

		let mut reader = &codec.inner()[..];

		let mut buffer = AlignedVec::new();
		let data: &Archived<Test> = archive_stream::<_, Test>(&mut reader, &mut buffer).await.unwrap();

		let value_sent: Test = data.deserialize(&mut Infallible).unwrap();

		assert_eq!(*TEST, value_sent);
	}
	#[async_std::test]
	async fn futures_ser_de() {
		let codec = RkyvCodec::<Test>::default();
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
    fn bench_ser_de(b: &mut Bencher) {
		async_std::task::block_on(async {
			b.iter(ser_de);
		})
    }
	#[bench]
    fn bench_futures_ser_de(b: &mut Bencher) {
		async_std::task::block_on(async {
			b.iter(futures_ser_de);
		})
    }

	#[bench]
	fn bench_futures_cbor_ser_de(b: &mut Bencher) {
		async_std::task::block_on(async {
			b.iter(futures_cbor_ser_de);
		})
	}
}

#[cfg(feature = "futures_stream")]
mod futures_stream;
