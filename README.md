# rkyv_codec
<p>
    <a href="https://docs.rs/rkyv_codec">
        <img src="https://img.shields.io/docsrs/rkyv_codec.svg" alt="docs.rs">
    </a>
    <a href="https://crates.io/crates/rkyv_codec">
        <img src="https://img.shields.io/crates/v/rkyv_codec.svg" alt="crates.io">
    </a>
</p>

Simple async codec for [rkyv](https://github.com/rkyv/rkyv). Reuses streaming buffer for maximum speed!

This crate provides a makeshift adaptor for streaming `&Archived<Object>`s from an `AsyncRead` using a reusable external buffer, as well as a `futures::Sink` implementation to serialize `Object`s to an `AsyncWrite`.
It uses multiformat's [unsigned_varint](https://docs.rs/unsigned-varint/latest/unsigned_varint/) for variable-length length encoding by default, but allows for other kinds of length encoding through the `LengthEncoding` trait.
It also supports directly using `bytes::BytesMut` and `#[no_std]` when default features are disabled.

## Examples
This crate has three examples: chat_client, chat_server & no-std. Run the first two at the same time to see a proof-of-concept Archive tcp echo server in action.

To run:

`cargo run --example chat_client`

`cargo run --example chat_server`

Simple usage example (RkyvCodec):
```rust
use rkyv::{Archive, Serialize, Deserialize, rancor, util::AlignedVec};
use rkyv_codec::{RkyvCodec, VarintLength};
use asynchronous_codec::{Framed, Decoder, Encoder};
use bytes::BytesMut;

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
#[rkyv(attr(derive(Debug)))]
struct Test {
    int: u8,
    string: String,
    option: Option<Vec<i32>>,
}

let value = Test {
    int: 42,
    string: "hello world".to_string(),
    option: Some(vec![1, 2, 3, 4]),
};
let mut codec = RkyvCodec::<Test, VarintLength>::default();
let mut buf = BytesMut::new();

// Encoding
codec.encode(&value, &mut buf).unwrap();

// Decoding
let decoded_value = codec.decode(&mut buf).unwrap().unwrap();
assert_eq!(value, decoded_value);
```

Zero-copy archive usage example (RkyvWriter/archive_stream):
```rust
use rkyv::{Archived, util::AlignedVec, Archive, Serialize, Deserialize, rancor};
use rkyv_codec::{archive_stream, RkyvWriter, VarintLength};
use futures::SinkExt;

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
#[rkyv(derive(Debug), compare(PartialEq))]
struct Test {
    int: u8,
    string: String,
    option: Option<Vec<i32>>,
}

let value = Test {
    int: 42,
    string: "hello world".to_string(),
    option: Some(vec![1, 2, 3, 4]),
};

// Writing
let writer = Vec::new();
let mut codec = RkyvWriter::<_, VarintLength>::new(writer);
codec.send(&value).await.unwrap();

// Reading
let mut reader = &codec.inner()[..];
let mut buffer = AlignedVec::new(); // Aligned streaming buffer for re-use
let value_archived: &Archived<Test> = archive_stream::<_, Test, VarintLength>(&mut reader, &mut buffer).await.unwrap(); // This returns a reference into the passed buffer
// can deserialize as normal as well (or do *partial* deserialization for blazingly fast speeds!)
let value_deserialized: Test = rkyv::deserialize::<_, rancor::Error>(value_archived).unwrap();

assert_eq!(value, *value_archived); // compare to archived version

assert_eq!(value, value_deserialized); // compare to deserialized version
```

See [`examples/no-std`](examples/no-std/src/main.rs) for an example with no-std support.

## Benchmarks

These are a set of benchmarks, each benchmark represents 50 test objects being either sent or received. (requires nightly)
```
test tests::bench_archive_sink_50                   ... bench:       6,276.07 ns/iter (+/- 250.20)
test tests::bench_archive_sink_prearchived_50       ... bench:       2,009.84 ns/iter (+/- 175.23)
test tests::bench_archive_stream_50                 ... bench:       2,110.66 ns/iter (+/- 150.51)
test tests::bench_futures_cbor_sink_50              ... bench:       8,178.37 ns/iter (+/- 123.13)
test tests::bench_futures_cbor_stream_50            ... bench:       6,606.01 ns/iter (+/- 129.89)
test tests::bench_rkyv_asynchronous_codec_sink_50   ... bench:       4,328.91 ns/iter (+/- 415.17)
test tests::bench_rkyv_asynchronous_codec_stream_50 ... bench:       4,059.72 ns/iter (+/- 243.54)
test tests::bench_rkyv_writer_50                    ... bench:       2,228.49 ns/iter (+/- 185.92)
test tests::bench_u64_length_encoding               ... bench:       2,494.63 ns/iter (+/- 143.85)
test tests::bench_varint_length_encoding            ... bench:       4,036.83 ns/iter (+/- 471.98)
```
The fastest real benchmark (full serialization and bytecheck) is using `RkyvWriter` for writing and `archive_stream` for reading.
This is compared to the slowest benchmark: the `asynchronous-codec` library's `CborCodec`.

Feel free to contribute your own benchmarks!