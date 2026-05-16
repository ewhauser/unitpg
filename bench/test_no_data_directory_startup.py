#!/usr/bin/env python3
"""Smoke-test fast-fork no-data-directory startup against an installed build."""

from __future__ import annotations

import argparse
import os
import shutil
import socket
import subprocess
import sys
import tempfile
from pathlib import Path

from fastfork_seed import copy_runtime_skeleton


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
CREATE TABLE no_data_runtime(id int PRIMARY KEY, val text);
ALTER TABLE no_data_runtime ADD COLUMN migrated text DEFAULT 'yes';
CREATE INDEX no_data_runtime_val_idx ON no_data_runtime(val);
INSERT INTO no_data_runtime(id, val)
SELECT g, 'value-' || g::text FROM generate_series(1, 10) AS g;
SELECT oid FROM pg_class WHERE relnamespace = 'public'::regnamespace
  AND relname = 'no_data_runtime';
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
        conf.write("\n# bench/test_no_data_directory_startup.py fast test settings\n")
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
    oid = int(psql_scalar(psql, conn_args, env, CREATE_RUNTIME_TABLE_SQL))
    row_count = psql_scalar(
        psql,
        conn_args,
        env,
        "SELECT count(*) FROM no_data_runtime WHERE migrated = 'yes';",
    )
    if row_count != "10":
        raise RuntimeError(f"unexpected migrated table row count: {row_count}")
    return oid


def assert_runtime_table_gone(psql: str, conn_args: list[str], env: dict[str, str]) -> None:
    status = psql_scalar(
        psql,
        conn_args,
        env,
        """
        SELECT CASE
          WHEN to_regclass('public.no_data_runtime') IS NULL THEN 'gone'
          ELSE 'present'
        END;
        """,
    )
    if status != "gone":
        raise RuntimeError("runtime-created table survived no-data-directory restart")


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

    workdir = Path(tempfile.mkdtemp(prefix="fastfork-no-data-dir-"))
    seed_dir = workdir / "seed"
    socket_dir = workdir / "socket"
    socket_dir.mkdir(parents=True, exist_ok=True)
    port = find_free_port()
    conn_args = ["-h", str(socket_dir), "-p", str(port), "-U", "postgres"]
    started = False
    current_data_dir: Path | None = None
    current_env: dict[str, str] | None = None
    round_counter = 0

    def start() -> None:
        nonlocal current_data_dir, current_env, round_counter, started
        round_counter += 1
        data_dir = workdir / f"runtime-{round_counter:03d}"
        log_file = workdir / f"postgres-{round_counter:03d}.log"
        stats = copy_runtime_skeleton(seed_dir, data_dir)
        if stats.files <= 0:
            raise RuntimeError("runtime skeleton copied no compatibility files")
        append_fast_config(data_dir, socket_dir, port)

        run_env = env.copy()
        run_env["PG_FASTFORK_SEED_DIR"] = str(seed_dir)
        run([pg_ctl, "-D", str(data_dir), "-l", str(log_file), "-w", "start"], env=run_env)
        current_data_dir = data_dir
        current_env = run_env
        started = True

    def stop(mode: str) -> None:
        nonlocal started
        if started and current_data_dir is not None and current_env is not None:
            run([pg_ctl, "-D", str(current_data_dir), "-m", mode, "-w", "stop"], env=current_env)
            started = False

    try:
        run([initdb, "-D", str(seed_dir), "--no-sync", "-A", "trust", "-U", "postgres"], env=env)

        start()
        assert current_env is not None
        first_oid = create_runtime_table(psql, conn_args, current_env)
        stop("fast")

        start()
        assert current_env is not None
        assert_runtime_table_gone(psql, conn_args, current_env)
        second_oid = create_runtime_table(psql, conn_args, current_env)
        if second_oid != first_oid:
            raise RuntimeError(
                f"no-data-directory clean restart did not reset OID state: {first_oid} != {second_oid}"
            )
        stop("immediate")

        start()
        assert current_env is not None
        assert_runtime_table_gone(psql, conn_args, current_env)
        third_oid = create_runtime_table(psql, conn_args, current_env)
        if third_oid != first_oid:
            raise RuntimeError(
                f"no-data-directory immediate restart did not reset OID state: {first_oid} != {third_oid}"
            )

        print("fast-fork no-data-directory startup smoke test passed")
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
        print(f"workdir: {workdir}", file=sys.stderr)
        return rc
    finally:
        if started and current_data_dir is not None and current_env is not None:
            subprocess.run(
                [pg_ctl, "-D", str(current_data_dir), "-m", "fast", "-w", "stop"],
                env=current_env,
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
