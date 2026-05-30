#![cfg(feature = "postgres-execution")]

use std::error::Error;
use std::io::Cursor;
use std::pin::pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use fastpg_testkit::TestServer;
use futures::SinkExt;
use futures::future::poll_fn;
use futures::future::try_join_all;
use postgres_types::{IsNull, ToSql, Type as PgType, accepts, to_sql_checked};
use tokio::sync::Mutex;
use tokio_postgres::binary_copy::BinaryCopyInWriter;
use tokio_postgres::types::Type;
use tokio_postgres::{AsyncMessage, NoTls, SimpleQueryMessage};

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
async fn simple_query_many_result_statements_survive_statement_growth() -> Result<(), Box<dyn Error>>
{
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let sql = (0..80)
        .map(|value| format!("SELECT {value}::bigint"))
        .collect::<Vec<_>>()
        .join("; ");

    let messages = client.simple_query(&sql).await?;
    let rows = rows_only(&messages);
    assert_eq!(rows.len(), 80);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row.get(0), Some(index.to_string().as_str()));
    }

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
async fn sequential_pgwire_connections_release_pgcore_backend_slots() -> Result<(), Box<dyn Error>>
{
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let connection_string = server.connection_string();

    for index in 0..140_i32 {
        let (client, connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
        let connection_task = tokio::spawn(connection);
        let value: i32 = client.query_one("SELECT $1::int4", &[&index]).await?.get(0);
        assert_eq!(value, index);

        drop(client);
        connection_task.abort();
        tokio::task::yield_now().await;
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn custom_guc_state_is_isolated_between_pgwire_sessions() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (first, first_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let (second, second_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let first_task = tokio::spawn(first_connection);
    let second_task = tokio::spawn(second_connection);

    first
        .query_one(
            "SELECT set_config('app.current_health_system_id', '123', false)",
            &[],
        )
        .await?;
    second
        .query_one(
            "SELECT set_config('app.current_health_system_id', '321', false)",
            &[],
        )
        .await?;

    let first_value: String = first
        .query_one(
            "SELECT current_setting('app.current_health_system_id', true)",
            &[],
        )
        .await?
        .get(0);
    let second_value: String = second
        .query_one(
            "SELECT current_setting('app.current_health_system_id', true)",
            &[],
        )
        .await?
        .get(0);

    assert_eq!(first_value, "123");
    assert_eq!(second_value, "321");

    let (third, third_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let third_task = tokio::spawn(third_connection);
    let third_value: String = third
        .query_one(
            "SELECT coalesce(current_setting('app.current_health_system_id', true), '')",
            &[],
        )
        .await?
        .get(0);
    assert_eq!(third_value, "");

    drop(first);
    drop(second);
    drop(third);
    first_task.abort();
    second_task.abort();
    third_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn custom_guc_state_survives_explicit_transaction() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);

    client
        .query_one(
            "SELECT set_config('app.current_health_system_id', '*', false)",
            &[],
        )
        .await?;
    client.batch_execute("BEGIN").await?;
    for _ in 0..3 {
        let value: String = client
            .query_one(
                "SELECT current_setting('app.current_health_system_id', true)",
                &[],
            )
            .await?
            .get(0);
        assert_eq!(value, "*");
    }
    client.batch_execute("ROLLBACK").await?;

    let value: String = client
        .query_one(
            "SELECT current_setting('app.current_health_system_id', true)",
            &[],
        )
        .await?
        .get(0);
    assert_eq!(value, "*");

    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn custom_guc_set_inside_explicit_transaction_survives_until_rollback()
-> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);

    client.batch_execute("BEGIN").await?;
    client
        .query_one(
            "SELECT set_config('app.current_health_system_id', '*', false)",
            &[],
        )
        .await?;
    for _ in 0..3 {
        let value: String = client
            .query_one(
                "SELECT current_setting('app.current_health_system_id', true)",
                &[],
            )
            .await?
            .get(0);
        assert_eq!(value, "*");
    }
    client.batch_execute("ROLLBACK").await?;

    let value: String = client
        .query_one(
            "SELECT coalesce(current_setting('app.current_health_system_id', true), '')",
            &[],
        )
        .await?
        .get(0);
    assert_eq!(value, "");

    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn temp_buffers_setting_does_not_poison_guc_restore() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);

    client.simple_query("SET temp_buffers = 100").await?;
    client
        .simple_query("CREATE TEMPORARY TABLE fastpg_temp_buffers_check(id int)")
        .await?;
    client
        .simple_query("INSERT INTO fastpg_temp_buffers_check VALUES (1)")
        .await?;
    let row = client
        .query_one("SELECT count(*) FROM fastpg_temp_buffers_check", &[])
        .await?;
    let count: i64 = row.get(0);
    assert_eq!(count, 1);

    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_user_sets_pgcore_current_user() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (admin, admin_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let admin_task = tokio::spawn(admin_connection);
    let role = format!("fastpg_startup_user_{}", std::process::id());

    admin
        .simple_query(&format!("DROP ROLE IF EXISTS {role}"))
        .await?;
    let connection_string = format!("postgres://{}@{}/postgres", role, server.addr());
    let missing_role_checks =
        (0..8).map(|_| expect_connect_error(&connection_string, "28000", &role));
    try_join_all(missing_role_checks).await?;

    admin
        .simple_query(&format!("CREATE ROLE {role} LOGIN"))
        .await?;

    let startup_checks = (0..8).map(|_| async {
        let (client, connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
        let connection_task = tokio::spawn(connection);
        let row = client
            .query_one("SELECT current_user, session_user", &[])
            .await?;
        let current_user: String = row.get(0);
        let session_user: String = row.get(1);
        drop(client);
        connection_task.abort();
        Ok::<_, Box<dyn Error>>((current_user, session_user))
    });
    let startup_results = try_join_all(startup_checks).await?;
    for (current_user, session_user) in startup_results {
        assert_eq!(current_user, role);
        assert_eq!(session_user, role);
    }

    let (client, connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
    let connection_task = tokio::spawn(connection);

    let row = client
        .query_one("SELECT current_user, session_user", &[])
        .await?;
    let current_user: String = row.get(0);
    let session_user: String = row.get(1);
    assert_eq!(current_user, role);
    assert_eq!(session_user, role);

    drop(client);
    connection_task.abort();
    admin
        .simple_query(&format!("DROP ROLE IF EXISTS {role}"))
        .await?;
    drop(admin);
    admin_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_rejects_missing_database_and_role() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let suffix = format!(
        "{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos()
    );
    let missing_db = format!("fastpg_missing_db_{suffix}");
    let missing_role = format!("fastpg_missing_role_{suffix}");

    expect_connect_error(
        &format!("postgres://postgres@{}/{missing_db}", server.addr()),
        "3D000",
        &format!("database \"{missing_db}\" does not exist"),
    )
    .await?;
    expect_connect_error(
        &format!("postgres://{missing_role}@{}/postgres", server.addr()),
        "28000",
        &format!("role \"{missing_role}\" does not exist"),
    )
    .await?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drop_database_rejects_later_startup() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (admin, admin_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let admin_task = tokio::spawn(admin_connection);
    let database = format!(
        "fastpg_drop_db_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos()
    );

    admin
        .simple_query(&format!("CREATE DATABASE {database}"))
        .await?;
    let connection_string = format!("postgres://postgres@{}/{database}", server.addr());
    let (client, connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
    let connection_task = tokio::spawn(connection);
    drop(client);
    connection_task.abort();

    admin
        .simple_query(&format!("DROP DATABASE {database}"))
        .await?;
    expect_connect_error(
        &connection_string,
        "3D000",
        &format!("database \"{database}\" does not exist"),
    )
    .await?;

    drop(admin);
    admin_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_database_migrations_do_not_deadlock_on_catalog_ddl()
-> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (admin, admin_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let admin_task = tokio::spawn(admin_connection);
    let suffix = format!(
        "{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos()
    );
    let first_db = format!("fastpg_catalog_a_{suffix}");
    let second_db = format!("fastpg_catalog_b_{suffix}");

    admin
        .simple_query(&format!("CREATE DATABASE {first_db}"))
        .await?;
    admin
        .simple_query(&format!("CREATE DATABASE {second_db}"))
        .await?;

    let first_conn_string = format!("postgres://postgres@{}/{first_db}", server.addr());
    let second_conn_string = format!("postgres://postgres@{}/{second_db}", server.addr());
    let (first, first_connection) = tokio_postgres::connect(&first_conn_string, NoTls).await?;
    let (second, second_connection) = tokio_postgres::connect(&second_conn_string, NoTls).await?;
    let first_task = tokio::spawn(first_connection);
    let second_task = tokio::spawn(second_connection);

    first.simple_query("BEGIN").await?;
    first
        .simple_query("CREATE TABLE fastpg_parallel_migration(id int)")
        .await?;

    let second_migration = tokio::spawn(async move {
        second.simple_query("BEGIN").await?;
        second
            .simple_query("CREATE TABLE fastpg_parallel_migration(id int)")
            .await?;
        second
            .simple_query("INSERT INTO fastpg_parallel_migration VALUES (2)")
            .await?;
        second.simple_query("COMMIT").await?;
        Ok::<_, tokio_postgres::Error>(second)
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    first
        .simple_query("INSERT INTO fastpg_parallel_migration VALUES (1)")
        .await?;
    first.simple_query("COMMIT").await?;

    let second = tokio::time::timeout(Duration::from_secs(5), second_migration).await???;
    let first_current_database: String = first
        .query_one("SELECT current_database()", &[])
        .await?
        .get(0);
    let second_current_database: String = second
        .query_one("SELECT current_database()", &[])
        .await?
        .get(0);
    assert_eq!(first_current_database, first_db);
    assert_eq!(second_current_database, second_db);
    let first_messages = first
        .simple_query("SELECT count(*) FROM fastpg_parallel_migration")
        .await?;
    let second_messages = second
        .simple_query("SELECT count(*) FROM fastpg_parallel_migration")
        .await?;
    assert_eq!(rows_only(&first_messages)[0].get(0), Some("1"));
    assert_eq!(rows_only(&second_messages)[0].get(0), Some("1"));

    drop(first);
    drop(second);
    first_task.abort();
    second_task.abort();
    drop(admin);
    admin_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_validation_does_not_block_catalog_owner_progress() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (admin, admin_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let admin_task = tokio::spawn(admin_connection);
    let suffix = format!(
        "{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos()
    );
    let owner_db = format!("fastpg_startup_owner_{suffix}");
    let waiter_db = format!("fastpg_startup_waiter_{suffix}");

    admin
        .simple_query(&format!("CREATE DATABASE {owner_db}"))
        .await?;
    admin
        .simple_query(&format!("CREATE DATABASE {waiter_db}"))
        .await?;

    let owner_conn_string = format!("postgres://postgres@{}/{owner_db}", server.addr());
    let (owner, owner_connection) = tokio_postgres::connect(&owner_conn_string, NoTls).await?;
    let owner_task = tokio::spawn(owner_connection);
    owner.simple_query("BEGIN").await?;
    owner
        .simple_query("CREATE TABLE fastpg_startup_owner(id int)")
        .await?;

    let waiter_conn_string = format!("postgres://postgres@{}/{waiter_db}", server.addr());
    let waiter_connect = tokio::spawn(async move {
        let (client, connection) = tokio_postgres::connect(&waiter_conn_string, NoTls).await?;
        Ok::<_, tokio_postgres::Error>((client, tokio::spawn(connection)))
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    owner
        .simple_query("INSERT INTO fastpg_startup_owner VALUES (1)")
        .await?;
    owner.simple_query("COMMIT").await?;

    let (waiter, waiter_task) =
        tokio::time::timeout(Duration::from_secs(5), waiter_connect).await???;
    waiter.simple_query("SELECT 1").await?;

    drop(waiter);
    waiter_task.abort();
    drop(owner);
    owner_task.abort();
    drop(admin);
    admin_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn query_error_releases_pgcore_lane() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);

    let query_result =
        tokio::time::timeout(Duration::from_secs(5), client.query("SELECT * FROM", &[])).await?;
    assert!(query_result.is_err());

    let row =
        tokio::time::timeout(Duration::from_secs(5), client.query_one("SELECT 1", &[])).await??;
    let value: i32 = row.get(0);
    assert_eq!(value, 1);

    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn advisory_lock_functions_respect_parallel_session_ownership() -> Result<(), Box<dyn Error>>
{
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let connection_string = server.connection_string();
    let (first, first_connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
    let first_task = tokio::spawn(first_connection);
    let (second, second_connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
    let second_task = tokio::spawn(second_connection);
    let shared_key = 44_i64;

    let acquired: bool = first
        .query_one("SELECT pg_try_advisory_lock($1)", &[&shared_key])
        .await?
        .get(0);
    assert!(acquired);
    let competing_acquired: bool = second
        .query_one("SELECT pg_try_advisory_lock($1)", &[&shared_key])
        .await?
        .get(0);
    assert!(!competing_acquired);
    let unlocked: bool = first
        .query_one("SELECT pg_advisory_unlock($1)", &[&shared_key])
        .await?
        .get(0);
    assert!(unlocked);
    let acquired_after_unlock: bool = second
        .query_one("SELECT pg_try_advisory_lock($1)", &[&shared_key])
        .await?
        .get(0);
    assert!(acquired_after_unlock);
    second
        .simple_query("SELECT pg_advisory_unlock_all()")
        .await?;

    first.execute("BEGIN", &[]).await?;
    let xact_acquired: bool = first
        .query_one("SELECT pg_try_advisory_xact_lock($1)", &[&shared_key])
        .await?
        .get(0);
    assert!(xact_acquired);
    let competing_xact_acquired: bool = second
        .query_one("SELECT pg_try_advisory_xact_lock($1)", &[&shared_key])
        .await?
        .get(0);
    assert!(!competing_xact_acquired);
    first.execute("COMMIT", &[]).await?;
    let acquired_after_commit: bool = second
        .query_one("SELECT pg_try_advisory_xact_lock($1)", &[&shared_key])
        .await?
        .get(0);
    assert!(acquired_after_commit);

    drop(first);
    first_task.abort();
    drop(second);
    second_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extended_query_accepts_current_database_catalog_qualified_relations()
-> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (admin, admin_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let admin_task = tokio::spawn(admin_connection);
    admin.simple_query("CREATE DATABASE pms_db").await?;
    drop(admin);
    admin_task.abort();

    let connection_string = format!("postgres://postgres@{}/pms_db", server.addr());
    let (client, connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
    let connection_task = tokio::spawn(connection);

    client
        .simple_query(
            "CREATE TABLE public.ehr_patient (
                id bigint,
                mrn text,
                ehr text,
                athena_practice_id text
            )",
        )
        .await?;
    client
        .execute(
            "INSERT INTO public.ehr_patient (id, mrn, ehr, athena_practice_id)
             VALUES (1, '123', 'ATHENA', '999')",
            &[],
        )
        .await?;

    let row = client
        .query_one(
            "SELECT count(*)
             FROM pms_db.public.ehr_patient
             WHERE mrn = $1 AND ehr = $2 AND athena_practice_id = $3",
            &[&"123", &"ATHENA", &"999"],
        )
        .await?;
    let count: i64 = row.get(0);
    assert_eq!(count, 1);

    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extended_query_prepares_authservice_user_upsert() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (admin, admin_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let admin_task = tokio::spawn(admin_connection);
    let database = format!("authservice_db_{}", std::process::id());
    admin
        .simple_query(&format!("CREATE DATABASE {database}"))
        .await?;
    drop(admin);
    admin_task.abort();

    let connection_string = format!("postgres://postgres@{}/{}", server.addr(), database);
    let (client, connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
    let connection_task = tokio::spawn(connection);

    client
        .simple_query(
            r#"CREATE TABLE public."user" (
                id uuid NOT NULL,
                sub character varying,
                email character varying NOT NULL,
                first_name character varying,
                last_name character varying,
                enabled boolean,
                created_at timestamp without time zone,
                updated_at timestamp without time zone,
                last_login_at timestamp without time zone,
                last_activity_at timestamp without time zone,
                username character varying(255),
                CONSTRAINT user_email_key UNIQUE (email),
                CONSTRAINT user_pkey PRIMARY KEY (id),
                CONSTRAINT user_sub_key UNIQUE (sub),
                CONSTRAINT user_username_key UNIQUE (username)
            )"#,
        )
        .await?;

    let statement = client
        .prepare(
            r#"INSERT INTO public."user" (
                id,
                sub,
                email,
                first_name,
                last_name,
                enabled,
                username,
                created_at,
                updated_at
            )
            VALUES (
                gen_random_uuid (),
                $1,
                $2,
                $3,
                $4,
                $5,
                $6,
                NOW(),
                NOW()
            )
            ON CONFLICT (email) DO UPDATE SET
                sub = EXCLUDED.sub,
                first_name = EXCLUDED.first_name,
                last_name = EXCLUDED.last_name,
                updated_at = NOW(),
                last_login_at = NOW()
            WHERE
                public."user".sub IS DISTINCT FROM EXCLUDED.sub
                OR public."user".first_name IS DISTINCT FROM EXCLUDED.first_name
                OR public."user".last_name IS DISTINCT FROM EXCLUDED.last_name
                OR public."user".last_login_at IS DISTINCT FROM NOW()
            RETURNING *"#,
        )
        .await?;
    assert_eq!(statement.params().len(), 6);

    let rows = client
        .query(
            &statement,
            &[
                &"auth0|123",
                &"user@example.test",
                &"Ada",
                &"Lovelace",
                &true,
                &"ada",
            ],
        )
        .await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].columns().len(), 11);
    let email: String = rows[0].try_get("email")?;
    assert_eq!(email, "user@example.test");

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
async fn extended_query_on_conflict_on_named_unique_constraint_updates()
-> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let table = format!(
        "fastpg_named_unique_constraint_upsert_{}",
        std::process::id()
    );
    let constraint = format!("{table}_patient_allergen_key");

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    client
        .simple_query(&format!(
            "CREATE TABLE {table}( \
               id int4 PRIMARY KEY, \
               patient_id int4 NOT NULL, \
               allergen varchar(256) NOT NULL, \
               criticality text NOT NULL, \
               CONSTRAINT {constraint} UNIQUE (patient_id, allergen) \
             )"
        ))
        .await?;
    client
        .simple_query(&format!(
            "CREATE INDEX {table}_patient_id_idx ON {table}(patient_id)"
        ))
        .await?;

    client
        .execute(
            &format!(
                "INSERT INTO {table}(id, patient_id, allergen, criticality) \
                 VALUES ($1, $2, $3, $4)"
            ),
            &[&1_i32, &10_i32, &"peanut", &"low"],
        )
        .await?;

    let rows = client
        .query(
            &format!(
                "INSERT INTO {table}(id, patient_id, allergen, criticality) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT ON CONSTRAINT {constraint} DO UPDATE SET \
                   criticality = EXCLUDED.criticality \
                 WHERE {table}.criticality IS DISTINCT FROM EXCLUDED.criticality \
                 RETURNING id, criticality"
            ),
            &[&2_i32, &10_i32, &"peanut", &"high"],
        )
        .await?;

    assert_eq!(rows.len(), 1);
    let id: i32 = rows[0].try_get("id")?;
    let criticality: String = rows[0].try_get("criticality")?;
    assert_eq!(id, 1);
    assert_eq!(criticality, "high");

    let count: i64 = client
        .query_one(&format!("SELECT count(*) FROM {table}"), &[])
        .await?
        .try_get(0)?;
    assert_eq!(count, 1);

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extended_query_on_conflict_from_select_updates_existing_key() -> Result<(), Box<dyn Error>>
{
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let table = format!(
        "fastpg_select_unique_constraint_upsert_{}",
        std::process::id()
    );
    let constraint = format!("{table}_patient_allergen_key");

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    client
        .simple_query(&format!(
            "CREATE TABLE {table}( \
               id int4 GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
               patient_id int4 NOT NULL, \
               allergen varchar(256) NOT NULL, \
               criticality varchar(16), \
               active boolean NOT NULL, \
               CONSTRAINT {constraint} UNIQUE (patient_id, allergen) \
             )"
        ))
        .await?;

    client
        .execute(
            &format!(
                "INSERT INTO {table}(patient_id, allergen, criticality, active) \
                 VALUES ($1, $2, $3, $4)"
            ),
            &[&10_i32, &"peanut", &"LOW", &true],
        )
        .await?;

    let rows = client
        .query(
            &format!(
                "INSERT INTO {table}(patient_id, allergen, criticality, active) \
                 SELECT $1::int4, $2::varchar, upper($3::varchar), $4::bool \
                 ON CONFLICT ON CONSTRAINT {constraint} DO UPDATE SET \
                   criticality = EXCLUDED.criticality, \
                   active = EXCLUDED.active \
                 WHERE {table}.criticality IS DISTINCT FROM EXCLUDED.criticality \
                    OR {table}.active IS DISTINCT FROM EXCLUDED.active \
                 RETURNING id, criticality, active"
            ),
            &[&10_i32, &"peanut", &"high", &true],
        )
        .await?;

    assert_eq!(rows.len(), 1);
    let id: i32 = rows[0].try_get("id")?;
    let criticality: String = rows[0].try_get("criticality")?;
    let active: bool = rows[0].try_get("active")?;
    assert_eq!(id, 1);
    assert_eq!(criticality, "HIGH");
    assert!(active);

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extended_query_on_conflict_from_cte_select_updates_existing_key()
-> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let table = format!("fastpg_cte_unique_constraint_upsert_{}", std::process::id());
    let constraint = format!("{table}_patient_allergen_key");

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    client
        .simple_query(&format!(
            "CREATE TABLE {table}( \
               id int4 GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
               patient_id int4 NOT NULL, \
               allergen varchar(256) NOT NULL, \
               criticality varchar(16), \
               active boolean NOT NULL, \
               CONSTRAINT {constraint} UNIQUE (patient_id, allergen) \
             )"
        ))
        .await?;

    client
        .execute(
            &format!(
                "INSERT INTO {table}(patient_id, allergen, criticality, active) \
                 VALUES ($1, $2, $3, $4)"
            ),
            &[&10_i32, &"peanut", &"LOW", &true],
        )
        .await?;

    let rows = client
        .query(
            &format!(
                "WITH \
                   data_table AS ( \
                     SELECT \
                       $1::int4 AS patient_id, \
                       $2::varchar AS allergen, \
                       upper($3::varchar) AS criticality, \
                       $4::bool AS active \
                   ), \
                   existing_allergies AS ( \
                     SELECT a.id, a.patient_id, a.allergen \
                     FROM data_table dt \
                     INNER JOIN {table} a \
                       ON a.patient_id = dt.patient_id \
                      AND a.allergen = dt.allergen \
                   ), \
                   updated_allergies AS ( \
                     SELECT \
                       a.id, \
                       dt.patient_id, \
                       dt.allergen, \
                       dt.criticality, \
                       dt.active \
                     FROM data_table dt \
                     INNER JOIN {table} a \
                       ON a.patient_id = dt.patient_id \
                      AND a.allergen = dt.allergen \
                   ), \
                   data_table_for_new_allergies AS ( \
                     SELECT dt.patient_id, dt.allergen, dt.criticality, dt.active \
                     FROM data_table dt \
                     LEFT JOIN existing_allergies ea \
                       ON dt.patient_id = ea.patient_id \
                      AND dt.allergen = ea.allergen \
                     WHERE ea.id IS NULL \
                   ), \
                   new_allergies AS ( \
                     INSERT INTO {table}(patient_id, allergen, criticality, active) \
                     SELECT patient_id, allergen, criticality, active \
                     FROM data_table_for_new_allergies \
                     RETURNING id, patient_id, allergen, criticality, active \
                   ) \
                 INSERT INTO {table}(patient_id, allergen, criticality, active) \
                 SELECT ua.patient_id, ua.allergen, ua.criticality, ua.active \
                 FROM updated_allergies ua \
                 ON CONFLICT ON CONSTRAINT {constraint} DO UPDATE SET \
                   criticality = EXCLUDED.criticality, \
                   active = EXCLUDED.active \
                 WHERE {table}.criticality IS DISTINCT FROM EXCLUDED.criticality \
                    OR {table}.active IS DISTINCT FROM EXCLUDED.active \
                 RETURNING id, criticality, active"
            ),
            &[&10_i32, &"peanut", &"high", &true],
        )
        .await?;

    assert_eq!(rows.len(), 1);
    let id: i32 = rows[0].try_get("id")?;
    let criticality: String = rows[0].try_get("criticality")?;
    let active: bool = rows[0].try_get("active")?;
    assert_eq!(id, 1);
    assert_eq!(criticality, "HIGH");
    assert!(active);

    let count: i64 = client
        .query_one(&format!("SELECT count(*) FROM {table}"), &[])
        .await?
        .try_get(0)?;
    assert_eq!(count, 1);

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    drop(client);
    connection_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extended_query_on_conflict_with_writable_cte_array_inputs_updates_existing_key()
-> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (client, connection) = tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let connection_task = tokio::spawn(connection);
    let table = format!("fastpg_patient_allergy_upsert_{}", std::process::id());
    let constraint = format!("{table}_patient_allergen_key");
    let patient_id = "11111111-1111-4111-8111-111111111111";
    let allergen = "eggs";
    let patient_ids = vec![PgUuid::parse(patient_id)?];
    let allergens = vec![allergen];
    let concacted_categories = vec!["food,medicine"];
    let onset_dates = vec![SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800)];
    let deactivated_dates: Vec<Option<SystemTime>> = vec![None];
    let active_list = vec![true];
    let concacted_reactions = vec!["rash,vomit"];

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .await?;
    client
        .simple_query(&format!(
            "CREATE TABLE {table}( \
               id uuid DEFAULT gen_random_uuid() NOT NULL, \
               patient_id uuid NOT NULL, \
               allergen varchar(256) NOT NULL, \
               criticality varchar(16), \
               categories text[], \
               onset_date timestamptz, \
               deactivated_date timestamptz, \
               active boolean NOT NULL, \
               untracked_time timestamptz DEFAULT NULL, \
               create_time timestamptz NOT NULL DEFAULT (now() AT TIME ZONE 'UTC'), \
               update_time timestamptz NOT NULL DEFAULT (now() AT TIME ZONE 'UTC'), \
               reactions text[], \
               CONSTRAINT {table}_pkey PRIMARY KEY (id), \
               CONSTRAINT {constraint} UNIQUE (patient_id, allergen) \
             )"
        ))
        .await?;
    client
        .simple_query(&format!(
            "CREATE INDEX {table}_patient_id_idx ON {table}(patient_id)"
        ))
        .await?;

    let upsert_sql = format!(
        "WITH \
                   data_table_concated_text_arrays AS ( \
                     SELECT \
                       unnest($1::uuid[]) AS patient_id, \
                       unnest($2::varchar[]) AS allergen, \
                       unnest($3::varchar[]) AS criticality, \
                       unnest($4::text[]) AS concacted_categories, \
                       unnest($5::timestamptz[]) AS onset_date, \
                       unnest($6::timestamptz[]) AS deactivated_date, \
                       unnest($7::boolean[]) AS active, \
                       unnest($8::text[]) AS concacted_reactions \
                   ), \
                   data_table AS ( \
                     SELECT \
                       patient_id, \
                       allergen, \
                       upper(criticality) AS criticality, \
                       string_to_array(upper(concacted_categories), ',')::text[] AS categories, \
                       onset_date, \
                       deactivated_date, \
                       active, \
                       string_to_array(concacted_reactions, ',')::text[] AS reactions \
                     FROM data_table_concated_text_arrays dt \
                   ), \
                   existing_allergies AS ( \
                     SELECT a.id, a.patient_id, a.allergen, a.criticality, a.categories, \
                            a.onset_date, a.deactivated_date, a.active, a.reactions \
                     FROM data_table dt \
                     INNER JOIN {table} a \
                       ON a.patient_id = dt.patient_id \
                      AND a.allergen = dt.allergen \
                   ), \
                   updated_allergies AS ( \
                     SELECT a.id, dt.patient_id, dt.allergen, dt.criticality, dt.categories, \
                            dt.onset_date, dt.deactivated_date, dt.active, dt.reactions \
                     FROM data_table dt \
                     INNER JOIN {table} a \
                       ON a.patient_id = dt.patient_id \
                      AND a.allergen = dt.allergen \
                   ), \
                   data_table_for_new_allergies AS ( \
                     SELECT dt.patient_id, dt.allergen, dt.criticality, dt.categories, \
                            dt.onset_date, dt.deactivated_date, dt.active, dt.reactions \
                     FROM data_table dt \
                     LEFT JOIN existing_allergies ea \
                       ON dt.patient_id = ea.patient_id \
                      AND dt.allergen = ea.allergen \
                     WHERE ea.id IS NULL \
                   ), \
                   new_allergies AS ( \
                     INSERT INTO {table}( \
                       patient_id, allergen, criticality, categories, reactions, onset_date, \
                       deactivated_date, active, create_time, update_time \
                     ) \
                     SELECT patient_id, allergen, criticality, categories, reactions, onset_date, \
                            CASE \
                              WHEN active = false AND deactivated_date IS NULL \
                                THEN (now() AT TIME ZONE 'UTC')::date \
                              ELSE deactivated_date \
                            END, \
                            active, now() AT TIME ZONE 'utc', now() AT TIME ZONE 'utc' \
                     FROM data_table_for_new_allergies \
                     RETURNING id, patient_id, allergen, criticality, categories, reactions, \
                               onset_date, deactivated_date, active \
                   ) \
                 INSERT INTO {table}( \
                   patient_id, allergen, criticality, categories, reactions, onset_date, \
                   deactivated_date, active, update_time, untracked_time \
                 ) \
                 SELECT ua.patient_id, ua.allergen, ua.criticality, ua.categories, ua.reactions, \
                        ua.onset_date, ua.deactivated_date, ua.active, now() AT TIME ZONE 'utc', NULL \
                 FROM updated_allergies ua \
                 ON CONFLICT ON CONSTRAINT {constraint} DO UPDATE SET \
                   criticality = EXCLUDED.criticality, \
                   categories = EXCLUDED.categories, \
                   reactions = EXCLUDED.reactions, \
                   onset_date = EXCLUDED.onset_date, \
                   deactivated_date = CASE \
                     WHEN {table}.active = true AND EXCLUDED.active = false \
                      AND EXCLUDED.deactivated_date IS NULL THEN (now() AT TIME ZONE 'UTC')::date \
                     ELSE EXCLUDED.deactivated_date \
                   END, \
                   active = EXCLUDED.active, \
                   update_time = now() AT TIME ZONE 'utc', \
                   untracked_time = NULL \
                 WHERE {table}.criticality IS DISTINCT FROM EXCLUDED.criticality \
                    OR {table}.categories IS DISTINCT FROM EXCLUDED.categories \
                    OR {table}.reactions IS DISTINCT FROM EXCLUDED.reactions \
                    OR {table}.onset_date IS DISTINCT FROM EXCLUDED.onset_date \
                    OR {table}.active IS DISTINCT FROM EXCLUDED.active \
                    OR {table}.deactivated_date IS DISTINCT FROM CASE \
                         WHEN {table}.active = true AND EXCLUDED.active = false \
                          AND EXCLUDED.deactivated_date IS NULL THEN (now() AT TIME ZONE 'UTC')::date \
                         ELSE EXCLUDED.deactivated_date \
                       END \
                    OR {table}.untracked_time IS NOT NULL"
    );

    client.batch_execute("BEGIN").await?;
    let upsert_stmt = client.prepare(&upsert_sql).await?;
    let backdate_stmt = client
        .prepare(&format!(
            "UPDATE {table} \
             SET update_time = $1 \
             WHERE patient_id = $2::uuid \
               AND allergen = $3"
        ))
        .await?;
    let past_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800);

    let criticalities = vec!["high"];
    client
        .execute(
            &upsert_stmt,
            &[
                &patient_ids,
                &allergens,
                &criticalities,
                &concacted_categories,
                &onset_dates,
                &deactivated_dates,
                &active_list,
                &concacted_reactions,
            ],
        )
        .await?;

    client
        .execute(
            &backdate_stmt,
            &[&past_time, &patient_ids[0], &allergens[0]],
        )
        .await?;

    let new_allergies_only_sql = format!(
        "WITH \
           data_table_concated_text_arrays AS ( \
             SELECT \
               unnest($1::uuid[]) AS patient_id, \
               unnest($2::varchar[]) AS allergen, \
               unnest($3::varchar[]) AS criticality, \
               unnest($4::text[]) AS concacted_categories, \
               unnest($5::timestamptz[]) AS onset_date, \
               unnest($6::timestamptz[]) AS deactivated_date, \
               unnest($7::boolean[]) AS active, \
               unnest($8::text[]) AS concacted_reactions \
           ), \
           data_table AS ( \
             SELECT patient_id, allergen, upper(criticality) AS criticality, \
                    string_to_array(upper(concacted_categories), ',')::text[] AS categories, \
                    onset_date, deactivated_date, active, \
                    string_to_array(concacted_reactions, ',')::text[] AS reactions \
             FROM data_table_concated_text_arrays dt \
           ), \
           existing_allergies AS ( \
             SELECT a.id, a.patient_id, a.allergen, a.criticality, a.categories, \
                    a.onset_date, a.deactivated_date, a.active, a.reactions \
             FROM data_table dt \
             INNER JOIN {table} a \
               ON a.patient_id = dt.patient_id \
              AND a.allergen = dt.allergen \
           ), \
           data_table_for_new_allergies AS ( \
             SELECT dt.patient_id, dt.allergen, dt.criticality, dt.categories, \
                    dt.onset_date, dt.deactivated_date, dt.active, dt.reactions \
             FROM data_table dt \
             LEFT JOIN existing_allergies ea \
               ON dt.patient_id = ea.patient_id \
              AND dt.allergen = ea.allergen \
             WHERE ea.id IS NULL \
           ), \
           new_allergies AS ( \
             INSERT INTO {table}( \
               patient_id, allergen, criticality, categories, reactions, onset_date, \
               deactivated_date, active, create_time, update_time \
             ) \
             SELECT patient_id, allergen, criticality, categories, reactions, onset_date, \
                    CASE \
                      WHEN active = false AND deactivated_date IS NULL \
                        THEN (now() AT TIME ZONE 'UTC')::date \
                      ELSE deactivated_date \
                    END, \
                    active, now() AT TIME ZONE 'utc', now() AT TIME ZONE 'utc' \
             FROM data_table_for_new_allergies \
             RETURNING id \
           ) \
         SELECT count(*)::bigint FROM new_allergies"
    );
    let criticalities = vec!["low"];
    let row = client
        .query_one(
            &new_allergies_only_sql,
            &[
                &patient_ids,
                &allergens,
                &criticalities,
                &concacted_categories,
                &onset_dates,
                &deactivated_dates,
                &active_list,
                &concacted_reactions,
            ],
        )
        .await?;
    let inserted_new_allergies: i64 = row.try_get(0)?;
    assert_eq!(inserted_new_allergies, 0);

    client
        .execute(
            &upsert_stmt,
            &[
                &patient_ids,
                &allergens,
                &criticalities,
                &concacted_categories,
                &onset_dates,
                &deactivated_dates,
                &active_list,
                &concacted_reactions,
            ],
        )
        .await?;

    let row = client
        .query_one(
            &format!("SELECT count(*)::bigint, max(criticality) FROM {table}"),
            &[],
        )
        .await?;
    let count: i64 = row.try_get(0)?;
    let criticality: String = row.try_get(1)?;
    assert_eq!(count, 1);
    assert_eq!(criticality, "LOW");

    client.batch_execute("ROLLBACK").await?;

    let concurrent_upsert_sql = Arc::new(upsert_sql);
    let concurrent_cases = [
        ("11111111-1111-4111-8111-111111111101", "eggs-a", "high"),
        ("11111111-1111-4111-8111-111111111102", "eggs-b", "low"),
        ("11111111-1111-4111-8111-111111111103", "eggs-c", "low"),
        ("11111111-1111-4111-8111-111111111104", "eggs-d", "low"),
    ];
    let concurrent_runs =
        concurrent_cases
            .into_iter()
            .map(|(patient_id, allergen, second_criticality)| {
                let connection_string = server.connection_string();
                let upsert_sql = Arc::clone(&concurrent_upsert_sql);
                let table = table.clone();
                async move {
                    let (client, connection) =
                        tokio_postgres::connect(&connection_string, NoTls).await?;
                    let connection_task = tokio::spawn(connection);
                    let patient_ids = vec![PgUuid::parse(patient_id)?];
                    let allergens = vec![allergen];
                    let concacted_categories = vec!["food,medicine"];
                    let onset_dates =
                        vec![SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800)];
                    let deactivated_dates: Vec<Option<SystemTime>> = vec![None];
                    let active_list = vec![true];
                    let concacted_reactions = vec!["rash,vomit"];

                    client.batch_execute("BEGIN").await?;
                    let criticalities = vec!["high"];
                    client
                        .execute(
                            upsert_sql.as_str(),
                            &[
                                &patient_ids,
                                &allergens,
                                &criticalities,
                                &concacted_categories,
                                &onset_dates,
                                &deactivated_dates,
                                &active_list,
                                &concacted_reactions,
                            ],
                        )
                        .await?;
                    client
                        .simple_query(&format!(
                            "UPDATE {table} \
                         SET update_time = '2020-01-01 00:00:00+00'::timestamptz \
                         WHERE patient_id = '{patient_id}'::uuid \
                           AND allergen = '{allergen}'"
                        ))
                        .await?;
                    let criticalities = vec![second_criticality];
                    client
                        .execute(
                            upsert_sql.as_str(),
                            &[
                                &patient_ids,
                                &allergens,
                                &criticalities,
                                &concacted_categories,
                                &onset_dates,
                                &deactivated_dates,
                                &active_list,
                                &concacted_reactions,
                            ],
                        )
                        .await?;
                    let row = client
                        .query_one(
                            &format!(
                                "SELECT count(*)::bigint, max(criticality) \
                             FROM {table} \
                             WHERE patient_id = $1::uuid \
                               AND allergen = $2"
                            ),
                            &[&patient_ids[0], &allergens[0]],
                        )
                        .await?;
                    let count: i64 = row.try_get(0)?;
                    let criticality: String = row.try_get(1)?;
                    assert_eq!(count, 1);
                    assert_eq!(criticality, second_criticality.to_ascii_uppercase());
                    client.batch_execute("ROLLBACK").await?;

                    drop(client);
                    connection_task.abort();
                    Ok::<(), Box<dyn Error>>(())
                }
            });

    try_join_all(concurrent_runs).await?;

    client
        .simple_query(&format!("DROP TABLE IF EXISTS {table}"))
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
async fn listen_notify_delivers_trigger_notifications() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (listener, mut listener_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let (sender, sender_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let sender_task = tokio::spawn(sender_connection);
    let (notification_tx, mut notification_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    let listener_task = tokio::spawn(async move {
        while let Some(message) = poll_fn(|cx| listener_connection.poll_message(cx)).await {
            match message {
                Ok(AsyncMessage::Notification(notification)) => {
                    let _ = notification_tx.send((
                        notification.channel().to_owned(),
                        notification.payload().to_owned(),
                    ));
                }
                Ok(_) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    });

    listener.simple_query("LISTEN fastpg_test_notify").await?;
    sender
        .simple_query(
            "DROP TABLE IF EXISTS fastpg_notify_smoke CASCADE; \
             CREATE TABLE fastpg_notify_smoke(id int primary key, payload text); \
             CREATE OR REPLACE FUNCTION fastpg_notify_smoke_fn() \
             RETURNS trigger \
             AS $$ \
             BEGIN \
               PERFORM pg_notify('fastpg_test_notify', NEW.payload); \
               RETURN NEW; \
             END; \
             $$ LANGUAGE plpgsql; \
             CREATE TRIGGER fastpg_notify_smoke_trg \
             AFTER INSERT ON fastpg_notify_smoke \
             FOR EACH ROW EXECUTE PROCEDURE fastpg_notify_smoke_fn(); \
             INSERT INTO fastpg_notify_smoke VALUES (1, 'from-trigger')",
        )
        .await?;

    let notification = tokio::time::timeout(Duration::from_secs(3), notification_rx.recv()).await?;
    assert_eq!(
        notification,
        Some(("fastpg_test_notify".to_owned(), "from-trigger".to_owned()))
    );

    sender
        .simple_query("DROP TABLE IF EXISTS fastpg_notify_smoke CASCADE")
        .await?;
    drop(sender);
    drop(listener);
    sender_task.abort();
    listener_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listen_notify_handles_prestarted_duplicate_listen_and_cte_notify()
-> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (admin, admin_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let admin_task = tokio::spawn(admin_connection);
    let role = format!("fastpg_notify_user_{}", std::process::id());

    admin
        .simple_query(&format!("DROP ROLE IF EXISTS {role}"))
        .await?;
    admin
        .simple_query(&format!("CREATE ROLE {role} LOGIN"))
        .await?;

    let connection_string = format!("postgres://{}@{}/postgres", role, server.addr());
    let (listener, mut listener_connection) =
        tokio_postgres::connect(&connection_string, NoTls).await?;
    let (sender, sender_connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
    let sender_task = tokio::spawn(sender_connection);
    let (notification_tx, mut notification_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, String)>();

    let listener_task = tokio::spawn(async move {
        while let Some(message) = poll_fn(|cx| listener_connection.poll_message(cx)).await {
            match message {
                Ok(AsyncMessage::Notification(notification)) => {
                    let _ = notification_tx.send((
                        notification.channel().to_owned(),
                        notification.payload().to_owned(),
                    ));
                }
                Ok(_) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    });

    listener
        .simple_query("LISTEN \"public.test_topic1\"")
        .await?;
    listener
        .simple_query("LISTEN \"public.test_topic1\"")
        .await?;

    sender
        .query(
            "WITH topic_to_notify AS ( \
                SELECT \
                    concat(current_schema(), '.', $1::text) AS topic, \
                    unnest(ARRAY[$2]::text[]) AS payload \
            ) \
            SELECT pg_notify(topic_to_notify.topic, topic_to_notify.payload) \
            FROM topic_to_notify",
            &[&"test_topic1", &"msg1"],
        )
        .await?;

    let notification = tokio::time::timeout(Duration::from_secs(3), notification_rx.recv()).await?;
    assert_eq!(
        notification,
        Some(("public.test_topic1".to_owned(), "msg1".to_owned()))
    );

    drop(sender);
    drop(listener);
    sender_task.abort();
    listener_task.abort();
    admin
        .simple_query(&format!("DROP ROLE IF EXISTS {role}"))
        .await?;
    drop(admin);
    admin_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listen_notify_survives_listener_connection_churn() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let connection_string = server.connection_string();
    let churn = (0..24)
        .map(|_| {
            let connection_string = connection_string.clone();
            tokio::spawn(async move {
                let (client, connection) =
                    tokio_postgres::connect(&connection_string, NoTls).await?;
                let connection_task = tokio::spawn(connection);
                client.simple_query("LISTEN fastpg_churn_notify").await?;
                client.simple_query("UNLISTEN fastpg_churn_notify").await?;
                drop(client);
                connection_task.abort();
                Ok::<(), tokio_postgres::Error>(())
            })
        })
        .collect::<Vec<_>>();
    for result in try_join_all(churn).await? {
        result?;
    }

    let (listener, mut listener_connection) =
        tokio_postgres::connect(&connection_string, NoTls).await?;
    let (sender, sender_connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
    let sender_task = tokio::spawn(sender_connection);
    let (notification_tx, mut notification_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, String)>();

    let listener_task = tokio::spawn(async move {
        while let Some(message) = poll_fn(|cx| listener_connection.poll_message(cx)).await {
            match message {
                Ok(AsyncMessage::Notification(notification)) => {
                    let _ = notification_tx.send((
                        notification.channel().to_owned(),
                        notification.payload().to_owned(),
                    ));
                }
                Ok(_) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    });

    listener.simple_query("LISTEN fastpg_churn_notify").await?;
    tokio::time::timeout(
        Duration::from_secs(5),
        sender.query(
            "WITH topic_to_notify AS ( \
                SELECT $1::text AS topic, unnest(ARRAY[$2]::text[]) AS payload \
            ) \
            SELECT pg_notify(topic_to_notify.topic, topic_to_notify.payload) \
            FROM topic_to_notify",
            &[&"fastpg_churn_notify", &"after-churn"],
        ),
    )
    .await??;

    let notification = tokio::time::timeout(Duration::from_secs(3), notification_rx.recv()).await?;
    assert_eq!(
        notification,
        Some(("fastpg_churn_notify".to_owned(), "after-churn".to_owned()))
    );

    drop(sender);
    drop(listener);
    sender_task.abort();
    listener_task.abort();

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_database_template_clones_rows_without_cross_database_leak()
-> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (admin, admin_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let admin_task = tokio::spawn(admin_connection);
    let suffix = format!(
        "{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos()
    );
    let template_db = format!("fastpg_template_{suffix}");
    let clone_one = format!("fastpg_clone_one_{suffix}");
    let clone_two = format!("fastpg_clone_two_{suffix}");

    admin
        .simple_query(&format!("CREATE DATABASE {template_db}"))
        .await?;

    let template_conn_string = format!("postgres://postgres@{}/{template_db}", server.addr());
    let (template, template_connection) =
        tokio_postgres::connect(&template_conn_string, NoTls).await?;
    let template_task = tokio::spawn(template_connection);
    template
        .simple_query(
            "CREATE TABLE template_marker(id text PRIMARY KEY); \
             INSERT INTO template_marker(id) VALUES ('seed')",
        )
        .await?;
    drop(template);
    template_task.abort();

    admin
        .simple_query(&format!(
            "UPDATE pg_database SET datistemplate = true WHERE datname = '{template_db}'"
        ))
        .await?;
    admin
        .simple_query(&format!(
            "CREATE DATABASE {clone_one} WITH TEMPLATE {template_db}"
        ))
        .await?;
    admin
        .simple_query(&format!(
            "CREATE DATABASE {clone_two} WITH TEMPLATE {template_db}"
        ))
        .await?;

    let clone_one_conn_string = format!("postgres://postgres@{}/{clone_one}", server.addr());
    let clone_two_conn_string = format!("postgres://postgres@{}/{clone_two}", server.addr());
    let (first, first_connection) = tokio_postgres::connect(&clone_one_conn_string, NoTls).await?;
    let (second, second_connection) =
        tokio_postgres::connect(&clone_two_conn_string, NoTls).await?;
    let first_task = tokio::spawn(first_connection);
    let second_task = tokio::spawn(second_connection);

    let seed_count: i64 = second
        .query_one(
            "SELECT count(*) FROM template_marker WHERE id = 'seed'",
            &[],
        )
        .await?
        .get(0);
    assert_eq!(seed_count, 1);

    first
        .simple_query("INSERT INTO template_marker(id) VALUES ('first-instance')")
        .await?;
    let leaked_count: i64 = second
        .query_one(
            "SELECT count(*) FROM template_marker WHERE id = 'first-instance'",
            &[],
        )
        .await?
        .get(0);
    assert_eq!(leaked_count, 0);

    drop(first);
    drop(second);
    first_task.abort();
    second_task.abort();
    drop(admin);
    admin_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_schema_relation_compatibility_uses_logical_backing_schema()
-> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (admin, admin_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let admin_task = tokio::spawn(admin_connection);
    let suffix = format!(
        "{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos()
    );
    let first_db = format!("fastpg_public_a_{suffix}");
    let second_db = format!("fastpg_public_b_{suffix}");
    let role = format!("fastpg_public_user_{suffix}");

    admin
        .simple_query(&format!("DROP ROLE IF EXISTS {role}"))
        .await?;
    admin
        .simple_query(&format!("CREATE ROLE {role} LOGIN"))
        .await?;
    admin
        .simple_query(&format!("CREATE DATABASE {first_db}"))
        .await?;
    admin
        .simple_query(&format!("CREATE DATABASE {second_db}"))
        .await?;

    let first_conn_string = format!("postgres://postgres@{}/{first_db}", server.addr());
    let second_conn_string = format!("postgres://postgres@{}/{second_db}", server.addr());
    let (first, first_connection) = tokio_postgres::connect(&first_conn_string, NoTls).await?;
    let (second, second_connection) = tokio_postgres::connect(&second_conn_string, NoTls).await?;
    let first_task = tokio::spawn(first_connection);
    let second_task = tokio::spawn(second_connection);

    first
        .simple_query(
            "CREATE OR REPLACE FUNCTION set_health_system_id(id text) \
             RETURNS void AS $$ BEGIN PERFORM set_config('app.current_health_system_id', id, false); END; $$ \
             LANGUAGE plpgsql",
        )
        .await?;
    let health_system_function_visible: bool = first
        .query_one(
            "SELECT EXISTS ( \
                SELECT \
                FROM pg_catalog.pg_proc p \
                JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
                WHERE n.nspname = 'public' \
                    AND p.proname = 'set_health_system_id' \
             )",
            &[],
        )
        .await?
        .get(0);
    assert!(health_system_function_visible);
    first
        .query_one("SELECT set_health_system_id($1)", &[&"4242"])
        .await?;
    let configured_health_system_id: String = first
        .query_one(
            "SELECT current_setting('app.current_health_system_id', true)",
            &[],
        )
        .await?
        .get(0);
    assert_eq!(configured_health_system_id, "4242");
    let current_schema: String = first
        .query_one("SELECT current_schema()", &[])
        .await?
        .get(0);
    assert_eq!(current_schema, "public");
    first
        .simple_query(&format!("GRANT CREATE, USAGE ON SCHEMA public TO {role}"))
        .await?;
    first
        .simple_query(
            "CREATE TABLE public.admin_owned_table(id int PRIMARY KEY); \
             INSERT INTO public.admin_owned_table(id) VALUES (1)",
        )
        .await?;
    first
        .simple_query(&format!(
            "GRANT ALL PRIVILEGES ON ALL TABLES IN SCHEMA public TO {role}"
        ))
        .await?;
    first
        .simple_query(&format!("GRANT postgres TO {role}"))
        .await?;
    let role_grant = first
        .query_one(
            "SELECT count(*)::bigint, coalesce(bool_or(m.set_option), false) \
             FROM pg_auth_members m \
             JOIN pg_roles granted_role ON granted_role.oid = m.roleid \
             JOIN pg_roles grantee ON grantee.oid = m.member \
             WHERE granted_role.rolname = 'postgres' AND grantee.rolname = $1",
            &[&role.as_str()],
        )
        .await?;
    let role_grant_count: i64 = role_grant.get(0);
    let role_grant_can_set: bool = role_grant.get(1);
    assert_eq!(role_grant_count, 1);
    assert!(role_grant_can_set);

    let first_user_conn_string = format!("postgres://{}@{}/{first_db}", role, server.addr());
    let (first_user, first_user_connection) =
        tokio_postgres::connect(&first_user_conn_string, NoTls).await?;
    let first_user_task = tokio::spawn(first_user_connection);
    first_user.simple_query("SET ROLE postgres").await?;
    let role_state = first_user
        .query_one(
            "SELECT current_user, session_user, current_setting('is_superuser')",
            &[],
        )
        .await?;
    let current_user: String = role_state.get(0);
    let session_user: String = role_state.get(1);
    let is_superuser: String = role_state.get(2);
    assert_eq!(current_user, "postgres");
    assert_eq!(session_user, role);
    assert_eq!(is_superuser, "on");
    first_user
        .simple_query("SET session_replication_role = replica")
        .await?;
    let replica_role: String = first_user
        .query_one("SELECT current_setting('session_replication_role')", &[])
        .await?
        .get(0);
    assert_eq!(replica_role, "replica");
    first_user
        .simple_query("RESET ROLE; SET session_replication_role = origin")
        .await?;
    let admin_owned_count: i64 = first_user
        .query_one("SELECT count(*) FROM public.admin_owned_table", &[])
        .await?
        .get(0);
    assert_eq!(admin_owned_count, 1);
    first_user
        .simple_query("INSERT INTO public.admin_owned_table(id) VALUES (2)")
        .await?;
    first_user
        .simple_query(
            "CREATE TABLE public.role_owned_table(id int PRIMARY KEY); \
             INSERT INTO public.role_owned_table(id) VALUES (1)",
        )
        .await?;
    let role_owned_count: i64 = first_user
        .query_one("SELECT count(*) FROM public.role_owned_table", &[])
        .await?
        .get(0);
    assert_eq!(role_owned_count, 1);
    drop(first_user);
    first_user_task.abort();

    first
        .simple_query(
            "CREATE TABLE public.schema_migrations(version text PRIMARY KEY, line text); \
             INSERT INTO public.schema_migrations(version, line) VALUES ('first', 'main')",
        )
        .await?;
    let first_count: i64 = first
        .query_one("SELECT count(*) FROM public.schema_migrations", &[])
        .await?
        .get(0);
    assert_eq!(first_count, 1);
    first
        .simple_query(
             "CREATE TABLE public.composite_arg_source(id int PRIMARY KEY, flag text NOT NULL); \
             CREATE OR REPLACE FUNCTION public.composite_arg_payload(row_arg public.composite_arg_source) \
             RETURNS text AS $$ SELECT (row_arg).flag $$ LANGUAGE sql; \
             INSERT INTO public.composite_arg_source(id, flag) VALUES (1, 'visible')",
        )
        .await?;
    let composite_arg_payload: String = first
        .query_one(
            "SELECT public.composite_arg_payload(t) FROM public.composite_arg_source t WHERE id = 1",
            &[],
        )
        .await?
        .get(0);
    assert_eq!(composite_arg_payload, "visible");
    first
        .simple_query(
            "CREATE TYPE public.public_drop_state AS ENUM ('ready'); \
             CREATE TABLE public.public_drop_subject(id int PRIMARY KEY, state public.public_drop_state NOT NULL); \
             DROP TABLE public_drop_subject; \
             DROP TYPE public_drop_state",
        )
        .await?;
    let first_information_schema_count: i64 = first
        .query_one(
            "SELECT count(*) FROM information_schema.tables \
             WHERE table_schema = $1 AND table_name = $2",
            &[&"public", &"schema_migrations"],
        )
        .await?
        .get(0);
    assert_eq!(first_information_schema_count, 1);
    let first_information_schema_column_count: i64 = first
        .query_one(
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema = $1 AND table_name = $2 AND column_name = $3",
            &[&"public", &"schema_migrations", &"line"],
        )
        .await?
        .get(0);
    assert_eq!(first_information_schema_column_count, 1);
    let (first_dropper, first_dropper_connection) =
        tokio_postgres::connect(&first_conn_string, NoTls).await?;
    let first_dropper_task = tokio::spawn(first_dropper_connection);
    first_dropper
        .simple_query("DROP TABLE public.schema_migrations")
        .await?;
    let dropped_table_error = first
        .query_one("SELECT count(*) FROM public.schema_migrations", &[])
        .await
        .expect_err("dropped public relation should not resolve through a stale OID");
    assert_eq!(
        dropped_table_error
            .as_db_error()
            .expect("dropped relation error should be a database error")
            .code()
            .code(),
        "42P01"
    );

    let second_information_schema_count: i64 = second
        .query_one(
            "SELECT count(*) FROM information_schema.tables \
             WHERE table_schema = $1 AND table_name = $2",
            &[&"public", &"schema_migrations"],
        )
        .await?
        .get(0);
    assert_eq!(second_information_schema_count, 0);
    let second_information_schema_column_count: i64 = second
        .query_one(
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema = $1 AND table_name = $2 AND column_name = $3",
            &[&"public", &"schema_migrations", &"line"],
        )
        .await?
        .get(0);
    assert_eq!(second_information_schema_column_count, 0);
    second
        .simple_query("CREATE TABLE public.schema_migrations(version text PRIMARY KEY)")
        .await?;
    let second_count: i64 = second
        .query_one("SELECT count(*) FROM public.schema_migrations", &[])
        .await?
        .get(0);
    assert_eq!(second_count, 0);

    first
        .simple_query("DROP TABLE IF EXISTS public.role_owned_table")
        .await?;
    first
        .simple_query("DROP TABLE IF EXISTS public.admin_owned_table")
        .await?;
    first
        .simple_query(&format!(
            "REVOKE CREATE, USAGE ON SCHEMA public FROM {role}"
        ))
        .await?;
    drop(first);
    drop(second);
    first_task.abort();
    second_task.abort();
    first_dropper_task.abort();
    admin
        .simple_query(&format!("DROP ROLE IF EXISTS {role}"))
        .await?;
    drop(admin);
    admin_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_schema_extension_opclasses_are_visible_to_gin_index_builds()
-> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let (admin, admin_connection) =
        tokio_postgres::connect(&server.connection_string(), NoTls).await?;
    let admin_task = tokio::spawn(admin_connection);
    let suffix = format!(
        "{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos()
    );
    let first_db = format!("fastpg_gin_extension_a_{suffix}");
    let second_db = format!("fastpg_gin_extension_b_{suffix}");

    admin
        .simple_query(&format!("CREATE DATABASE {first_db}"))
        .await?;
    admin
        .simple_query(&format!("CREATE DATABASE {second_db}"))
        .await?;

    for database in [&first_db, &second_db] {
        let connection_string = format!("postgres://postgres@{}/{database}", server.addr());
        let (client, connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
        let connection_task = tokio::spawn(connection);

        client
            .simple_query(
                "CREATE EXTENSION IF NOT EXISTS btree_gin; \
                 CREATE EXTENSION IF NOT EXISTS pg_trgm",
            )
            .await?;
        let int2_ops_visible: bool = client
            .query_one(
                "SELECT EXISTS ( \
                    SELECT \
                    FROM pg_catalog.pg_opfamily f \
                    JOIN pg_catalog.pg_am am ON am.oid = f.opfmethod \
                    JOIN pg_catalog.pg_namespace n ON n.oid = f.opfnamespace \
                    WHERE am.amname = 'gin' \
                        AND f.opfname = 'int2_ops' \
                        AND n.nspname = 'public' \
                 )",
                &[],
            )
            .await?
            .get(0);
        assert!(int2_ops_visible);

        client
            .simple_query(
                "CREATE TABLE public.gin_extension_subject( \
                    small_value smallint, \
                    label text \
                 ); \
                 CREATE INDEX gin_extension_subject_small_value_idx \
                    ON public.gin_extension_subject USING gin (small_value); \
                 CREATE INDEX gin_extension_subject_label_idx \
                    ON public.gin_extension_subject USING gin (label gin_trgm_ops)",
            )
            .await?;

        drop(client);
        connection_task.abort();
    }

    drop(admin);
    admin_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blocking_advisory_lock_calls_do_not_starve_pgcore_lane() -> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let connection_string = server.connection_string();
    let (first, first_connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
    let first_task = tokio::spawn(first_connection);
    let (second, second_connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
    let second_task = tokio::spawn(second_connection);
    let shared_key = 45_i64;

    first.execute("BEGIN", &[]).await?;
    let first_acquired: bool = first
        .query_one("SELECT pg_try_advisory_xact_lock($1)", &[&shared_key])
        .await?
        .get(0);
    assert!(first_acquired);

    tokio::time::timeout(
        Duration::from_secs(2),
        second.query_one("SELECT pg_advisory_xact_lock($1)", &[&shared_key]),
    )
    .await??;
    let second_did_not_steal_lock: bool = second
        .query_one("SELECT pg_try_advisory_xact_lock($1)", &[&shared_key])
        .await?
        .get(0);
    assert!(!second_did_not_steal_lock);

    first.execute("COMMIT", &[]).await?;
    let second_acquired_after_commit: bool = second
        .query_one("SELECT pg_try_advisory_xact_lock($1)", &[&shared_key])
        .await?
        .get(0);
    assert!(second_acquired_after_commit);

    drop(first);
    first_task.abort();
    drop(second);
    second_task.abort();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn storage2_large_values_are_self_contained_under_parallel_writes()
-> Result<(), Box<dyn Error>> {
    let _guard = PGWIRE_TEST_MUTEX.lock().await;
    let server = TestServer::start().await?;
    let connection_string = server.connection_string();
    let (admin, admin_connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
    let admin_task = tokio::spawn(admin_connection);

    admin
        .batch_execute(
            "DROP TABLE IF EXISTS fastpg_large_values;
             CREATE TABLE fastpg_large_values(id int primary key, payload text);",
        )
        .await?;

    let mut writers = Vec::new();
    for writer_index in 0..6_i32 {
        let connection_string = connection_string.clone();
        writers.push(tokio::spawn(async move {
            let (client, connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
            let connection_task = tokio::spawn(connection);

            for iteration in 0..6_i32 {
                let id = writer_index * 100 + iteration;
                let payload = large_text_payload(id, 24_000);
                client
                    .execute(
                        "INSERT INTO fastpg_large_values(id, payload) VALUES ($1, $2)",
                        &[&id, &payload],
                    )
                    .await?;
                let row = client
                    .query_one(
                        "SELECT length(payload), payload = $2 FROM fastpg_large_values WHERE id = $1",
                        &[&id, &payload],
                    )
                    .await?;
                let len: i32 = row.get(0);
                let matches: bool = row.get(1);
                assert_eq!(len as usize, payload.len());
                assert!(matches);

                let updated = large_text_payload(id + 10_000, 24_000);
                client
                    .execute(
                        "UPDATE fastpg_large_values SET payload = $2 WHERE id = $1",
                        &[&id, &updated],
                    )
                    .await?;
                let row = client
                    .query_one(
                        "SELECT length(payload), payload = $2 FROM fastpg_large_values WHERE id = $1",
                        &[&id, &updated],
                    )
                    .await?;
                let len: i32 = row.get(0);
                let matches: bool = row.get(1);
                assert_eq!(len as usize, updated.len());
                assert!(matches);
            }

            drop(client);
            connection_task.abort();
            Ok::<(), Box<dyn Error + Send + Sync>>(())
        }));
    }

    for result in try_join_all(writers).await? {
        result.map_err(|error| -> Box<dyn Error> { error })?;
    }

    let toast_rel: String = admin
        .query_one(
            "SELECT reltoastrelid::regclass::text FROM pg_class WHERE oid = 'fastpg_large_values'::regclass",
            &[],
        )
        .await?
        .get(0);
    let count_sql = format!("SELECT count(*)::bigint FROM {toast_rel}");
    let toast_rows: i64 = admin.query_one(&count_sql, &[]).await?.get(0);
    assert_eq!(toast_rows, 0);

    drop(admin);
    admin_task.abort();

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

async fn expect_connect_error(
    connection_string: &str,
    expected_sqlstate: &str,
    expected: &str,
) -> Result<(), Box<dyn Error>> {
    match tokio_postgres::connect(connection_string, NoTls).await {
        Ok((client, connection)) => {
            let connection_task = tokio::spawn(connection);
            drop(client);
            connection_task.abort();
            Err(std::io::Error::other(format!(
                "connection unexpectedly succeeded for {connection_string}"
            ))
            .into())
        }
        Err(error) => {
            if let Some(db_error) = error.as_db_error() {
                assert_eq!(db_error.code().code(), expected_sqlstate);
                assert!(
                    db_error.message().contains(expected),
                    "expected connect error to contain {expected:?}, got {:?}",
                    db_error.message()
                );
            } else {
                let message = error.to_string();
                assert!(
                    message.contains(expected),
                    "expected connect error to contain {expected:?}, got {message:?}"
                );
            }
            Ok(())
        }
    }
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

#[derive(Debug, Clone)]
struct PgUuid([u8; 16]);

impl PgUuid {
    fn parse(input: &str) -> Result<Self, std::io::Error> {
        let hex = input.replace('-', "");
        if hex.len() != 32 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "uuid must contain 32 hexadecimal digits",
            ));
        }

        let mut bytes = [0_u8; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let offset = index * 2;
            *byte = u8::from_str_radix(&hex[offset..offset + 2], 16)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        }
        Ok(Self(bytes))
    }
}

impl ToSql for PgUuid {
    fn to_sql(
        &self,
        _: &PgType,
        out: &mut postgres_types::private::BytesMut,
    ) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        out.extend_from_slice(&self.0);
        Ok(IsNull::No)
    }

    accepts!(UUID);
    to_sql_checked!();
}

fn large_text_payload(seed: i32, len: usize) -> String {
    let mut state = seed as u64 ^ 0x9e37_79b9_7f4a_7c15;
    let mut output = String::with_capacity(len);
    while output.len() < len {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        output.push_str(&format!("{state:016x}"));
    }
    output.truncate(len);
    output
}

async fn count_rows(client: &tokio_postgres::Client, table: &str) -> Result<i64, Box<dyn Error>> {
    let messages = client
        .simple_query(&format!("SELECT count(*) FROM {table}"))
        .await?;
    let rows = rows_only(&messages);
    assert_eq!(rows.len(), 1);
    Ok(rows[0].get("count").expect("count column").parse()?)
}
