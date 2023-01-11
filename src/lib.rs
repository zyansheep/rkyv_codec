#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(test, feature(test))]

//! Simple usage example:
//! ```rust
//! # use rkyv::{Infallible, Archived, AlignedVec, Archive, Serialize, Deserialize};
//! # use rkyv_codec::{archive_stream, RkyvWriter, VarintLength};
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
//! let mut codec = RkyvWriter::<_, VarintLength>::new(writer);
//! codec.send(&value).await.unwrap();
//!
//! // Reading
//! let mut reader = &codec.inner()[..];
//! let mut buffer = AlignedVec::new(); // Aligned streaming buffer for re-use
//! let data: &Archived<Test> = archive_stream::<_, Test, VarintLength>(&mut reader, &mut buffer).await.unwrap(); // This returns a reference into the passed buffer
//! let value_received: Test = data.deserialize(&mut Infallible).unwrap();
//!
//! assert_eq!(value, value_received);
//! # })
//! ```

/// Abstract length encodings for reading and writing streams
pub mod length_codec;
pub use length_codec::VarintLength;

#[cfg(feature = "futures_stream")]
mod futures_stream;

/// Error type for rkyv_codec
#[cfg(feature = "std")]
use thiserror::Error;
#[cfg(feature = "std")]
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
#[cfg(not(feature = "std"))]
#[derive(Debug)]
pub enum RkyvCodecError {
	CheckArchiveError,
	SerializeError,
	ReadLengthError,
}

#[cfg(feature = "std")]
mod rkyv_codec;
#[cfg(feature = "std")]
pub use crate::rkyv_codec::*;

mod no_std_feature {
	use bytes::{Buf, BufMut, Bytes, BytesMut};
	use rkyv::{AlignedVec, Archive, Archived};

	use crate::{length_codec::LengthCodec, RkyvCodecError};

	/// Writes a single `Object` from a `bytes::Bytes`
	pub fn archive_sink_bytes<Packet: Archive, L: LengthCodec>(
		bytes: &mut BytesMut,
		archived: &[u8],
	) -> Result<(), RkyvCodecError> {
		let length_buf = &mut L::Buffer::default();
		let length_buf = L::encode(archived.len(), length_buf);
		bytes.put(length_buf);
		bytes.put(archived);
		Ok(())
	}
	/// Reads a single `&Archived<Object>` from a `bytes::Bytes` into the passed buffer
	/// # Safety
	/// This will cause undefined behavior if the bytestream is not the correct format (i.e. not generated through `archive_sink[_bytes]`, `RkyvWriter`, or `RkyvCodec`) with the correct LengthCodec
	pub unsafe fn archive_stream_bytes_unsafe<'b, Packet: Archive, L: LengthCodec>(
		bytes: &mut Bytes,
		buffer: &'b mut AlignedVec,
	) -> Result<&'b Archived<Packet>, RkyvCodecError> {
		// Read length
		let mut length_buf = L::Buffer::default();
		let length_buf_slice = L::as_slice(&mut length_buf);
		let copy_len = usize::min(length_buf_slice.len(), bytes.len());
		length_buf_slice[..copy_len].copy_from_slice(&bytes[..copy_len]);

		// Decode length
		let (archive_len, remaining) =
			L::decode(length_buf_slice).map_err(|_| RkyvCodecError::ReadLengthError)?;

		// Read into aligned buffer
		let begin = length_buf_slice.len() - remaining.len();
		let end = begin + archive_len;
		buffer.extend_from_slice(&bytes[begin..end]);
		bytes.advance(end);

		let archive = rkyv::archived_root::<Packet>(buffer);
		Ok(archive)
	}
	/// Reads a single `&Archived<Object>` from a `bytes::Bytes` into the passed buffer if
	/// validation enabled.
	pub fn archive_stream_bytes<'b, Packet, L: LengthCodec>(
		bytes: &mut Bytes,
		buffer: &'b mut AlignedVec,
	) -> Result<&'b Archived<Packet>, RkyvCodecError>
	where
		Packet: rkyv::Archive,
		Packet::Archived: bytecheck::CheckBytes<rkyv::validation::validators::DefaultValidator<'b>> + 'b
	{
		// Read length
		let mut length_buf = L::Buffer::default();
		let length_buf_slice = L::as_slice(&mut length_buf);
		let copy_len = usize::min(length_buf_slice.len(), bytes.len());
		length_buf_slice[..copy_len].copy_from_slice(&bytes[..copy_len]);

		// Decode length
		let (archive_len, remaining) =
			L::decode(length_buf_slice).map_err(|_| RkyvCodecError::ReadLengthError)?;

		// Read into aligned buffer
		let begin = length_buf_slice.len() - remaining.len();
		let end = begin + archive_len;
		buffer.extend_from_slice(&bytes[begin..end]);
		bytes.advance(end);

		let archive = rkyv::check_archived_root::<'b, Packet>(buffer)
			.map_err(|_| RkyvCodecError::CheckArchiveError)?;
		Ok(archive)
	}
}
pub use no_std_feature::*;

#[cfg(test)]
#[cfg(feature = "std")]
mod tests {
	extern crate test;

	use async_std::task::block_on;
	use bytecheck::CheckBytes;
	use bytes::BytesMut;
	use futures::{io::Cursor, AsyncRead, AsyncWrite, SinkExt, StreamExt, TryStreamExt};
	use futures_codec::{CborCodec, Framed};
	use rkyv::{to_bytes, AlignedVec, Archive, Archived, Deserialize, Infallible, Serialize};

	use crate::{
		archive_sink, archive_sink_bytes, archive_stream, archive_stream_bytes,
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
	#[test]
	fn bytes_functions() {
		let mut writer = BytesMut::new();
		for _ in 0..50 {
			archive_sink_bytes::<Test, VarintLength>(&mut writer, *TEST_BYTES).unwrap();
		}

		let mut reader = writer.freeze();

		let mut buffer = AlignedVec::with_capacity(256);

		for _ in 0..50 {
			let data: &Archived<Test> =
				archive_stream_bytes::<Test, VarintLength>(&mut reader, &mut buffer).unwrap();
			assert_eq!(*TEST, *data);
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
