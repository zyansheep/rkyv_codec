use std::net::SocketAddr;

use anyhow::Context;
use async_broadcast::{Receiver, Sender};
use async_std::{
	io,
	net::{TcpListener, TcpStream},
	task,
};
use futures::{SinkExt, StreamExt, prelude::*};

use rkyv::{Archive, Deserialize, Serialize, rancor, util::AlignedVec};

use rkyv_codec::{RkyvWriter, VarintLength, archive_stream};

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
// This will generate a PartialEq impl between our unarchived and archived types
// To use the safe API, you have to enable the check_bytes option for the archive
#[rkyv(compare(PartialEq), attr(derive(Debug)))]
struct ChatMessage {
	sender: Option<String>,
	message: String,
}

// Process a given TcpStream
async fn handle_conn(
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

	let reader = stream.clone();
	let mut writer = RkyvWriter::<_, VarintLength>::new(stream);

	let mut buffer = AlignedVec::new();

	let mut reader = futures::io::BufReader::new(reader);

	loop {
		futures::select! {
			// Read incoming messages
			archive = archive_stream::<_, ChatMessage, VarintLength>(&mut reader, &mut buffer).fuse() => match archive {
				Ok(archive) => {
					let mut msg: ChatMessage = rkyv::deserialize::<ChatMessage, rancor::Error>(archive).unwrap();
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
	let listener = TcpListener::bind("127.0.0.1:8080").await?; // Bind to local addr
	println!("Listening on {}", listener.local_addr()?);

	// Init broadcast channels
	let (broadcast_sender, broadcast_receiver) = async_broadcast::broadcast::<ChatMessage>(20);

	// Listen for incoming connections
	let mut incoming = listener.incoming();
	while let Some(stream) = incoming.next().await {
		match stream {
			Ok(stream) => {
				let outgoing = broadcast_sender.clone(); // clone the channels
				let incoming = broadcast_receiver.clone();

				task::spawn(async move {
					// spawn a greenthread to handle the connection
					let addr = stream.peer_addr().unwrap();
					if let Err(err) = handle_conn(stream, outgoing, incoming, &addr).await {
						println!("[{addr}] error: {err}")
					}
				});
			}
			Err(err) => println!("error: {err}"),
		};
	}

	Ok(())
}
