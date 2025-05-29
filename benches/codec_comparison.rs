use asynchronous_codec::CborCodec;
use bincode::{Decode, Encode};
use bytes::{Buf, BufMut, BytesMut};
use criterion::{
	AxisScale, BenchmarkId, Criterion, PlotConfiguration, Throughput,
	async_executor::AsyncStdExecutor, criterion_group, criterion_main,
};
// rkyv imports for the Packet struct and util functions
use rkyv::{
	Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize, to_bytes, util::AlignedVec,
};

// rkyv_codec imports
use rkyv_codec::{
	LengthCodec,
	RkyvCodec, // RkyvCodecError is implicitly handled by unwrap
	U32Length,
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

// Create a packet with a specific size
fn gen_packet(size: usize) -> Packet {
	Packet {
		id: rand::random(), // Random ID for variety
		data: vec![0u8; size.saturating_sub(size_of::<Packet>() + 12)],
		name: "bench_packet".to_string(), // Fixed short name to reduce overhead variance
	}
}

// Codecs to use
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BenchCodec {
	CborCodec,
	BincodeVarint,
	BincodeU32,
	RkyvCodecVarint,
	RkyvCodecU32,
	RkyvUtilVarint,
}

// Orders
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BenchOrder {
	/// serialize-only
	Ser,
	/// deserialize-only
	De,
	/// serialize + deserialize
	SerDe,
}

/// config for a given bench
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BenchConfig {
	codec: BenchCodec,
	order: BenchOrder,
	packet_size: usize,
	repeat_count: usize,
}

/// state needed for a given bench
enum BenchState {
	CborCodec(CborCodec<Packet, Packet>, BytesMut),
	BincodeVarint(bincode::config::Configuration, BytesMut, Vec<u8>),
	BincodeU32(bincode::config::Configuration, BytesMut, Vec<u8>),
	RkyvCodecVarint(RkyvCodec<Packet, VarintLength>, BytesMut),
	RkyvCodecU32(RkyvCodec<Packet, U32Length>, BytesMut),
	RkyvUtilVarint(BytesMut, Vec<u8>),
}

// gen a bunch of packets given a bench config
fn bench_gen_packets(config: BenchConfig) -> Vec<Packet> {
	std::iter::from_fn(|| Some(gen_packet(config.packet_size)))
		.take(config.repeat_count)
		.collect::<Vec<Packet>>()
}
fn bench_gen_state(config: BenchConfig) -> BenchState {
	let size = config.packet_size * config.repeat_count + 256;
	match config.codec {
		BenchCodec::CborCodec => BenchState::CborCodec(
			CborCodec::<Packet, Packet>::default(),
			BytesMut::with_capacity(size),
		),
		BenchCodec::BincodeVarint => BenchState::BincodeVarint(
			bincode::config::standard(),
			BytesMut::with_capacity(size),
			Vec::new(),
		),
		BenchCodec::BincodeU32 => BenchState::BincodeU32(
			bincode::config::standard(),
			BytesMut::with_capacity(size),
			Vec::new(),
		),
		BenchCodec::RkyvCodecVarint => BenchState::RkyvCodecVarint(
			RkyvCodec::<Packet, VarintLength>::default(),
			BytesMut::new(),
		),
		BenchCodec::RkyvCodecU32 => BenchState::RkyvCodecU32(
			RkyvCodec::<Packet, U32Length>::default(),
			BytesMut::with_capacity(size),
		),
		BenchCodec::RkyvUtilVarint => {
			BenchState::RkyvUtilVarint(BytesMut::with_capacity(size), Vec::new())
		}
	}
}
// take a bunch of packets
fn bench_serialize_packets(
	config: BenchConfig,
	bench_state: &mut BenchState,
	packets: Vec<Packet>,
) {
	match bench_state {
		BenchState::CborCodec(codec, buffer) => {
			for packet in packets {
				asynchronous_codec::Encoder::encode(codec, packet, buffer).unwrap();
			}
		}
		BenchState::BincodeVarint(config, buffer, tmp_buf) => {
			for packet in packets {
				tmp_buf.clear();
				let bytes_written =
					bincode::encode_into_std_write(&packet, tmp_buf, *config).unwrap();
				let mut lb = <VarintLength as LengthCodec>::Buffer::default();
				let len_bytes = VarintLength::encode(bytes_written, &mut lb);
				buffer.extend_from_slice(len_bytes);
				buffer.extend_from_slice(&tmp_buf[..bytes_written]);
			}
		}
		BenchState::BincodeU32(config, buffer, tmp_buf) => {
			for packet in packets {
				tmp_buf.clear();
				let bytes_written =
					bincode::encode_into_std_write(&packet, tmp_buf, *config).unwrap();
				let mut lb = <U32Length as LengthCodec>::Buffer::default();
				let len_bytes = U32Length::encode(bytes_written, &mut lb);
				buffer.extend_from_slice(len_bytes);
				buffer.extend_from_slice(&tmp_buf[..bytes_written]);
			}
		}
		BenchState::RkyvCodecVarint(codec, buffer) => {
			for packet in packets {
				asynchronous_codec::Encoder::encode(codec, packet, buffer).unwrap();
			}
		}
		BenchState::RkyvCodecU32(codec, buffer) => {
			for packet in packets {
				asynchronous_codec::Encoder::encode(codec, packet, buffer).unwrap();
			}
		}
		BenchState::RkyvUtilVarint(buffer, tmp_buf) => {
			let mut buf = tmp_buf;
			for packet in packets {
				buf.clear();
				buf = rkyv::api::high::to_bytes_in::<_, rkyv::rancor::Error>(&packet, buf).unwrap();
				rkyv_codec::archive_sink_bytes::<Packet, VarintLength>(buffer, buf.as_slice())
					.unwrap();
			}
		}
	}
}
// deserialize whatever packets are stored in the benchstate
fn bench_deserialize_packets(bench_state: &mut BenchState) {
	match bench_state {
		BenchState::CborCodec(codec, buffer) => {
			while buffer.has_remaining() {
				let decoded: Option<Packet> =
					asynchronous_codec::Decoder::decode(codec, buffer).unwrap();
				black_box(decoded);
			}
		}
		BenchState::BincodeVarint(config, buffer, _) => {
			while buffer.has_remaining() {
				let len = VarintLength::decode_bytes(buffer).unwrap();
				let data_segment = buffer.copy_to_bytes(len);
				let decoded: (Packet, usize) =
					bincode::decode_from_slice(&data_segment, *config).unwrap();
				black_box(decoded);
			}
		}
		BenchState::BincodeU32(config, buffer, _) => {
			while buffer.has_remaining() {
				let len = U32Length::decode_bytes(buffer).unwrap();
				let data_segment = buffer.copy_to_bytes(len);
				let decoded: (Packet, usize) =
					bincode::decode_from_slice(&data_segment, *config).unwrap();
				black_box(decoded);
			}
		}
		BenchState::RkyvCodecVarint(codec, buffer) => {
			while buffer.has_remaining() {
				let decoded: Option<Packet> =
					asynchronous_codec::Decoder::decode(codec, buffer).unwrap();
				black_box(decoded);
			}
		}
		BenchState::RkyvCodecU32(codec, buffer) => {
			while buffer.has_remaining() {
				let decoded: Option<Packet> =
					asynchronous_codec::Decoder::decode(codec, buffer).unwrap();
				black_box(decoded);
			}
		}
		BenchState::RkyvUtilVarint(buffer, _) => {
			while buffer.has_remaining() {
				let len = VarintLength::decode_bytes(buffer).unwrap();
				let data_segment = buffer.copy_to_bytes(len);
				let archived = rkyv::check_archived_root::<Packet>(&data_segment[..]).unwrap();
				let deserialized_packet: Packet =
					archived.deserialize(&mut rkyv::Infallible).unwrap();
				black_box(deserialized_packet);
			}
		}
	}
}

fn all_codec_benchmarks(c: &mut Criterion) {
	let codecs = [
		BenchCodec::CborCodec,
		BenchCodec::BincodeVarint,
		BenchCodec::BincodeU32,
		BenchCodec::RkyvCodecVarint,
		BenchCodec::RkyvCodecU32,
		BenchCodec::RkyvUtilVarint,
	];
	let packet_data_sizes = [1, 10, 100, 1000, 10000];
	let orders = [BenchOrder::Ser, BenchOrder::De, BenchOrder::SerDe];
	let repeats = [1, 5, 20];

	let mut group = c.benchmark_group("ModularCodecComparison");
	group.plot_config(PlotConfiguration::default().summary_scale(AxisScale::Logarithmic));

	for &codec_variant in &codecs {
		for &packet_data_size in &packet_data_sizes {
			for &order in &orders {
				for &repeat_count in &repeats {
					let config = BenchConfig {
						codec: codec_variant,
						order,
						packet_size: packet_data_size,
						repeat_count,
					};

					let packets = bench_gen_packets(config);

					group.throughput(Throughput::Bytes((packet_data_size * repeat_count) as u64));

					let bench_id_str = format!(
						"{:?}_{:?}_{:?}_{:?}",
						config.codec, config.packet_size, config.order, config.repeat_count
					);

					group.bench_function(BenchmarkId::from_parameter(bench_id_str), |b| {
						let setup = || {
							let p_item_clone = packet.clone();
							let ser_buf = BytesMut::with_capacity(packet_data_size * 2 + 256);
							let de_only_buf_clone = if order == BenchOrder::De {
								pre_serialized_data_for_de_only.clone()
							} else {
								BytesMut::new()
							};
							let ser_de_cycle_buf =
								BytesMut::with_capacity(packet_data_size * 2 + 256);

							let state = match codec_variant {
								BenchCodec::RkyvCodecVarint => {
									CodecState::RkyvVarint(RkyvCodec::default())
								}
								BenchCodec::RkyvCodecU32 => {
									CodecState::RkyvU32(RkyvCodec::default())
								}
								_ => CodecState::Stateless,
							};
							(
								state,
								p_item_clone,
								ser_buf,
								de_only_buf_clone,
								ser_de_cycle_buf,
							)
						};

						let routine = |(
							mut codec_state,
							p_item,
							mut current_ser_buf,
							mut current_de_only_buf,
							mut current_ser_de_cycle_buf,
						): (
							CodecState,
							Packet,
							BytesMut,
							BytesMut,
							BytesMut,
						)| async move {
							for _ in 0..repeat_count {
								match order {
									BenchOrder::Ser => {
										current_ser_buf.clear();
										match codec_state {
											CodecState::RkyvVarint(ref mut codec) => {
												asynchronous_codec::Encoder::encode(
													codec,
													p_item.clone(),
													&mut current_ser_buf,
												)
												.unwrap();
											}
											CodecState::RkyvU32(ref mut codec) => {
												asynchronous_codec::Encoder::encode(
													codec,
													p_item.clone(),
													&mut current_ser_buf,
												)
												.unwrap();
											}
											CodecState::Stateless => match codec_variant {
												BenchCodec::BincodeVarint
												| BenchCodec::BincodeU32 => {
													let mut temp_ser_buf =
														Vec::with_capacity(p_item.data.len() + 128);
													let bytes_written = bincode::encode_into_slice(
														&p_item,
														&mut temp_ser_buf,
														bincode::config::standard(),
													)
													.unwrap();
													let len_bytes = if codec_variant
														== BenchCodec::BincodeVarint
													{
														let mut lb = <VarintLength as LengthCodec>::Buffer::default();
														VarintLength::encode(bytes_written, &mut lb)
													} else {
														let mut lb = <U32Length as LengthCodec>::Buffer::default();
														U32Length::encode(bytes_written, &mut lb)
													};
													current_ser_buf.extend_from_slice(len_bytes);
													current_ser_buf.extend_from_slice(
														&temp_ser_buf[..bytes_written],
													);
												}
												BenchCodec::CborVarint | BenchCodec::CborU32 => {
													let mut temp_ser_buf =
														Vec::with_capacity(p_item.data.len() + 128);
													serde_cbor::to_writer(
														&mut temp_ser_buf,
														&p_item,
													)
													.unwrap();
													let bytes_written = temp_ser_buf.len();
													let len_bytes = if codec_variant
														== BenchCodec::CborVarint
													{
														let mut lb = <VarintLength as LengthCodec>::Buffer::default();
														VarintLength::encode(bytes_written, &mut lb)
													} else {
														let mut lb = <U32Length as LengthCodec>::Buffer::default();
														U32Length::encode(bytes_written, &mut lb)
													};
													current_ser_buf.extend_from_slice(len_bytes);
													current_ser_buf.extend_from_slice(
														&temp_ser_buf[..bytes_written],
													);
												}
												BenchCodec::RkyvUtilVarint => {
													let mut temp_ser_buf =
														Vec::with_capacity(p_item.data.len() + 128);
													let bytes_written =
														archive_sink_bytes::<_, Packet>(
															&p_item,
															&mut temp_ser_buf,
														)
														.unwrap();
													let mut lb = <VarintLength as LengthCodec>::Buffer::default();
													let len_bytes = VarintLength::encode(
														bytes_written,
														&mut lb,
													);
													current_ser_buf.extend_from_slice(len_bytes);
													current_ser_buf.extend_from_slice(
														&temp_ser_buf[..bytes_written],
													);
												}
												_ => unreachable!(
													"Stateless matched RkyvCodec variant"
												),
											},
										}
										black_box(&current_ser_buf);
									}
									BenchOrder::De => {
										let mut de_source_this_repeat = current_de_only_buf.clone();
										match codec_state {
											CodecState::RkyvVarint(ref mut codec) => {
												let decoded: Option<Packet> =
													asynchronous_codec::Decoder::decode(
														codec,
														&mut de_source_this_repeat,
													)
													.unwrap();
												black_box(decoded);
											}
											CodecState::RkyvU32(ref mut codec) => {
												let decoded: Option<Packet> =
													asynchronous_codec::Decoder::decode(
														codec,
														&mut de_source_this_repeat,
													)
													.unwrap();
												black_box(decoded);
											}
											CodecState::Stateless => match codec_variant {
												BenchCodec::BincodeVarint
												| BenchCodec::BincodeU32 => {
													let len = if codec_variant
														== BenchCodec::BincodeVarint
													{
														VarintLength::decode_bytes(
															&mut de_source_this_repeat,
														)
														.unwrap()
													} else {
														U32Length::decode_bytes(
															&mut de_source_this_repeat,
														)
														.unwrap()
													};
													let data_segment =
														de_source_this_repeat.copy_to_bytes(len);
													let decoded: (Packet, usize) =
														bincode::decode_from_slice(
															&data_segment,
															bincode::config::standard(),
														)
														.unwrap();
													black_box(decoded);
												}
												BenchCodec::CborVarint | BenchCodec::CborU32 => {
													let len = if codec_variant
														== BenchCodec::CborVarint
													{
														VarintLength::decode_bytes(
															&mut de_source_this_repeat,
														)
														.unwrap()
													} else {
														U32Length::decode_bytes(
															&mut de_source_this_repeat,
														)
														.unwrap()
													};
													let data_segment =
														de_source_this_repeat.copy_to_bytes(len);
													let decoded: Packet =
														serde_cbor::from_slice(&data_segment)
															.unwrap();
													black_box(decoded);
												}
												BenchCodec::RkyvUtilVarint => {
													let len = VarintLength::decode_bytes(
														&mut de_source_this_repeat,
													)
													.unwrap();
													let data_segment =
														de_source_this_repeat.copy_to_bytes(len);
													let archived = check_archived_root::<Packet>(
														&data_segment[..],
													)
													.unwrap();
													let deserialized_packet: Packet = archived
														.deserialize(&mut Infallible)
														.unwrap();
													black_box(deserialized_packet);
												}
												_ => unreachable!(
													"Stateless matched RkyvCodec variant"
												),
											},
										}
									}
									BenchOrder::SerDe => {
										current_ser_de_cycle_buf.clear();
										// Serialize part
										match codec_state {
											CodecState::RkyvVarint(ref mut codec) => {
												asynchronous_codec::Encoder::encode(
													codec,
													p_item.clone(),
													&mut current_ser_de_cycle_buf,
												)
												.unwrap();
											}
											CodecState::RkyvU32(ref mut codec) => {
												asynchronous_codec::Encoder::encode(
													codec,
													p_item.clone(),
													&mut current_ser_de_cycle_buf,
												)
												.unwrap();
											}
											CodecState::Stateless => match codec_variant {
												BenchCodec::BincodeVarint
												| BenchCodec::BincodeU32 => {
													let mut temp_ser_buf =
														Vec::with_capacity(p_item.data.len() + 128);
													let bytes_written = bincode::encode_into_slice(
														&p_item,
														&mut temp_ser_buf,
														bincode::config::standard(),
													)
													.unwrap();
													let len_bytes = if codec_variant
														== BenchCodec::BincodeVarint
													{
														<VarintLength as LengthCodec>::encode(
															bytes_written,
															&mut Default::default(),
														)
													} else {
														<U32Length as LengthCodec>::encode(
															bytes_written,
															&mut Default::default(),
														)
													};
													current_ser_de_cycle_buf
														.extend_from_slice(len_bytes);
													current_ser_de_cycle_buf.extend_from_slice(
														&temp_ser_buf[..bytes_written],
													);
												}
												BenchCodec::CborVarint | BenchCodec::CborU32 => {
													let mut temp_ser_buf =
														Vec::with_capacity(p_item.data.len() + 128);
													serde_cbor::to_writer(
														&mut temp_ser_buf,
														&p_item,
													)
													.unwrap();
													let bytes_written = temp_ser_buf.len();
													let len_bytes = if codec_variant
														== BenchCodec::CborVarint
													{
														<VarintLength as LengthCodec>::encode(
															bytes_written,
															&mut Default::default(),
														)
													} else {
														<U32Length as LengthCodec>::encode(
															bytes_written,
															&mut Default::default(),
														)
													};
													current_ser_de_cycle_buf
														.extend_from_slice(len_bytes);
													current_ser_de_cycle_buf.extend_from_slice(
														&temp_ser_buf[..bytes_written],
													);
												}
												BenchCodec::RkyvUtilVarint => {
													let mut temp_ser_buf =
														Vec::with_capacity(p_item.data.len() + 128);
													let bytes_written =
														archive_sink_bytes::<_, Packet>(
															&p_item,
															&mut temp_ser_buf,
														)
														.unwrap();
													let len_bytes =
														<VarintLength as LengthCodec>::encode(
															bytes_written,
															&mut Default::default(),
														);
													current_ser_de_cycle_buf
														.extend_from_slice(len_bytes);
													current_ser_de_cycle_buf.extend_from_slice(
														&temp_ser_buf[..bytes_written],
													);
												}
												_ => unreachable!(
													"Stateless matched RkyvCodec variant"
												),
											},
										}

										// Deserialize part
										match codec_state {
											CodecState::RkyvVarint(ref mut codec) => {
												let decoded: Option<Packet> =
													asynchronous_codec::Decoder::decode(
														codec,
														&mut current_ser_de_cycle_buf,
													)
													.unwrap();
												black_box(decoded);
											}
											CodecState::RkyvU32(ref mut codec) => {
												let decoded: Option<Packet> =
													asynchronous_codec::Decoder::decode(
														codec,
														&mut current_ser_de_cycle_buf,
													)
													.unwrap();
												black_box(decoded);
											}
											CodecState::Stateless => match codec_variant {
												BenchCodec::BincodeVarint
												| BenchCodec::BincodeU32 => {
													let len = if codec_variant
														== BenchCodec::BincodeVarint
													{
														VarintLength::decode_bytes(
															&mut current_ser_de_cycle_buf,
														)
														.unwrap()
													} else {
														U32Length::decode_bytes(
															&mut current_ser_de_cycle_buf,
														)
														.unwrap()
													};
													let data_segment =
														current_ser_de_cycle_buf.copy_to_bytes(len);
													let decoded: (Packet, usize) =
														bincode::decode_from_slice(
															&data_segment,
															bincode::config::standard(),
														)
														.unwrap();
													black_box(decoded);
												}
												BenchCodec::CborVarint | BenchCodec::CborU32 => {
													let len = if codec_variant
														== BenchCodec::CborVarint
													{
														VarintLength::decode_bytes(
															&mut current_ser_de_cycle_buf,
														)
														.unwrap()
													} else {
														U32Length::decode_bytes(
															&mut current_ser_de_cycle_buf,
														)
														.unwrap()
													};
													let data_segment =
														current_ser_de_cycle_buf.copy_to_bytes(len);
													let decoded: Packet =
														serde_cbor::from_slice(&data_segment)
															.unwrap();
													black_box(decoded);
												}
												BenchCodec::RkyvUtilVarint => {
													let len = VarintLength::decode_bytes(
														&mut current_ser_de_cycle_buf,
													)
													.unwrap();
													let data_segment =
														current_ser_de_cycle_buf.copy_to_bytes(len);
													let archived = check_archived_root::<Packet>(
														&data_segment[..],
													)
													.unwrap();
													let deserialized_packet: Packet = archived
														.deserialize(&mut Infallible)
														.unwrap();
													black_box(deserialized_packet);
												}
												_ => unreachable!(
													"Stateless matched RkyvCodec variant"
												),
											},
										}
									}
								}
							}
							black_box(());
						};

						b.to_async(AsyncStdExecutor).iter_batched(
							setup,
							routine,
							criterion::BatchSize::SmallInput,
						);
					});
				}
			}
		}
	}
	group.finish();
}

criterion_group! {
	name = benches;
	config = Criterion::default()
		.measurement_time(Duration::from_secs(1))
		.warm_up_time(Duration::from_millis(200))
		.sample_size(200);
	targets = all_codec_benchmarks
}

criterion_main!(benches);
