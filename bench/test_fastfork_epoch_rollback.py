#!/usr/bin/env python3
"""Smoke-test fast-fork epoch rollback against an installed build."""

from __future__ import annotations

import argparse
import os
import shutil
import socket
import subprocess
import sys
import tempfile
from pathlib import Path


FAST_SETTINGS = {
    "listen_addresses": "",
    "fsync": "off",
    "synchronous_commit": "off",
    "full_page_writes": "off",
    "wal_level": "minimal",
    "archive_mode": "off",
    "max_wal_senders": "0",
    "max_replication_slots": "0",
    "autovacuum": "off",
    "track_counts": "off",
    "jit": "off",
}


SETUP_SQL = r"""
\set ON_ERROR_STOP on
CREATE TABLE epoch_parent(id int PRIMARY KEY, val text NOT NULL, bucket int NOT NULL);
CREATE INDEX epoch_parent_bucket_idx ON epoch_parent(bucket, id);
CREATE TABLE epoch_child(
    id int PRIMARY KEY,
    parent_id int NOT NULL REFERENCES epoch_parent(id),
    amount int NOT NULL
);
CREATE INDEX epoch_child_parent_amount_idx ON epoch_child(parent_id, amount);
INSERT INTO epoch_parent
SELECT g, 'base-' || g::text, g % 4 FROM generate_series(1, 25) AS g;
INSERT INTO epoch_child
SELECT g, ((g - 1) % 25) + 1, g % 10 FROM generate_series(1, 80) AS g;
SELECT pg_fastfork_snapshot('fixture');
"""


CAPTURE_SQL = r"""
\set ON_ERROR_STOP on
SELECT pg_fastfork_snapshot('fixture');
"""


DML_SQL = r"""
\set ON_ERROR_STOP on
SELECT pg_fastfork_restore('fixture');
BEGIN;
SELECT pg_fastfork_epoch_begin();
SELECT pg_fastfork_epoch_status();
UPDATE epoch_parent SET val = 'changed' WHERE id = 1;
DELETE FROM epoch_child WHERE parent_id = 1;
INSERT INTO epoch_child VALUES (1000, 2, 42);
SAVEPOINT s;
INSERT INTO epoch_child VALUES (1001, 3, 43);
ROLLBACK TO s;
SELECT 1 / CASE WHEN (SELECT val FROM epoch_parent WHERE id = 1) = 'changed'
           THEN 1 ELSE 0 END;
SELECT 1 / CASE WHEN NOT EXISTS (SELECT 1 FROM epoch_child WHERE id = 1001)
           THEN 1 ELSE 0 END;
ROLLBACK;
DO $$
BEGIN
  IF (SELECT val FROM epoch_parent WHERE id = 1) <> 'base-1' THEN
    RAISE EXCEPTION 'epoch rollback leaked heap update';
  END IF;

  IF (SELECT count(*) FROM epoch_child WHERE parent_id = 1) <> 4 THEN
    RAISE EXCEPTION 'epoch rollback leaked child delete';
  END IF;

  IF EXISTS (SELECT 1 FROM epoch_child WHERE id IN (1000, 1001)) THEN
    RAISE EXCEPTION 'epoch rollback leaked inserted rows';
  END IF;
END $$;
SET enable_seqscan = off;
SELECT val FROM epoch_parent WHERE bucket = 1 AND id = 5;
"""


COMMIT_SQL = r"""
\set ON_ERROR_STOP on
SELECT pg_fastfork_restore('fixture');
BEGIN;
SELECT pg_fastfork_epoch_begin();
UPDATE epoch_parent SET val = 'commit-leak' WHERE id = 1;
COMMIT;
"""


DDL_SQL = r"""
\set ON_ERROR_STOP on
SELECT pg_fastfork_restore('fixture');
BEGIN;
SELECT pg_fastfork_epoch_begin();
CREATE TABLE epoch_bad(id int);
ROLLBACK;
"""


PREPARE_SQL = r"""
\set ON_ERROR_STOP on
SELECT pg_fastfork_restore('fixture');
BEGIN;
SELECT pg_fastfork_epoch_begin();
PREPARE TRANSACTION 'epoch_bad';
"""


def run(
    cmd: list[str],
    *,
    env: dict[str, str],
    input_text: str | None = None,
    capture: bool = True,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        env=env,
        input=input_text,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
        check=check,
    )


def find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def quote_conf_value(value: str) -> str:
    if value in {"on", "off"} or value.isdigit():
        return value
    return "'" + value.replace("'", "''") + "'"


def append_fast_config(data_dir: Path, socket_dir: Path, port: int) -> None:
    with (data_dir / "postgresql.conf").open("a", encoding="utf-8") as conf:
        conf.write("\n# bench/test_fastfork_epoch_rollback.py fast test settings\n")
        for key, value in FAST_SETTINGS.items():
            conf.write(f"{key} = {quote_conf_value(value)}\n")
        conf.write(f"unix_socket_directories = {quote_conf_value(str(socket_dir))}\n")
        conf.write(f"port = {port}\n")


def bin_path(bin_dir: Path, name: str) -> str:
    path = bin_dir / name
    if not path.exists():
        raise SystemExit(f"missing required PostgreSQL binary: {path}")
    return str(path)


def expect_error(
    psql: str,
    conn_args: list[str],
    env: dict[str, str],
    sql: str,
    expected: str,
) -> None:
    proc = run([psql, *conn_args, "-d", "bench"], env=env, input_text=sql, check=False)
    combined = (proc.stdout or "") + (proc.stderr or "")
    if proc.returncode == 0:
        raise RuntimeError(f"expected psql command to fail with {expected!r}")
    if expected not in combined:
        raise RuntimeError(f"expected error containing {expected!r}, got:\n{combined}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", required=True, type=Path, help="PostgreSQL install bin directory")
    parser.add_argument("--keep-workdir", action="store_true", help="do not delete the disposable cluster")
    args = parser.parse_args()

    bin_dir = args.bin.resolve()
    initdb = bin_path(bin_dir, "initdb")
    pg_ctl = bin_path(bin_dir, "pg_ctl")
    createdb = bin_path(bin_dir, "createdb")
    psql = bin_path(bin_dir, "psql")

    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}{os.pathsep}{env.get('PATH', '')}"
    env.setdefault("LC_ALL", "C")

    workdir = Path(tempfile.mkdtemp(prefix="fastfork-epoch-"))
    data_dir = workdir / "data"
    socket_dir = workdir / "socket"
    log_file = workdir / "postgres.log"
    socket_dir.mkdir(parents=True, exist_ok=True)
    port = find_free_port()
    started = False

    try:
        run([initdb, "-D", str(data_dir), "--no-sync", "-A", "trust", "-U", "postgres"], env=env)
        append_fast_config(data_dir, socket_dir, port)
        run([pg_ctl, "-D", str(data_dir), "-l", str(log_file), "-w", "start"], env=env)
        started = True

        conn_args = ["-h", str(socket_dir), "-p", str(port), "-U", "postgres"]
        run([createdb, *conn_args, "bench"], env=env)
        run([psql, *conn_args, "-d", "bench"], env=env, input_text=SETUP_SQL)
        run([psql, *conn_args, "-d", "bench"], env=env, input_text=CAPTURE_SQL + DML_SQL)
        expect_error(
            psql,
            conn_args,
            env,
            CAPTURE_SQL + COMMIT_SQL,
            "fast-fork epoch transactions are rollback-only",
        )
        expect_error(
            psql,
            conn_args,
            env,
            CAPTURE_SQL + DDL_SQL,
            "DDL inside fast-fork epoch transactions requires test_ephemeral_catalog",
        )
        expect_error(
            psql,
            conn_args,
            env,
            CAPTURE_SQL + PREPARE_SQL,
            "prepared transactions are not supported in fast-fork epoch transactions",
        )
        run([psql, *conn_args, "-d", "bench"], env=env, input_text=CAPTURE_SQL + DML_SQL)
        print("fast-fork epoch rollback smoke test passed")
        return 0
    except (RuntimeError, subprocess.CalledProcessError) as exc:
        if isinstance(exc, subprocess.CalledProcessError):
            if exc.stdout:
                print(exc.stdout, file=sys.stderr)
            if exc.stderr:
                print(exc.stderr, file=sys.stderr)
        else:
            print(str(exc), file=sys.stderr)
        print(f"cluster log: {log_file}", file=sys.stderr)
        return 1
    finally:
        if started:
            subprocess.run(
                [pg_ctl, "-D", str(data_dir), "-m", "fast", "-w", "stop"],
                env=env,
                text=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )
        if args.keep_workdir:
            print(f"kept workdir: {workdir}", file=sys.stderr)
        else:
            shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
