#!/usr/bin/env python3
"""Verify the macOS fast-fork build does not allocate SysV semaphores."""

from __future__ import annotations

import argparse
import os
import platform
import shutil
import socket
import subprocess
import sys
import tempfile
import time
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


def bin_path(bin_dir: Path, name: str) -> str:
    path = bin_dir / name
    if not path.exists():
        raise SystemExit(f"missing required PostgreSQL binary: {path}")
    return str(path)


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
        conf.write("\n# bench/test_macos_named_posix_semaphores.py settings\n")
        for key, value in FAST_SETTINGS.items():
            conf.write(f"{key} = {quote_conf_value(value)}\n")
        conf.write(f"unix_socket_directories = {quote_conf_value(str(socket_dir))}\n")
        conf.write(f"port = {port}\n")


def sysv_sem_ids(env: dict[str, str]) -> set[str]:
    proc = subprocess.run(
        ["ipcs", "-s"],
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            "ipcs -s failed while checking SysV semaphores:\n"
            + proc.stdout
            + proc.stderr
        )

    ids: set[str] = set()
    for line in proc.stdout.splitlines():
        fields = line.split()
        if len(fields) >= 2 and fields[0] == "s":
            ids.add(fields[1])
    return ids


def read_postmaster_pid(data_dir: Path) -> int:
    pid_line = (data_dir / "postmaster.pid").read_text(encoding="utf-8").splitlines()[0]
    return int(pid_line)


def process_exists(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def kill_postmaster(data_dir: Path) -> None:
    pid = read_postmaster_pid(data_dir)
    os.kill(pid, 9)

    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if not process_exists(pid):
            return
        time.sleep(0.05)

    raise RuntimeError(f"postmaster pid {pid} did not exit after SIGKILL")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", required=True, type=Path, help="PostgreSQL install bin directory")
    parser.add_argument("--keep-workdir", action="store_true", help="do not delete the disposable cluster")
    args = parser.parse_args()

    if platform.system() != "Darwin":
        print("macOS named POSIX semaphore smoke test skipped")
        return 0

    bin_dir = args.bin.resolve()
    initdb = bin_path(bin_dir, "initdb")
    pg_ctl = bin_path(bin_dir, "pg_ctl")

    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}{os.pathsep}{env.get('PATH', '')}"
    env.setdefault("LC_ALL", "C")

    workdir = Path(tempfile.mkdtemp(prefix="fastfork-macos-sema-"))
    data_dir = workdir / "data"
    socket_dir = workdir / "socket"
    log_file = workdir / "postgres.log"
    socket_dir.mkdir(parents=True, exist_ok=True)
    started = False

    try:
        before = sysv_sem_ids(env)
        run([initdb, "-D", str(data_dir), "--no-sync", "-A", "trust", "-U", "postgres"], env=env)
        append_fast_config(data_dir, socket_dir, find_free_port())

        run([pg_ctl, "-D", str(data_dir), "-l", str(log_file), "-w", "start"], env=env)
        started = True
        kill_postmaster(data_dir)
        started = False
        time.sleep(0.2)

        after = sysv_sem_ids(env)
        leaked = sorted(after - before)
        if leaked:
            raise RuntimeError(
                "fast-fork macOS semaphore test created SysV semaphore IDs after "
                f"postmaster SIGKILL: {', '.join(leaked)}"
            )

        print("fast-fork macOS named POSIX semaphore smoke test passed")
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
        if started:
            subprocess.run(
                [pg_ctl, "-D", str(data_dir), "-m", "immediate", "-w", "stop"],
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
