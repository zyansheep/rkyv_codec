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
It uses multiformat's [unsigned_varint](https://docs.rs/unsigned-varint/latest/unsigned_varint/) for variable-length length encoding.

## Examples
This crate has two examples: chat_client & chat_server. Run both of them at the same time to see a proof-of-concept Archive tcp stream in action.

To run:

`cargo run --example chat_client`

`cargo run --example chat_server`

Simple usage example:
```rust
#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
#[archive_attr(derive(CheckBytes, Debug))] // Checkbytes is required
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
let mut codec = RkyvWriter::new(writer);
codec.send(value.clone()).await.unwrap();

// Reading
let mut reader = &codec.inner()[..];
let mut buffer = AlignedVec::new(); // Aligned streaming buffer for re-use
let data: &Archived<Test> = stream::<_, Test>(&mut reader, &mut buffer).await.unwrap(); // This returns a reference into the passed buffer
let value_received: Test = data.deserialize(&mut Infallible).unwrap();

assert_eq!(value, value_received);
```