use std::marker::PhantomData;

use bytecheck::CheckBytes;
use bytes_old::Buf;
use futures_codec::{BytesMut, Decoder, Encoder};
use rkyv::{
	ser::{
		serializers::{
			AllocScratch, CompositeSerializer, FallbackScratch, HeapScratch, SharedSerializeMap,
			WriteSerializer,
		},
		Serializer,
	},
	AlignedVec, Archive, Deserialize, Infallible, Serialize,
};

use crate::{length_codec::LengthCodec, RkyvCodecError};

pub struct RkyvCodec<Packet: Archive, L: LengthCodec> {
	_data: PhantomData<Packet>,
	_length: PhantomData<L>,
	encode_buffer: AlignedVec,
	decode_buffer: AlignedVec,
}
impl<Packet: Archive, L: LengthCodec> Default for RkyvCodec<Packet, L> {
	fn default() -> Self {
		Self {
			_data: PhantomData,
			_length: PhantomData::default(),
			encode_buffer: AlignedVec::new(),
			decode_buffer: AlignedVec::new(),
		}
	}
}
/// Encoder impl encodes object streams to bytes
impl<Packet, L: LengthCodec> Encoder for RkyvCodec<Packet, L>
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
	type Item = Packet;
	type Error = RkyvCodecError;

	fn encode(&mut self, data: Self::Item, buf: &mut BytesMut) -> Result<(), Self::Error> {
		self.encode_buffer.clear();
		let serializer = WriteSerializer::new(&mut self.encode_buffer);
		let _rkyv_root = CompositeSerializer::new(
			serializer,
			FallbackScratch::default(),
			SharedSerializeMap::default(),
		)
		.serialize_value(&data)
		.map_err(|_| RkyvCodecError::SerializeError)?;

		let mut length_buffer = L::Buffer::default();
		let length_buffer = L::encode(self.encode_buffer.len(), &mut length_buffer);
		buf.extend_from_slice(length_buffer);
		buf.extend_from_slice(&self.encode_buffer[..]);
		Ok(())
	}
}

/// Decoder impl parses json objects from bytes
impl<Packet, L: LengthCodec> Decoder for RkyvCodec<Packet, L>
where
	Packet: Archive + 'static,
	Packet::Archived: for<'b> CheckBytes<rkyv::validation::validators::DefaultValidator<'b>>
		+ Deserialize<Packet, Infallible>,
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
		let archive: &<Packet as Archive>::Archived =
			rkyv::check_archived_root::<'_, Packet>(&self.decode_buffer)
				.map_err(|_| RkyvCodecError::CheckArchiveError)?;
		let packet: Packet = archive.deserialize(&mut Infallible).unwrap();

		// NOTE: This is the only place where I use bytes_old :(
		let amount_read = length + buf.len() - remaining.len();
		buf.advance(amount_read);

		Ok(Some(packet))
	}
}
