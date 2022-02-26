use std::fmt;
use std::time::{Duration, Instant};

use async_std::io::{self, BufReader, stdin};
use async_std::net::TcpStream;
use async_std::{prelude::*, task};
use futures::SinkExt;
use rkyv::{AlignedVec, Archive, Deserialize, Infallible, Serialize};
use bytecheck::CheckBytes;
use rkyv_codec::{RkyvWriter, stream};

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
// This will generate a PartialEq impl between our unarchived and archived types
#[archive(compare(PartialEq))]
// To use the safe API, you have to derive CheckBytes for the archived type
#[archive_attr(derive(CheckBytes, Debug))]
struct ChatMessage {
	time_elapsed: Duration,
	message: String,
	index: u32,
}
impl fmt::Display for ChatMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] after {}s: {}", self.index, self.time_elapsed.as_secs(), self.message)
    }
}

#[async_std::main]
async fn main() -> io::Result<()> {
	let mut tcp_stream = TcpStream::connect("127.0.0.1:8080").await?;
	println!("Connected to {}", &tcp_stream.peer_addr()?);

	let mut sender = RkyvWriter::new(tcp_stream.clone());

	task::spawn(async move {
		let mut buffer = AlignedVec::new();
		while let Ok(message) = stream::<'_, _, ChatMessage>(&mut tcp_stream, &mut buffer).await {
			let message: ChatMessage = message.deserialize(&mut Infallible).unwrap();
			println!("{}", message);
		}
	});

	let mut past = Instant::now();
	let mut index = 0;

	let mut lines = BufReader::new(stdin()).lines();

	while let Some(line) = lines.next().await {
		let now = Instant::now();
		
		let message = ChatMessage {
			time_elapsed: now - past,
			message: line?,
			index,
		};
		past = now;
		index += 1;
		
		sender.send(message).await.unwrap();
	}

	Ok(())
}