use std::{fmt, io::Write};

use async_std::{io, net::TcpStream};
use futures::{FutureExt, SinkExt};

use rkyv::{Archive, Deserialize, Serialize, rancor, util::AlignedVec};

use rkyv_codec::{RkyvWriter, VarintLength, archive_stream};

use rustyline_async::{Readline, ReadlineEvent};

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Clone)]
// This will generate a PartialEq impl between our unarchived and archived types
// To use the safe API, you must use the check_bytes option for the archive
#[rkyv(compare(PartialEq), attr(derive(Debug)))]
struct ChatMessage {
	sender: Option<String>,
	message: String,
}
impl fmt::Display for ChatMessage {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(
			f,
			"<{}>: {}",
			self.sender.as_deref().unwrap_or("Anonymous"),
			self.message
		)
	}
}

#[async_std::main]
async fn main() -> io::Result<()> {
	// Connect to server
	let tcp_stream = TcpStream::connect("127.0.0.1:8080").await?;
	println!("Connected to {}", &tcp_stream.peer_addr()?);

	// Setup outgoing packet stream
	let mut packet_sender = RkyvWriter::<_, VarintLength>::new(tcp_stream.clone());

	// Incoming packet buffer
	let mut buffer = AlignedVec::new();

	let (mut rl, mut writer) = Readline::new("> ".to_owned()).unwrap();

	// buffer the tcp stream
	let mut tcp_stream = futures::io::BufReader::new(tcp_stream);

	loop {
		futures::select! {
			archive = archive_stream::<_, ChatMessage, VarintLength>(&mut tcp_stream, &mut buffer).fuse() => match archive {
				Ok(archive) => {
					let message: ChatMessage = rkyv::deserialize::<_, rancor::Error>(archive).unwrap();
					writeln!(writer, "{message}")?;
				}
				Err(err) => {
					writeln!(writer, "error parsing message: {err}")?;
					break;
				}
			},
			line = rl.readline().fuse() => match line {
				Ok(ReadlineEvent::Line(line)) => {
					let message = ChatMessage {
						sender: None,
						message: line,
					};
					packet_sender.send(&message).await.unwrap();
				}
				Ok(ReadlineEvent::Interrupted) => {
					writeln!(writer, "CTRL-C, do CTRL-D to exit")?;
				},
				Ok(ReadlineEvent::Eof) => {
					writeln!(writer, "CTRL-D")?;
					break
				},
				Err(err) => { write!(writer, "error reading input: {err}")?; break },
			}
		}
	}

	let _ = rl.flush();

	Ok(())
}
