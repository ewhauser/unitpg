use std::error::Error;

use fastpg_server::{DEFAULT_ADDR, serve_addr};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let addr = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("FASTPG_ADDR").ok())
        .unwrap_or_else(|| DEFAULT_ADDR.to_owned());

    eprintln!("fastpg listening on {addr}");
    serve_addr(&addr).await?;
    Ok(())
}
