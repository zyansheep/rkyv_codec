use std::{fmt, io::Write};

use futures::{SinkExt, FutureExt};
use async_std::{io, net::TcpStream};

use rkyv::{AlignedVec, Archive, Deserialize, Infallible, Serialize};
use bytecheck::CheckBytes;

use rkyv_codec::{archive_stream, RkyvWriter, VarintLength};

use rustyline_async::{Readline, ReadlineError};

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
// This will generate a PartialEq impl between our unarchived and archived types
#[archive(compare(PartialEq))]
// To use the safe API, you have to derive CheckBytes for the archived type
#[archive_attr(derive(CheckBytes, Debug))]
struct ChatMessage {
	sender: Option<String>,
	message: String,
}
impl fmt::Display for ChatMessage {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "<{}>: {}", self.sender.as_deref().unwrap_or("Anonymous"), self.message)
	}
}

#[async_std::main]
async fn main() -> io::Result<()> {
	// Connect to server
	let mut tcp_stream = TcpStream::connect("127.0.0.1:8080").await?;
	println!("Connected to {}", &tcp_stream.peer_addr()?);

	// Setup outgoing packet stream
	let mut packet_sender = RkyvWriter::<_, VarintLength>::new(tcp_stream.clone());

	// Incoming packet buffer
	let mut buffer = AlignedVec::new();

	let (mut rl, mut writer) = Readline::new("> ".to_owned()).unwrap();
	
	loop {
		futures::select! {
			archive = archive_stream::<_, ChatMessage, VarintLength>(&mut tcp_stream, &mut buffer).fuse() => match archive {
				Ok(archive) => {
					let message: ChatMessage = archive.deserialize(&mut Infallible).unwrap();
					writeln!(writer, "{message}")?;
				}
				Err(err) => {
					writeln!(writer, "error parsing message: {err}")?;
					break;
				}
			},
			line = rl.readline().fuse() => match line {
				Ok(line) => {
					let message = ChatMessage {
						sender: None,
						message: line,
					};
					packet_sender.send(&message).await.unwrap();
				}
				Err(ReadlineError::Interrupted) => {
					writeln!(writer, "CTRL-C, do CTRL-D to exit")?;
				},
				Err(ReadlineError::Eof) => {
					writeln!(writer, "CTRL-D")?;
					break
				},
				Err(err) => write!(writer, "error reading input: {err}")?,
			}
		}
	}

	Ok(())
}
