#![cfg(feature = "postgres-execution")]

use std::error::Error;
use std::io::Cursor;
use std::pin::pin;
use std::time::{Duration, SystemTime};

use fastpg_testkit::TestServer;
use futures::SinkExt;
use tokio::sync::Mutex;
use tokio_postgres::binary_copy::BinaryCopyInWriter;
use tokio_postgres::types::Type;
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
            "CREATE TABLE {table}(created_at timestamp NOT NULL DEFAULT current_timestamp, version bigint NOT NULL)"
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
    let _created_at: SystemTime = rows[0].try_get("created_at")?;
    let version: i64 = rows[0].try_get("version")?;
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
async fn extended_query_returns_binary_bool_and_bytea_values() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);

    let bytes = b"a\0b".to_vec();
    let row = client
        .query_one(
            "SELECT $1::bool AS flag, $2::bytea AS payload",
            &[&true, &bytes],
        )
        .await?;
    let flag: bool = row.try_get("flag")?;
    let payload: Vec<u8> = row.try_get("payload")?;

    assert!(flag);
    assert_eq!(payload, bytes);

    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extended_query_returns_mixed_text_and_binary_columns() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let table = format!("river_migration_line_{}", std::process::id());

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    client
        .simple_query(&format!(
            "CREATE TABLE {table}(line text NOT NULL, version bigint NOT NULL, created_at timestamptz NOT NULL DEFAULT now())"
        ))
        .await?;

    let rows = client
        .query(
            &format!(
                "INSERT INTO {table} (line, version) \
                 SELECT $1, unnest($2::bigint[]) \
                 RETURNING line, version, created_at"
            ),
            &[&"main", &vec![5_i64]],
        )
        .await?;

    assert_eq!(rows.len(), 1);
    let line: String = rows[0].try_get("line")?;
    let version: i64 = rows[0].try_get("version")?;
    let _created_at: SystemTime = rows[0].try_get("created_at")?;
    assert_eq!(line, "main");
    assert_eq!(version, 5);

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extended_query_after_river_migration_rebuild_returns_rows() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let suffix = std::process::id();
    let table = format!("fastpg_river_migration_{suffix}");
    let old_table = format!("{table}_old");

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}, {old_table}"))
        .await?;
    client
        .simple_query(&format!(
            "CREATE TABLE {table}( \
               id bigserial PRIMARY KEY, \
               created_at timestamptz NOT NULL DEFAULT now(), \
               version bigint NOT NULL \
             ); \
             INSERT INTO {table}(version) VALUES (1); \
             BEGIN; \
             DO \
             $body$ \
             BEGIN \
               ALTER TABLE {table} RENAME TO {old_table}; \
               CREATE TABLE {table}( \
                 line text NOT NULL, \
                 version bigint NOT NULL, \
                 created_at timestamptz NOT NULL DEFAULT now(), \
                 PRIMARY KEY (line, version) \
               ); \
               INSERT INTO {table}(created_at, line, version) \
               SELECT created_at, 'main', version FROM {old_table}; \
               DROP TABLE {old_table}; \
             END; \
             $body$ \
             LANGUAGE plpgsql"
        ))
        .await?;

    let statement = tokio::time::timeout(
        Duration::from_secs(5),
        client.prepare(&format!(
            "INSERT INTO {table} (line, version) \
             SELECT $1, unnest($2::bigint[]) \
             RETURNING line, version, created_at"
        )),
    )
    .await??;
    assert_eq!(statement.columns().len(), 3);

    let rows = tokio::time::timeout(
        Duration::from_secs(5),
        client.query(&statement, &[&"main", &vec![5_i64]]),
    )
    .await??;
    assert_eq!(rows.len(), 1);
    let line: String = rows[0].try_get("line")?;
    let version: i64 = rows[0].try_get("version")?;
    let _created_at: SystemTime = rows[0].try_get("created_at")?;
    assert_eq!(line, "main");
    assert_eq!(version, 5);

    client.simple_query("ROLLBACK").await?;
    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}, {old_table}"))
        .await?;
    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn binary_copy_from_accepts_non_text_payloads() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let table = format!("fastpg_binary_copy_{}", std::process::id());

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    client
        .simple_query(&format!(
            "CREATE TABLE {table}(id int4 NOT NULL, label text NOT NULL, payload bytea NOT NULL)"
        ))
        .await?;

    let payload = b"a\0b".as_slice();
    let sink = client
        .copy_in(&format!(
            "COPY {table} (id, label, payload) FROM STDIN BINARY"
        ))
        .await?;
    let mut writer = pin!(BinaryCopyInWriter::new(
        sink,
        &[Type::INT4, Type::TEXT, Type::BYTEA]
    ));
    writer
        .as_mut()
        .write(&[&7_i32, &"copied", &payload])
        .await?;
    let copied = writer.finish().await?;
    assert_eq!(copied, 1);

    let row = client
        .query_one(&format!("SELECT id, label, payload FROM {table}"), &[])
        .await?;
    let id: i32 = row.try_get("id")?;
    let label: String = row.try_get("label")?;
    let stored_payload: Vec<u8> = row.try_get("payload")?;
    assert_eq!(id, 7);
    assert_eq!(label, "copied");
    assert_eq!(stored_payload, payload);

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn binary_copy_from_accepts_river_job_shaped_rows() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let suffix = std::process::id();
    let table = format!("fastpg_copy_river_job_{suffix}");
    let state_type = format!("fastpg_copy_river_job_state_{suffix}");
    let function = format!("fastpg_copy_river_job_notify_{suffix}");

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table} CASCADE"))
        .await?;
    client
        .simple_query(&format!("DROP TYPE IF EXISTS {state_type} CASCADE"))
        .await?;
    client
        .simple_query(&format!(
            "CREATE TYPE {state_type} AS ENUM ('available'); \
             CREATE TABLE {table}( \
               args jsonb NOT NULL, \
               created_at timestamptz NOT NULL, \
               kind text NOT NULL, \
               max_attempts smallint NOT NULL, \
               metadata jsonb NOT NULL, \
               priority smallint NOT NULL, \
               queue text NOT NULL, \
               scheduled_at timestamptz NOT NULL, \
               state {state_type} NOT NULL, \
               tags varchar(255)[] NOT NULL, \
               unique_key bytea, \
               unique_states bit(8) \
             ); \
             CREATE OR REPLACE FUNCTION {function}() \
             RETURNS TRIGGER \
             AS $$ \
             BEGIN \
               IF NEW.state = 'available' THEN \
                 PERFORM pg_notify('river_insert', NEW.state::text); \
               END IF; \
               RETURN NULL; \
             END; \
             $$ \
             LANGUAGE plpgsql; \
             CREATE TRIGGER river_notify \
             AFTER INSERT ON {table} \
             FOR EACH ROW \
             EXECUTE PROCEDURE {function}()"
        ))
        .await?;

    client.simple_query("BEGIN").await?;

    let mut sink = pin!(
        client
            .copy_in::<_, Cursor<Vec<u8>>>(&format!(
                "COPY {table} (args, created_at, kind, max_attempts, metadata, priority, queue, scheduled_at, state, tags, unique_key, unique_states) \
                 FROM STDIN BINARY"
            ))
            .await?
    );
    sink.as_mut()
        .send(Cursor::new(river_job_binary_copy_data()))
        .await?;
    let copied = sink.finish().await?;
    assert_eq!(copied, 1);

    let row = client
        .query_one(
            &format!(
                "SELECT args::text, kind, state::text, tags, unique_key, unique_states::text FROM {table}"
            ),
            &[],
        )
        .await?;
    let args: String = row.try_get(0)?;
    let kind: String = row.try_get(1)?;
    let state: String = row.try_get(2)?;
    let tags: Vec<String> = row.try_get(3)?;
    let unique_key: Option<Vec<u8>> = row.try_get(4)?;
    let unique_states: Option<String> = row.try_get(5)?;
    assert_eq!(args, "{\"noteId\": 1}");
    assert_eq!(kind, "cadence.river.examples.NoteExample");
    assert_eq!(state, "available");
    assert!(tags.is_empty());
    assert_eq!(unique_key, None);
    assert_eq!(unique_states, None);

    client.simple_query("COMMIT").await?;

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table} CASCADE"))
        .await?;
    client
        .simple_query(&format!("DROP TYPE IF EXISTS {state_type} CASCADE"))
        .await?;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ddl_transaction_owner_can_finish_while_other_session_waits() -> Result<(), Box<dyn Error>>
{
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (owner, owner_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let owner_connection_task = tokio::spawn(owner_connection);
    let (waiter, waiter_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let waiter_connection_task = tokio::spawn(waiter_connection);
    let table = format!("fastpg_catalog_lane_{}", std::process::id());

    owner
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    owner
        .simple_query(&format!("BEGIN; CREATE TABLE {table}(id int4)"))
        .await?;

    let waiter_query = tokio::spawn(async move { waiter.simple_query("SELECT 1").await });
    tokio::time::sleep(Duration::from_millis(100)).await;

    tokio::time::timeout(Duration::from_secs(5), owner.simple_query("COMMIT")).await??;
    let messages = tokio::time::timeout(Duration::from_secs(5), waiter_query).await???;
    let rows = rows_only(&messages);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some("1"));

    owner
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    drop(owner);
    owner_connection_task.abort();
    waiter_connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creates_minimal_plpgsql_function() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);

    client
        .simple_query(
            "CREATE OR REPLACE FUNCTION public.fastpg_min_crash() \
             RETURNS void AS $$ BEGIN PERFORM 1; END; $$ LANGUAGE plpgsql",
        )
        .await?;
    client
        .simple_query("DROP FUNCTION public.fastpg_min_crash()")
        .await?;

    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_statement_schema_migration_allows_dollar_quoted_function()
-> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let suffix = std::process::id();
    let table = format!("fastpg_river_job_{suffix}");
    let function = format!("fastpg_river_job_notify_{suffix}");

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table} CASCADE"))
        .await?;
    client
        .simple_query(&format!("DROP FUNCTION IF EXISTS {function}()"))
        .await?;
    client
        .simple_query(&format!(
            "CREATE TABLE {table}(id bigserial PRIMARY KEY, state text NOT NULL DEFAULT 'available'); \
             CREATE OR REPLACE FUNCTION {function}() \
             RETURNS TRIGGER \
             AS $$ \
             BEGIN \
               IF NEW.state = 'available' THEN \
                 PERFORM pg_notify('river_insert', NEW.state); \
               END IF; \
               RETURN NULL; \
             END; \
             $$ \
             LANGUAGE plpgsql; \
             CREATE TRIGGER river_notify \
             AFTER INSERT ON {table} \
             FOR EACH ROW \
             EXECUTE PROCEDURE {function}(); \
             INSERT INTO {table}(state) VALUES ('available')"
        ))
        .await?;
    assert_eq!(count_rows(&client, &table).await?, 1);

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table} CASCADE"))
        .await?;
    client
        .simple_query(&format!("DROP FUNCTION IF EXISTS {function}()"))
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

fn river_job_binary_copy_data() -> Vec<u8> {
    let fields = [
        Some(binary_jsonb(br#"{"noteId":1}"#)),
        Some(0_i64.to_be_bytes().to_vec()),
        Some(binary_text("cadence.river.examples.NoteExample")),
        Some(3_i16.to_be_bytes().to_vec()),
        Some(binary_jsonb(b"{}")),
        Some(1_i16.to_be_bytes().to_vec()),
        Some(binary_text("default")),
        Some(0_i64.to_be_bytes().to_vec()),
        Some(binary_text("available")),
        Some(empty_varchar_array()),
        None,
        None,
    ];
    binary_copy_data(&fields)
}

fn binary_copy_data(fields: &[Option<Vec<u8>>]) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(b"PGCOPY\n\xff\r\n\0");
    data.extend_from_slice(&0_i32.to_be_bytes());
    data.extend_from_slice(&0_i32.to_be_bytes());
    data.extend_from_slice(&(fields.len() as i16).to_be_bytes());
    for field in fields {
        match field {
            Some(bytes) => {
                data.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
                data.extend_from_slice(bytes);
            }
            None => data.extend_from_slice(&(-1_i32).to_be_bytes()),
        }
    }
    data.extend_from_slice(&(-1_i16).to_be_bytes());
    data
}

fn binary_jsonb(json: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(json.len() + 1);
    bytes.push(1);
    bytes.extend_from_slice(json);
    bytes
}

fn binary_text(value: &str) -> Vec<u8> {
    value.as_bytes().to_vec()
}

fn empty_varchar_array() -> Vec<u8> {
    const VARCHAR_OID: u32 = 1043;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0_i32.to_be_bytes());
    bytes.extend_from_slice(&0_i32.to_be_bytes());
    bytes.extend_from_slice(&VARCHAR_OID.to_be_bytes());
    bytes
}

async fn count_rows(client: &tokio_postgres::Client, table: &str) -> Result<i64, Box<dyn Error>> {
    let messages = client
        .simple_query(&format!("SELECT count(*) FROM {table}"))
        .await?;
    let rows = rows_only(&messages);
    assert_eq!(rows.len(), 1);
    Ok(rows[0].get("count").expect("count column").parse()?)
}
