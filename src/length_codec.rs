use core::{error, fmt};

#[cfg(feature = "std")]
use futures::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

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

	#[cfg(feature = "std")]
	#[allow(async_fn_in_trait)]
	async fn decode_async<'a, W: AsyncBufRead + Unpin>(
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

/// Variable-bit length encoding based using unsigned_varint crate, currently can handle lengths up to 2^63
#[cfg(feature = "varint")]
#[derive(Debug)]
pub struct VarintLength;
#[cfg(feature = "varint")]
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
		unsigned_varint::decode::usize(buf).map_err(|e| match e {
			unsigned_varint::decode::Error::Insufficient => VarintLengthError::Insufficient,
			unsigned_varint::decode::Error::Overflow => VarintLengthError::Overflow,
			unsigned_varint::decode::Error::NotMinimal => VarintLengthError::NotMinimal,
			_ => unreachable!(),
		})
	}

	#[cfg(feature = "std")]
	async fn decode_async<'a, R: AsyncBufRead + Unpin>(
		reader: &'a mut R,
	) -> Result<usize, WithIOError<Self::Error>> {
		let max_varint_len = Self::Buffer::default().len();

		loop {
			// get current buf
			let available_bytes = reader.fill_buf().await.map_err(WithIOError::IoError)?;

			if available_bytes.is_empty() {
				// EOF reached. If we are in this loop, it implies a previous decode attempt
				// resulted in Insufficient, so this is a definitive error.
				return Err(WithIOError::LengthDecodeError(
					VarintLengthError::Insufficient,
				));
			}
			// first 10 bytes as slice.
			let slice_to_decode = if available_bytes.len() > max_varint_len {
				&available_bytes[..max_varint_len]
			} else {
				available_bytes
			};
			// attempt to decode byte slice
			match Self::decode(slice_to_decode) {
				// success, advance reader buffer
				Ok((length_value, remaining_in_slice)) => {
					let varint_byte_count = slice_to_decode.len() - remaining_in_slice.len();
					reader.consume_unpin(varint_byte_count);
					return Ok(length_value);
				}
				Err(decode_error) => {
					match decode_error {
						VarintLengthError::Insufficient => {
							// Not enough bytes in slice_to_decode.
							// If slice_to_decode was already max_varint_len, then the varint is malformed/too long.
							if slice_to_decode.len() == max_varint_len {
								return Err(WithIOError::LengthDecodeError(
									VarintLengthError::Insufficient,
								));
							}
							// Otherwise, we need more data. Loop again to call fill_buf().
							// No bytes are consumed from the reader at this point.
						}
						err => return Err(WithIOError::LengthDecodeError(err)),
					}
				}
			}
		}
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

/// Big-endian 32-bit length encoding, can handle lengths up to 2^32
#[derive(Debug)]
pub struct U32Length;
impl LengthCodec for U32Length {
	type Error = NotEnoughBytesError;
	type Buffer = [u8; 4];

	#[inline]
	fn as_slice(buffer: &mut Self::Buffer) -> &mut [u8] {
		&mut buffer[..]
	}

	#[inline]
	fn encode(length: usize, buf: &mut Self::Buffer) -> &[u8] {
		*buf = u32::to_be_bytes(length as u32);
		&buf[..]
	}

	#[inline]
	fn decode(buf: &[u8]) -> Result<(usize, &[u8]), Self::Error> {
		let bytes: [u8; 4] = buf.try_into().map_err(|_| NotEnoughBytesError)?;
		Ok((u32::from_be_bytes(bytes) as usize, &buf[4..]))
	}

	#[cfg(feature = "std")]
	async fn decode_async<'a, R: AsyncBufRead + Unpin>(
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
/// Big-endian 64-bit length encoding, can handle length up to 2^64
#[derive(Debug)]
pub struct U64Length;
impl LengthCodec for U64Length {
	type Error = NotEnoughBytesError;
	type Buffer = [u8; 8];

	#[inline]
	fn as_slice(buffer: &mut Self::Buffer) -> &mut [u8] {
		&mut buffer[..]
	}

	#[inline]
	fn encode(length: usize, buf: &mut Self::Buffer) -> &[u8] {
		*buf = u64::to_be_bytes(length as u64);
		&buf[..]
	}

	#[inline]
	fn decode(buf: &[u8]) -> Result<(usize, &[u8]), Self::Error> {
		let bytes: [u8; 8] = buf.try_into().map_err(|_| NotEnoughBytesError)?;
		Ok((u64::from_be_bytes(bytes) as usize, &buf[8..]))
	}

	#[cfg(feature = "std")]
	async fn decode_async<'a, R: AsyncBufRead + AsyncBufReadExt + Unpin>(
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
