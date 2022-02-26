use async_std::{io, task};
use async_std::net::{TcpListener, TcpStream};
use async_std::prelude::*;

async fn process(stream: TcpStream) -> io::Result<()> {
    println!("Accepted from: {}", stream.peer_addr()?);

    let mut reader = stream.clone();
    let mut writer = stream;
    io::copy(&mut reader, &mut writer).await?;

    Ok(())
}

#[async_std::main]
async fn main() -> io::Result<()> {
	let listener = TcpListener::bind("127.0.0.1:8080").await?;
	println!("Listening on {}", listener.local_addr()?);

	let mut incoming = listener.incoming();

	while let Some(stream) = incoming.next().await {
		let stream = stream?;
		task::spawn(async {
			process(stream).await.unwrap();
		});
	}
	Ok(())
}