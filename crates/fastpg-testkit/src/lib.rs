#![forbid(unsafe_code)]

use std::io;
use std::net::SocketAddr;

use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestServerConfig {
    pub addr: String,
}

#[derive(Debug)]
pub struct TestServer {
    addr: SocketAddr,
    server_task: JoinHandle<io::Result<()>>,
}

impl TestServer {
    pub async fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let server_task = tokio::spawn(fastpg_server::serve_listener(listener));

        Ok(Self { addr, server_task })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn connection_string(&self) -> String {
        format!("postgres://{}/postgres", self.addr)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.server_task.abort();
    }
}
