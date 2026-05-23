use std::error::Error;
use std::io::{Error as IoError, ErrorKind};
use std::path::Path;

use fastpg_server::{DEFAULT_ADDR, serve_addr};
use tokio::signal;

const POSTGRES_SAFE_THREAD_STACK_SIZE: usize = 8 * 1024 * 1024;
const TOKIO_WORKER_THREADS_ENV: &str = "FASTPG_TOKIO_WORKER_THREADS";
const DEFAULT_TOKIO_WORKER_THREADS: usize = 1;

fn main() -> Result<(), Box<dyn Error>> {
    let worker_threads = tokio_worker_threads();
    let runtime = if worker_threads == 1 {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .thread_stack_size(POSTGRES_SAFE_THREAD_STACK_SIZE)
            .build()?
    } else {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_stack_size(POSTGRES_SAFE_THREAD_STACK_SIZE)
            .worker_threads(worker_threads)
            .build()?
    };
    runtime.block_on(run())
}

fn tokio_worker_threads() -> usize {
    std::env::var(TOKIO_WORKER_THREADS_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|threads| *threads > 0)
        .unwrap_or(DEFAULT_TOKIO_WORKER_THREADS)
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
    if postgres_catalog_enabled() {
        validate_postgres_catalog_mode()
    } else {
        Ok(())
    }
}

fn postgres_catalog_enabled() -> bool {
    !cfg!(feature = "rust-catalog")
}

fn validate_postgres_catalog_mode() -> Result<(), Box<dyn Error>> {
    let pgdata = std::env::var("FASTPG_PGDATA").map_err(|_| {
        IoError::new(
            ErrorKind::InvalidInput,
            "Postgres catalog mode requires FASTPG_PGDATA",
        )
    })?;
    if pgdata.is_empty() {
        return Err(invalid_input(
            "Postgres catalog mode requires non-empty FASTPG_PGDATA",
        ));
    }
    let pgdata = Path::new(&pgdata);
    if !pgdata.is_absolute() {
        return Err(invalid_input(
            "Postgres catalog mode requires absolute FASTPG_PGDATA",
        ));
    }
    if !pgdata.is_dir() {
        return Err(invalid_input(format!(
            "Postgres catalog mode FASTPG_PGDATA does not exist or is not a directory: {}",
            pgdata.display()
        )));
    }
    Ok(())
}

fn invalid_input(message: impl Into<String>) -> Box<dyn Error> {
    IoError::new(ErrorKind::InvalidInput, message.into()).into()
}
