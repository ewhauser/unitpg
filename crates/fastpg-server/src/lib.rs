#![forbid(unsafe_code)]

use std::io;
use std::num::NonZeroUsize;
#[cfg(unix)]
use std::path::Path;
use std::sync::Arc;

#[cfg(unix)]
use fastpg_wire::process_socket_unix;
use fastpg_wire::{FastPgServerHandlers, process_socket};
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::UnixListener;

pub const DEFAULT_ADDR: &str = "127.0.0.1:55432";
pub const EXECUTION_CONCURRENCY_ENV: &str = "FASTPG_EXECUTION_CONCURRENCY";

pub async fn serve_addr(addr: &str) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(path) = addr.strip_prefix("unix:") {
        return serve_unix_path(path).await;
    }

    let listener = TcpListener::bind(addr).await?;
    serve_listener(listener).await
}

pub async fn serve_listener(listener: TcpListener) -> io::Result<()> {
    serve_listener_with_handlers(listener, default_handlers()).await
}

pub async fn serve_listener_with_handlers(
    listener: TcpListener,
    handlers: Arc<FastPgServerHandlers>,
) -> io::Result<()> {
    loop {
        let (socket, peer_addr) = listener.accept().await?;
        let handlers = handlers.clone();

        tokio::spawn(async move {
            if let Err(error) = process_socket(socket, handlers).await {
                eprintln!("fastpg connection {peer_addr} closed with error: {error}");
            }
        });
    }
}

#[cfg(unix)]
pub async fn serve_unix_path(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    serve_unix_listener(listener).await
}

#[cfg(unix)]
pub async fn serve_unix_listener(listener: UnixListener) -> io::Result<()> {
    serve_unix_listener_with_handlers(listener, default_handlers()).await
}

#[cfg(unix)]
pub async fn serve_unix_listener_with_handlers(
    listener: UnixListener,
    handlers: Arc<FastPgServerHandlers>,
) -> io::Result<()> {
    loop {
        let (socket, _peer_addr) = listener.accept().await?;
        let handlers = handlers.clone();

        tokio::spawn(async move {
            if let Err(error) = process_socket_unix(socket, handlers).await {
                eprintln!("fastpg Unix socket connection closed with error: {error}");
            }
        });
    }
}

fn default_handlers() -> Arc<FastPgServerHandlers> {
    Arc::new(FastPgServerHandlers::with_execution_concurrency(
        execution_concurrency_from_env(),
    ))
}

fn execution_concurrency_from_env() -> NonZeroUsize {
    std::env::var(EXECUTION_CONCURRENCY_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .and_then(NonZeroUsize::new)
        .unwrap_or_else(|| std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN))
}
