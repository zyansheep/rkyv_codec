use std::{net::SocketAddr, sync::Arc};

use async_std::{
	channel::{bounded, Receiver, Sender, TrySendError},
	io,
	net::{TcpListener, TcpStream},
	sync::Mutex,
	task,
};
use futures::{prelude::*, SinkExt, StreamExt};

use bytecheck::CheckBytes;
use rkyv::{AlignedVec, Archive, Deserialize, Infallible, Serialize};

use anyhow::Context;

use rkyv_codec::{archive_stream, RkyvWriter, VarintLength};

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
// This will generate a PartialEq impl between our unarchived and archived types
#[archive(compare(PartialEq))]
// To use the safe API, you have to derive CheckBytes for the archived type
#[archive_attr(derive(CheckBytes, Debug))]
struct ChatMessage {
	sender: Option<String>,
	message: String,
}

async fn process(
	stream: TcpStream,
	outgoing: Sender<ChatMessage>,
	mut incoming: Receiver<ChatMessage>,
	addr: &SocketAddr,
) -> anyhow::Result<()> {
	println!("[{addr}] Joined Server");
	outgoing.send(ChatMessage { sender: Some("Sever".to_owned()), message: format!("{} Joined the Chat!", addr) }).await?;

	let mut reader = stream.clone();

	let mut writer = RkyvWriter::<_, VarintLength>::new(stream);

	let mut buffer = AlignedVec::new();

	loop {
		futures::select! {
			archive = archive_stream::<_, ChatMessage, VarintLength>(&mut reader, &mut buffer).fuse() => match archive {
				Ok(archive) => {
					let mut msg: ChatMessage = archive.deserialize(&mut Infallible)?;
					msg.sender = Some(format!("{addr}"));
					println!("[{addr}] sent {msg:?}");
					outgoing.send(msg).await?;
				}
				_ => break,
			},
			msg = incoming.next().fuse() => {
				writer.send(&msg.context("incoming channel closed")?).await.unwrap()
			}
		}
	}

	Ok(())
}

#[async_std::main]
async fn main() -> io::Result<()> {
	let listener = TcpListener::bind("127.0.0.1:8080").await?;
	println!("Listening on {}", listener.local_addr()?);

	let mut incoming = listener.incoming();

	let (broadcast_sender, message_receiver) = bounded::<ChatMessage>(20);

	// Broadcast incoming messages to everyone else connected to the server.
	let outgoing_send_list = Arc::new(Mutex::new(Vec::<Sender<ChatMessage>>::new()));
	let sender_list = outgoing_send_list.clone();
	task::spawn(async move {
		while let Ok(msg) = message_receiver.recv().await {
			outgoing_send_list.lock().await.retain(|sender| {
				if let Err(TrySendError::Closed(_)) = sender.try_send(msg.clone()) {
					false
				} else {
					true
				}
			})
		}
	});

	while let Some(stream) = incoming.next().await {
		let stream = match stream {
			Ok(stream) => stream,
			Err(err) => {
				println!("error: {err}");
				continue;
			}
		};
		let outgoing = broadcast_sender.clone();

		let (sender, incoming) = bounded(20);

		sender_list.lock().await.push(sender);
		task::spawn(async move {
			let addr = stream.peer_addr().unwrap();
			if let Err(err) = process(stream, outgoing, incoming, &addr).await {
				println!("[{addr}] error: {err}")
			}
		});
	}
	Ok(())
}
