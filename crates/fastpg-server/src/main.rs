use std::error::Error;
use std::io::{Error as IoError, ErrorKind};
use std::path::Path;

use fastpg_server::{DEFAULT_ADDR, serve_addr};
use tokio::signal;

const POSTGRES_SAFE_THREAD_STACK_SIZE: usize = 8 * 1024 * 1024;

fn main() -> Result<(), Box<dyn Error>> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(POSTGRES_SAFE_THREAD_STACK_SIZE)
        .build()?
        .block_on(run())
}

async fn run() -> Result<(), Box<dyn Error>> {
    validate_catalog_mode()?;

    let addr = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("FASTPG_ADDR").ok())
        .unwrap_or_else(|| DEFAULT_ADDR.to_owned());

    eprintln!("fastpg listening on {addr}");
    serve_until_shutdown(&addr).await?;
    Ok(())
}

async fn serve_until_shutdown(addr: &str) -> Result<(), Box<dyn Error>> {
    #[cfg(unix)]
    {
        let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = serve_addr(addr) => {
                result?;
            }
            signal_result = signal::ctrl_c() => {
                signal_result?;
            }
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        tokio::select! {
            result = serve_addr(addr) => {
                result?;
            }
            signal_result = signal::ctrl_c() => {
                signal_result?;
            }
        }
    }

    Ok(())
}

fn validate_catalog_mode() -> Result<(), Box<dyn Error>> {
    let catalog_mode = std::env::var("FASTPG_CATALOG_MODE").unwrap_or_else(|_| "rust".to_owned());
    match catalog_mode.as_str() {
        "rust" => Ok(()),
        "postgres" => validate_postgres_catalog_mode(),
        other => Err(invalid_input(format!(
            "FASTPG_CATALOG_MODE must be \"rust\" or \"postgres\", got {other:?}"
        ))),
    }
}

fn validate_postgres_catalog_mode() -> Result<(), Box<dyn Error>> {
    if std::env::var("FASTPG_STORAGE_ENGINE")
        .map(|value| value == "storage2")
        .unwrap_or(false)
    {
        return Err(invalid_input(
            "FASTPG_CATALOG_MODE=postgres currently supports FASTPG_STORAGE_ENGINE=storage1 only",
        ));
    }

    let pgdata = std::env::var("FASTPG_PGDATA").map_err(|_| {
        IoError::new(
            ErrorKind::InvalidInput,
            "FASTPG_CATALOG_MODE=postgres requires FASTPG_PGDATA",
        )
    })?;
    if pgdata.is_empty() {
        return Err(invalid_input(
            "FASTPG_CATALOG_MODE=postgres requires non-empty FASTPG_PGDATA",
        ));
    }
    let pgdata = Path::new(&pgdata);
    if !pgdata.is_absolute() {
        return Err(invalid_input(
            "FASTPG_CATALOG_MODE=postgres requires absolute FASTPG_PGDATA",
        ));
    }
    if !pgdata.is_dir() {
        return Err(invalid_input(format!(
            "FASTPG_CATALOG_MODE=postgres FASTPG_PGDATA does not exist or is not a directory: {}",
            pgdata.display()
        )));
    }
    Ok(())
}

fn invalid_input(message: impl Into<String>) -> Box<dyn Error> {
    IoError::new(ErrorKind::InvalidInput, message.into()).into()
}
