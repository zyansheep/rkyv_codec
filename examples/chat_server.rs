use std::net::SocketAddr;

use anyhow::Context;
use async_broadcast::{Receiver, Sender};
use async_std::{
	io,
	net::{TcpListener, TcpStream},
	task,
};
use futures::{prelude::*, SinkExt, StreamExt};

use rkyv::{AlignedVec, Archive, Deserialize, Infallible, Serialize};

use rkyv_codec::{archive_stream, RkyvWriter, VarintLength};

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
// This will generate a PartialEq impl between our unarchived and archived types
// To use the safe API, you have to enable the check_bytes option for the archive
#[archive(compare(PartialEq), check_bytes)]
#[archive_attr(derive(Debug))]
struct ChatMessage {
	sender: Option<String>,
	message: String,
}

// Process a given TcpStream
async fn process(
	stream: TcpStream,
	outgoing: Sender<ChatMessage>,
	mut incoming: Receiver<ChatMessage>,
	addr: &SocketAddr,
) -> anyhow::Result<()> {
	println!("[{addr}] Joined Server");
	outgoing
		.broadcast(ChatMessage {
			sender: Some("Sever".to_owned()),
			message: format!("{} Joined the Chat!", addr),
		})
		.await?;

	let mut reader = stream.clone();
	let mut writer = RkyvWriter::<_, VarintLength>::new(stream);

	let mut buffer = AlignedVec::new();

	loop {
		futures::select! {
			// Read incoming messages
			archive = archive_stream::<_, ChatMessage, VarintLength>(&mut reader, &mut buffer).fuse() => match archive {
				Ok(archive) => {
					let mut msg: ChatMessage = archive.deserialize(&mut Infallible)?;
					msg.sender = Some(format!("{addr}"));
					println!("[{addr}] sent {msg:?}");
					outgoing.broadcast(msg).await?;
				}
				_ => break,
			},
			msg = incoming.next().fuse() => {
				writer.send(&msg.context("incoming channel closed")?).await.unwrap()
			}
		}
	}

	println!("[{addr}] Left Server");
	outgoing
		.broadcast(ChatMessage {
			sender: Some("Sever".to_owned()),
			message: format!("{} Left the Chat!", addr),
		})
		.await?;

	Ok(())
}

#[async_std::main]
async fn main() -> io::Result<()> {
	let listener = TcpListener::bind("127.0.0.1:8080").await?;
	println!("Listening on {}", listener.local_addr()?);

	let mut incoming = listener.incoming();

	// Broadcast channels
	let (broadcast_sender, broadcast_receiver) = async_broadcast::broadcast::<ChatMessage>(20);

	// Listen for incoming connections
	while let Some(stream) = incoming.next().await {
		match stream {
			Ok(stream) => {
				let outgoing = broadcast_sender.clone();
				let incoming = broadcast_receiver.clone();

				task::spawn(async move {
					let addr = stream.peer_addr().unwrap();
					if let Err(err) = process(stream, outgoing, incoming, &addr).await {
						println!("[{addr}] error: {err}")
					}
				});
			}
			Err(err) => println!("error: {err}"),
		};
	}

	Ok(())
}
