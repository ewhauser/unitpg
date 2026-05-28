use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{Error as IoError, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cli::{PidFileConfig, ServerConfig, StartupAction};
use fastpg_server::serve_addr;
use tokio::signal;

mod cli;

const POSTGRES_SAFE_THREAD_STACK_SIZE: usize = 8 * 1024 * 1024;
const TOKIO_WORKER_THREADS_ENV: &str = "FASTPG_TOKIO_WORKER_THREADS";
const DEFAULT_TOKIO_WORKER_THREADS: usize = 1;
const FASTPG_SERVER_POSTMASTER_PID_ENV: &str = "FASTPG_SERVER_POSTMASTER_PID";

fn main() {
    if let Err(error) = real_main() {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<(), Box<dyn Error>> {
    let config = match cli::parse_cli()? {
        StartupAction::Serve(config) => config,
        StartupAction::PrintAndExit(output) => {
            print!("{output}");
            return Ok(());
        }
    };
    apply_startup_environment(&config)?;

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
    runtime.block_on(run(config))
}

fn tokio_worker_threads() -> usize {
    std::env::var(TOKIO_WORKER_THREADS_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|threads| *threads > 0)
        .unwrap_or(DEFAULT_TOKIO_WORKER_THREADS)
}

async fn run(config: ServerConfig) -> Result<(), Box<dyn Error>> {
    validate_catalog_mode()?;

    let _pid_file = config
        .pid_file
        .as_ref()
        .map(PostmasterPidFile::create)
        .transpose()?;

    eprintln!("fastpg listening on {}", config.addr);
    serve_until_shutdown(&config.addr).await?;
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

fn apply_startup_environment(config: &ServerConfig) -> Result<(), Box<dyn Error>> {
    if let Some(pgdata) = &config.pgdata {
        set_env_var("FASTPG_PGDATA", pgdata);
    }
    if config.pid_file.is_some() {
        set_env_var(FASTPG_SERVER_POSTMASTER_PID_ENV, "1");
    }
    set_default_packaged_pgcore_env()?;
    Ok(())
}

fn set_default_packaged_pgcore_env() -> Result<(), Box<dyn Error>> {
    let exe = std::env::current_exe()?;
    let Some(bindir) = exe.parent() else {
        return Ok(());
    };
    if bindir.file_name().and_then(|name| name.to_str()) != Some("bin") {
        return Ok(());
    }
    let Some(prefix) = bindir.parent() else {
        return Ok(());
    };

    if std::env::var_os("FASTPG_EXEC_PATH").is_none() {
        set_env_var("FASTPG_EXEC_PATH", &exe);
    }
    if std::env::var_os("FASTPG_PGLIBDIR").is_none() && std::env::var_os("PG_LIBDIR").is_none() {
        let pkglibdir = prefix.join("lib/postgresql");
        let libdir = prefix.join("lib");
        if pkglibdir.is_dir() {
            set_env_var("FASTPG_PGLIBDIR", pkglibdir);
        } else if libdir.is_dir() {
            set_env_var("FASTPG_PGLIBDIR", libdir);
        }
    }

    Ok(())
}

fn set_env_var(key: &str, value: impl AsRef<std::ffi::OsStr>) {
    // SAFETY: FastPG sets these process environment variables before creating
    // the Tokio runtime and before pgcore initializes its global PostgreSQL
    // state, so no concurrent environment readers have been started by us yet.
    unsafe {
        std::env::set_var(key, value);
    }
}

struct PostmasterPidFile {
    path: PathBuf,
}

impl PostmasterPidFile {
    fn create(config: &PidFileConfig) -> Result<Self, Box<dyn Error>> {
        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| IoError::other(format!("system clock is before UNIX_EPOCH: {error}")))?
            .as_secs();
        let contents = format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n\nready   \n",
            std::process::id(),
            config.data_dir.display(),
            start_time,
            config.port,
            config.socket_dir,
            config.listen_addr
        );

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&config.path)
            .map_err(|error| {
                IoError::new(
                    error.kind(),
                    format!("could not create {}: {error}", config.path.display()),
                )
            })?;
        file.write_all(contents.as_bytes())?;
        Ok(Self {
            path: config.path.clone(),
        })
    }
}

impl Drop for PostmasterPidFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
