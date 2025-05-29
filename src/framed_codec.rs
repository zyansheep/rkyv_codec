use std::marker::PhantomData;

use asynchronous_codec::{BytesMut, Decoder, Encoder};
use bytes::Buf;
use rkyv::{
	Archive, Archived, Deserialize, Portable, Serialize,
	api::{
		high::{HighDeserializer, HighSerializer, HighValidator},
		serialize_using,
	},
	de::Pool,
	rancor,
	ser::{
		Serializer,
		allocator::{Arena, ArenaHandle},
		sharing::Share,
	},
	util::AlignedVec,
};

use crate::{RkyvCodecError, length_codec::LengthCodec};

/// A futures-compatible Codec using the `asynchronous-codec` library.
///
/// This struct is used to encode and decode rkyv packets.
/// # Example
///
/// ```rust
/// # use rkyv::{Archive, Serialize, Deserialize, rancor, util::AlignedVec};
/// # use rkyv_codec::{RkyvCodec, VarintLength};
/// # use asynchronous_codec::{Framed, Decoder, Encoder};
/// # use bytes::BytesMut;
/// #
/// #[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
/// #[rkyv(attr(derive(Debug)))]
/// struct Test {
///     int: u8,
///     string: String,
///     option: Option<Vec<i32>>,
/// }
///
/// let value = Test {
///     int: 42,
///     string: "hello world".to_string(),
///     option: Some(vec![1, 2, 3, 4]),
/// };
///
/// let mut codec = RkyvCodec::<Test, VarintLength>::default();
/// let mut buf = BytesMut::new();
///
/// // Encoding
/// codec.encode(&value, &mut buf).unwrap();
///
/// // Decoding
/// let decoded_value = codec.decode(&mut buf).unwrap().unwrap();
///
/// assert_eq!(value, decoded_value);
/// ```

pub struct RkyvCodec<Packet: Archive, L: LengthCodec> {
	_data: PhantomData<Packet>,
	_length: PhantomData<L>,
	encode_buffer: Option<AlignedVec>,
	ser_arena: Arena,
	ser_share: Option<Share>,

	decode_buffer: AlignedVec,
	des_pool: Pool,
}
impl<Packet: Archive, L: LengthCodec> Default for RkyvCodec<Packet, L> {
	fn default() -> Self {
		Self {
			_data: PhantomData,
			_length: PhantomData::default(),
			encode_buffer: Some(AlignedVec::new()),
			ser_arena: Arena::new(),
			ser_share: Some(Share::new()),
			decode_buffer: AlignedVec::new(),
			des_pool: Pool::new(),
		}
	}
}
/// Encoder impl encodes object streams to bytes
impl<Packet: 'static, L: LengthCodec> Encoder for RkyvCodec<Packet, L>
where
	Packet: Archive + for<'b> Serialize<HighSerializer<AlignedVec, ArenaHandle<'b>, rancor::Error>>,
{
	type Item<'a> = &'a Packet;
	type Error = RkyvCodecError<L>;

	fn encode<'a>(&mut self, data: Self::Item<'a>, buf: &mut BytesMut) -> Result<(), Self::Error> {
		let mut encode_buffer = self.encode_buffer.take().unwrap();
		let share = self.ser_share.take().unwrap();
		encode_buffer.clear();
		let mut serializer = Serializer::new(encode_buffer, self.ser_arena.acquire(), share);
		let _ = serialize_using(data, &mut serializer)?;

		let (encode_buffer, _, share) = serializer.into_raw_parts();

		let mut length_buffer = L::Buffer::default();
		let length_buffer = L::encode(encode_buffer.len(), &mut length_buffer);
		buf.extend_from_slice(length_buffer);
		buf.extend_from_slice(&encode_buffer[..]);

		self.encode_buffer = Some(encode_buffer);
		self.ser_share = Some(share);
		Ok(())
	}
}

/// Decoder impl parses json objects from bytes
impl<Packet, L: LengthCodec> Decoder for RkyvCodec<Packet, L>
where
	Packet: Archive + 'static,
	Packet::Archived: Portable
		+ for<'b> rkyv::bytecheck::CheckBytes<HighValidator<'b, rancor::Error>>
		+ Deserialize<Packet, HighDeserializer<rancor::Error>>,
{
	type Item = Packet;
	type Error = RkyvCodecError<L>;

	fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
		if buf.is_empty() {
			return Ok(None);
		}
		self.decode_buffer.clear();

		let (length, remaining) = L::decode(buf).map_err(RkyvCodecError::ReadLengthError)?;
		self.decode_buffer.extend_from_slice(&remaining[0..length]);
		let archive: &Archived<Packet> = rkyv::access::<_, rancor::Error>(&self.decode_buffer)?;
		let packet: Packet =
			archive.deserialize(&mut HighDeserializer::wrap(&mut self.des_pool))?;

		// NOTE: This is the only place where I use bytes_old :(
		let amount_read = length + buf.len() - remaining.len();
		buf.advance(amount_read);

		Ok(Some(packet))
	}
}
