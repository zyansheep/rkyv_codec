use bincode::{Decode, Encode};
use bytes::{Buf, BytesMut};
use criterion::{
	AxisScale, BenchmarkId, Criterion, PlotConfiguration, Throughput,
	async_executor::AsyncStdExecutor, criterion_group, criterion_main,
};
// rkyv imports for the Packet struct
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
// rkyv_codec imports
use rkyv_codec::{
	LengthCodec,
	RkyvCodec,      // The one for asynchronous-codec
	RkyvCodecError, // The error type
	U32Length,
	U64Length,
	VarintLength,
};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use std::{hint::black_box, time::Duration};

// Define a sample data structure to be serialized/deserialized
#[derive(
	Archive,
	RkyvDeserialize,
	RkyvSerialize,
	SerdeDeserialize,
	SerdeSerialize,
	Debug,
	PartialEq,
	Encode,
	Decode,
	Clone,
)]
struct Packet {
	id: u32,
	data: Vec<u8>,
	name: String,
}

fn create_packet(size: usize) -> Packet {
	Packet {
		id: rand::random(),
		data: vec![0u8; size - 50], // Adjust to make overall packet size roughly `size`
		name: "benchmark_packet_name_string_long_enough".to_string(),
	}
}

// --- Rkyv Benchmarking ---
// Adjusted to use asynchronous-codec style with BytesMut
async fn run_rkyv_benchmark<L: LengthCodec + Unpin + Send + Sync + 'static>(
	c: &mut Criterion,
	group_name: &str,
	packet_size: usize,
) where
	for<'a> <L as LengthCodec>::Error: std::fmt::Debug,
	RkyvCodecError<L>: From<rkyv::rancor::Error>, // Ensure RkyvCodecError can be created from rkyv errors
{
	let packet = create_packet(packet_size);
	let mut group = c.benchmark_group(group_name);
	group.throughput(Throughput::Bytes(packet_size as u64));
	group.plot_config(PlotConfiguration::default().summary_scale(AxisScale::Logarithmic));

	group.bench_function(
		BenchmarkId::new(
			format!(
				"rkyv_{}",
				std::any::type_name::<L>().split("::").last().unwrap_or("")
			),
			packet_size,
		),
		|b| {
			b.to_async(AsyncStdExecutor).iter_batched(
				|| {
					let codec: RkyvCodec<Packet, L> = RkyvCodec::default();
					let transport_buf = BytesMut::new();
					(codec, transport_buf, packet.clone())
				},
				|(mut codec, mut transport_buf, p)| async move {
					use asynchronous_codec::{Decoder, Encoder};
					// Encode (Send)
					codec.encode(p, &mut transport_buf).unwrap();

					// Decode (Receive)
					let _received_packet: Option<Packet> =
						codec.decode(&mut transport_buf).unwrap();
					black_box((codec, transport_buf, _received_packet));
				},
				criterion::BatchSize::SmallInput,
			)
		},
	);
	group.finish();
}

// --- Bincode Benchmarking ---
async fn run_bincode_benchmark<L: LengthCodec + Unpin + Send + Sync + 'static>(
	c: &mut Criterion,
	group_name: &str,
	packet_size: usize,
) where
	for<'a> <L as LengthCodec>::Error: std::fmt::Debug,
{
	let packet = create_packet(packet_size);
	let mut group = c.benchmark_group(group_name);
	group.throughput(Throughput::Bytes(packet_size as u64));
	group.plot_config(PlotConfiguration::default().summary_scale(AxisScale::Logarithmic));

	group.bench_function(
		BenchmarkId::new(
			format!(
				"bincode_{}",
				std::any::type_name::<L>().split("::").last().unwrap_or("")
			),
			packet_size,
		),
		|b| {
			b.to_async(AsyncStdExecutor).iter_batched(
				|| {
					let transport = BytesMut::new();
					let mut buf = Vec::new();
					buf.resize(packet_size + 1024, 0);
					// Simulate a framed transport for bincode using LengthCodec manually
					(transport, packet.clone(), buf)
				},
				|(mut transport, p, mut serialize_buf)| async move {
					// Encode
					let bytes_written = bincode::encode_into_slice(
						&p,
						&mut serialize_buf,
						bincode::config::standard(),
					)
					.unwrap();
					let mut length_storage = L::Buffer::default(); // Use the LengthCodec's own buffer type
					let encoded_len_bytes = L::encode(serialize_buf.len(), &mut length_storage);
					transport.extend_from_slice(encoded_len_bytes);
					transport.extend_from_slice(&serialize_buf[..bytes_written]);

					// Decode
					// Simulate reading from the 'transport' BytesMut
					let len = L::decode_bytes(&mut transport).unwrap();

					let data = transport.take(len);
					let _received_packet: Packet = bincode::decode_from_std_read(
						&mut data.reader(),
						bincode::config::standard(),
					)
					.unwrap();

					// For black_box, ensure transport is what's left after reading one message
					// Or, if we want to black_box the state *before* decode, clone transport earlier.
					// Here, transport is modified. If the bench is just one send/recv, this is fine.
					black_box(_received_packet);
				},
				criterion::BatchSize::SmallInput,
			)
		},
	);
	group.finish();
}

fn codec_benchmarks(c: &mut Criterion) {
	let packet_sizes = [64, 256, 1024, 4096, 16384]; // Example packet sizes

	for &size in &packet_sizes {
		// Rkyv
		async_std::task::block_on(run_rkyv_benchmark::<U32Length>(
			c,
			"Codec Comparison (U32Length)",
			size,
		));
		async_std::task::block_on(run_rkyv_benchmark::<U64Length>(
			c,
			"Codec Comparison (U64Length)",
			size,
		));
		async_std::task::block_on(run_rkyv_benchmark::<VarintLength>(
			c,
			"Codec Comparison (VarintLength)",
			size,
		));

		// Bincode
		async_std::task::block_on(run_bincode_benchmark::<U32Length>(
			c,
			"Codec Comparison (U32Length)",
			size,
		));
		async_std::task::block_on(run_bincode_benchmark::<U64Length>(
			c,
			"Codec Comparison (U64Length)",
			size,
		));
		async_std::task::block_on(run_bincode_benchmark::<VarintLength>(
			c,
			"Codec Comparison (VarintLength)",
			size,
		));
	}
}

criterion_group! {
	name = benches;
	config = Criterion::default()
		.measurement_time(Duration::from_millis(100)) // Adjust measurement time
		.warm_up_time(Duration::from_millis(100))    // Adjust warm-up time
		.sample_size(300);                        // Adjust sample size
	targets = codec_benchmarks
}

criterion_main!(benches);
