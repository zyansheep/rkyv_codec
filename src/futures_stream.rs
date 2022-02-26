use std::marker::PhantomData;

use bytecheck::CheckBytes;
use bytes::buf::BufMutExt;
use futures_codec::{BytesMut, Decoder, Encoder};
use rkyv::{AlignedVec, Archive, Deserialize, Infallible, Serialize, ser::{Serializer, serializers::{AllocScratch, CompositeSerializer, FallbackScratch, HeapScratch, SharedSerializeMap, WriteSerializer}}};

use crate::RkyvCodecError;

#[derive(Default)]
pub struct RkyvCodec<Packet: Archive> {
	_data: PhantomData<Packet>,
    decode_buffer: AlignedVec,
}
/// Encoder impl encodes object streams to bytes
impl<Packet> Encoder for RkyvCodec<Packet>
where
	Packet: Archive
		+ for<'b> Serialize<
			CompositeSerializer<
				WriteSerializer<bytes::buf::ext::Writer<&'b mut BytesMut>>,
				FallbackScratch<HeapScratch<0>, AllocScratch>,
				SharedSerializeMap,
			>,
		>,
{
	type Item = Packet;
	type Error = RkyvCodecError;

	fn encode(&mut self, data: Self::Item, buf: &mut BytesMut) -> Result<(), Self::Error> {
		let writer = buf.writer();
		let serializer = WriteSerializer::new(writer);
		let mut serializer = CompositeSerializer::new(
			serializer,
			FallbackScratch::default(),
			SharedSerializeMap::default(),
		);
		serializer
			.serialize_value(&data)
			.map_err(|_| RkyvCodecError::SerializeError)?;

		Ok(())
	}
}

/// Decoder impl parses json objects from bytes
impl<Packet> Decoder for RkyvCodec<Packet>
where
	Packet: for<'b> Archive<Archived: CheckBytes<rkyv::validation::validators::DefaultValidator<'b>> + Deserialize<Packet, Infallible> + 'b> + 'static,
{
	type Item = Packet;
	type Error = RkyvCodecError;

	fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
		self.decode_buffer.clear();

		let (length, remaining) = unsigned_varint::decode::usize(buf).map_err(|_|RkyvCodecError::ReadLengthError)?;
		self.decode_buffer.extend_from_slice(&remaining[0..length]);
		let archive: &<Packet as Archive>::Archived = rkyv::check_archived_root::<'_, Packet>(&self.decode_buffer).map_err(|_|RkyvCodecError::CheckArchiveError)?;
		let packet: Packet = archive.deserialize(&mut Infallible).unwrap();
        Ok(Some(packet))
	}
}
