#![cfg(feature = "postgres-execution")]

use std::error::Error;

use fastpg_testkit::TestServer;
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

    let messages = client.simple_query("SELECT 2").await?;
    let rows = rows_only(&messages);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some("2"));

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

    let row = client.query_one("SELECT 2", &[]).await?;
    let value: i32 = row.get(0);
    assert_eq!(value, 2);

    let row = client.query_one("SELECT $1::int4", &[&41i32]).await?;
    let value: i32 = row.get(0);
    assert_eq!(value, 41);

    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test]
async fn transactions_are_isolated_per_client() -> Result<(), Box<dyn Error>> {
    let server = TestServer::start().await?;
    let (client_a, connection_a) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let (client_b, connection_b) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task_a = tokio::spawn(connection_a);
    let connection_task_b = tokio::spawn(connection_b);
    let table = format!("fastpg_session_state_{}", std::process::id());

    client_a
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    client_a
        .simple_query(&format!("CREATE TABLE {table}(id int)"))
        .await?;

    client_a.simple_query("BEGIN").await?;
    client_a
        .simple_query(&format!("INSERT INTO {table} VALUES (1)"))
        .await?;
    assert_eq!(count_rows(&client_a, &table).await?, 1);
    assert_eq!(count_rows(&client_b, &table).await?, 0);

    client_a.simple_query("ROLLBACK").await?;
    assert_eq!(count_rows(&client_a, &table).await?, 0);

    client_a.simple_query("BEGIN").await?;
    client_a
        .simple_query(&format!("INSERT INTO {table} VALUES (2)"))
        .await?;
    client_a.simple_query("COMMIT").await?;
    assert_eq!(count_rows(&client_b, &table).await?, 1);

    client_a
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    drop((client_a, client_b));
    connection_task_a.abort();
    connection_task_b.abort();

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

async fn count_rows(client: &tokio_postgres::Client, table: &str) -> Result<i64, Box<dyn Error>> {
    let messages = client
        .simple_query(&format!("SELECT count(*) FROM {table}"))
        .await?;
    let rows = rows_only(&messages);
    assert_eq!(rows.len(), 1);
    Ok(rows[0].get("count").expect("count column").parse()?)
}
