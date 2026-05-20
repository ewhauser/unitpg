use std::error::Error;

use fastpg_server::{DEFAULT_ADDR, serve_addr};

const POSTGRES_SAFE_THREAD_STACK_SIZE: usize = 8 * 1024 * 1024;

fn main() -> Result<(), Box<dyn Error>> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(POSTGRES_SAFE_THREAD_STACK_SIZE)
        .build()?
        .block_on(run())
}

async fn run() -> Result<(), Box<dyn Error>> {
    let addr = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("FASTPG_ADDR").ok())
        .unwrap_or_else(|| DEFAULT_ADDR.to_owned());

    eprintln!("fastpg listening on {addr}");
    serve_addr(&addr).await?;
    Ok(())
}
