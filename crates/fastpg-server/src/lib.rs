#![forbid(unsafe_code)]

use std::io;
use std::sync::Arc;

use fastpg_wire::FastPgServerHandlers;
use pgwire::tokio::process_socket;
use tokio::net::TcpListener;

pub const DEFAULT_ADDR: &str = "127.0.0.1:55432";

pub async fn serve_addr(addr: &str) -> io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    serve_listener(listener).await
}

pub async fn serve_listener(listener: TcpListener) -> io::Result<()> {
    serve_listener_with_handlers(listener, Arc::new(FastPgServerHandlers::default())).await
}

pub async fn serve_listener_with_handlers(
    listener: TcpListener,
    handlers: Arc<FastPgServerHandlers>,
) -> io::Result<()> {
    loop {
        let (socket, peer_addr) = listener.accept().await?;
        let handlers = handlers.clone();

        tokio::spawn(async move {
            if let Err(error) = process_socket(socket, None, handlers).await {
                eprintln!("fastpg connection {peer_addr} closed with error: {error}");
            }
        });
    }
}
