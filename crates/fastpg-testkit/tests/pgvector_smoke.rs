#![cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]

use std::{error::Error, io, sync::Arc};

use fastpg_testkit::TestServer;
use tokio::sync::Barrier;
use tokio_postgres::{NoTls, SimpleQueryMessage};

const CLIENTS: usize = 6;
const ROWS_PER_CLIENT: usize = 8;
type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pgvector_scalar_storage_and_concurrency_smoke() -> TestResult {
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let table = format!("fastpg_pgvector_smoke_{}", std::process::id());

    client
        .simple_query("CREATE EXTENSION IF NOT EXISTS vector")
        .await?;
    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    client
        .simple_query(&format!(
            "CREATE TABLE {table}(id int not null, embedding vector(3))"
        ))
        .await?;

    let metrics_before = fastpg_pgcore::pgcore_lane_metrics();
    let barrier = Arc::new(Barrier::new(CLIENTS));
    let mut tasks = Vec::with_capacity(CLIENTS);

    for client_index in 0..CLIENTS {
        let connection_string = server.connection_string();
        let table = table.clone();
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            run_pgvector_client(client_index, &connection_string, &table, barrier).await
        }));
    }

    for task in tasks {
        task.await??;
    }

    let messages = client
        .simple_query(&format!("SELECT count(*) FROM {table}"))
        .await?;
    let rows = rows_only(&messages);
    assert_eq!(rows.len(), 1);
    let expected_count = (CLIENTS * ROWS_PER_CLIENT).to_string();
    assert_eq!(rows[0].get("count"), Some(expected_count.as_str()));

    let metrics_after = fastpg_pgcore::pgcore_lane_metrics();
    assert!(
        metrics_after.operations >= metrics_before.operations + (CLIENTS * ROWS_PER_CLIENT) as u64,
        "expected pgcore metrics to observe the pgvector workload, before={metrics_before:?}, after={metrics_after:?}"
    );
    assert_eq!(
        metrics_after.max_active,
        metrics_before.max_active.max(1),
        "expected postgres-catalog pgcore execution to remain serialized under concurrent pgvector clients, before={metrics_before:?}, after={metrics_after:?}"
    );

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pgvector_ivfflat_index_definition_smoke() -> TestResult {
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let table = format!("fastpg_pgvector_ivfflat_{}", std::process::id());
    let index = format!("fastpg_pgvector_ivfflat_idx_{}", std::process::id());

    client
        .simple_query("CREATE EXTENSION IF NOT EXISTS vector")
        .await?;
    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    client
        .simple_query(&format!(
            "CREATE TABLE {table}(id int not null, embedding vector(3))"
        ))
        .await?;
    client
        .simple_query(&format!(
            "INSERT INTO {table} VALUES (1, '[1,2,3]'::vector)"
        ))
        .await?;
    client
        .simple_query(&format!(
            "CREATE INDEX {index} ON {table} USING ivfflat \
             (embedding vector_cosine_ops) WITH (lists = 1)"
        ))
        .await?;
    client
        .simple_query(&format!(
            "INSERT INTO {table} VALUES (2, '[3,2,1]'::vector)"
        ))
        .await?;
    client.simple_query(&format!("ANALYZE {table}")).await?;

    let messages = client
        .simple_query(&format!("SELECT count(*) AS row_count FROM {table}"))
        .await?;
    let rows = rows_only(&messages);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("row_count"), Some("2"));

    let messages = client
        .simple_query(&format!(
            "SELECT count(*) AS index_count FROM pg_indexes \
             WHERE tablename = '{table}' AND indexname = '{index}'"
        ))
        .await?;
    let rows = rows_only(&messages);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("index_count"), Some("1"));

    let messages = client
        .simple_query(&format!(
            "SELECT id FROM {table} \
             ORDER BY embedding <=> '[1,2,3]'::vector, id \
             LIMIT 1"
        ))
        .await?;
    let rows = rows_only(&messages);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("id"), Some("1"));

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    drop(client);
    connection_task.abort();

    Ok(())
}

async fn run_pgvector_client(
    client_index: usize,
    connection_string: &str,
    table: &str,
    barrier: Arc<Barrier>,
) -> TestResult {
    let (client, connection) = tokio_postgres::connect(connection_string, NoTls).await?;
    let connection_task = tokio::spawn(connection);
    barrier.wait().await;

    for row_index in 0..ROWS_PER_CLIENT {
        let id = (client_index * 1_000 + row_index) as i32;
        let base = client_index as i32 * 100;
        let vector = format!("[{},{},{}]", base, row_index, base + row_index as i32);
        client
            .simple_query(&format!(
                "INSERT INTO {table} VALUES ({id}, '{vector}'::vector)"
            ))
            .await
            .map_err(|error| client_step_error(client_index, row_index, "insert", error))?;

        let messages = client
            .simple_query(&format!(
                "SELECT id, embedding::text AS embedding, \
                 vector_dims(embedding) AS dims, \
                 (embedding <-> '{vector}'::vector)::text AS distance \
                 FROM {table} \
                 ORDER BY embedding <-> '{vector}'::vector, id \
                 LIMIT 1"
            ))
            .await
            .map_err(|error| client_step_error(client_index, row_index, "nearest query", error))?;
        let rows = rows_only(&messages);
        assert_eq!(rows.len(), 1);
        let expected_id = id.to_string();
        assert_eq!(rows[0].get("id"), Some(expected_id.as_str()));
        assert_eq!(rows[0].get("embedding"), Some(vector.as_str()));
        assert_eq!(rows[0].get("dims"), Some("3"));
        assert_eq!(rows[0].get("distance"), Some("0"));
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

fn client_step_error(
    client_index: usize,
    row_index: usize,
    phase: &'static str,
    error: tokio_postgres::Error,
) -> Box<dyn Error + Send + Sync> {
    Box::new(io::Error::other(format!(
        "client {client_index}, row {row_index}, phase {phase}: {error:?}"
    )))
}
