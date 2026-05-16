#!/usr/bin/env python3
"""Smoke-test fast-fork seed-only startup against an installed build."""

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


CREATE_RUNTIME_TABLE_SQL = """
CREATE TABLE seed_only_runtime(id int PRIMARY KEY, val text);
CREATE INDEX seed_only_runtime_val_idx ON seed_only_runtime(val);
INSERT INTO seed_only_runtime
SELECT g, 'value-' || g::text FROM generate_series(1, 10) AS g;
SELECT oid FROM pg_class WHERE relnamespace = 'public'::regnamespace
  AND relname = 'seed_only_runtime';
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
        conf.write("\n# bench/test_seed_only_startup.py fast test settings\n")
        for key, value in FAST_SETTINGS.items():
            conf.write(f"{key} = {quote_conf_value(value)}\n")
        conf.write(f"unix_socket_directories = {quote_conf_value(str(socket_dir))}\n")
        conf.write(f"port = {port}\n")


def bin_path(bin_dir: Path, name: str) -> str:
    path = bin_dir / name
    if not path.exists():
        raise SystemExit(f"missing required PostgreSQL binary: {path}")
    return str(path)


def psql_scalar(psql: str, conn_args: list[str], env: dict[str, str], sql: str) -> str:
    proc = run(
        [
            psql,
            *conn_args,
            "-d",
            "postgres",
            "-X",
            "-v",
            "ON_ERROR_STOP=1",
            "-Atq",
            "-c",
            sql,
        ],
        env=env,
    )
    lines = [line.strip() for line in proc.stdout.splitlines() if line.strip()]
    if not lines:
        raise RuntimeError("psql query returned no rows")
    return lines[-1]


def create_runtime_table(psql: str, conn_args: list[str], env: dict[str, str]) -> int:
    return int(psql_scalar(psql, conn_args, env, CREATE_RUNTIME_TABLE_SQL))


def assert_runtime_table_gone(psql: str, conn_args: list[str], env: dict[str, str]) -> None:
    status = psql_scalar(
        psql,
        conn_args,
        env,
        """
        SELECT CASE
          WHEN to_regclass('public.seed_only_runtime') IS NULL THEN 'gone'
          ELSE 'present'
        END;
        """,
    )
    if status != "gone":
        raise RuntimeError("runtime-created table survived seed-only restart")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", required=True, type=Path, help="PostgreSQL install bin directory")
    parser.add_argument("--keep-workdir", action="store_true", help="do not delete the disposable cluster")
    args = parser.parse_args()

    bin_dir = args.bin.resolve()
    initdb = bin_path(bin_dir, "initdb")
    pg_ctl = bin_path(bin_dir, "pg_ctl")
    psql = bin_path(bin_dir, "psql")

    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}{os.pathsep}{env.get('PATH', '')}"
    env.setdefault("LC_ALL", "C")

    workdir = Path(tempfile.mkdtemp(prefix="fastfork-seed-only-"))
    data_dir = workdir / "data"
    socket_dir = workdir / "socket"
    log_file = workdir / "postgres.log"
    socket_dir.mkdir(parents=True, exist_ok=True)
    port = find_free_port()
    conn_args = ["-h", str(socket_dir), "-p", str(port), "-U", "postgres"]
    started = False

    def start() -> None:
        nonlocal started
        run([pg_ctl, "-D", str(data_dir), "-l", str(log_file), "-w", "start"], env=env)
        started = True

    def stop(mode: str) -> None:
        nonlocal started
        if started:
            run([pg_ctl, "-D", str(data_dir), "-m", mode, "-w", "stop"], env=env)
            started = False

    try:
        run([initdb, "-D", str(data_dir), "--no-sync", "-A", "trust", "-U", "postgres"], env=env)
        append_fast_config(data_dir, socket_dir, port)

        start()
        first_oid = create_runtime_table(psql, conn_args, env)
        stop("fast")

        start()
        assert_runtime_table_gone(psql, conn_args, env)
        second_oid = create_runtime_table(psql, conn_args, env)
        if second_oid != first_oid:
            raise RuntimeError(
                f"seed-only clean restart did not reset OID state: {first_oid} != {second_oid}"
            )
        stop("immediate")

        start()
        assert_runtime_table_gone(psql, conn_args, env)
        third_oid = create_runtime_table(psql, conn_args, env)
        if third_oid != first_oid:
            raise RuntimeError(
                f"seed-only immediate restart did not reset OID state: {first_oid} != {third_oid}"
            )

        print("fast-fork seed-only startup smoke test passed")
        return 0
    except (RuntimeError, subprocess.CalledProcessError) as exc:
        if isinstance(exc, subprocess.CalledProcessError):
            if exc.stdout:
                print(exc.stdout, file=sys.stderr)
            if exc.stderr:
                print(exc.stderr, file=sys.stderr)
            rc = exc.returncode
        else:
            print(str(exc), file=sys.stderr)
            rc = 1
        print(f"cluster log: {log_file}", file=sys.stderr)
        return rc
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
