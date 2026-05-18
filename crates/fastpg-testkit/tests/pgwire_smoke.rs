#![cfg(any(feature = "mini-sql-testkit", feature = "postgres-execution"))]

use std::error::Error;

use fastpg_testkit::TestServer;
#[cfg(feature = "mini-sql-testkit")]
use tokio_postgres::error::SqlState;
use tokio_postgres::{NoTls, SimpleQueryMessage};

#[tokio::test]
async fn tokio_postgres_simple_query_smoke() -> Result<(), Box<dyn Error>> {
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);

    let messages = client.simple_query("SELECT 1").await?;
    let rows = rows_only(&messages);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some("1"));

    client.simple_query("DROP TABLE IF EXISTS smoke").await?;
    client
        .simple_query("CREATE TABLE smoke(id int, value int)")
        .await?;
    let messages = client.simple_query("SELECT count(*) FROM smoke").await?;
    let rows = rows_only(&messages);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("count"), Some("0"));

    #[cfg(feature = "mini-sql-testkit")]
    {
        let messages = client.simple_query("SHOW server_version").await?;
        let rows = rows_only(&messages);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("server_version"), Some("17.0-fastpg"));

        let error = client
            .simple_query("SELECT 2")
            .await
            .expect_err("unsupported SQL should return a Postgres-shaped error");
        assert_eq!(error.code(), Some(&SqlState::FEATURE_NOT_SUPPORTED));
    }

    #[cfg(feature = "postgres-execution")]
    {
        let messages = client.simple_query("SELECT 2").await?;
        let rows = rows_only(&messages);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(0), Some("2"));
    }

    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test]
async fn tokio_postgres_extended_query_smoke() -> Result<(), Box<dyn Error>> {
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);

    let row = client.query_one("SELECT 1", &[]).await?;
    let value: i32 = row.get(0);
    assert_eq!(value, 1);

    #[cfg(feature = "mini-sql-testkit")]
    {
        let row = client.query_one("SHOW server_version", &[]).await?;
        let server_version: String = row.get("server_version");
        assert_eq!(server_version, "17.0-fastpg");

        let row = client.query_one("SELECT $1::int4", &[&41i32]).await?;
        let value: i32 = row.get(0);
        assert_eq!(value, 41);

        let error = client
            .query_one("SELECT 2", &[])
            .await
            .expect_err("unsupported extended SQL should return a Postgres-shaped error");
        assert_eq!(error.code(), Some(&SqlState::FEATURE_NOT_SUPPORTED));
    }

    #[cfg(feature = "postgres-execution")]
    {
        let row = client.query_one("SELECT 2", &[]).await?;
        let value: i32 = row.get(0);
        assert_eq!(value, 2);

        let row = client.query_one("SELECT $1::int4", &[&41i32]).await?;
        let value: i32 = row.get(0);
        assert_eq!(value, 41);
    }

    drop(client);
    connection_task.abort();

    Ok(())
}

fn rows_only(messages: &[SimpleQueryMessage]) -> Vec<&tokio_postgres::SimpleQueryRow> {
    messages
        .iter()
        .filter_map(|message| match message {
            SimpleQueryMessage::Row(row) => Some(row),
            _ => None,
        })
        .collect()
}
