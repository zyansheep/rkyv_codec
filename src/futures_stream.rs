use std::marker::PhantomData;

use bytes_old::Buf;
use futures_codec::{BytesMut, Decoder, Encoder};
use rkyv::{
	api::{
		high::{HighDeserializer, HighSerializer, HighValidator},
		serialize_using,
	},
	de::Pool,
	rancor,
	ser::{
		allocator::{Arena, ArenaHandle},
		sharing::Share,
		Serializer,
	},
	util::AlignedVec,
	Archive, Archived, Deserialize, Portable, Serialize,
};

use crate::{length_codec::LengthCodec, RkyvCodecError};

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
impl<Packet, L: LengthCodec> Encoder for RkyvCodec<Packet, L>
where
	Packet: Archive + for<'b> Serialize<HighSerializer<AlignedVec, ArenaHandle<'b>, rancor::Error>>,
{
	type Item = Packet;
	type Error = RkyvCodecError;

	fn encode(&mut self, data: Self::Item, buf: &mut BytesMut) -> Result<(), Self::Error> {
		let mut encode_buffer = self.encode_buffer.take().unwrap();
		let share = self.ser_share.take().unwrap();
		encode_buffer.clear();
		let mut serializer = Serializer::new(encode_buffer, self.ser_arena.acquire(), share);
		let _ = serialize_using(&data, &mut serializer)?;

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
	type Error = RkyvCodecError;

	fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
		if buf.is_empty() {
			return Ok(None);
		}
		self.decode_buffer.clear();

		let (length, remaining) = L::decode(buf).map_err(|_| RkyvCodecError::ReadLengthError)?;
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
