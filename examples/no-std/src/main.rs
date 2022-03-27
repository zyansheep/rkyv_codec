#![no_std]
extern crate alloc;

use alloc::vec::Vec;

use bytes::BytesMut;
use rkyv::{AlignedVec, Archive, Deserialize, Serialize};
use rkyv_codec::VarintLength;

#[derive(Debug, PartialEq, Archive, Serialize, Deserialize)]
#[archive(compare(PartialEq))]
#[archive_attr(derive(Debug))]
struct Test {
	bytes: Vec<u8>,
	number: u64,
}

fn main() {
	let mut buffer = BytesMut::with_capacity(1024);
	// let archived_bytes = rkyv::a
	// Or you can customize your serialization for better performance
	// and compatibility with #![no_std] environments
	let value = Test {
		bytes: Vec::from_iter([1, 2, 3]),
		number: 42,
	};

	use rkyv::ser::{serializers::AllocSerializer, Serializer};

	let mut serializer = AllocSerializer::<0>::default();
	serializer.serialize_value(&value).unwrap();
	let bytes = serializer.into_serializer().into_inner();

	rkyv_codec::archive_sink_bytes::<Test, VarintLength>(&mut buffer, &bytes[..]).unwrap();
	let mut buffer = buffer.freeze();

	let mut stream_buffer = AlignedVec::new();
	let result = unsafe { rkyv_codec::archive_stream_bytes_unsafe::<Test, VarintLength>(
		&mut buffer,
		&mut stream_buffer,
	) }
	.unwrap();

	assert_eq!(&value, result);
}
