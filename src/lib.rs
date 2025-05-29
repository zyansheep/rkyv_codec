#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(test, feature(test))]

#[cfg_attr(
	feature = "std",
	doc = r##"
//! Simple usage example:
//! ```rust
//! # use rkyv::{Archived, util::AlignedVec, Archive, Serialize, Deserialize, rancor};
//! # use rkyv_codec::{archive_stream, RkyvWriter, VarintLength};
//! # use futures::SinkExt;
//! # async_std::task::block_on(async {
//! #[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
//! #[rkyv(attr(derive(Debug)))]
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
//! let value_received: Test = rkyv::deserialize::<_, rancor::Error>(data).unwrap();
//!
//! assert_eq!(value, value_received);
//! # })
//! ```
"##
)]

/// Abstract length encodings for reading and writing streams
pub mod length_codec;

#[cfg(feature = "varint")]
pub use length_codec::VarintLength;

#[cfg(feature = "aio")]
mod aio;
#[cfg(feature = "aio")]
pub use aio::*;

#[cfg(feature = "std")]
use length_codec::LengthCodec;

#[cfg(feature = "std")]
use length_codec::WithIOError;

/// Error type for rkyv_codec
use thiserror::Error;
#[derive(Debug, Error)]
pub enum RkyvCodecError<L: LengthCodec> {
	#[cfg(feature = "std")]
	#[error(transparent)]
	IoError(#[from] futures::io::Error),
	#[error("packet not correctly archived: {0}")]
	CheckArchiveError(#[from] rkyv::rancor::Error),
	#[error("Failed to parse length: {0}")]
	ReadLengthError(L::Error),
	#[error("Premature End of File Error")]
	EOFError,
}

#[cfg(feature = "std")]
impl<L: LengthCodec> From<WithIOError<L::Error>> for RkyvCodecError<L> {
	fn from(value: WithIOError<L::Error>) -> Self {
		match value {
			WithIOError::IoError(err) => RkyvCodecError::IoError(err),
			WithIOError::LengthDecodeError(err) => RkyvCodecError::ReadLengthError(err),
		}
	}
}

#[cfg(feature = "std")]
mod rkyv_codec;
#[cfg(feature = "std")]
pub use rkyv_codec::*;

use bytes::{Buf, BufMut};
#[cfg(feature = "std")]
use rkyv::api::high as cur_api;
#[cfg(feature = "std")]
use rkyv::api::high::HighValidator as CurrentValidator;
#[cfg(not(feature = "std"))]
use rkyv::api::low as cur_api;
#[cfg(not(feature = "std"))]
use rkyv::api::low::LowValidator as CurrentValidator;

use rkyv::{Archive, Archived, bytecheck::CheckBytes, rancor, util::AlignedVec};

/// Writes a single `Object` from a `bytes::Bytes`
pub fn archive_sink_bytes<Packet: Archive, L: LengthCodec>(
	bytes: &mut impl BufMut,
	archived: &[u8],
) -> Result<(), RkyvCodecError<L>> {
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
	bytes: &mut impl Buf,
	buffer: &'b mut AlignedVec,
) -> Result<&'b Archived<Packet>, RkyvCodecError<L>> {
	// Read length
	let archive_len = L::decode_bytes(bytes).map_err(RkyvCodecError::ReadLengthError)?;

	// Read specific amount into aligned buffer
	buffer.extend_from_reader(&mut bytes.take(archive_len).reader())?;

	// reinterpret cast
	let archive = unsafe { rkyv::access_unchecked::<Packet::Archived>(buffer) };
	Ok(archive)
}
/// Reads a single `&Archived<Object>` from a `bytes::Bytes` into the passed buffer if validation enabled.
pub fn archive_stream_bytes<'b, Packet: Archive, L: LengthCodec>(
	bytes: &mut impl Buf,
	buffer: &'b mut AlignedVec,
) -> Result<&'b Archived<Packet>, RkyvCodecError<L>>
where
	<Packet as Archive>::Archived: for<'a> CheckBytes<CurrentValidator<'a, rancor::Error>>,
{
	// Read length
	let archive_len = L::decode_bytes(bytes).map_err(RkyvCodecError::ReadLengthError)?;

	// Read specific amount into aligned buffer
	buffer.extend_from_reader(&mut bytes.take(archive_len).reader())?;

	let archive = cur_api::access::<Archived<Packet>, rancor::Error>(&*buffer)?;
	Ok(archive)
}

#[cfg(test)]
#[cfg(feature = "std")]
mod tests {
	extern crate test;

	use async_std::task::block_on;
	use asynchronous_codec::{CborCodec, Framed};
	use bytes::BytesMut;
	use futures::{AsyncBufRead, AsyncRead, AsyncWrite, SinkExt, StreamExt, io::Cursor};
	use rkyv::{Archive, Archived, Deserialize, Serialize, rancor, to_bytes, util::AlignedVec};

	use crate::{
		RkyvWriter, archive_sink, archive_sink_bytes, archive_stream, archive_stream_bytes,
		length_codec::{self, LengthCodec, U64Length},
	};

	#[cfg(feature = "varint")]
	type TestLengthCodec = length_codec::VarintLength;
	#[cfg(not(feature = "varint"))]
	type TestLengthCodec = length_codec::U32Length;

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
	// To use the safe API, you have to use the check_bytes option for the archive
	#[rkyv(compare(PartialEq), attr(derive(Debug)))]
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
			let vec = rkyv::to_bytes::<rancor::Error>(&*TEST).unwrap();
			Box::leak(vec.into_boxed_slice())
		};
		static ref TEST_ARCHIVED: &'static Archived<Test> = unsafe { rkyv::access_unchecked::<Archived<Test>>(*TEST_BYTES) };
	}

	#[inline]
	async fn gen_amount<W: AsyncWrite + Unpin, L: LengthCodec>(writer: &mut W, count: usize) {
		for _ in 0..count {
			archive_sink::<_, L>(writer, *TEST_BYTES).await.unwrap();
		}
	}
	#[inline]
	async fn consume_amount<R: AsyncRead + AsyncBufRead + Unpin, L: LengthCodec>(
		mut reader: R,
		count: usize,
	) {
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
			archive_sink_bytes::<Test, TestLengthCodec>(&mut writer, *TEST_BYTES).unwrap();
		}

		let mut reader = writer.freeze();

		let mut buffer = AlignedVec::with_capacity(256);

		for _ in 0..50 {
			let data: &Archived<Test> =
				archive_stream_bytes::<Test, TestLengthCodec>(&mut reader, &mut buffer).unwrap();
			assert_eq!(*TEST, *data);
		}
	}

	#[async_std::test]
	async fn functions() {
		let mut writer = Vec::new();
		archive_sink::<_, TestLengthCodec>(&mut writer, *TEST_BYTES)
			.await
			.unwrap();

		let mut reader = &writer[..];

		let mut buffer = AlignedVec::with_capacity(256);
		let data: &Archived<Test> =
			archive_stream::<_, Test, TestLengthCodec>(&mut reader, &mut buffer)
				.await
				.unwrap();

		let value_sent: Test = rkyv::deserialize::<_, rancor::Error>(data).unwrap();

		assert_eq!(*TEST, value_sent);
	}

	#[cfg(feature = "varint")]
	#[async_std::test]
	/// Tests the edgecase where we hit EOF while reading the length because the whole structure fits within the 10 byte max varint length.
	async fn functions_varint_edge_case() {
		#[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Clone)]
		#[rkyv(compare(PartialEq), attr(derive(Debug)))]
		struct SmallTest {
			int: u8,
		}
		const SMALL_TEST: SmallTest = SmallTest { int: 1 };
		let mut writer = Vec::new();
		let mut sink = RkyvWriter::<_, TestLengthCodec>::new(&mut writer);
		for _ in 0..2 {
			sink.send(&SMALL_TEST).await.unwrap();
		}

		let mut reader = &writer[..];
		println!("reader: {reader:?}");

		let mut buffer = AlignedVec::with_capacity(256);
		let data: &Archived<SmallTest> =
			archive_stream::<_, SmallTest, crate::VarintLength>(&mut reader, &mut buffer)
				.await
				.unwrap();

		let value_sent: SmallTest = rkyv::deserialize::<_, rancor::Error>(data).unwrap();

		assert_eq!(SMALL_TEST, value_sent);

		println!("reader: {reader:?}");

		let data: &Archived<SmallTest> =
			archive_stream::<_, SmallTest, crate::VarintLength>(&mut reader, &mut buffer)
				.await
				.unwrap();

		let value_sent: SmallTest = rkyv::deserialize::<_, rancor::Error>(data).unwrap();

		assert_eq!(SMALL_TEST, value_sent);
	}

	#[async_std::test]
	async fn rkyv_writer() {
		let mut writer = Vec::new();
		let mut sink = RkyvWriter::<_, TestLengthCodec>::new(&mut writer);
		sink.send(&*TEST).await.unwrap();

		let mut reader = &writer[..];

		let mut buffer = AlignedVec::with_capacity(256);
		let data: &Archived<Test> =
			archive_stream::<_, Test, TestLengthCodec>(&mut reader, &mut buffer)
				.await
				.unwrap();

		assert_eq!(*TEST, *data);
	}

	#[async_std::test]
	#[cfg(feature = "aio")]
	async fn futures_ser_de() {
		let codec = crate::aio::RkyvCodec::<Test, TestLengthCodec>::default();
		let mut buffer = vec![0u8; 256];
		let mut framed = asynchronous_codec::Framed::new(Cursor::new(&mut buffer), codec);
		framed.send(TEST.clone()).await.unwrap();

		let codec = framed.into_parts().codec;

		let mut framed = asynchronous_codec::Framed::new(Cursor::new(&mut buffer), codec);
		let received_value = framed.next().await.unwrap().unwrap();

		assert_eq!(*TEST, received_value);
	}
	#[async_std::test]
	async fn futures_cbor_ser_de() {
		let codec = CborCodec::<Test, Test>::new();

		let mut buffer = vec![0u8; 256];
		let mut framed = Framed::new(Cursor::new(&mut buffer), codec);

		framed.send(TEST.clone()).await.unwrap();

		let codec = framed.into_parts().codec;

		let mut framed = asynchronous_codec::Framed::new(Cursor::new(&mut buffer), codec);
		let received_value = framed.next().await.unwrap().unwrap();

		assert_eq!(*TEST, received_value);
	}

	use test::Bencher;

	#[cfg(feature = "varint")]
	#[bench]
	fn bench_varint_length_encoding(b: &mut Bencher) {
		let mut buffer = Vec::with_capacity(1024);
		b.iter(|| {
			block_on(async {
				buffer.clear();
				gen_amount::<_, crate::VarintLength>(&mut buffer, 50).await;
				consume_amount::<_, crate::VarintLength>(&mut &buffer[..], 50).await;
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
					archive_sink::<_, TestLengthCodec>(&mut buffer, &*TEST_BYTES)
						.await
						.unwrap()
				}
			})
		});
		block_on(consume_amount::<_, TestLengthCodec>(&mut &buffer[..], 50));
	}
	#[bench]
	fn bench_archive_sink_50(b: &mut Bencher) {
		let mut buffer = Vec::with_capacity(1024);
		b.iter(|| {
			block_on(async {
				buffer.clear();

				for _ in 0..50 {
					let bytes = to_bytes::<rancor::Error>(&*TEST).unwrap(); // This makes it very slow
					archive_sink::<_, TestLengthCodec>(&mut buffer, &bytes)
						.await
						.unwrap();
				}
			})
		});
		block_on(consume_amount::<_, TestLengthCodec>(&mut &buffer[..], 50))
	}
	#[bench]
	fn bench_rkyv_writer_50(b: &mut Bencher) {
		let mut buffer = Vec::with_capacity(1024);
		b.iter(|| {
			block_on(async {
				buffer.clear();
				let mut sink = RkyvWriter::<_, TestLengthCodec>::new(&mut buffer);
				for _ in 0..50 {
					sink.send(&*TEST).await.unwrap();
				}
			})
		});
		block_on(consume_amount::<_, TestLengthCodec>(&mut &buffer[..], 50))
	}
	#[bench]
	fn bench_archive_stream_50(b: &mut Bencher) {
		let mut buffer = Vec::with_capacity(1024);

		block_on(gen_amount::<_, TestLengthCodec>(&mut buffer, 50));

		b.iter(|| {
			block_on(async {
				consume_amount::<_, TestLengthCodec>(&mut &buffer[..], 50).await;
			})
		});
	}

	#[cfg(feature = "aio")]
	#[bench]
	fn bench_rkyv_asynchronous_codec_sink_50(b: &mut Bencher) {
		let mut buffer = Vec::with_capacity(1024);
		b.iter(|| {
			block_on(async {
				buffer.clear();
				let codec = crate::aio::RkyvCodec::<Test, TestLengthCodec>::default();
				let mut framed = asynchronous_codec::Framed::new(Cursor::new(&mut buffer), codec);
				for _ in 0..50 {
					framed.send(TEST.clone()).await.unwrap();
				}
			})
		});
		block_on(consume_amount::<_, TestLengthCodec>(&mut &buffer[..], 50));
	}
	#[cfg(feature = "aio")]
	#[bench]
	fn bench_rkyv_asynchronous_codec_stream_50(b: &mut Bencher) {
		use futures::TryStreamExt;
		let mut buffer = Vec::with_capacity(1024);

		block_on(gen_amount::<_, TestLengthCodec>(&mut buffer, 50));

		let codec = crate::aio::RkyvCodec::<Test, TestLengthCodec>::default();
		let mut framed = asynchronous_codec::Framed::new(Cursor::new(&mut buffer), codec);
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

		let codec = framed.into_parts().codec;

		let mut framed = asynchronous_codec::Framed::new(Cursor::new(&mut buffer), codec);

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
