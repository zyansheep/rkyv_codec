/// Length encoding trait, allows for different kinds of length encoding
pub trait LengthCodec: 'static {
	type Error;
	type Buffer: Default + Sized;

	fn as_slice(buffer: &mut Self::Buffer) -> &mut [u8];
	/// Encode into buffer and return part of buffer that was written to
	fn encode(length: usize, buffer: &mut Self::Buffer) -> &[u8];
	/// Decode from buffer, fails if length is formatted incorrectly, returns length and remaining buffer
	fn decode(buffer: &[u8]) -> Result<(usize, &[u8]), Self::Error>;
}

/// Variable-bit length encoding based using unsigned_varint crate, currently can handle lengths up to 2^63
#[cfg(feature = "varint")]
pub struct VarintLength;
#[cfg(feature = "varint")]
impl LengthCodec for VarintLength {
	type Error = unsigned_varint::decode::Error;
	type Buffer = [u8; 10];

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
		unsigned_varint::decode::usize(buf)
	}
}

/// Error emitted by const-length encodings when there aren't bytes in the passed buffer when decoding
pub struct NotEnoughBytesError;

/// Big-endian 32-bit length encoding, can handle lengths up to 2^32
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
}
/// Big-endian 64-bit length encoding, can handle length up to 2^64
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
}
