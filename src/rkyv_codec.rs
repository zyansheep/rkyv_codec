use futures::{ready, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, Sink};
use std::{
	ops::Range,
	pin::Pin,
	task::{Context, Poll},
};

use pin_project::pin_project;

use rkyv::{
	api::{
		high::{HighSerializer, HighValidator},
		serialize_using,
	},
	rancor,
	ser::{
		allocator::{Arena, ArenaHandle},
		sharing::Share,
		Serializer,
	},
	util::AlignedVec,
	Archive, Archived, Portable, Serialize,
};

use crate::{length_codec::LengthCodec, RkyvCodecError};

/// Rewrites a single buffer representing an Archive to an `AsyncWrite`
pub async fn archive_sink<'b, Inner: AsyncWrite + Unpin, L: LengthCodec>(
	inner: &mut Inner,
	archived: &[u8],
) -> Result<(), RkyvCodecError> {
	let length_buf = &mut L::Buffer::default();
	let length_buf = L::encode(archived.len(), length_buf);
	inner.write_all(length_buf).await?;
	inner.write_all(archived).await?;
	Ok(())
}
/// Reads a single `&Archived<Object>` from an `AsyncRead` without checking for correct byte formatting
/// # Safety
/// This may cause undefined behavior if the bytestream is not a valid archive (i.e. not generated through `archive_sink[_bytes]`, or `RkyvWriter`)
///
/// As an optimisation, this function may pass uninitialized bytes to the reader for the reader to read into. Make sure the particular reader in question is implemented correctly and does not read from its passed buffer in the poll_read() function without first writing to it.
/// # Warning
/// Passed buffer is reallocated so it may fit the size of the packet being written. This may allow for DOS attacks if remote sends too large a length encoding
/// # Errors
/// Will return an error if there are not enough bytes to read to read the length of the packet, or read the packet itself. Will also return an error if the length encoding format is invalid.
pub async unsafe fn archive_stream_unsafe<
	'b,
	Inner: AsyncRead + Unpin,
	Packet: Archive + Portable + 'b,
	L: LengthCodec,
>(
	inner: &mut Inner,
	buffer: &'b mut AlignedVec,
) -> Result<&'b Archived<Packet>, RkyvCodecError> {
	buffer.clear();

	// Read Length
	let mut length_buf = L::Buffer::default();
	let length_buf = L::as_slice(&mut length_buf);
	inner.read_exact(&mut *length_buf).await?;
	let (archive_len, unused) =
		L::decode(length_buf).map_err(|_| RkyvCodecError::ReadLengthError)?;

	// If not enough capacity in buffer to fit `archive_len`, reserve more.
	if buffer.capacity() < archive_len {
		buffer.reserve(archive_len - buffer.len())
	}
	// Write any potentially unused bytes from length_buf to buffer
	buffer.extend_from_slice(unused);

	// Safety: Caller should make sure that reader does not read from this potentially uninitialized buffer passed to poll_read()
	unsafe { buffer.set_len(archive_len) }

	// Read into buffer, after any unused length bytes
	inner.read_exact(&mut buffer[unused.len()..]).await?;

	// Safety: Caller should make sure that reader does not produce invalid packets.
	unsafe { Ok(rkyv::access_unchecked(buffer)) }
}

/// Reads a single `&Archived<Object>` from an `AsyncRead` using the passed buffer.
///
/// Until streaming iterators (and streaming futures) are implemented in rust, this currently the fastest method I could come up with that requires no recurring heap allocations.
///
/// Requires rkyv "validation" feature
/// # Safety
/// As an optimisation, this function may pass uninitialized bytes to the reader for the reader to read into. Make sure the particular reader in question is implemented correctly and does not read from its passed buffer in the poll_read() function without first writing to it.
/// # Warning
/// Passed buffer is reallocated so it may fit the size of the packet being written. This may allow for DOS attacks if remote sends too large a length encoding
/// # Errors
/// Will return an error if there are not enough bytes to read to read the length of the packet, or read the packet itself. Will also return an error if the length encoding format is invalid or the packet archive itself is invalid.
pub async fn archive_stream<'b, Inner: AsyncRead + Unpin, Packet, L: LengthCodec>(
	inner: &mut Inner,
	buffer: &'b mut AlignedVec,
) -> Result<&'b Archived<Packet>, RkyvCodecError>
where
	Packet: rkyv::Archive + 'b,
	Packet::Archived: for<'a> rkyv::bytecheck::CheckBytes<HighValidator<'a, rancor::Error>>,
{
	buffer.clear();

	// Read Length
	let mut length_buf = L::Buffer::default();
	let length_buf = L::as_slice(&mut length_buf);
	inner.read_exact(&mut *length_buf).await?;
	let (archive_len, unused) =
		L::decode(length_buf).map_err(|_| RkyvCodecError::ReadLengthError)?;

	// If not enough capacity in buffer to fit `archive_len`, reserve more.
	if buffer.capacity() < archive_len {
		buffer.reserve(archive_len - buffer.len())
	}
	// Write any potentially unused bytes from length_buf to buffer
	buffer.extend_from_slice(unused);

	// Safety: Caller should make sure that reader does not read from this potentially uninitialized buffer passed to poll_read()
	unsafe { buffer.set_len(archive_len) }

	// Read into buffer, after any unused length bytes
	inner.read_exact(&mut buffer[unused.len()..]).await?;

	let archive = rkyv::access::<Packet::Archived, rancor::Error>(buffer)?;

	Ok(archive)
}

/// Wraps an `AsyncWrite` and implements `Sink` to serialize `Archive` objects.
#[pin_project]
pub struct RkyvWriter<Writer: AsyncWrite, L: LengthCodec> {
	#[pin]
	writer: Writer,
	length_buffer: L::Buffer,
	len_state: Range<usize>, // How much of the length buffer has been written
	buf_state: usize, // Whether or not the aligned buf is being written and if so, how much so far
	buffer: Option<AlignedVec>,
	arena: Arena,
	share: Option<Share>,
}

// Safety: This should be safe because while HeapScratch is not Send (because it contains BufferScratch which contains NonNull), that NonNull is not used in a way that violates Send.
unsafe impl<Writer: AsyncWrite, L: LengthCodec> Send for RkyvWriter<Writer, L> {}

impl<Writer: AsyncWrite, L: LengthCodec> RkyvWriter<Writer, L> {
	pub fn new(writer: Writer) -> Self {
		Self {
			writer,
			length_buffer: L::Buffer::default(),
			len_state: Default::default(),
			buf_state: 0,
			buffer: Some(AlignedVec::new()),
			arena: Arena::new(),
			share: Some(Share::new()),
		}
	}
	pub fn inner(self) -> Writer {
		self.writer
	}
}

impl<Writer: AsyncWrite, Packet: std::fmt::Debug, L: LengthCodec> Sink<&Packet>
	for RkyvWriter<Writer, L>
where
	Packet: Archive + for<'b> Serialize<HighSerializer<AlignedVec, ArenaHandle<'b>, rancor::Error>>,
{
	type Error = RkyvCodecError;

	fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		self.project()
			.writer
			.poll_flush(cx)
			.map_err(RkyvCodecError::IoError)
	}

	fn start_send(self: Pin<&mut Self>, item: &Packet) -> Result<(), Self::Error> {
		let this = self.project();
		let buffer_len = {
			// Serializer
			let mut buffer = this.buffer.take().unwrap();
			buffer.clear();
			let share = this.share.take().unwrap();
			let mut serializer = Serializer::new(buffer, this.arena.acquire(), share);
			// serialize
			let _ = serialize_using(item, &mut serializer)?;

			let (buffer, _, share) = serializer.into_raw_parts();
			let buffer_len = buffer.len();
			*this.buffer = Some(buffer);
			*this.share = Some(share);
			buffer_len
		};

		*this.len_state = 0..L::encode(buffer_len, this.length_buffer).len();
		*this.buf_state = 0;

		Ok(())
	}

	fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		let mut this = self.project();

		// keep writing length buffer for as long as is required
		let len_state = this.len_state;
		if len_state.start <= len_state.end {
			let length_buffer = L::as_slice(this.length_buffer);
			let length_buffer = &mut length_buffer[len_state.clone()];

			let written = ready!(Pin::new(&mut this.writer).poll_write(cx, length_buffer)?);
			len_state.start += written;
		}
		let buffer = this.buffer.take().unwrap();

		while *this.buf_state < buffer.len() {
			let buffer_left = &buffer[*this.buf_state..buffer.len()];
			let bytes_written = ready!(Pin::new(&mut this.writer).poll_write(cx, buffer_left))?;
			if bytes_written == 0 {
				return Poll::Ready(Err(RkyvCodecError::EOFError));
			}
			*this.buf_state += bytes_written;
		}

		*this.buffer = Some(buffer);

		ready!(this.writer.poll_flush(cx)?);
		Poll::Ready(Ok(()))
	}

	fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		self.project()
			.writer
			.poll_close(cx)
			.map_err(RkyvCodecError::IoError)
	}
}
