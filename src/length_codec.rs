use core::{error, fmt};

use bytes::Buf;
#[cfg(feature = "std")]
use futures::{AsyncRead, AsyncReadExt};

#[cfg(feature = "std")]
#[derive(Debug)]
pub enum WithIOError<E: fmt::Display> {
	LengthDecodeError(E),
	IoError(std::io::Error),
}

/// Length encoding trait, allows for different kinds of length encoding
pub trait LengthCodec: fmt::Debug + 'static {
	type Error: core::error::Error + Send + Sync + 'static;
	type Buffer: Default + Sized + Send;

	fn as_slice(buffer: &mut Self::Buffer) -> &mut [u8];
	/// Encode into buffer and return part of buffer that was written to
	fn encode(length: usize, buffer: &mut Self::Buffer) -> &[u8];
	/// Decode from buffer, fails if length is formatted incorrectly, returns length and remaining buffer
	fn decode(buffer: &[u8]) -> Result<(usize, &[u8]), Self::Error>;

	fn decode_bytes(buf: &mut impl Buf) -> Result<usize, Self::Error>;

	#[cfg(feature = "std")]
	#[allow(async_fn_in_trait)]
	async fn decode_async<'a, W: AsyncRead + Unpin>(
		reader: &'a mut W,
	) -> Result<usize, WithIOError<Self::Error>>;
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum VarintLengthError {
	#[error("not enough bytes for varint")]
	Insufficient,
	#[error("too many bytes")]
	Overflow,
	#[error("not minimal, too many trailing zero bytes")]
	NotMinimal,
}
impl From<unsigned_varint::decode::Error> for VarintLengthError {
	fn from(error: unsigned_varint::decode::Error) -> Self {
		match error {
			unsigned_varint::decode::Error::Insufficient => VarintLengthError::Insufficient,
			unsigned_varint::decode::Error::Overflow => VarintLengthError::Overflow,
			unsigned_varint::decode::Error::NotMinimal => VarintLengthError::NotMinimal,
			_ => unreachable!(),
		}
	}
}

/// Variable-bit length encoding based using unsigned_varint crate, currently can handle lengths up to 2^63
#[derive(Debug)]
pub struct VarintLength;
impl LengthCodec for VarintLength {
	type Error = VarintLengthError;
	type Buffer = [u8; 10]; // max varint length possible

	#[inline]
	fn as_slice(buffer: &mut Self::Buffer) -> &mut [u8] {
		&mut buffer[..]
	}

	#[inline]
	fn encode(length: usize, buf: &mut Self::Buffer) -> &[u8] {
		unsigned_varint::encode::usize(length, buf)
	}

	#[inline]
	fn decode(buf: &[u8]) -> Result<(usize, &[u8]), Self::Error> {
		Ok(unsigned_varint::decode::usize(buf)?)
	}

	fn decode_bytes(buf: &mut impl Buf) -> Result<usize, Self::Error> {
		let mut n = 0; // number to return
		let mut i = 0; // what byte we are reading

		#[cfg(target_pointer_width = "32")]
		let max_bytes = 4;
		#[cfg(target_pointer_width = "64")]
		let max_bytes = 9;

		let mut buf = buf.take(max_bytes);

		while let Ok(b) = buf.try_get_u8() {
			let k = usize::from(b & 0x7F); // mask out first byte
			n |= k << (i * 7); // record that byte
			if unsigned_varint::decode::is_last(b) {
				if b == 0 && i > 0 {
					// If last byte (of a multi-byte varint) is zero, it could have been "more
					// minimally" encoded by dropping that trailing zero.
					return Err(VarintLengthError::NotMinimal);
				}
				return Ok(n);
			}
			if i == max_bytes {
				return Err(VarintLengthError::Overflow);
			}
			i += 1;
		}
		Err(VarintLengthError::Insufficient)
	}

	#[cfg(feature = "std")]
	async fn decode_async<'a, R: AsyncRead + Unpin>(
		reader: &'a mut R,
	) -> Result<usize, WithIOError<Self::Error>> {
		let len = unsigned_varint::aio::read_usize(reader)
			.await
			.map_err(|e| match e {
				unsigned_varint::io::ReadError::Io(error) => WithIOError::IoError(error),
				unsigned_varint::io::ReadError::Decode(error) => {
					WithIOError::LengthDecodeError(error.into())
				}
				_ => todo!(),
			})?;
		Ok(len)
	}
}

/// Error emitted by const-length encodings when there aren't bytes in the passed buffer when decoding
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotEnoughBytesError;

impl fmt::Display for NotEnoughBytesError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "not enough bytes in buffer to decode length")
	}
}

impl error::Error for NotEnoughBytesError {}

macro_rules! impl_uint_length_codec {
	($name:ident, $uint_type:ty, $byte_count:expr) => {
		/// Big-endian fixed-size integer length encoding.
		#[derive(Debug)]
		pub struct $name;

		impl LengthCodec for $name {
			type Error = NotEnoughBytesError;
			type Buffer = [u8; $byte_count];

			#[inline]
			fn as_slice(buffer: &mut Self::Buffer) -> &mut [u8] {
				&mut buffer[..]
			}

			#[inline]
			fn encode(length: usize, buf: &mut Self::Buffer) -> &[u8] {
				*buf = <$uint_type>::to_be_bytes(length as $uint_type);
				&buf[..]
			}

			#[inline]
			fn decode(buf: &[u8]) -> Result<(usize, &[u8]), Self::Error> {
				if buf.len() < $byte_count {
					return Err(NotEnoughBytesError);
				}
				let (int_bytes, rest) = buf.split_at($byte_count);
				let bytes: [u8; $byte_count] =
					int_bytes.try_into().map_err(|_| NotEnoughBytesError)?;
				Ok((<$uint_type>::from_be_bytes(bytes) as usize, rest))
			}

			#[inline]
			fn decode_bytes(buf: &mut impl Buf) -> Result<usize, Self::Error> {
				let mut buffer = Self::Buffer::default();
				buf.copy_to_slice(&mut buffer);
				Self::decode(&buffer).map(|(len, _)| len)
			}

			#[cfg(feature = "std")]
			async fn decode_async<'a, R: AsyncRead + Unpin>(
				reader: &'a mut R,
			) -> Result<usize, WithIOError<Self::Error>> {
				let mut buffer = Self::Buffer::default();
				reader
					.read_exact(Self::as_slice(&mut buffer))
					.await
					.map_err(|e| {
						if e.kind() == std::io::ErrorKind::UnexpectedEof {
							WithIOError::LengthDecodeError(NotEnoughBytesError)
						} else {
							WithIOError::IoError(e)
						}
					})?;
				Self::decode(&buffer)
					.map(|(len, _)| len)
					.map_err(WithIOError::LengthDecodeError)
			}
		}
	};
}

// Generate U8Length
impl_uint_length_codec!(U8Length, u8, 1);
impl_uint_length_codec!(U16Length, u16, 2);
impl_uint_length_codec!(U32Length, u32, 4);
impl_uint_length_codec!(U64Length, u64, 8);

#[cfg(feature = "std")]
#[cfg(test)]
mod tests {
	use super::*;
	use paste::paste; // For concatenating identifiers in macro

	macro_rules! impl_uint_length_codec_tests {
		($test_suffix:ident, $codec_type:ty, $test_value:expr, $encoded_bytes:expr, $byte_count:expr) => {
			paste! {
				#[test]
				fn [<test_ $test_suffix _length_codec>]() {
					let mut buf = <$codec_type as LengthCodec>::Buffer::default();
					let encoded = <$codec_type>::encode($test_value, &mut buf);
					assert_eq!(encoded, $encoded_bytes);
					let (decoded, rest) = <$codec_type>::decode(encoded).unwrap();
					assert_eq!(decoded, $test_value);
					assert!(rest.is_empty());

					// Test decoding with extra bytes
					let mut extended_bytes: Vec<u8> = ($encoded_bytes).to_vec();
					extended_bytes.extend_from_slice(&[1, 2, 3]);
					let (decoded_partial, rest_partial) = <$codec_type>::decode(&extended_bytes).unwrap();
					assert_eq!(decoded_partial, $test_value);
					assert_eq!(rest_partial, &[1, 2, 3]);

					// Test decoding insufficient bytes
					if $byte_count > 0 {
						let short_bytes = &$encoded_bytes[..$byte_count -1];
						assert_eq!(<$codec_type>::decode(short_bytes), Err(NotEnoughBytesError));
					}
					assert_eq!(<$codec_type>::decode(&[]), Err(NotEnoughBytesError));
				}

				#[async_std::test]
				async fn [<test_ $test_suffix _decode_async>]() {
					let data = $encoded_bytes;
					let mut reader = &data[..];
					let len = <$codec_type>::decode_async(&mut reader).await.unwrap();
					assert_eq!(len, $test_value);

					if $byte_count > 0 {
						let data_short = &$encoded_bytes[..$byte_count-1];
						let mut reader_short = &data_short[..];
						let err = <$codec_type>::decode_async(&mut reader_short)
							.await
							.unwrap_err();
						match err {
							WithIOError::LengthDecodeError(NotEnoughBytesError) => {}
							_ => panic!("Unexpected error type for short data: {:?}", err),
						}
					}

					let data_empty: [u8;0] = [];
					let mut reader_empty = &data_empty[..];
					let err_empty = <$codec_type>::decode_async(&mut reader_empty).await.unwrap_err();
					match err_empty {
						WithIOError::LengthDecodeError(NotEnoughBytesError) => {}
						_ => panic!("Unexpected error type for empty data: {:?}", err_empty),
					}
				}
			}
		};
	}

	impl_uint_length_codec_tests!(u8, U8Length, 42, &[42], 1);
	impl_uint_length_codec_tests!(u16, U16Length, 300, &[1, 44], 2); // 0x012C
	impl_uint_length_codec_tests!(u32, U32Length, 70000, &[0, 1, 17, 112], 4); // 0x00011170
	impl_uint_length_codec_tests!(
		u64,
		U64Length,
		1_000_000_000_000,
		&[0, 0, 0, 232, 212, 165, 16, 0],
		8
	); // 0x000000E8_D4A51000
}
