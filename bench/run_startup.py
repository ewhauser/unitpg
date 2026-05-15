#!/usr/bin/env python3
"""Measure PostgreSQL initdb/start/first-query/stop time for one install."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from statistics import fmean, median


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


def run(
    cmd: list[str],
    *,
    env: dict[str, str],
    capture: bool = True,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        env=env,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
        check=True,
    )


def timed_run(
    cmd: list[str],
    *,
    env: dict[str, str],
    capture: bool = True,
) -> tuple[subprocess.CompletedProcess[str], float]:
    start = time.perf_counter()
    proc = run(cmd, env=env, capture=capture)
    return proc, round(time.perf_counter() - start, 6)


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
        conf.write("\n# bench/run_startup.py fast test settings\n")
        for key, value in settings.items():
            conf.write(f"{key} = {quote_conf_value(value)}\n")
        for line in extra:
            conf.write(f"{line}\n")

    return settings


def bin_path(bin_dir: Path, name: str) -> str:
    path = bin_dir / name
    if not path.exists():
        raise SystemExit(f"missing required PostgreSQL binary: {path}")
    return str(path)


def summarize(values: list[float]) -> dict[str, float | None]:
    if not values:
        return {"min": None, "max": None, "mean": None, "median": None}
    return {
        "min": min(values),
        "max": max(values),
        "mean": fmean(values),
        "median": median(values),
    }


def start_query_stop_round(
    *,
    data_dir: Path,
    socket_dir: Path,
    log_file: Path,
    pg_ctl: str,
    psql: str,
    port: int,
    stop_mode: str,
    env: dict[str, str],
) -> tuple[dict[str, object], bool]:
    started = False
    round_result: dict[str, object] = {"postgres_log_path": str(log_file)}
    query_cmd = [
        psql,
        "-h",
        str(socket_dir),
        "-p",
        str(port),
        "-U",
        "postgres",
        "-d",
        "postgres",
        "-Atq",
        "-c",
        "SELECT 1",
    ]

    try:
        start_time = time.perf_counter()
        launch_start = time.perf_counter()
        run(
            [pg_ctl, "-D", str(data_dir), "-l", str(log_file), "-W", "start"],
            env=env,
        )
        round_result["pg_ctl_launch_seconds"] = round(time.perf_counter() - launch_start, 6)
        started = True
        deadline = start_time + 10
        query_attempts = 0
        first_query_failure = None
        last_query_failure = None

        while True:
            query_start = time.perf_counter()
            proc = subprocess.run(
                query_cmd,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            query_attempts += 1
            query_seconds = round(time.perf_counter() - query_start, 6)
            if proc.returncode == 0:
                round_result["pg_ctl_start_seconds"] = round(time.perf_counter() - start_time, 6)
                round_result["first_query_seconds"] = query_seconds
                round_result["query_attempts"] = query_attempts
                if first_query_failure is not None:
                    round_result["first_query_failure"] = first_query_failure
                    round_result["last_query_failure"] = last_query_failure
                break

            failure = (proc.stderr or proc.stdout).strip()
            if failure:
                if first_query_failure is None:
                    first_query_failure = failure
                last_query_failure = failure

            if time.perf_counter() >= deadline:
                raise subprocess.CalledProcessError(
                    proc.returncode,
                    query_cmd,
                    output=proc.stdout,
                    stderr=proc.stderr,
                )

            time.sleep(0.005)
    finally:
        if started:
            stop_start = time.perf_counter()
            subprocess.run(
                [pg_ctl, "-D", str(data_dir), "-m", stop_mode, "-w", "stop"],
                env=env,
                text=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )
            round_result["pg_ctl_stop_seconds"] = round(time.perf_counter() - stop_start, 6)

    return round_result, started


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", required=True, type=Path, help="PostgreSQL install bin directory")
    parser.add_argument("--label", default="postgres", help="label to record in the JSON output")
    parser.add_argument("--output", type=Path, help="write JSON results to this path")
    parser.add_argument("--workdir", type=Path, help="directory for the disposable cluster")
    parser.add_argument("--keep-workdir", action="store_true", help="do not delete the disposable cluster")
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--mode", choices=["reuse", "copy"], default="reuse")
    parser.add_argument(
        "--stop-mode",
        choices=["fast", "immediate"],
        default="fast",
        help="shutdown mode to use between startup rounds",
    )
    parser.add_argument(
        "--config",
        action="append",
        default=[],
        help="extra postgresql.conf line, for example --config shared_buffers=256MB",
    )
    args = parser.parse_args()

    if args.rounds < 1:
        raise SystemExit("--rounds must be at least 1")

    bin_dir = args.bin.resolve()
    initdb = bin_path(bin_dir, "initdb")
    pg_ctl = bin_path(bin_dir, "pg_ctl")
    psql = bin_path(bin_dir, "psql")
    postgres = bin_path(bin_dir, "postgres")

    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}{os.pathsep}{env.get('PATH', '')}"
    env.setdefault("LC_ALL", "C")

    if args.workdir:
        workdir = args.workdir.resolve()
        workdir.mkdir(parents=True, exist_ok=True)
        remove_workdir = False
    else:
        workdir = Path(tempfile.mkdtemp(prefix="postgres-startup-"))
        remove_workdir = not args.keep_workdir

    seed_dir = workdir / "seed"
    socket_dir = workdir / "socket"
    socket_dir.mkdir(parents=True, exist_ok=True)
    port = find_free_port()

    started_at = dt.datetime.now(dt.UTC).isoformat()
    total_start = time.perf_counter()

    try:
        _, initdb_seconds = timed_run(
            [initdb, "-D", str(seed_dir), "--no-sync", "-A", "trust", "-U", "postgres"],
            env=env,
        )
        settings = append_fast_config(seed_dir, socket_dir, port, args.config)

        rounds: list[dict[str, object]] = []
        for round_number in range(1, args.rounds + 1):
            if args.mode == "reuse":
                data_dir = seed_dir
                copy_seconds = None
            else:
                data_dir = workdir / f"round-{round_number:03d}"
                copy_start = time.perf_counter()
                shutil.copytree(seed_dir, data_dir)
                copy_seconds = round(time.perf_counter() - copy_start, 6)

            log_file = workdir / f"postgres-{round_number:03d}.log"
            round_result, _ = start_query_stop_round(
                data_dir=data_dir,
                socket_dir=socket_dir,
                log_file=log_file,
                pg_ctl=pg_ctl,
                psql=psql,
                port=port,
                stop_mode=args.stop_mode,
                env=env,
            )
            round_result["round"] = round_number
            if copy_seconds is not None:
                round_result["copy_seconds"] = copy_seconds
            rounds.append(round_result)

            if args.mode == "copy" and not args.keep_workdir:
                shutil.rmtree(data_dir, ignore_errors=True)

        start_times = [float(round_data["pg_ctl_start_seconds"]) for round_data in rounds]
        launch_times = [float(round_data["pg_ctl_launch_seconds"]) for round_data in rounds]
        first_query_times = [float(round_data["first_query_seconds"]) for round_data in rounds]
        stop_times = [float(round_data["pg_ctl_stop_seconds"]) for round_data in rounds]
        query_attempts = [float(round_data["query_attempts"]) for round_data in rounds]
        copy_times = [
            float(round_data["copy_seconds"])
            for round_data in rounds
            if "copy_seconds" in round_data
        ]

        payload = {
            "label": args.label,
            "started_at": started_at,
            "duration_seconds": round(time.perf_counter() - total_start, 6),
            "bin_dir": str(bin_dir),
            "workdir": str(workdir),
            "postgres_version": run([postgres, "--version"], env=env).stdout.strip(),
            "settings": settings,
            "mode": args.mode,
            "stop_mode": args.stop_mode,
            "rounds": args.rounds,
            "initdb_seconds": initdb_seconds,
            "round_results": rounds,
            "summary": {
                "copy_seconds": summarize(copy_times),
                "pg_ctl_launch_seconds": summarize(launch_times),
                "pg_ctl_start_seconds": summarize(start_times),
                "first_query_seconds": summarize(first_query_times),
                "query_attempts": summarize(query_attempts),
                "pg_ctl_stop_seconds": summarize(stop_times),
            },
        }

        output = json.dumps(payload, indent=2, sort_keys=True)
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(output + "\n", encoding="utf-8")
        print(output)
        return 0
    except subprocess.CalledProcessError as exc:
        if exc.stdout:
            print(exc.stdout, file=sys.stderr)
        if exc.stderr:
            print(exc.stderr, file=sys.stderr)
        print(f"startup benchmark workdir: {workdir}", file=sys.stderr)
        return exc.returncode
    finally:
        if args.keep_workdir and not args.workdir:
            print(f"kept workdir: {workdir}", file=sys.stderr)
        if remove_workdir:
            shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
