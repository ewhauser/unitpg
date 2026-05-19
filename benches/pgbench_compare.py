#!/usr/bin/env python3
"""Build normal/fastpg Postgres variants and compare them with pgbench."""

from __future__ import annotations

import argparse
import html
import json
import os
import platform
import re
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from statistics import median
from typing import Any


TPS_RE = re.compile(r"^tps =\s+([0-9.]+)\s+\(without initial connection time\)", re.MULTILINE)
LATENCY_RE = re.compile(r"^latency average =\s+([0-9.]+)\s+ms", re.MULTILINE)
SVG_TITLE_RE = re.compile(r"<title>(.*?)</title>")
SVG_TOTAL_SAMPLES_RE = re.compile(r'<svg id="frames"[^>]*\btotal_samples="([\d,]+)"')
HOTSPOT_RE = re.compile(r"^(.*) \(([\d,]+) samples?, ([0-9.]+)%\)$")
DEFAULT_PGBENCH_INIT_STEPS = "dtgvp"
PROFILE_COMPONENT_ORDER = [
    "Socket / protocol",
    "Query dispatch",
    "Parsing / analysis",
    "Planning / rewrite",
    "Execution",
    "Storage / index AM",
    "Catalog / metadata",
    "Transactions / WAL",
    "Runtime / memory",
    "Other",
]
PROFILE_HOTSPOT_LIMIT = 40
PROFILE_COMPONENT_CAPTURE_LIMIT = 25
PROFILE_COMPONENT_MARKDOWN_LIMIT = 3
PROFILE_COMPONENT_HTML_LIMIT = 8


@dataclass(frozen=True)
class Variant:
    name: str
    fastpg: bool
    engine: str


@dataclass
class CommandResult:
    command: list[str]
    cwd: str
    returncode: int
    stdout: str
    stderr: str
    seconds: float

    def as_json(self) -> dict[str, Any]:
        return {
            "command": self.command,
            "cwd": self.cwd,
            "returncode": self.returncode,
            "seconds": self.seconds,
        }


class ProcessMemorySampler:
    def __init__(self, pid: int, interval_seconds: float = 0.1):
        self.pid = pid
        self.interval_seconds = interval_seconds
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._samples: list[int] = []

    def __enter__(self) -> "ProcessMemorySampler":
        self._thread.start()
        return self

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> None:
        self.stop()

    def _run(self) -> None:
        while not self._stop.is_set():
            rss_kb = process_rss_kb(self.pid)
            if rss_kb is not None:
                self._samples.append(rss_kb * 1024)
            self._stop.wait(self.interval_seconds)

    def stop(self) -> dict[str, Any]:
        self._stop.set()
        self._thread.join(timeout=1.0)
        if not self._samples:
            return {
                "pid": self.pid,
                "sample_count": 0,
                "interval_seconds": self.interval_seconds,
            }
        return {
            "pid": self.pid,
            "sample_count": len(self._samples),
            "interval_seconds": self.interval_seconds,
            "first_rss_bytes": self._samples[0],
            "last_rss_bytes": self._samples[-1],
            "max_rss_bytes": max(self._samples),
        }


class PostgresBackendMemorySampler:
    def __init__(self, postmaster_pid: int, interval_seconds: float = 0.1):
        self.postmaster_pid = postmaster_pid
        self.interval_seconds = interval_seconds
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._samples: list[int] = []

    def __enter__(self) -> "PostgresBackendMemorySampler":
        self._thread.start()
        return self

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> None:
        self.stop()

    def _run(self) -> None:
        while not self._stop.is_set():
            backend_pid = postgres_backend_child_pid(self.postmaster_pid)
            if backend_pid is not None:
                rss_kb = process_rss_kb(backend_pid)
                if rss_kb is not None:
                    self._samples.append(rss_kb * 1024)
            self._stop.wait(self.interval_seconds)

    def stop(self) -> dict[str, Any]:
        self._stop.set()
        self._thread.join(timeout=1.0)
        if not self._samples:
            return {
                "postmaster_pid": self.postmaster_pid,
                "sample_count": 0,
                "interval_seconds": self.interval_seconds,
            }
        return {
            "postmaster_pid": self.postmaster_pid,
            "sample_count": len(self._samples),
            "interval_seconds": self.interval_seconds,
            "first_rss_bytes": self._samples[0],
            "last_rss_bytes": self._samples[-1],
            "max_rss_bytes": max(self._samples),
        }


class BenchmarkFailure(Exception):
    def __init__(self, variant: str, phase: str, result: CommandResult, output_dir: Path):
        self.variant = variant
        self.phase = phase
        self.result = result
        self.output_dir = output_dir
        super().__init__(f"{variant} failed during {phase}")


class PgBenchCompare:
    def __init__(
        self,
        args: argparse.Namespace,
        *,
        result_subdir: str = "pgbench",
        timestamp: str | None = None,
    ):
        self.args = args
        self.source_root = Path(__file__).resolve().parents[1]
        self.bench_root = self.source_root / "benches"
        self.build_root = self.bench_root / ".build" / "pgbench"
        self.pgbench_client_paths: dict[str, Path] | None = None
        timestamp = timestamp or datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        self.result_root = self.bench_root / "results" / result_subdir / timestamp
        self.result_root.mkdir(parents=True, exist_ok=True)
        self.results: dict[str, Any] = {
            "status": "running",
            "created_at": timestamp,
            "config": {
                "builtin": args.builtin,
                "init_steps": args.init_steps,
                "scale": args.scale,
                "transactions": args.transactions,
                "clients": args.clients,
                "jobs": args.jobs,
                "runs": args.runs,
                "protocol": args.protocol,
                "fastpg_engine": args.fastpg_engine,
                "meson_buildtype": args.meson_buildtype,
                "rust_build_profile": args.rust_build_profile,
                "rust_pgcore": args.rust_pgcore,
                "profile_fastpg_rust_server": args.profile_fastpg_rust_server,
                "profile_normal_postgres": args.profile_normal_postgres,
                "profile_tool": args.profile_tool,
                "profile_phase": args.profile_phase,
                "profile_open": args.profile_open,
                "profile_warmup_seconds": args.profile_warmup_seconds,
                "profile_hyperfine": args.profile_hyperfine,
                "profile_hyperfine_runs": args.profile_hyperfine_runs,
                "profile_hyperfine_warmup": args.profile_hyperfine_warmup,
                "profile_server_memory": args.profile_server_memory,
            },
            "workload": workload_metadata(args.builtin, args.init_steps),
            "warnings": workload_warnings(args.init_steps),
            "variants": {},
        }

    def run(self) -> int:
        print(f"results: {self.result_root}")
        for variant in (
            Variant("normal", False, "postgres"),
            Variant("fastpg", True, self.args.fastpg_engine),
        ):
            self.run_variant(variant)

        self.results["status"] = "ok"
        self.write_results()
        self.print_success()
        return 0

    def run_variant(self, variant: Variant) -> None:
        variant_dir = self.result_root / variant.name
        variant_dir.mkdir(parents=True, exist_ok=True)
        self.results["variants"][variant.name] = {
            "fastpg": variant.fastpg,
            "engine": variant.engine,
            "status": "running",
            "runs": [],
        }

        print(f"[{variant.name}] build/install")
        if variant.engine == "rust-server":
            paths = self.ensure_rust_server_install(variant, variant_dir / "setup")
            self.results["variants"][variant.name]["server_binary"] = str(paths["server_binary"])
            self.results["variants"][variant.name]["pgbench_client_prefix"] = str(paths["client_prefix"])
        else:
            paths = self.ensure_variant_install(variant, variant_dir / "setup")
            self.results["variants"][variant.name]["build_dir"] = str(paths["build_dir"])
            self.results["variants"][variant.name]["install_prefix"] = str(paths["prefix"])
            if variant.name == "normal":
                self.pgbench_client_paths = paths

        for run_index in range(1, self.args.runs + 1):
            print(f"[{variant.name}] pgbench run {run_index}/{self.args.runs}")
            run_result = self.run_pgbench_once(variant, paths, run_index, variant_dir)
            self.results["variants"][variant.name]["runs"].append(run_result)
            self.write_results()

        self.results["variants"][variant.name]["status"] = "ok"
        self.results["variants"][variant.name]["summary"] = summarize_runs(
            self.results["variants"][variant.name]["runs"]
        )
        self.write_results()

    def ensure_variant_install(self, variant: Variant, output_dir: Path) -> dict[str, Path]:
        output_dir.mkdir(parents=True, exist_ok=True)
        build_dir = self.build_root / variant.name
        setup_args = [
            "meson",
            "setup",
            str(build_dir),
            str(self.source_root),
            f"--buildtype={self.args.meson_buildtype}",
            "--auto-features=disabled",
            "-Dtap_tests=disabled",
            f"-Dfastpg={'true' if variant.fastpg else 'false'}",
        ]
        reconfigure_args = [
            "meson",
            "setup",
            "--reconfigure",
            str(build_dir),
            f"--buildtype={self.args.meson_buildtype}",
            "--auto-features=disabled",
            "-Dtap_tests=disabled",
            f"-Dfastpg={'true' if variant.fastpg else 'false'}",
        ]

        if (build_dir / "build.ninja").exists():
            self.checked_command(variant.name, "setup", reconfigure_args, output_dir, "meson-reconfigure")
        else:
            self.checked_command(variant.name, "setup", setup_args, output_dir, "meson-setup")

        self.checked_command(
            variant.name,
            "setup",
            ["meson", "test", "-C", str(build_dir), "--suite", "setup", "--print-errorlogs"],
            output_dir,
            "meson-test-setup",
        )

        prefix = build_dir / "tmp_install" / "usr" / "local" / "pgsql"
        bindir = prefix / "bin"
        libdir = prefix / "lib"
        for program in ("initdb", "pg_ctl", "psql", "pgbench"):
            if not (bindir / program).exists():
                raise BenchmarkFailure(
                    variant.name,
                    "setup",
                    CommandResult([str(bindir / program)], str(self.source_root), 1, "", "missing installed binary", 0.0),
                    output_dir,
                )

        self.repair_macos_libpq_names(variant.name, bindir, libdir, output_dir)
        return {"build_dir": build_dir, "prefix": prefix, "bindir": bindir, "libdir": libdir}

    def ensure_rust_server_install(self, variant: Variant, output_dir: Path) -> dict[str, Path]:
        output_dir.mkdir(parents=True, exist_ok=True)
        if self.pgbench_client_paths is None:
            raise BenchmarkFailure(
                variant.name,
                "setup",
                CommandResult([], str(self.source_root), 1, "", "normal pgbench client paths are missing", 0.0),
                output_dir,
            )

        build_command = ["cargo", "build", "-p", "fastpg-server"]
        build_env = os.environ.copy()
        if self.args.rust_pgcore == "raw-parser":
            build_command.extend(["--features", "postgres-linked"])
            build_env["FASTPG_POSTGRES_BUILD_DIR"] = str(self.pgbench_client_paths["build_dir"])
        elif self.args.rust_pgcore == "full":
            pgcore_build_dir = self.ensure_fastpg_pgcore_build(variant.name, output_dir)
            build_command.extend(["--features", "postgres-execution"])
            build_env["FASTPG_POSTGRES_BUILD_DIR"] = str(pgcore_build_dir)
        if self.args.rust_build_profile == "release":
            build_command.append("--release")

        build = self.checked_command(
            variant.name,
            "setup",
            build_command,
            output_dir,
            "cargo-build-fastpg-server",
            env=build_env,
        )
        server_binary_name = "fastpg-server.exe" if os.name == "nt" else "fastpg-server"
        server_binary = self.source_root / "target" / self.args.rust_build_profile / server_binary_name
        if not server_binary.exists():
            raise BenchmarkFailure(
                variant.name,
                "setup",
                CommandResult(
                    build.command,
                    str(self.source_root),
                    1,
                    build.stdout,
                    f"missing built server binary: {server_binary}",
                    build.seconds,
                ),
                output_dir,
            )

        paths = {
            "server_binary": server_binary,
            "client_prefix": self.pgbench_client_paths["prefix"],
            "client_bindir": self.pgbench_client_paths["bindir"],
            "client_libdir": self.pgbench_client_paths["libdir"],
        }
        if self.args.rust_pgcore == "full":
            paths["pgcore_build_dir"] = Path(build_env["FASTPG_POSTGRES_BUILD_DIR"])
        return paths

    def ensure_fastpg_pgcore_build(self, variant_name: str, output_dir: Path) -> Path:
        build_dir = self.build_root / "fastpg"
        setup_args = [
            "meson",
            "setup",
            str(build_dir),
            str(self.source_root),
            f"--buildtype={self.args.meson_buildtype}",
            "--auto-features=disabled",
            "-Dtap_tests=disabled",
            "-Dfastpg=true",
        ]
        reconfigure_args = [
            "meson",
            "setup",
            "--reconfigure",
            str(build_dir),
            f"--buildtype={self.args.meson_buildtype}",
            "--auto-features=disabled",
            "-Dtap_tests=disabled",
            "-Dfastpg=true",
        ]

        if (build_dir / "build.ninja").exists():
            self.checked_command(variant_name, "setup", reconfigure_args, output_dir, "meson-reconfigure-fastpg")
        else:
            self.checked_command(variant_name, "setup", setup_args, output_dir, "meson-setup-fastpg")

        self.checked_command(
            variant_name,
            "setup",
            ["meson", "compile", "-C", str(build_dir), "backend"],
            output_dir,
            "meson-compile-fastpg-backend",
        )
        return build_dir

    def repair_macos_libpq_names(self, variant: str, bindir: Path, libdir: Path, output_dir: Path) -> None:
        if platform.system() != "Darwin":
            return

        old_name = "/usr/local/pgsql/lib/libpq.5.dylib"
        new_name = str(libdir / "libpq.5.dylib")
        for binary_name in ("psql", "pgbench"):
            binary = bindir / binary_name
            otool = self.command(["otool", "-L", str(binary)], output_dir, f"otool-{binary_name}")
            if otool.returncode != 0:
                raise BenchmarkFailure(variant, "setup", otool, output_dir)
            if old_name in otool.stdout:
                changed = self.command(
                    ["install_name_tool", "-change", old_name, new_name, str(binary)],
                    output_dir,
                    f"install-name-{binary_name}",
                )
                if changed.returncode != 0:
                    raise BenchmarkFailure(variant, "setup", changed, output_dir)

    def run_pgbench_once(
        self, variant: Variant, paths: dict[str, Path], run_index: int, variant_dir: Path
    ) -> dict[str, Any]:
        if variant.engine == "rust-server":
            return self.run_rust_server_pgbench_once(variant, paths, run_index, variant_dir)
        return self.run_postgres_pgbench_once(variant, paths, run_index, variant_dir)

    def run_postgres_pgbench_once(
        self, variant: Variant, paths: dict[str, Path], run_index: int, variant_dir: Path
    ) -> dict[str, Any]:
        run_dir = variant_dir / f"run-{run_index}"
        run_dir.mkdir(parents=True, exist_ok=True)
        data_dir = run_dir / "data"
        socket_dir = Path(f"/private/tmp/fpgb-{os.getpid()}-{variant.name}-{run_index}")
        socket_dir.mkdir(parents=True, exist_ok=True)
        port = free_port()
        env = postgres_env(paths["bindir"], paths["libdir"])
        logfile = run_dir / "postgres.log"
        started = False
        profiler: dict[str, Any] | None = None
        stop_failure: BenchmarkFailure | None = None

        run_record: dict[str, Any] = {
            "run": run_index,
            "data_dir": str(data_dir),
            "socket_dir": str(socket_dir),
            "port": port,
            "commands": {},
        }

        try:
            initdb = self.checked_command(
                variant.name,
                "initdb",
                [
                    str(paths["bindir"] / "initdb"),
                    "-D",
                    str(data_dir),
                    "-U",
                    "postgres",
                    "-A",
                    "trust",
                    "--no-locale",
                ],
                run_dir,
                "initdb",
                env=env,
            )
            run_record["commands"]["initdb"] = initdb.as_json()

            start = self.checked_command(
                variant.name,
                "start",
                [
                    str(paths["bindir"] / "pg_ctl"),
                    "-D",
                    str(data_dir),
                    "-l",
                    str(logfile),
                    "-o",
                    f"-p {port} -k {socket_dir}",
                    "start",
                    "-w",
                ],
                run_dir,
                "pg_ctl-start",
                env=env,
            )
            started = True
            run_record["commands"]["start"] = start.as_json()

            init = self.checked_command(
                variant.name,
                "pgbench-init",
                self.pgbench_init_command(paths["bindir"], str(socket_dir), port),
                run_dir,
                "pgbench-init",
                env=env,
            )
            run_record["commands"]["pgbench_init"] = init.as_json()

            if self.should_profile_normal_postgres(variant) and self.args.profile_phase == "run":
                postmaster_pid = read_postmaster_pid(data_dir)
                bench, profiler, server_memory = self.run_profiled_postgres_pgbench(
                    variant.name,
                    self.pgbench_run_command(paths["bindir"], str(socket_dir), port),
                    postmaster_pid,
                    run_dir,
                    env,
                )
                run_record["commands"]["profile_start"] = profiler["result"].as_json()
                if server_memory is not None:
                    run_record.setdefault("memory", {})["pgbench_run"] = server_memory
            else:
                bench = self.checked_command(
                    variant.name,
                    "pgbench-run",
                    self.pgbench_run_command(paths["bindir"], str(socket_dir), port),
                    run_dir,
                    "pgbench-run",
                    env=env,
                )
            run_record["commands"]["pgbench_run"] = bench.as_json()
            run_record["metrics"] = parse_pgbench_metrics(bench.stdout)
            if profiler is not None:
                profile_result = self.finish_profiler(profiler, run_dir)
                run_record["commands"]["profile_stop"] = profile_result.as_json()
                run_record["profile"] = profiler["profile_record"]
                profiler = None
                if profile_result.returncode != 0:
                    raise BenchmarkFailure(variant.name, "profile", profile_result, run_dir)
            if self.args.profile_hyperfine:
                hyperfine = self.run_hyperfine_pgbench(
                    variant.name,
                    self.pgbench_run_command(paths["bindir"], str(socket_dir), port),
                    run_dir,
                    env,
                    memory_pid=None,
                    memory_postmaster_pid=read_postmaster_pid(data_dir),
                )
                run_record["commands"]["hyperfine"] = hyperfine["result"].as_json()
                run_record["hyperfine"] = hyperfine["record"]
            return run_record
        finally:
            active_failure = sys.exc_info()[0] is not None
            if profiler is not None:
                profile_result = self.finish_profiler(profiler, run_dir)
                run_record["commands"]["profile_stop"] = profile_result.as_json()
                run_record["profile"] = profiler["profile_record"]
                if profile_result.returncode != 0 and stop_failure is None and not active_failure:
                    stop_failure = BenchmarkFailure(variant.name, "profile", profile_result, run_dir)
            if started:
                stopped = self.command(
                    [
                        str(paths["bindir"] / "pg_ctl"),
                        "-D",
                        str(data_dir),
                        "stop",
                        "-m",
                        "fast",
                        "-w",
                    ],
                    run_dir,
                    "pg_ctl-stop",
                    env=env,
                )
                run_record["commands"]["stop"] = stopped.as_json()
                if stopped.returncode != 0 and not active_failure:
                    stop_failure = BenchmarkFailure(variant.name, "stop", stopped, run_dir)
            shutil.rmtree(socket_dir, ignore_errors=True)
            self.write_results()
            if stop_failure is not None:
                raise stop_failure

    def run_rust_server_pgbench_once(
        self, variant: Variant, paths: dict[str, Path], run_index: int, variant_dir: Path
    ) -> dict[str, Any]:
        run_dir = variant_dir / f"run-{run_index}"
        run_dir.mkdir(parents=True, exist_ok=True)
        port = free_port()
        socket_dir: Path | None = None
        socket_path: Path | None = None
        if os.name == "nt":
            host = "127.0.0.1"
            listen_address = f"{host}:{port}"
        else:
            socket_dir = Path(f"/private/tmp/fpgb-{os.getpid()}-{variant.name}-{run_index}")
            socket_dir.mkdir(parents=True, exist_ok=True)
            socket_path = socket_dir / f".s.PGSQL.{port}"
            host = str(socket_dir)
            listen_address = f"unix:{socket_path}"
        env = rust_server_pgbench_env(paths["client_bindir"], paths["client_libdir"])
        server: dict[str, Any] | None = None
        profiler: dict[str, Any] | None = None
        stop_failure: BenchmarkFailure | None = None

        run_record: dict[str, Any] = {
            "run": run_index,
            "host": host,
            "port": port,
            "commands": {},
        }
        if socket_dir is not None:
            run_record["socket_dir"] = str(socket_dir)

        try:
            server = self.start_rust_server(
                variant.name,
                paths["server_binary"],
                listen_address,
                run_dir,
                host=host,
                port=port,
                socket_path=socket_path,
            )
            run_record["commands"]["start"] = server["result"].as_json()
            if "profiler" in server:
                profiler = server["profiler"]
                run_record["commands"]["profile_start"] = profiler["result"].as_json()

            init = self.checked_command(
                variant.name,
                "pgbench-init",
                self.pgbench_init_command(paths["client_bindir"], host, port),
                run_dir,
                "pgbench-init",
                env=env,
            )
            run_record["commands"]["pgbench_init"] = init.as_json()

            if self.should_profile_rust_server(variant) and self.args.profile_phase == "run":
                profiler = self.start_rust_profiler(variant.name, server, run_dir)
                run_record["commands"]["profile_start"] = profiler["result"].as_json()

            memory_sampler = (
                ProcessMemorySampler(server["process"].pid)
                if self.args.profile_server_memory
                else None
            )
            if memory_sampler is not None:
                memory_sampler.__enter__()
            try:
                bench = self.checked_command(
                    variant.name,
                    "pgbench-run",
                    self.pgbench_run_command(paths["client_bindir"], host, port),
                    run_dir,
                    "pgbench-run",
                    env=env,
                )
            finally:
                if memory_sampler is not None:
                    run_record.setdefault("memory", {})["pgbench_run"] = memory_sampler.stop()
            run_record["commands"]["pgbench_run"] = bench.as_json()
            run_record["metrics"] = parse_pgbench_metrics(bench.stdout)
            if profiler is not None:
                profile_result = self.finish_rust_profiler(profiler, run_dir)
                run_record["commands"]["profile_stop"] = profile_result.as_json()
                run_record["profile"] = profiler["profile_record"]
                profiler = None
                if profile_result.returncode != 0:
                    raise BenchmarkFailure(variant.name, "profile", profile_result, run_dir)
            if self.args.profile_hyperfine:
                hyperfine = self.run_hyperfine_pgbench(
                    variant.name,
                    self.pgbench_run_command(paths["client_bindir"], host, port),
                    run_dir,
                    env,
                    memory_pid=server["process"].pid,
                    memory_postmaster_pid=None,
                )
                run_record["commands"]["hyperfine"] = hyperfine["result"].as_json()
                run_record["hyperfine"] = hyperfine["record"]
            return run_record
        finally:
            active_failure = sys.exc_info()[0] is not None
            if server is not None:
                stopped = self.stop_rust_server(server, run_dir)
                run_record["commands"]["stop"] = stopped.as_json()
                if stopped.returncode != 0 and not active_failure:
                    stop_failure = BenchmarkFailure(variant.name, "stop", stopped, run_dir)
            if profiler is not None:
                profile_result = self.finish_rust_profiler(profiler, run_dir)
                run_record["commands"]["profile_stop"] = profile_result.as_json()
                run_record["profile"] = profiler["profile_record"]
                if profile_result.returncode != 0 and stop_failure is None and not active_failure:
                    stop_failure = BenchmarkFailure(variant.name, "profile", profile_result, run_dir)
            self.write_results()
            if socket_dir is not None:
                shutil.rmtree(socket_dir, ignore_errors=True)
            if stop_failure is not None:
                raise stop_failure

    def start_rust_server(
        self,
        variant: str,
        server_binary: Path,
        listen_address: str,
        output_dir: Path,
        *,
        host: str,
        port: int,
        socket_path: Path | None = None,
    ) -> dict[str, Any]:
        output_dir.mkdir(parents=True, exist_ok=True)
        command = [str(server_binary), listen_address]
        stdout_path = output_dir / "fastpg-server.stdout"
        stderr_path = output_dir / "fastpg-server.stderr"
        stdout_file = stdout_path.open("w")
        stderr_file = stderr_path.open("w")
        started = time.monotonic()
        process = subprocess.Popen(
            command,
            cwd=self.source_root,
            env=os.environ.copy(),
            text=True,
            stdout=stdout_file,
            stderr=stderr_file,
        )
        result = CommandResult(command, str(self.source_root), 0, "", "", time.monotonic() - started)
        server = {
            "command": command,
            "process": process,
            "stdout_file": stdout_file,
            "stderr_file": stderr_file,
            "stdout_path": stdout_path,
            "stderr_path": stderr_path,
            "result": result,
        }

        if socket_path is None:
            ready = wait_for_tcp_server(process, host, port)
        else:
            ready = wait_for_unix_server(process, socket_path)
        if not ready:
            stdout_file.flush()
            stderr_file.flush()
            failure = CommandResult(
                command,
                str(self.source_root),
                process.poll() if process.poll() is not None else 1,
                read_text(stdout_path),
                read_text(stderr_path),
                time.monotonic() - started,
            )
            self.stop_rust_server(server, output_dir)
            raise BenchmarkFailure(variant, "start", failure, output_dir)

        result.seconds = time.monotonic() - started
        (output_dir / "fastpg-server-start.command.json").write_text(
            json.dumps(result.as_json(), indent=2) + "\n"
        )

        if self.should_profile_rust_server_name(variant) and self.args.profile_phase == "init-and-run":
            profiler = self.start_rust_profiler(
                variant,
                server,
                output_dir,
            )
            server["profiler"] = profiler

        return server

    def stop_rust_server(self, server: dict[str, Any], output_dir: Path) -> CommandResult:
        started = time.monotonic()
        process = server["process"]
        command = ["terminate", f"pid={process.pid}"]
        terminated_by_harness = False
        if process.poll() is None:
            terminated_by_harness = True
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)

        for key in ("stdout_file", "stderr_file"):
            server[key].close()

        result = CommandResult(
            command,
            str(self.source_root),
            0 if terminated_by_harness else (process.returncode if process.returncode is not None else 1),
            read_text(server["stdout_path"]),
            read_text(server["stderr_path"]),
            time.monotonic() - started,
        )
        (output_dir / "fastpg-server-stop.command.json").write_text(
            json.dumps(result.as_json(), indent=2) + "\n"
        )
        return result

    def should_profile_rust_server(self, variant: Variant) -> bool:
        return self.should_profile_rust_server_name(variant.name) and variant.engine == "rust-server"

    def should_profile_rust_server_name(self, variant_name: str) -> bool:
        return self.args.profile_fastpg_rust_server and variant_name == "fastpg"

    def should_profile_normal_postgres(self, variant: Variant) -> bool:
        return (
            self.args.profile_normal_postgres
            and variant.name == "normal"
            and variant.engine == "postgres"
        )

    def run_profiled_postgres_pgbench(
        self,
        variant: str,
        command: list[str],
        postmaster_pid: int,
        output_dir: Path,
        env: dict[str, str],
    ) -> tuple[CommandResult, dict[str, Any], dict[str, Any] | None]:
        output_dir.mkdir(parents=True, exist_ok=True)
        stdout_path = output_dir / "pgbench-run.stdout"
        stderr_path = output_dir / "pgbench-run.stderr"
        stdout_file = stdout_path.open("w")
        stderr_file = stderr_path.open("w")
        started = time.monotonic()
        pgbench_process = subprocess.Popen(
            command,
            cwd=self.source_root,
            env=env,
            text=True,
            stdout=stdout_file,
            stderr=stderr_file,
        )
        profiler: dict[str, Any] | None = None
        memory_sampler: ProcessMemorySampler | None = None
        server_memory: dict[str, Any] | None = None
        try:
            backend_pid = wait_for_postgres_backend(postmaster_pid, pgbench_process)
            if backend_pid is None:
                failure = CommandResult(
                    command,
                    str(self.source_root),
                    pgbench_process.poll() if pgbench_process.poll() is not None else 1,
                    read_text(stdout_path),
                    read_text(stderr_path),
                    time.monotonic() - started,
                )
                raise BenchmarkFailure(
                    variant,
                    "profile",
                    CommandResult(
                        ["find-postgres-backend", f"postmaster={postmaster_pid}"],
                        str(self.source_root),
                        1,
                        failure.stdout,
                        "could not find active Postgres backend to profile",
                        failure.seconds,
                    ),
                    output_dir,
                )
            profiler = self.start_profiler(
                variant,
                backend_pid,
                output_dir,
                "normal-postgres-flamegraph.svg",
                "normal Postgres pgbench backend",
            )
            memory_sampler = (
                ProcessMemorySampler(backend_pid)
                if self.args.profile_server_memory
                else None
            )
            if memory_sampler is not None:
                memory_sampler.__enter__()
            pgbench_process.wait()
        finally:
            if memory_sampler is not None:
                server_memory = memory_sampler.stop()
            stdout_file.close()
            stderr_file.close()

        result = CommandResult(
            command=command,
            cwd=str(self.source_root),
            returncode=pgbench_process.returncode if pgbench_process.returncode is not None else 1,
            stdout=read_text(stdout_path),
            stderr=read_text(stderr_path),
            seconds=time.monotonic() - started,
        )
        (output_dir / "pgbench-run.command.json").write_text(json.dumps(result.as_json(), indent=2) + "\n")
        if result.returncode != 0:
            raise BenchmarkFailure(variant, "pgbench-run", result, output_dir)
        assert profiler is not None
        return result, profiler, server_memory

    def run_hyperfine_pgbench(
        self,
        variant: str,
        pgbench_command: list[str],
        output_dir: Path,
        env: dict[str, str],
        *,
        memory_pid: int | None,
        memory_postmaster_pid: int | None,
    ) -> dict[str, Any]:
        hyperfine = shutil.which("hyperfine")
        if hyperfine is None:
            raise BenchmarkFailure(
                variant,
                "hyperfine",
                CommandResult(["hyperfine"], str(self.source_root), 1, "", "missing hyperfine", 0.0),
                output_dir,
            )

        json_path = output_dir / "hyperfine.json"
        command = [
            hyperfine,
            "--style",
            "none",
            "--runs",
            str(self.args.profile_hyperfine_runs),
            "--warmup",
            str(self.args.profile_hyperfine_warmup),
            "--export-json",
            str(json_path),
            "--command-name",
            f"{variant} pgbench run",
            shlex.join(pgbench_command),
        ]
        started = time.monotonic()
        memory_sampler: ProcessMemorySampler | PostgresBackendMemorySampler | None = None
        if self.args.profile_server_memory:
            if memory_pid is not None:
                memory_sampler = ProcessMemorySampler(memory_pid)
            elif memory_postmaster_pid is not None:
                memory_sampler = PostgresBackendMemorySampler(memory_postmaster_pid)
        if memory_sampler is not None:
            memory_sampler.__enter__()
        try:
            completed = subprocess.run(
                command,
                cwd=self.source_root,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
        finally:
            server_memory = memory_sampler.stop() if memory_sampler is not None else None

        result = CommandResult(
            command,
            str(self.source_root),
            completed.returncode,
            completed.stdout,
            completed.stderr,
            time.monotonic() - started,
        )
        (output_dir / "hyperfine.command.json").write_text(json.dumps(result.as_json(), indent=2) + "\n")
        (output_dir / "hyperfine.stdout").write_text(completed.stdout)
        (output_dir / "hyperfine.stderr").write_text(completed.stderr)
        if result.returncode != 0:
            raise BenchmarkFailure(variant, "hyperfine", result, output_dir)

        record: dict[str, Any] = {
            "path": str(json_path),
            "summary": parse_hyperfine_summary(json_path),
        }
        if server_memory is not None:
            record["server_memory"] = server_memory
        return {"result": result, "record": record}

    def start_rust_profiler(
        self,
        variant: str,
        server: dict[str, Any],
        output_dir: Path,
    ) -> dict[str, Any]:
        process = server["process"]
        return self.start_profiler(
            variant,
            process.pid,
            output_dir,
            "fastpg-server-flamegraph.svg",
            "fastpg Rust server pgbench",
        )

    def start_profiler(
        self,
        variant: str,
        pid: int,
        output_dir: Path,
        output_name: str,
        title: str,
    ) -> dict[str, Any]:
        profile_dir = output_dir / "profile"
        profile_dir.mkdir(parents=True, exist_ok=True)
        tool = self.args.profile_tool
        if tool == "flamegraph":
            command, profile_path = self.flamegraph_command(pid, profile_dir, output_name, title)
        else:
            raise BenchmarkFailure(
                variant,
                "profile",
                CommandResult([], str(self.source_root), 1, "", f"unsupported profile tool: {tool}", 0.0),
                profile_dir,
            )

        stdout_path = profile_dir / f"{tool}.stdout"
        stderr_path = profile_dir / f"{tool}.stderr"
        stdout_file = stdout_path.open("w")
        stderr_file = stderr_path.open("w")
        started = time.monotonic()
        profiler_process = subprocess.Popen(
            command,
            cwd=profile_dir,
            text=True,
            stdout=stdout_file,
            stderr=stderr_file,
            start_new_session=(os.name != "nt"),
        )
        if self.args.profile_warmup_seconds > 0:
            time.sleep(self.args.profile_warmup_seconds)
        if profiler_process.poll() is not None:
            stdout_file.flush()
            stderr_file.flush()
            failure = CommandResult(
                command,
                str(profile_dir),
                profiler_process.returncode if profiler_process.returncode is not None else 1,
                read_text(stdout_path),
                read_text(stderr_path),
                time.monotonic() - started,
            )
            stdout_file.close()
            stderr_file.close()
            raise BenchmarkFailure(variant, "profile", failure, profile_dir)

        result = CommandResult(command, str(profile_dir), 0, "", "", time.monotonic() - started)
        (profile_dir / f"{tool}-start.command.json").write_text(json.dumps(result.as_json(), indent=2) + "\n")
        return {
            "command": command,
            "process": profiler_process,
            "stdout_file": stdout_file,
            "stderr_file": stderr_file,
            "stdout_path": stdout_path,
            "stderr_path": stderr_path,
            "profile_dir": profile_dir,
            "profile_path": profile_path,
            "result": result,
            "profile_record": {
                "tool": tool,
                "pid": pid,
                "path": str(profile_path),
                "opened": bool(self.args.profile_open),
            },
        }

    def flamegraph_command(
        self, pid: int, profile_dir: Path, output_name: str, title: str
    ) -> tuple[list[str], Path]:
        flamegraph = shutil.which("flamegraph")
        if flamegraph is None:
            raise BenchmarkFailure(
                "fastpg",
                "profile",
                CommandResult(
                    ["flamegraph"],
                    str(self.source_root),
                    1,
                    "",
                    "missing profiler: install cargo-flamegraph or put `flamegraph` on PATH",
                    0.0,
                ),
                profile_dir,
            )

        output = profile_dir / output_name
        command = [
            flamegraph,
            "-p",
            str(pid),
            "-o",
            str(output),
            "--title",
            title,
            "--notes",
            f"builtin={self.args.builtin} scale={self.args.scale} transactions={self.args.transactions}",
        ]
        if self.args.profile_open:
            command.append("--open")
        return command, output

    def finish_rust_profiler(self, profiler: dict[str, Any], output_dir: Path) -> CommandResult:
        return self.finish_profiler(profiler, output_dir, stop_running=True)

    def finish_profiler(
        self,
        profiler: dict[str, Any],
        output_dir: Path,
        *,
        stop_running: bool = False,
    ) -> CommandResult:
        started = time.monotonic()
        process = profiler["process"]
        command = ["wait-profile", f"pid={process.pid}"]
        if stop_running and process.poll() is None:
            interrupt_process_group(process)
        try:
            process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)

        for key in ("stdout_file", "stderr_file"):
            profiler[key].close()

        profile_path = profiler["profile_path"]
        returncode = process.returncode if process.returncode is not None else 1
        if returncode == 0 and not profile_path.exists():
            returncode = 1
        result = CommandResult(
            command,
            str(profiler["profile_dir"]),
            returncode,
            read_text(profiler["stdout_path"]),
            read_text(profiler["stderr_path"]),
            time.monotonic() - started,
        )
        if profile_path.exists():
            profiler["profile_record"]["hotspots"] = profile_hotspots(profile_path)
            profiler["profile_record"]["component_hotspots"] = profile_component_hotspots(profile_path)
            profiler["profile_record"]["total_samples"] = profile_total_samples(profile_path)
        (profiler["profile_dir"] / "profile-stop.command.json").write_text(
            json.dumps(result.as_json(), indent=2) + "\n"
        )
        return result

    def pgbench_init_command(self, bindir: Path, host: str, port: int) -> list[str]:
        command = [
            str(bindir / "pgbench"),
            "-h",
            host,
            "-p",
            str(port),
            "-U",
            "postgres",
            "-i",
            "-s",
            str(self.args.scale),
            "-q",
        ]
        init_steps = normalize_init_steps(self.args.init_steps)
        if init_steps is not None:
            command.append(f"--init-steps={init_steps}")
        command.append("postgres")
        return command

    def pgbench_run_command(self, bindir: Path, host: str, port: int) -> list[str]:
        return [
            str(bindir / "pgbench"),
            "-h",
            host,
            "-p",
            str(port),
            "-U",
            "postgres",
            "-b",
            self.args.builtin,
            "-s",
            str(self.args.scale),
            "-t",
            str(self.args.transactions),
            "-c",
            str(self.args.clients),
            "-j",
            str(self.args.jobs),
            "-M",
            self.args.protocol,
            "-n",
            "-r",
            "postgres",
        ]

    def checked_command(
        self,
        variant: str,
        phase: str,
        command: list[str],
        output_dir: Path,
        label: str,
        env: dict[str, str] | None = None,
    ) -> CommandResult:
        result = self.command(command, output_dir, label, env=env)
        if result.returncode != 0:
            raise BenchmarkFailure(variant, phase, result, output_dir)
        return result

    def command(
        self,
        command: list[str],
        output_dir: Path,
        label: str,
        env: dict[str, str] | None = None,
    ) -> CommandResult:
        output_dir.mkdir(parents=True, exist_ok=True)
        started = time.monotonic()
        completed = subprocess.run(
            command,
            cwd=self.source_root,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        result = CommandResult(
            command=command,
            cwd=str(self.source_root),
            returncode=completed.returncode,
            stdout=completed.stdout,
            stderr=completed.stderr,
            seconds=time.monotonic() - started,
        )
        (output_dir / f"{label}.stdout").write_text(result.stdout)
        (output_dir / f"{label}.stderr").write_text(result.stderr)
        (output_dir / f"{label}.command.json").write_text(json.dumps(result.as_json(), indent=2) + "\n")
        return result

    def write_results(self) -> None:
        result_json = self.result_root / "summary.json"
        result_json.write_text(json.dumps(self.results, indent=2, sort_keys=True) + "\n")
        (self.result_root / "summary.md").write_text(render_markdown(self.results, self.result_root))
        if profile_records_by_variant(self.results):
            (self.result_root / "profile-side-by-side.html").write_text(
                render_profile_comparison_html(self.results, self.result_root)
            )

    def print_success(self) -> None:
        normal = self.results["variants"]["normal"]["summary"]
        fastpg = self.results["variants"]["fastpg"]["summary"]
        print(f"normal median TPS: {normal.get('median_tps')}")
        print(f"fastpg median TPS: {fastpg.get('median_tps')}")
        ratio = speedup_ratio(fastpg.get("median_tps"), normal.get("median_tps"))
        print(f"fastpg/normal TPS ratio: {ratio}")
        print(f"summary: {self.result_root / 'summary.md'}")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--builtin", default="simple-update")
    parser.add_argument("--init-steps", default="dt")
    parser.add_argument("--scale", type=int, default=1)
    parser.add_argument("--transactions", type=int, default=20)
    parser.add_argument("--clients", type=int, default=1)
    parser.add_argument("--jobs", type=int, default=1)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--protocol", choices=["simple", "extended", "prepared"], default="simple")
    parser.add_argument(
        "--meson-buildtype",
        choices=["plain", "debug", "debugoptimized", "release", "minsize"],
        default="release",
        help="Meson buildtype for normal/Postgres-wrapper variants and pgbench client binaries",
    )
    parser.add_argument(
        "--rust-build-profile",
        choices=["debug", "release"],
        default="release",
        help="Cargo build profile for the Rust server when --fastpg-engine=rust-server",
    )
    parser.add_argument(
        "--rust-pgcore",
        choices=["off", "raw-parser", "full"],
        default="full",
        help="Postgres C components to link into the Rust server",
    )
    parser.add_argument(
        "--fastpg-engine",
        choices=["rust-server", "postgres-wrapper"],
        default="rust-server",
        help="run fastpg as the Rust single-process server or as the Postgres tableam wrapper",
    )
    parser.add_argument(
        "--profile-fastpg-rust-server",
        action="store_true",
        help="profile the fastpg Rust server during the pgbench run",
    )
    parser.add_argument(
        "--profile-normal-postgres",
        action="store_true",
        help="profile the normal Postgres backend process during the pgbench run",
    )
    parser.add_argument(
        "--profile-tool",
        choices=["flamegraph"],
        default="flamegraph",
        help="profiler to use with --profile-fastpg-rust-server",
    )
    parser.add_argument(
        "--profile-phase",
        choices=["run", "init-and-run"],
        default="run",
        help="profile only the transaction run or both pgbench init and run",
    )
    parser.add_argument(
        "--profile-open",
        action="store_true",
        help="open the generated profile after recording, when supported by the profiler",
    )
    parser.add_argument(
        "--profile-warmup-seconds",
        type=float,
        default=1.0,
        help="seconds to wait after starting the profiler before running pgbench",
    )
    parser.add_argument(
        "--profile-hyperfine",
        action="store_true",
        help="run a hyperfine timing pass against the already-started server",
    )
    parser.add_argument(
        "--profile-hyperfine-runs",
        type=int,
        default=1,
        help="number of hyperfine timing runs when --profile-hyperfine is set",
    )
    parser.add_argument(
        "--profile-hyperfine-warmup",
        type=int,
        default=0,
        help="number of hyperfine warmup runs when --profile-hyperfine is set",
    )
    parser.add_argument(
        "--profile-server-memory",
        action="store_true",
        help="sample server RSS during profiled pgbench and hyperfine runs",
    )
    args = parser.parse_args(argv)
    if args.runs < 1:
        parser.error("--runs must be at least 1")
    if args.profile_fastpg_rust_server and args.fastpg_engine != "rust-server":
        parser.error("--profile-fastpg-rust-server requires --fastpg-engine=rust-server")
    if args.profile_normal_postgres and args.clients != 1:
        parser.error("--profile-normal-postgres currently requires --clients=1")
    if args.profile_warmup_seconds < 0:
        parser.error("--profile-warmup-seconds must be non-negative")
    if args.profile_hyperfine_runs < 1:
        parser.error("--profile-hyperfine-runs must be at least 1")
    if args.profile_hyperfine_warmup < 0:
        parser.error("--profile-hyperfine-warmup must be non-negative")
    return args


def normalize_init_steps(init_steps: str | None) -> str | None:
    if init_steps is None:
        return None
    cleaned = init_steps.strip()
    if cleaned == "" or cleaned == "default":
        return None
    return cleaned


def postgres_env(bindir: Path, libdir: Path) -> dict[str, str]:
    env = os.environ.copy()
    env["PATH"] = f"{bindir}{os.pathsep}{env.get('PATH', '')}"
    library_path = "DYLD_LIBRARY_PATH" if platform.system() == "Darwin" else "LD_LIBRARY_PATH"
    env[library_path] = f"{libdir}{os.pathsep}{env.get(library_path, '')}"
    return env


def rust_server_pgbench_env(bindir: Path, libdir: Path) -> dict[str, str]:
    env = postgres_env(bindir, libdir)
    env["PGMAXPROTOCOLVERSION"] = "3.0"
    env["PGSSLMODE"] = "disable"
    env["PGGSSENCMODE"] = "disable"
    return env


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def wait_for_tcp_server(
    process: subprocess.Popen[str],
    host: str,
    port: int,
    timeout_seconds: float = 5.0,
) -> bool:
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        if process.poll() is not None:
            return False
        try:
            with socket.create_connection((host, port), timeout=0.2):
                return True
        except OSError:
            time.sleep(0.05)
    return False


def wait_for_unix_server(
    process: subprocess.Popen[str],
    socket_path: Path,
    timeout_seconds: float = 5.0,
) -> bool:
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        if process.poll() is not None:
            return False
        if socket_path.exists():
            return True
        time.sleep(0.05)
    return False


def read_text(path: Path) -> str:
    try:
        return path.read_text()
    except FileNotFoundError:
        return ""


def process_rss_kb(pid: int) -> int | None:
    ps = subprocess.run(
        ["ps", "-o", "rss=", "-p", str(pid)],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if ps.returncode != 0:
        return None
    output = ps.stdout.strip()
    if not output:
        return None
    try:
        return int(output.splitlines()[-1].strip())
    except ValueError:
        return None


def interrupt_process_group(process: subprocess.Popen[str]) -> None:
    try:
        if os.name != "nt":
            os.killpg(process.pid, signal.SIGINT)
        else:
            process.send_signal(signal.SIGINT)
    except ProcessLookupError:
        return


def read_postmaster_pid(data_dir: Path) -> int:
    pid_file = data_dir / "postmaster.pid"
    first_line = pid_file.read_text().splitlines()[0]
    return int(first_line)


def wait_for_postgres_backend(
    postmaster_pid: int,
    pgbench_process: subprocess.Popen[str],
    timeout_seconds: float = 10.0,
) -> int | None:
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        backend_pid = postgres_backend_child_pid(postmaster_pid)
        if backend_pid is not None:
            return backend_pid
        if pgbench_process.poll() is not None:
            return None
        time.sleep(0.02)
    return None


def postgres_backend_child_pid(postmaster_pid: int) -> int | None:
    ps = subprocess.run(
        ["ps", "-axo", "pid=,ppid=,command="],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if ps.returncode != 0:
        return None
    candidates: list[int] = []
    for line in ps.stdout.splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) != 3:
            continue
        try:
            pid = int(parts[0])
            ppid = int(parts[1])
        except ValueError:
            continue
        command = parts[2]
        if ppid == postmaster_pid and is_postgres_client_backend(command):
            candidates.append(pid)
    return max(candidates) if candidates else None


def is_postgres_client_backend(command: str) -> bool:
    lowered = command.lower()
    if "postgres:" not in lowered:
        return False
    if "[local]" not in lowered and "127.0.0.1" not in lowered:
        return False
    auxiliary_processes = (
        "autovacuum launcher",
        "background writer",
        "checkpointer",
        "logger",
        "logical replication launcher",
        "startup",
        "walwriter",
        "writer",
    )
    return not any(name in lowered for name in auxiliary_processes)


def profile_hotspots(profile_path: Path, limit: int = PROFILE_HOTSPOT_LIMIT) -> list[dict[str, Any]]:
    return profile_frame_hotspots(profile_path)[:limit]


def profile_total_samples(profile_path: Path) -> int | None:
    match = SVG_TOTAL_SAMPLES_RE.search(read_text(profile_path))
    if match is None:
        return None
    return int(match.group(1).replace(",", ""))


def profile_frame_hotspots(profile_path: Path) -> list[dict[str, Any]]:
    text = read_text(profile_path)
    by_name: dict[str, dict[str, Any]] = {}
    for raw_title in SVG_TITLE_RE.findall(text):
        title = html.unescape(raw_title)
        match = HOTSPOT_RE.match(title)
        if match is None:
            continue
        name = match.group(1)
        if is_profile_noise(name):
            continue
        hotspot = {
            "name": name,
            "samples": int(match.group(2).replace(",", "")),
            "percent": float(match.group(3)),
        }
        previous = by_name.get(name)
        if previous is None or hotspot["percent"] > previous["percent"]:
            by_name[name] = hotspot
    return sorted(by_name.values(), key=lambda value: value["percent"], reverse=True)


def profile_component_hotspots(
    profile_path: Path,
    limit_per_component: int = PROFILE_COMPONENT_CAPTURE_LIMIT,
) -> dict[str, list[dict[str, Any]]]:
    return profile_hotspots_by_component(profile_frame_hotspots(profile_path), limit_per_component)


def profile_component(name: str) -> str:
    lowered = name.lower()
    if any(
        token in lowered
        for token in (
            "exec_simple_query",
            "portalrun",
            "portalstart",
            "portaldrop",
            "createportal",
            "processquery",
            "endcommand",
            "fastpg_exec::queryexecutor",
            "fastpg_pgcore::preparedstatement",
            "preparedstatement::execute_with_params",
            "fastpg_pgcore::pgcoresession::prepare",
            "fastpg_pgcore_prepare",
            "fastpg_pgcore_execute_params",
            "fastpg_pgcore_execute_utility",
            "pgwire::api::query::simplequeryhandler",
            "pgwire::tokio::server::process_message",
            "fastpg_wire::fastpgwirehandler",
        )
    ):
        return "Query dispatch"
    if any(
        token in lowered
        for token in (
            "socket",
            "pgwire",
            "protocol",
            "readcommand",
            "secure_read",
            "secure_write",
            "pq_get",
            "pq_put",
            "pq_recv",
            "pq_send",
            "internal_flush",
            "printtup",
            "destremote",
            "tokio::net",
            "mio::net",
        )
    ):
        return "Socket / protocol"
    if any(
        token in lowered
        for token in (
            "raw_parser",
            "base_yy",
            "core_yylex",
            "scanner_yy",
            "pg_parse_query",
            "parse_analyze",
            "parse_analyze_",
            "parse_fixed",
            "parser",
            "transform",
            "analyze",
            "settargettable",
            "addrangetableentry",
            "make_op",
        )
    ):
        return "Parsing / analysis"
    if any(
        token in lowered
        for token in (
            "planner",
            "pg_plan_queries",
            "subquery_planner",
            "grouping_planner",
            "query_planner",
            "preprocess_",
            "make_one_rel",
            "set_plan_refs",
            "create_plan",
            "create_index_paths",
            "build_index_paths",
            "create_index_path",
            "finalize_plan",
            "standard_qp_callback",
            "set_rel_pathlist",
            "set_plan_references",
            "add_base_rels",
            "build_simple_rel",
            "get_relation_info",
            "get_index_paths",
            "deconstruct_jointree",
            "distribute_quals_to_rels",
            "set_baserel_size_estimates",
            "build_base_rel_tlists",
            "eval_const_expressions",
            "expression_tree_mutator",
            "firerirrules",
            "get_row_security_policies",
            "check_enable_rls",
            "list_copy_deep",
            "cost_",
            "selectivity",
            "eqsel",
            "pg_rewrite_query",
            "queryrewrite",
            "rewritequery",
            "_copyquery",
            "_copyrangetblentry",
            "_copyalias",
        )
    ):
        return "Planning / rewrite"
    if any(
        token in lowered
        for token in (
            "fastpg_catalog",
            "catalog",
            "catcache",
            "relcache",
            "syscache",
            "pg_class",
            "pg_attribute",
            "pg_index",
            "relation_by_oid",
            "relation_record",
            "relationidgetrelation",
            "relation_open",
            "relation_close",
            "relationclose",
            "relationincrementreferencecount",
            "relationbuild",
            "rangevargetrelid",
            "index_open",
            "get_relname_relid",
            "searchsyscache",
            "builddesc",
            "namespace",
            "table_open",
        )
    ):
        return "Catalog / metadata"
    if any(
        token in lowered
        for token in (
            "fastpg_mem",
            "fastpg_storage",
            "primary_key_index",
            "index_getnext",
            "index_beginscan",
            "index_rescan",
            "index_endscan",
            "heap_",
            "heapam",
            "heapget",
            "heaptuplesatisfies",
            "mvcc",
            "table_",
            "tableam",
            "bt",
            "bufmgr",
            "buffer",
            "smgr",
            "visibilitymap",
            "toast",
            "relation_insert",
            "relation_update",
            "relation_delete",
            "relation_fetch",
            "scan_begin",
            "scan_next",
            "scan_end",
            "tuple_update",
        )
    ):
        return "Storage / index AM"
    if any(
        token in lowered
        for token in (
            "xact",
            "transaction",
            "commit",
            "rollback",
            "abort",
            "xlog",
            "clog",
            "subtrans",
            "lockacquire",
            "lockrelease",
            "lockrelation",
            "procreleaselocks",
            "deadlock",
            "lwlock",
        )
    ):
        return "Transactions / WAL"
    if any(
        token in lowered
        for token in (
            "portalrun",
            "processquery",
            "executor",
            "exec",
            "modifytable",
            "indexnext",
            "seqnext",
            "seqscan",
            "tuplestore",
            "projectset",
            "slot",
            "node",
        )
    ):
        return "Execution"
    if any(
        token in lowered
        for token in (
            "alloc::",
            "core::",
            "std::",
            "tokio::",
            "hashbrown::",
            "malloc",
            "calloc",
            "realloc",
            "free",
            "palloc",
            "pfree",
            "memcpy",
            "memmove",
            "memset",
            "bzero",
            "clock_gettime",
            "clock_gettime_nsec_np",
            "_xzm",
        )
    ):
        return "Runtime / memory"
    return "Other"


def profile_hotspots_by_component(
    hotspots: list[dict[str, Any]],
    limit_per_component: int | None = None,
) -> dict[str, list[dict[str, Any]]]:
    grouped = {component: [] for component in PROFILE_COMPONENT_ORDER}
    for hotspot in hotspots:
        component = profile_component(str(hotspot["name"]))
        if limit_per_component is None or len(grouped[component]) < limit_per_component:
            grouped[component].append(hotspot)
    return grouped


def is_profile_noise(name: str) -> bool:
    if name in {"all", "main", "start", "<deduplicated_symbol>"}:
        return True
    noisy_prefixes = (
        "std::rt::",
        "std::sys::backtrace",
        "std::thread::local::LocalKey<",
        "tokio::runtime::context::",
        "tokio::runtime::runtime::Runtime::block_on",
        "tokio::runtime::scheduler::current_thread::Context::",
        "tokio::runtime::scheduler::current_thread::CoreGuard",
        "tokio::runtime::task::harness::Harness<",
        "fastpg_server::serve_unix_listener_with_handlers::",
    )
    noisy_exact = {
        "BackendRun",
        "BackendMain",
        "fastpg_server::main",
        "postmaster_child_launch",
        "PostgresMain",
        "ServerLoop",
        "PostmasterMain",
    }
    return name in noisy_exact or any(name.startswith(prefix) for prefix in noisy_prefixes)


def parse_pgbench_metrics(stdout: str) -> dict[str, float | None]:
    tps_match = TPS_RE.search(stdout)
    latency_match = LATENCY_RE.search(stdout)
    return {
        "tps_without_initial_connection": float(tps_match.group(1)) if tps_match else None,
        "latency_average_ms": float(latency_match.group(1)) if latency_match else None,
    }


def parse_hyperfine_summary(path: Path) -> dict[str, Any]:
    data = json.loads(path.read_text())
    results = data.get("results", [])
    if not results:
        return {}
    result = results[0]
    memory_values = [int(value) for value in result.get("memory_usage_byte", []) if value is not None]
    return {
        "mean_seconds": result.get("mean"),
        "stddev_seconds": result.get("stddev"),
        "median_seconds": result.get("median"),
        "min_seconds": result.get("min"),
        "max_seconds": result.get("max"),
        "user_seconds": result.get("user"),
        "system_seconds": result.get("system"),
        "runs": len(result.get("times", [])),
        "command_max_rss_bytes": max(memory_values) if memory_values else None,
    }


def summarize_runs(runs: list[dict[str, Any]]) -> dict[str, Any]:
    tps_values = [
        run.get("metrics", {}).get("tps_without_initial_connection")
        for run in runs
        if run.get("metrics", {}).get("tps_without_initial_connection") is not None
    ]
    latency_values = [
        run.get("metrics", {}).get("latency_average_ms")
        for run in runs
        if run.get("metrics", {}).get("latency_average_ms") is not None
    ]
    return {
        "median_tps": median(tps_values) if tps_values else None,
        "median_latency_average_ms": median(latency_values) if latency_values else None,
        "runs": len(runs),
    }


def speedup_ratio(fastpg_tps: float | None, normal_tps: float | None) -> float | None:
    if fastpg_tps is None or normal_tps in (None, 0):
        return None
    return fastpg_tps / normal_tps


def first_run_for_variant(results: dict[str, Any], variant_name: str) -> dict[str, Any] | None:
    runs = results.get("variants", {}).get(variant_name, {}).get("runs", [])
    return runs[0] if runs else None


def run_metric_value(run: dict[str, Any] | None, path: tuple[str, ...]) -> Any:
    value: Any = run
    for key in path:
        if not isinstance(value, dict):
            return None
        value = value.get(key)
    return value


def metric_delta(fastpg: float | int | None, normal: float | int | None) -> float | None:
    if fastpg is None or normal is None:
        return None
    return float(fastpg) - float(normal)


def metric_ratio(fastpg: float | int | None, normal: float | int | None) -> float | None:
    if fastpg is None or normal in (None, 0):
        return None
    return float(fastpg) / float(normal)


def profile_records_by_variant(results: dict[str, Any]) -> dict[str, dict[str, Any]]:
    records: dict[str, dict[str, Any]] = {}
    for variant_name, variant in results.get("variants", {}).items():
        for run in variant.get("runs", []):
            profile = run.get("profile")
            if profile is not None:
                records[variant_name] = profile
                break
    return records


def profile_components_from_record(profile: dict[str, Any] | None) -> dict[str, list[dict[str, Any]]]:
    if profile is None:
        return {component: [] for component in PROFILE_COMPONENT_ORDER}

    path = profile.get("path")
    if path:
        profile_path = Path(str(path))
        if profile_path.exists():
            return profile_component_hotspots(profile_path)

    stored_components = profile.get("component_hotspots")
    if isinstance(stored_components, dict):
        grouped = {component: [] for component in PROFILE_COMPONENT_ORDER}
        for component in PROFILE_COMPONENT_ORDER:
            hotspots = stored_components.get(component, [])
            if isinstance(hotspots, list):
                grouped[component] = hotspots[:PROFILE_COMPONENT_CAPTURE_LIMIT]
        return grouped

    return profile_hotspots_by_component(
        profile.get("hotspots", []),
        PROFILE_COMPONENT_CAPTURE_LIMIT,
    )


def profile_hotspots_from_record(profile: dict[str, Any] | None) -> list[dict[str, Any]]:
    if profile is None:
        return []
    path = profile.get("path")
    if path:
        profile_path = Path(str(path))
        if profile_path.exists():
            return profile_hotspots(profile_path)
    return profile.get("hotspots", [])


def profile_total_samples_from_record(profile: dict[str, Any] | None) -> int | None:
    if profile is None:
        return None
    path = profile.get("path")
    if path:
        profile_path = Path(str(path))
        if profile_path.exists():
            return profile_total_samples(profile_path)
    total_samples = profile.get("total_samples")
    return int(str(total_samples).replace(",", "")) if total_samples is not None else None


def effective_init_steps(init_steps: str | None) -> str:
    if init_steps is None:
        return DEFAULT_PGBENCH_INIT_STEPS
    cleaned = init_steps.strip()
    if cleaned == "" or cleaned == "default":
        return DEFAULT_PGBENCH_INIT_STEPS
    return cleaned


def has_primary_key_init_step(init_steps: str | None) -> bool:
    return "p" in effective_init_steps(init_steps)


def workload_metadata(builtin: str, init_steps: str | None) -> dict[str, Any]:
    indexed = has_primary_key_init_step(init_steps)
    workload_name = "TPC-B-like" if builtin == "tpcb-like" else builtin
    if indexed:
        if builtin == "simple-update":
            label = "pgbench indexed simple-update comparison"
        elif builtin == "tpcb-like":
            label = "pgbench indexed TPC-B-like comparison"
        else:
            label = f"pgbench indexed {workload_name} comparison"
    else:
        label = f"pgbench unindexed {workload_name} smoke"

    return {
        "label": label,
        "effective_init_steps": effective_init_steps(init_steps),
        "has_primary_key_indexes": indexed,
    }


def workload_warnings(init_steps: str | None) -> list[str]:
    if has_primary_key_init_step(init_steps):
        return []
    effective = effective_init_steps(init_steps)
    return [
        f"init_steps={effective} does not create pgbench primary-key indexes (`p` step missing). "
        "Treat this as an unindexed smoke path, not UPDATE performance evidence.",
    ]


def render_markdown(results: dict[str, Any], result_root: Path) -> str:
    workload = results.get("workload", {})
    title = workload.get("label", "pgbench comparison")
    lines = [
        f"# {title}",
        "",
        f"Status: `{results['status']}`",
        "",
    ]
    warnings = results.get("warnings", [])
    if warnings:
        lines.extend(["## Warnings", ""])
        for warning in warnings:
            lines.append(f"- {warning}")
        lines.append("")

    lines.extend(["## Config", ""])
    for key, value in results["config"].items():
        lines.append(f"- `{key}`: `{value}`")
    lines.extend(["", "## Variants", ""])
    variants = results.get("variants", {})
    for name in ("normal", "fastpg"):
        if name not in variants:
            continue
        variant = variants[name]
        lines.append(f"### {name}")
        lines.append("")
        lines.append(f"- status: `{variant.get('status')}`")
        if "summary" in variant:
            summary = variant["summary"]
            lines.append(f"- median TPS: `{summary.get('median_tps')}`")
            lines.append(f"- median average latency ms: `{summary.get('median_latency_average_ms')}`")
        for run in variant.get("runs", []):
            metrics = run.get("metrics", {})
            lines.append(
                f"- run {run['run']}: TPS `{metrics.get('tps_without_initial_connection')}`, "
                f"average latency ms `{metrics.get('latency_average_ms')}`"
            )
            if "profile" in run:
                lines.append(f"- run {run['run']} profile: `{run['profile'].get('path')}`")
        lines.append("")

    if all(name in variants and "summary" in variants[name] for name in ("normal", "fastpg")):
        ratio = speedup_ratio(
            variants["fastpg"]["summary"].get("median_tps"),
            variants["normal"]["summary"].get("median_tps"),
        )
        lines.extend(["## Comparison", "", f"- fastpg/normal median TPS ratio: `{ratio}`", ""])

    metric_rows = [
        row
        for row in profile_run_metric_rows(results)
        if row["normal"] is not None or row["fastpg"] is not None
    ]
    if metric_rows:
        lines.extend(["## Run Metrics", ""])
        lines.append(
            "Wall-time rows come from pgbench and, when enabled, hyperfine. Memory rows are "
            "RSS measurements for the measured command/server process, not stack-level "
            "allocation traces."
        )
        lines.append("")
        lines.append("| metric | normal Postgres | fastpg Rust server | fastpg - normal | fastpg / normal |")
        lines.append("| --- | --- | --- | --- | --- |")
        for row in metric_rows:
            unit = str(row["unit"])
            lines.append(
                f"| {row['label']} | {format_metric_value(row['normal'], unit)} | "
                f"{format_metric_value(row['fastpg'], unit)} | "
                f"{format_metric_delta(row['delta'], unit)} | {format_metric_ratio(row['ratio'])} |"
            )
        lines.append("")

    profiles = profile_records_by_variant(results)
    if profiles:
        lines.extend(["## Profiles", ""])
        lines.append(f"- side-by-side HTML: `{result_root / 'profile-side-by-side.html'}`")
        for variant_name in ("normal", "fastpg"):
            profile = profiles.get(variant_name)
            if profile is None:
                continue
            lines.append(f"- {variant_name} flamegraph: `{profile.get('path')}`")
            total_samples = profile_total_samples_from_record(profile)
            if total_samples is not None:
                lines.append(f"- {variant_name} profile samples: `{total_samples}`")
        lines.append("")
        if all(name in profiles for name in ("normal", "fastpg")):
            normal_hotspots = profile_hotspots_from_record(profiles["normal"])
            fastpg_hotspots = profile_hotspots_from_record(profiles["fastpg"])
            normal_components = profile_components_from_record(profiles["normal"])
            fastpg_components = profile_components_from_record(profiles["fastpg"])

            lines.extend(["### Component Hotspots", ""])
            lines.append(
                "Inclusive flamegraph percentages are not additive. The component table scans "
                "all captured flamegraph frames and each cell shows the top frames in that "
                "component. Percentages use each side's own total sample count. Sample deltas "
                "compare the component peak frame only because displayed frames are inclusive "
                "and can contain the same samples."
            )
            lines.append("")
            lines.append("| component | normal Postgres | fastpg Rust server | fastpg +/- peak samples |")
            lines.append("| --- | --- | --- | --- |")
            for component in PROFILE_COMPONENT_ORDER:
                normal_component_hotspots = normal_components[component]
                fastpg_component_hotspots = fastpg_components[component]
                normal_cell = format_component_hotspot_cell(normal_component_hotspots)
                fastpg_cell = format_component_hotspot_cell(fastpg_component_hotspots)
                if component == "Other" and not normal_cell and not fastpg_cell:
                    continue
                delta_cell = format_sample_delta_cell(
                    component_sample_delta(normal_component_hotspots, fastpg_component_hotspots)
                )
                lines.append(f"| {component} | {normal_cell} | {fastpg_cell} | {delta_cell} |")
            lines.append("")

            lines.extend(["### Hotspot Comparison", ""])
            lines.append("| rank | normal Postgres | fastpg Rust server |")
            lines.append("| --- | --- | --- |")
            for index in range(min(12, max(len(normal_hotspots), len(fastpg_hotspots)))):
                normal = format_hotspot_cell(normal_hotspots, index)
                fastpg = format_hotspot_cell(fastpg_hotspots, index)
                lines.append(f"| {index + 1} | {normal} | {fastpg} |")
            lines.append("")

    if results.get("failure"):
        failure = results["failure"]
        lines.extend(
            [
                "## Failure",
                "",
                f"- variant: `{failure['variant']}`",
                f"- phase: `{failure['phase']}`",
                f"- classification: `{failure.get('classification')}`",
                f"- exit code: `{failure['exit_code']}`",
                f"- command: `{failure['command']}`",
                "",
                "### stdout tail",
                "",
                "```text",
                failure["stdout_tail"],
                "```",
                "",
                "### stderr tail",
                "",
                "```text",
                failure["stderr_tail"],
                "```",
                "",
            ]
        )

    lines.append(f"Raw results: `{result_root / 'summary.json'}`")
    lines.append("")
    return "\n".join(lines)


def format_hotspot_cell(hotspots: list[dict[str, Any]], index: int) -> str:
    if index >= len(hotspots):
        return ""
    hotspot = hotspots[index]
    return f"`{hotspot['name']}` {hotspot['percent']:.2f}%"


def format_component_hotspot_cell(
    hotspots: list[dict[str, Any]],
    limit: int = PROFILE_COMPONENT_MARKDOWN_LIMIT,
) -> str:
    if not hotspots:
        return ""
    return "<br>".join(format_hotspot_cell(hotspots, index) for index in range(min(limit, len(hotspots))))


def component_sample_delta(
    normal_hotspots: list[dict[str, Any]],
    fastpg_hotspots: list[dict[str, Any]],
) -> int:
    normal_samples = int(normal_hotspots[0]["samples"]) if normal_hotspots else 0
    fastpg_samples = int(fastpg_hotspots[0]["samples"]) if fastpg_hotspots else 0
    return fastpg_samples - normal_samples


def format_sample_delta_cell(delta: int) -> str:
    sign = "+" if delta > 0 else ""
    return f"`{sign}{delta:,}` samples"


def profile_run_metric_rows(results: dict[str, Any]) -> list[dict[str, Any]]:
    normal_run = first_run_for_variant(results, "normal")
    fastpg_run = first_run_for_variant(results, "fastpg")
    profiles = profile_records_by_variant(results)

    def add_row(label: str, unit: str, normal: Any, fastpg: Any) -> dict[str, Any]:
        return {
            "label": label,
            "unit": unit,
            "normal": normal,
            "fastpg": fastpg,
            "delta": metric_delta(fastpg, normal),
            "ratio": metric_ratio(fastpg, normal),
        }

    return [
        add_row(
            "pgbench average latency",
            "milliseconds",
            run_metric_value(normal_run, ("metrics", "latency_average_ms")),
            run_metric_value(fastpg_run, ("metrics", "latency_average_ms")),
        ),
        add_row(
            "pgbench TPS",
            "float",
            run_metric_value(normal_run, ("metrics", "tps_without_initial_connection")),
            run_metric_value(fastpg_run, ("metrics", "tps_without_initial_connection")),
        ),
        add_row(
            "pgbench command wall time",
            "seconds",
            run_metric_value(normal_run, ("commands", "pgbench_run", "seconds")),
            run_metric_value(fastpg_run, ("commands", "pgbench_run", "seconds")),
        ),
        add_row(
            "hyperfine mean wall time",
            "seconds",
            run_metric_value(normal_run, ("hyperfine", "summary", "mean_seconds")),
            run_metric_value(fastpg_run, ("hyperfine", "summary", "mean_seconds")),
        ),
        add_row(
            "hyperfine median wall time",
            "seconds",
            run_metric_value(normal_run, ("hyperfine", "summary", "median_seconds")),
            run_metric_value(fastpg_run, ("hyperfine", "summary", "median_seconds")),
        ),
        add_row(
            "hyperfine stddev",
            "seconds",
            run_metric_value(normal_run, ("hyperfine", "summary", "stddev_seconds")),
            run_metric_value(fastpg_run, ("hyperfine", "summary", "stddev_seconds")),
        ),
        add_row(
            "hyperfine command max RSS",
            "bytes",
            run_metric_value(normal_run, ("hyperfine", "summary", "command_max_rss_bytes")),
            run_metric_value(fastpg_run, ("hyperfine", "summary", "command_max_rss_bytes")),
        ),
        add_row(
            "server max RSS during pgbench",
            "bytes",
            run_metric_value(normal_run, ("memory", "pgbench_run", "max_rss_bytes")),
            run_metric_value(fastpg_run, ("memory", "pgbench_run", "max_rss_bytes")),
        ),
        add_row(
            "server max RSS during hyperfine",
            "bytes",
            run_metric_value(normal_run, ("hyperfine", "server_memory", "max_rss_bytes")),
            run_metric_value(fastpg_run, ("hyperfine", "server_memory", "max_rss_bytes")),
        ),
        add_row(
            "profile samples",
            "samples",
            profile_total_samples_from_record(profiles.get("normal")),
            profile_total_samples_from_record(profiles.get("fastpg")),
        ),
    ]


def format_metric_value(value: Any, unit: str) -> str:
    if value is None:
        return ""
    if unit == "bytes":
        return format_bytes(float(value))
    if unit == "seconds":
        return f"{float(value):.3f} s"
    if unit == "milliseconds":
        return f"{float(value):.3f} ms"
    if unit == "samples":
        return f"{int(value):,}"
    return f"{float(value):.3f}"


def format_metric_delta(value: Any, unit: str) -> str:
    if value is None:
        return ""
    sign = "+" if float(value) > 0 else ""
    if unit == "bytes":
        return f"{sign}{format_bytes(float(value))}"
    if unit == "seconds":
        return f"{sign}{float(value):.3f} s"
    if unit == "milliseconds":
        return f"{sign}{float(value):.3f} ms"
    if unit == "samples":
        return f"{sign}{int(value):,}"
    return f"{sign}{float(value):.3f}"


def format_metric_ratio(value: Any) -> str:
    if value is None:
        return ""
    return f"{float(value):.3f}x"


def format_bytes(value: float) -> str:
    sign = "-" if value < 0 else ""
    value = abs(value)
    units = ("B", "KiB", "MiB", "GiB")
    unit = units[0]
    for unit in units:
        if value < 1024 or unit == units[-1]:
            break
        value /= 1024
    if unit == "B":
        return f"{sign}{value:.0f} {unit}"
    return f"{sign}{value:.2f} {unit}"


def render_profile_comparison_html(results: dict[str, Any], result_root: Path) -> str:
    profiles = profile_records_by_variant(results)
    normal = profiles.get("normal")
    fastpg = profiles.get("fastpg")
    normal_path = relative_profile_path(normal, result_root)
    fastpg_path = relative_profile_path(fastpg, result_root)
    return f"""<!doctype html>
<html lang=\"en\">
<head>
  <meta charset=\"utf-8\">
  <title>pgbench profile comparison</title>
  <style>
    body {{ font-family: -apple-system, BlinkMacSystemFont, sans-serif; margin: 24px; }}
    .profiles {{ display: grid; grid-template-columns: 1fr 1fr; gap: 16px; align-items: start; }}
    .profile {{ min-width: 0; }}
    object {{ width: 100%; height: 760px; border: 1px solid #ddd; }}
    table {{ border-collapse: collapse; width: 100%; margin-top: 24px; }}
    th, td {{ border: 1px solid #ddd; padding: 6px 8px; vertical-align: top; }}
    th {{ text-align: left; background: #f6f6f6; }}
    code {{ white-space: normal; overflow-wrap: anywhere; }}
    .note {{ color: #555; line-height: 1.4; max-width: 960px; }}
    .component-name {{ width: 180px; font-weight: 600; }}
    .component-peak {{ color: #555; font-size: 12px; margin-bottom: 4px; }}
    .sample-delta {{ font-variant-numeric: tabular-nums; font-weight: 700; white-space: nowrap; }}
    .sample-delta-positive {{ color: #b42318; }}
    .sample-delta-negative {{ color: #027a48; }}
    .sample-delta-zero {{ color: #777; }}
    .frame-list {{ margin: 0 0 0 18px; padding: 0; }}
    .frame-list li {{ margin: 4px 0; }}
    .frame-meta {{ color: #555; white-space: nowrap; }}
    .muted {{ color: #777; }}
  </style>
</head>
<body>
  <h1>pgbench profile comparison</h1>
  {render_run_metrics_html(results)}
  <div class=\"profiles\">
    <section class=\"profile\">
      <h2>normal Postgres</h2>
      {profile_object(normal_path)}
      {profile_sample_summary(normal)}
    </section>
    <section class=\"profile\">
      <h2>fastpg Rust server</h2>
      {profile_object(fastpg_path)}
      {profile_sample_summary(fastpg)}
    </section>
  </div>
  {render_component_table_html(normal, fastpg)}
  {render_hotspot_table_html(normal, fastpg)}
</body>
</html>
"""


def relative_profile_path(profile: dict[str, Any] | None, result_root: Path) -> str | None:
    if profile is None or profile.get("path") is None:
        return None
    return os.path.relpath(profile["path"], result_root)


def profile_object(path: str | None) -> str:
    if path is None:
        return "<p>No profile captured.</p>"
    escaped = html.escape(path)
    return f'<object data="{escaped}" type="image/svg+xml"><a href="{escaped}">{escaped}</a></object>'


def profile_sample_summary(profile: dict[str, Any] | None) -> str:
    total_samples = profile_total_samples_from_record(profile)
    if total_samples is None:
        return ""
    return f'<p class="note">total profile samples: {total_samples:,}</p>'


def render_run_metrics_html(results: dict[str, Any]) -> str:
    rows = []
    for row in profile_run_metric_rows(results):
        unit = str(row["unit"])
        normal = format_metric_value(row["normal"], unit)
        fastpg = format_metric_value(row["fastpg"], unit)
        if not normal and not fastpg:
            continue
        rows.append(
            "<tr>"
            f"<th scope=\"row\">{html.escape(str(row['label']))}</th>"
            f"<td>{html_metric_value(normal)}</td>"
            f"<td>{html_metric_value(fastpg)}</td>"
            f"<td>{html_metric_value(format_metric_delta(row['delta'], unit))}</td>"
            f"<td>{html_metric_value(format_metric_ratio(row['ratio']))}</td>"
            "</tr>"
        )
    if not rows:
        return ""
    return (
        "<h2>Run metrics</h2>"
        "<p class=\"note\">Wall-time rows come from pgbench and, when enabled, hyperfine. "
        "Memory rows are RSS measurements for the measured command/server process; they are "
        "allocation-pressure indicators, not stack-level allocation traces.</p>"
        "<table><thead><tr><th>metric</th><th>normal Postgres</th>"
        "<th>fastpg Rust server</th><th>fastpg - normal</th><th>fastpg / normal</th>"
        "</tr></thead><tbody>"
        + "\n".join(rows)
        + "</tbody></table>"
    )


def html_metric_value(value: str) -> str:
    if not value:
        return "<span class=\"muted\">not captured</span>"
    return html.escape(value)


def render_component_table_html(
    normal: dict[str, Any] | None,
    fastpg: dict[str, Any] | None,
) -> str:
    normal_components = profile_components_from_record(normal)
    fastpg_components = profile_components_from_record(fastpg)
    rows = []
    for component in PROFILE_COMPONENT_ORDER:
        normal_component_hotspots = normal_components[component]
        fastpg_component_hotspots = fastpg_components[component]
        if component == "Other" and not normal_component_hotspots and not fastpg_component_hotspots:
            continue
        normal_cell = html_component_hotspots(normal_component_hotspots)
        fastpg_cell = html_component_hotspots(fastpg_component_hotspots)
        sample_delta = html_sample_delta_cell(
            component_sample_delta(normal_component_hotspots, fastpg_component_hotspots)
        )
        rows.append(
            "<tr>"
            f"<th class=\"component-name\" scope=\"row\">{html.escape(component)}</th>"
            f"<td>{normal_cell}</td>"
            f"<td>{fastpg_cell}</td>"
            f"<td>{sample_delta}</td>"
            "</tr>"
        )
    if not rows:
        return ""
    return (
        "<h2>Component view</h2>"
        "<p class=\"note\">Inclusive flamegraph percentages are not additive. This table scans "
        "all captured flamegraph frames and each cell shows the top frames in that component, "
        "with the largest frame listed as the component peak. Percentages use each side's own "
        "total sample count. Sample delta compares the component peak frame only because "
        "displayed frames are inclusive and can contain the same samples.</p>"
        "<table class=\"component-table\"><thead><tr><th>component</th>"
        "<th>normal Postgres</th><th>fastpg Rust server</th><th>fastpg +/- peak samples</th>"
        "</tr></thead><tbody>"
        + "\n".join(rows)
        + "</tbody></table>"
    )


def html_sample_delta_cell(delta: int) -> str:
    if delta > 0:
        class_name = "sample-delta sample-delta-positive"
        sign = "+"
    elif delta < 0:
        class_name = "sample-delta sample-delta-negative"
        sign = ""
    else:
        class_name = "sample-delta sample-delta-zero"
        sign = ""
    return f"<span class=\"{class_name}\">{sign}{delta:,} samples</span>"


def html_component_hotspots(
    hotspots: list[dict[str, Any]],
    limit: int = PROFILE_COMPONENT_HTML_LIMIT,
) -> str:
    if not hotspots:
        return "<span class=\"muted\">No frame captured in this component.</span>"
    peak = hotspots[0]
    items = []
    for hotspot in hotspots[:limit]:
        name = html.escape(str(hotspot["name"]))
        items.append(
            "<li>"
            f"<code>{name}</code> "
            f"<span class=\"frame-meta\">{hotspot['percent']:.2f}% "
            f"({int(hotspot['samples']):,} samples)</span>"
            "</li>"
        )
    return (
        f"<div class=\"component-peak\">component peak: {peak['percent']:.2f}% "
        f"({int(peak['samples']):,} samples)</div>"
        "<ol class=\"frame-list\">"
        + "\n".join(items)
        + "</ol>"
    )


def render_hotspot_table_html(
    normal: dict[str, Any] | None,
    fastpg: dict[str, Any] | None,
) -> str:
    normal_hotspots = profile_hotspots_from_record(normal)
    fastpg_hotspots = profile_hotspots_from_record(fastpg)
    rows = []
    for index in range(min(25, max(len(normal_hotspots), len(fastpg_hotspots)))):
        rows.append(
            "<tr>"
            f"<td>{index + 1}</td>"
            f"<td>{html_hotspot_cell(normal_hotspots, index)}</td>"
            f"<td>{html_hotspot_cell(fastpg_hotspots, index)}</td>"
            "</tr>"
        )
    return (
        "<h2>Flat hot frames</h2>"
        "<table><thead><tr><th>rank</th><th>normal Postgres</th>"
        "<th>fastpg Rust server</th></tr></thead><tbody>"
        + "\n".join(rows)
        + "</tbody></table>"
    )


def html_hotspot_cell(hotspots: list[dict[str, Any]], index: int) -> str:
    if index >= len(hotspots):
        return ""
    hotspot = hotspots[index]
    name = html.escape(str(hotspot["name"]))
    return f"<code>{name}</code><br>{hotspot['percent']:.2f}% ({hotspot['samples']} samples)"


def tail(text: str, limit: int = 4000) -> str:
    if len(text) <= limit:
        return text
    return text[-limit:]


def classify_failure(result: CommandResult) -> str | None:
    text = result.stdout + "\n" + result.stderr
    marker = "FASTPG_INTERNAL_IPC_FORBIDDEN"
    if marker not in text:
        return None
    match = re.search(
        r"FASTPG_INTERNAL_IPC_FORBIDDEN: fastpg internal IPC path reached: ([^\n\r]+)",
        text,
    )
    if match is None:
        return "fastpg-internal-ipc"
    return f"fastpg-internal-ipc:{match.group(1).strip()}"


def main(argv: list[str]) -> int:
    runner = PgBenchCompare(parse_args(argv))
    try:
        return runner.run()
    except BenchmarkFailure as failure:
        runner.results["status"] = "failed"
        runner.results["failure"] = {
            "variant": failure.variant,
            "phase": failure.phase,
            "classification": classify_failure(failure.result),
            "exit_code": failure.result.returncode,
            "command": " ".join(failure.result.command),
            "stdout_tail": tail(failure.result.stdout),
            "stderr_tail": tail(failure.result.stderr),
            "output_dir": str(failure.output_dir),
        }
        if failure.variant in runner.results["variants"]:
            runner.results["variants"][failure.variant]["status"] = "failed"
        runner.write_results()
        if failure.variant == "normal":
            print("normal Postgres failed; the benchmark harness is broken.", file=sys.stderr)
        else:
            print("fastpg failed; this is a benchmark-driven implementation target.", file=sys.stderr)
        print(f"phase: {failure.phase}", file=sys.stderr)
        classification = classify_failure(failure.result)
        if classification is not None:
            print(f"classification: {classification}", file=sys.stderr)
        print(f"command: {' '.join(failure.result.command)}", file=sys.stderr)
        print(f"exit code: {failure.result.returncode}", file=sys.stderr)
        print(f"stdout tail:\n{tail(failure.result.stdout)}", file=sys.stderr)
        print(f"stderr tail:\n{tail(failure.result.stderr)}", file=sys.stderr)
        print(f"results: {runner.result_root}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
