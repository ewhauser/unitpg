#!/usr/bin/env python3
"""Smoke-test fast-fork fixture snapshot/restore against an installed build."""

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


SQL = r"""
\set ON_ERROR_STOP on
CREATE TABLE fixture_parent(id int PRIMARY KEY, val text);
CREATE INDEX fixture_parent_val_idx ON fixture_parent(val);
CREATE SEQUENCE fixture_seq CACHE 1;
INSERT INTO fixture_parent
SELECT g, 'base-' || g::text FROM generate_series(1, 25) AS g;
SELECT nextval('fixture_seq');
SELECT pg_fastfork_snapshot('fixture');
CREATE TABLE after_snapshot(id int);
INSERT INTO after_snapshot VALUES (1);
INSERT INTO fixture_parent VALUES (1000, 'mutated');
UPDATE fixture_parent SET val = 'changed' WHERE id = 1;
SELECT nextval('fixture_seq');
SELECT pg_fastfork_restore('fixture');
DO $$
BEGIN
  IF (SELECT count(*) FROM fixture_parent) <> 25 THEN
    RAISE EXCEPTION 'fixture row count was not restored';
  END IF;

  IF (SELECT val FROM fixture_parent WHERE id = 1) <> 'base-1' THEN
    RAISE EXCEPTION 'fixture row value was not restored';
  END IF;

  IF to_regclass('after_snapshot') IS NOT NULL THEN
    RAISE EXCEPTION 'post-snapshot table still exists after restore';
  END IF;

  IF nextval('fixture_seq') <> 2 THEN
    RAISE EXCEPTION 'sequence state was not restored';
  END IF;
END $$;
SET enable_seqscan = off;
SELECT val FROM fixture_parent WHERE id = 5;
SELECT pg_fastfork_drop_snapshot('fixture');
"""


def run(
    cmd: list[str],
    *,
    env: dict[str, str],
    input_text: str | None = None,
    capture: bool = True,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        env=env,
        input=input_text,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
        check=True,
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
        conf.write("\n# bench/test_fastfork_snapshot.py fast test settings\n")
        for key, value in FAST_SETTINGS.items():
            conf.write(f"{key} = {quote_conf_value(value)}\n")
        conf.write(f"unix_socket_directories = {quote_conf_value(str(socket_dir))}\n")
        conf.write(f"port = {port}\n")


def bin_path(bin_dir: Path, name: str) -> str:
    path = bin_dir / name
    if not path.exists():
        raise SystemExit(f"missing required PostgreSQL binary: {path}")
    return str(path)


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

    workdir = Path(tempfile.mkdtemp(prefix="fastfork-snapshot-"))
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
        run([psql, *conn_args, "-d", "bench"], env=env, input_text=SQL)
        print("fast-fork snapshot smoke test passed")
        return 0
    except subprocess.CalledProcessError as exc:
        if exc.stdout:
            print(exc.stdout, file=sys.stderr)
        if exc.stderr:
            print(exc.stderr, file=sys.stderr)
        print(f"cluster log: {log_file}", file=sys.stderr)
        return exc.returncode
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
