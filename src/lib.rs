#![feature(associated_type_bounds)]

use std::{marker::PhantomData, pin::Pin, task::{Context, Poll}};

#[macro_use]
extern crate thiserror;
#[macro_use]
extern crate pin_project;

use bytes::Buf;
use bytecheck::CheckBytes;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, Sink};
use rkyv::{AlignedVec, Archive, Archived, Serialize, ser::{Serializer, serializers::{WriteSerializer}}, validation::validators::DefaultValidator};

#[derive(Debug, Error)]
pub enum PacketCodecError {
	#[error(transparent)]
	IoError(#[from] futures::io::Error),
	#[error("Packet not correctly archived")]
	CheckArchiveError,
	#[error("Failed to Serialize")]
	SerializeError,
	#[error("Failed to parse length")]
	ReadError(#[from] unsigned_varint::io::ReadError),
	#[error("Premature End of File Error")]
	EOFError,
}

macro_rules! ready {
	($e:expr $(,)?) => {
		match $e {
			$crate::Poll::Ready(t) => t,
			$crate::Poll::Pending => return $crate::Poll::Pending,
		}
	};
}
// const MAX_PACKET_LEN: usize = 8 * 2 ^ 16;

#[pin_project]
pub struct RkyvCodec<Inner: AsyncRead + AsyncWrite, Packet: rkyv::Archive> {
	#[pin]
	inner: Inner,
	sink_buffer: AlignedVec,
	_packet: PhantomData<Packet>,
}
impl<Inner, Packet> RkyvCodec<Inner, Packet>
where
	Inner: AsyncRead + AsyncWrite,
	Packet: rkyv::Archive<Archived: for<'d> CheckBytes<DefaultValidator<'d>>>,
{
	pub fn new(inner: Inner) -> Self {
		Self {
			inner,
			_packet: Default::default(),
			sink_buffer: Default::default(),
		}
	}
	pub fn into_inner(self) -> Inner { self.inner }
}

/// Reads a single Archived Packet into as passed buffer from an AsyncRead
pub async fn stream<'b, Inner: AsyncRead + Unpin, Packet>(mut inner: &mut Inner, buffer: &'b mut AlignedVec) -> Result<&'b Archived<Packet>, PacketCodecError>
where
	Inner: AsyncRead + AsyncWrite,
	Packet: rkyv::Archive<Archived: for<'d> CheckBytes<DefaultValidator<'d>> + 'b>,
{
	let archive_len = unsigned_varint::aio::read_usize(&mut inner).await?;
	buffer.reserve(archive_len - buffer.len()); // Reserve at least the amount of bytes needed
	unsafe { buffer.set_len(archive_len); } // Safety: Already reserved the required space

	inner.read_exact(buffer).await?;
	let archive = rkyv::check_archived_root::<'b, Packet>(buffer).map_err(|_|PacketCodecError::CheckArchiveError)?;
	Ok(archive)
}

impl<Inner, Packet> Sink<Packet> for RkyvCodec<Inner, Packet>
where
	Inner: AsyncRead + AsyncWrite,
	Packet: Archive + for<'b> Serialize<WriteSerializer<&'b mut AlignedVec>>,
{
	type Error = PacketCodecError;

	fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		self.project().inner.poll_flush(cx).map_err(|e|PacketCodecError::IoError(e))
	}

	fn start_send(self: Pin<&mut Self>, item: Packet) -> Result<(), Self::Error> {
		let mut serializer = WriteSerializer::new(self.project().sink_buffer);
		serializer.serialize_value(&item).map_err(|_|PacketCodecError::CheckArchiveError)?;
		Ok(())
	}

	fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		let mut this = self.project();

		let mut slice_left = &this.sink_buffer[..];
		while !slice_left.is_empty() {
			let bytes_written = ready!(Pin::new(&mut this.inner).poll_write(cx, slice_left))?;
			if bytes_written == 0 {
				return Poll::Ready(Err(PacketCodecError::EOFError))
			}
			slice_left.advance(bytes_written);
			
		}
		ready!(this.inner.poll_flush(cx)?);
		Poll::Ready(Ok(()))
	}

	fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		self.project().inner.poll_close(cx).map_err(|e|PacketCodecError::IoError(e))
	}
}