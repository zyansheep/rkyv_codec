#![no_std]
extern crate alloc;

use core::mem::MaybeUninit;

use alloc::vec::Vec;

use bytes::BytesMut;
use rkyv::{
	api::low,
	rancor,
	ser::{allocator::SubAllocator, writer::Buffer},
	util::{Align, AlignedVec},
	Archive, Deserialize, Serialize,
};
use rkyv_codec::VarintLength;

#[derive(Debug, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq), attr(derive(Debug)))]
struct Test {
	bytes: Vec<u8>,
	number: u64,
}

fn main() {
	// let archived_bytes = rkyv::a
	// Or you can customize your serialization for better performance
	// and compatibility with #![no_std] environments
	let value = Test {
		bytes: Vec::from_iter([1, 2, 3]),
		number: 42,
	};

	// serialization scratchpad
	let mut output = Align([MaybeUninit::<u8>::uninit(); 256]);
	let mut alloc = [MaybeUninit::<u8>::uninit(); 256];

	let bytes = low::to_bytes_in_with_alloc::<_, _, rancor::Failure>(
		&value,
		Buffer::from(&mut *output),
		SubAllocator::new(&mut alloc),
	)
	.unwrap();

	let mut buffer = BytesMut::with_capacity(1024);

	rkyv_codec::archive_sink_bytes::<Test, VarintLength>(&mut buffer, &bytes[..]).unwrap();
	let mut buffer = buffer.freeze();

	let mut stream_buffer = AlignedVec::new();
	let result = unsafe {
		rkyv_codec::archive_stream_bytes_unsafe::<Test, VarintLength>(
			&mut buffer,
			&mut stream_buffer,
		)
	}
	.unwrap();

	assert_eq!(&value, result);
}
