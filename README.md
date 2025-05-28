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

Simple usage example:
```rust
#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
#[archive(check_bytes)] // check_bytes is required
#[archive_attr(derive(Debug))]
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
let data: &Archived<Test> = archive_stream::<_, Test, VarintLength>(&mut reader, &mut buffer).await.unwrap(); // This returns a reference into the passed buffer
let value_received: Test = data.deserialize(&mut Infallible).unwrap();
assert_eq!(value, value_received);
```

See [`examples/no-std`](examples/no-std/src/main.rs) for an example with no-std support.

## Benchmarks

These are a set of benchmarks, each benchmark represents 50 test objects being either sent or received. (requires nightly)
```
test tests::bench_archive_sink_50              ... bench:       6,347.69 ns/iter (+/- 141.25)
test tests::bench_archive_sink_prearchived_50  ... bench:       1,541.42 ns/iter (+/- 62.43)
test tests::bench_archive_stream_50            ... bench:       2,807.07 ns/iter (+/- 123.06)
test tests::bench_futures_cbor_sink_50         ... bench:       8,926.48 ns/iter (+/- 96.53)
test tests::bench_futures_cbor_stream_50       ... bench:       6,711.30 ns/iter (+/- 58.57)
test tests::bench_rkyv_futures_codec_sink_50   ... bench:       4,326.63 ns/iter (+/- 95.19)
test tests::bench_rkyv_futures_codec_stream_50 ... bench:       4,256.65 ns/iter (+/- 100.94)
test tests::bench_rkyv_writer_50               ... bench:       2,241.17 ns/iter (+/- 143.36)
test tests::bench_u64_length_encoding          ... bench:       4,204.34 ns/iter (+/- 394.26)
test tests::bench_varint_length_encoding       ... bench:       4,271.78 ns/iter (+/- 478.77)
```
The fastest real benchmark (full serialization and bytecheck) is using `RkyvWriter` for writing and `archive_stream` for reading.
This is compared to the slowest benchmark: futures_codec library's `CborCodec`.

Feel free to contribute your own benchmarks!