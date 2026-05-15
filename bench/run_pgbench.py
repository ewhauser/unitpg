#!/usr/bin/env python3
"""Run the rollback-heavy pgbench workload against a PostgreSQL build."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKLOAD = ROOT / "bench" / "unit-test-rollback.pgbench"


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


def run(cmd: list[str], *, env: dict[str, str], capture: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        env=env,
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


def append_fast_config(data_dir: Path, socket_dir: Path, port: int, extra: list[str]) -> dict[str, str]:
    settings = dict(FAST_SETTINGS)
    settings["unix_socket_directories"] = str(socket_dir)
    settings["port"] = str(port)

    with (data_dir / "postgresql.conf").open("a", encoding="utf-8") as conf:
        conf.write("\n# bench/run_pgbench.py fast test settings\n")
        for key, value in settings.items():
            conf.write(f"{key} = {quote_conf_value(value)}\n")
        for line in extra:
            conf.write(f"{line}\n")

    return settings


def parse_pgbench_output(stdout: str) -> dict[str, object]:
    metrics: dict[str, object] = {"raw_stdout": stdout}
    patterns: list[tuple[str, str, object]] = [
        ("clients", r"number of clients:\s+(\d+)", int),
        ("threads", r"number of threads:\s+(\d+)", int),
        ("transactions_per_client", r"number of transactions per client:\s+(\d+)", int),
        ("transactions_processed", r"number of transactions actually processed:\s+(\d+)(?:/\d+)?", int),
        ("latency_average_ms", r"latency average =\s+([0-9.]+) ms", float),
        ("tps", r"tps =\s+([0-9.]+) \(without initial connection time\)", float),
    ]
    for key, pattern, caster in patterns:
        match = re.search(pattern, stdout)
        if match:
            metrics[key] = caster(match.group(1))
    return metrics


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def bin_path(bin_dir: Path, name: str) -> str:
    path = bin_dir / name
    if not path.exists():
        raise SystemExit(f"missing required PostgreSQL binary: {path}")
    return str(path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", required=True, type=Path, help="PostgreSQL install bin directory")
    parser.add_argument("--label", default="postgres", help="label to record in the JSON output")
    parser.add_argument("--output", type=Path, help="write JSON results to this path")
    parser.add_argument("--workdir", type=Path, help="directory for the disposable cluster")
    parser.add_argument("--keep-workdir", action="store_true", help="do not delete the disposable cluster")
    parser.add_argument("--clients", type=int, default=1)
    parser.add_argument("--jobs", type=int, default=1)
    parser.add_argument("--transactions", type=int, default=100)
    parser.add_argument("--rows", type=int, default=200, help="rows inserted into the parent table per transaction")
    parser.add_argument("--warmup-transactions", type=int, default=10)
    parser.add_argument("--random-seed", default="1")
    parser.add_argument(
        "--config",
        action="append",
        default=[],
        help="extra postgresql.conf line, for example --config shared_buffers=256MB",
    )
    args = parser.parse_args()

    bin_dir = args.bin.resolve()
    initdb = bin_path(bin_dir, "initdb")
    pg_ctl = bin_path(bin_dir, "pg_ctl")
    createdb = bin_path(bin_dir, "createdb")
    pgbench = bin_path(bin_dir, "pgbench")
    postgres = bin_path(bin_dir, "postgres")

    base_env = os.environ.copy()
    base_env["PATH"] = f"{bin_dir}{os.pathsep}{base_env.get('PATH', '')}"
    base_env.setdefault("LC_ALL", "C")

    if args.workdir:
        workdir = args.workdir.resolve()
        workdir.mkdir(parents=True, exist_ok=True)
        remove_workdir = False
    else:
        workdir = Path(tempfile.mkdtemp(prefix="pgbench-rollback-"))
        remove_workdir = not args.keep_workdir

    data_dir = workdir / "data"
    socket_dir = workdir / "socket"
    log_file = workdir / "postgres.log"
    socket_dir.mkdir(parents=True, exist_ok=True)
    port = find_free_port()
    started = False

    started_at = dt.datetime.now(dt.UTC).isoformat()
    t0 = time.perf_counter()

    try:
        run([initdb, "-D", str(data_dir), "--no-sync", "-A", "trust", "-U", "postgres"], env=base_env)
        settings = append_fast_config(data_dir, socket_dir, port, args.config)
        run([pg_ctl, "-D", str(data_dir), "-l", str(log_file), "-w", "start"], env=base_env)
        started = True

        conn_args = ["-h", str(socket_dir), "-p", str(port), "-U", "postgres"]
        run([createdb, *conn_args, "bench"], env=base_env)

        common_pgbench = [
            pgbench,
            *conn_args,
            "-d",
            "bench",
            "-n",
            "--protocol=simple",
            f"--random-seed={args.random_seed}",
            "-D",
            f"bench_rows={args.rows}",
            "-c",
            str(args.clients),
            "-j",
            str(args.jobs),
            "-f",
            str(WORKLOAD),
        ]

        warmup = None
        if args.warmup_transactions > 0:
            warmup_proc = run(
                [*common_pgbench, "-t", str(args.warmup_transactions)],
                env=base_env,
            )
            warmup = parse_pgbench_output(warmup_proc.stdout)

        bench_proc = run([*common_pgbench, "-t", str(args.transactions)], env=base_env)
        result = parse_pgbench_output(bench_proc.stdout)

        payload = {
            "label": args.label,
            "started_at": started_at,
            "duration_seconds": round(time.perf_counter() - t0, 6),
            "bin_dir": str(bin_dir),
            "workdir": str(workdir),
            "postgres_version": run([postgres, "--version"], env=base_env).stdout.strip(),
            "pgbench_version": run([pgbench, "--version"], env=base_env).stdout.strip(),
            "workload": str(WORKLOAD),
            "workload_sha256": sha256(WORKLOAD),
            "settings": settings,
            "parameters": {
                "clients": args.clients,
                "jobs": args.jobs,
                "transactions": args.transactions,
                "rows": args.rows,
                "warmup_transactions": args.warmup_transactions,
                "random_seed": args.random_seed,
            },
            "warmup": warmup,
            "result": result,
        }

        output = json.dumps(payload, indent=2, sort_keys=True)
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(output + "\n", encoding="utf-8")
        print(output)
        return 0
    finally:
        if started:
            subprocess.run(
                [pg_ctl, "-D", str(data_dir), "-m", "fast", "-w", "stop"],
                env=base_env,
                text=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )
        if args.keep_workdir and not args.workdir:
            print(f"kept workdir: {workdir}", file=sys.stderr)
        if remove_workdir:
            shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
