#![cfg(feature = "postgres-execution")]

use std::error::Error;

use fastpg_testkit::TestServer;
use tokio::sync::Mutex;
use tokio_postgres::{NoTls, SimpleQueryMessage};

static PGWIRE_TEST_MUTEX: Mutex<()> = Mutex::const_new(());

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tokio_postgres_simple_query_smoke() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tokio_postgres_extended_query_smoke() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extended_query_accepts_binary_bigint_array_parameter() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let table = format!("river_migration_{}", std::process::id());

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    client
        .simple_query(&format!(
            "CREATE TABLE {table}(created_at text NOT NULL DEFAULT 'now', version bigint NOT NULL)"
        ))
        .await?;

    let sql = format!(
        "INSERT INTO {table} (version) \
         SELECT unnest($1::bigint[]) \
         RETURNING created_at, version"
    );
    let statement = client.prepare(&sql).await?;
    assert_eq!(statement.columns().len(), 2);
    assert_eq!(statement.columns()[0].name(), "created_at");
    assert_eq!(statement.columns()[1].name(), "version");

    let versions = vec![1_i64];
    let rows = client.query(&statement, &[&versions]).await?;

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].columns().len(), 2);
    let created_at: String = rows[0].try_get("created_at")?;
    let version: i64 = rows[0].try_get("version")?;
    assert_eq!(created_at, "now");
    assert_eq!(version, 1);

    let messages = client
        .simple_query(&format!("SELECT version FROM {table}"))
        .await?;
    let inserted = rows_only(&messages);
    assert_eq!(inserted.len(), 1);
    assert_eq!(inserted[0].get("version"), Some("1"));

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extended_query_accepts_other_binary_parameter_types() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);

    let row = client.query_one("SELECT $1::bool", &[&true]).await?;
    let value: bool = row.get(0);
    assert!(value);

    let bytes = b"a\0b".to_vec();
    let row = client
        .query_one("SELECT encode($1::bytea, 'hex')", &[&bytes])
        .await?;
    let value: String = row.get(0);
    assert_eq!(value, "610062");

    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transactions_are_isolated_per_client() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_keys_see_rows_inserted_earlier_in_transaction() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let suffix = std::process::id();
    let trigger_parent = format!("fastpg_fk_trigger_parent_{suffix}");
    let trigger_child = format!("fastpg_fk_trigger_child_{suffix}");
    let validate_parent = format!("fastpg_fk_validate_parent_{suffix}");
    let validate_child = format!("fastpg_fk_validate_child_{suffix}");
    let validate_constraint = format!("fastpg_fk_validate_{suffix}");

    client
        .simple_query(&format!(
            "DROP TABLE IF EXISTS {trigger_child}, {trigger_parent}, {validate_child}, {validate_parent}"
        ))
        .await?;

    client
        .simple_query(&format!(
            "CREATE TABLE {trigger_parent}(id int PRIMARY KEY)"
        ))
        .await?;
    client
        .simple_query(&format!(
            "CREATE TABLE {trigger_child}(id int REFERENCES {trigger_parent}(id))"
        ))
        .await?;

    client.simple_query("BEGIN").await?;
    client
        .simple_query(&format!("INSERT INTO {trigger_parent} VALUES (1)"))
        .await?;
    client
        .simple_query(&format!("INSERT INTO {trigger_child} VALUES (1)"))
        .await?;
    client.simple_query("COMMIT").await?;
    assert_eq!(count_rows(&client, &trigger_child).await?, 1);

    client
        .simple_query(&format!(
            "CREATE TABLE {validate_parent}(id int PRIMARY KEY)"
        ))
        .await?;
    client
        .simple_query(&format!("CREATE TABLE {validate_child}(id int)"))
        .await?;

    client.simple_query("BEGIN").await?;
    client
        .simple_query(&format!("INSERT INTO {validate_parent} VALUES (1)"))
        .await?;
    client
        .simple_query(&format!("INSERT INTO {validate_child} VALUES (1)"))
        .await?;
    client
        .simple_query(&format!(
            "ALTER TABLE {validate_child} ADD CONSTRAINT {validate_constraint} FOREIGN KEY (id) REFERENCES {validate_parent}(id)"
        ))
        .await?;
    client.simple_query("COMMIT").await?;
    assert_eq!(count_rows(&client, &validate_child).await?, 1);

    client
        .simple_query(&format!(
            "DROP TABLE IF EXISTS {trigger_child}, {trigger_parent}, {validate_child}, {validate_parent}"
        ))
        .await?;
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

async fn count_rows(client: &tokio_postgres::Client, table: &str) -> Result<i64, Box<dyn Error>> {
    let messages = client
        .simple_query(&format!("SELECT count(*) FROM {table}"))
        .await?;
    let rows = rows_only(&messages);
    assert_eq!(rows.len(), 1);
    Ok(rows[0].get("count").expect("count column").parse()?)
}
