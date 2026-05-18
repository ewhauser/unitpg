#![cfg(feature = "postgres-execution")]

use std::error::Error;

use fastpg_testkit::TestServer;
use tokio_postgres::NoTls;

const CLIENTS: usize = 8;
const QUERIES_PER_CLIENT: usize = 25;
type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rust_server_handles_concurrent_client_transactions() -> TestResult {
    let server = TestServer::start().await?;
    let mut tasks = Vec::with_capacity(CLIENTS);

    for client_index in 0..CLIENTS {
        let connection_string = server.connection_string();
        tasks.push(tokio::spawn(async move {
            run_client(client_index, &connection_string).await
        }));
    }

    for task in tasks {
        task.await??;
    }

    let metrics = fastpg_pgcore::pgcore_lane_metrics();
    assert!(
        metrics.operations >= (CLIENTS * QUERIES_PER_CLIENT) as u64,
        "expected pgcore lane to observe the concurrent workload, got {metrics:?}"
    );
    assert_eq!(
        metrics.max_active, 1,
        "pgcore lane must serialize PostgreSQL backend globals, got {metrics:?}"
    );

    Ok(())
}

async fn run_client(client_index: usize, connection_string: &str) -> TestResult {
    let (client, connection) = tokio_postgres::connect(connection_string, NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let table = format!("fastpg_torture_{}_{}", std::process::id(), client_index);

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    client
        .simple_query(&format!(
            "CREATE TABLE {table}(id int not null, value int not null)"
        ))
        .await?;

    let mut committed_rows = 0i64;
    for query_index in 0..QUERIES_PER_CLIENT {
        let selected = client
            .query_one(
                "SELECT $1::int4",
                &[&((client_index * 10_000 + query_index) as i32)],
            )
            .await?;
        let selected: i32 = selected.get(0);
        assert_eq!(selected, (client_index * 10_000 + query_index) as i32);

        client.simple_query("BEGIN").await?;
        client
            .execute(
                &format!("INSERT INTO {table} VALUES ($1, $2)"),
                &[&(query_index as i32), &selected],
            )
            .await?;

        let in_transaction_count: i64 = client
            .query_one(&format!("SELECT count(*) FROM {table}"), &[])
            .await?
            .get(0);
        assert_eq!(in_transaction_count, committed_rows + 1);

        if query_index % 2 == 0 {
            client.simple_query("COMMIT").await?;
            committed_rows += 1;
        } else {
            client.simple_query("ROLLBACK").await?;
        }

        let visible_count: i64 = client
            .query_one(&format!("SELECT count(*) FROM {table}"), &[])
            .await?
            .get(0);
        assert_eq!(visible_count, committed_rows);
    }

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    drop(client);
    connection_task.abort();
    Ok(())
}
