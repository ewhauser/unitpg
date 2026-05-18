#!/usr/bin/env python3
"""Build normal/fastpg Postgres variants and compare them with pgbench."""

from __future__ import annotations

import argparse
import html
import json
import os
import platform
import re
import shutil
import socket
import subprocess
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from statistics import median
from typing import Any


TPS_RE = re.compile(r"^tps =\s+([0-9.]+)\s+\(without initial connection time\)", re.MULTILINE)
LATENCY_RE = re.compile(r"^latency average =\s+([0-9.]+)\s+ms", re.MULTILINE)
SVG_TITLE_RE = re.compile(r"<title>(.*?)</title>")
HOTSPOT_RE = re.compile(r"^(.*) \((\d+) samples?, ([0-9.]+)%\)$")


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


class BenchmarkFailure(Exception):
    def __init__(self, variant: str, phase: str, result: CommandResult, output_dir: Path):
        self.variant = variant
        self.phase = phase
        self.result = result
        self.output_dir = output_dir
        super().__init__(f"{variant} failed during {phase}")


class PgBenchCompare:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.source_root = Path(__file__).resolve().parents[1]
        self.bench_root = self.source_root / "benches"
        self.build_root = self.bench_root / ".build" / "pgbench"
        self.pgbench_client_paths: dict[str, Path] | None = None
        timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        self.result_root = self.bench_root / "results" / "pgbench" / timestamp
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
            },
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
                bench, profiler = self.run_profiled_postgres_pgbench(
                    variant.name,
                    self.pgbench_run_command(paths["bindir"], str(socket_dir), port),
                    postmaster_pid,
                    run_dir,
                    env,
                )
                run_record["commands"]["profile_start"] = profiler["result"].as_json()
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
        host = "127.0.0.1"
        port = free_port()
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

        try:
            server = self.start_rust_server(variant.name, paths["server_binary"], host, port, run_dir)
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

            bench = self.checked_command(
                variant.name,
                "pgbench-run",
                self.pgbench_run_command(paths["client_bindir"], host, port),
                run_dir,
                "pgbench-run",
                env=env,
            )
            run_record["commands"]["pgbench_run"] = bench.as_json()
            run_record["metrics"] = parse_pgbench_metrics(bench.stdout)
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
            if stop_failure is not None:
                raise stop_failure

    def start_rust_server(
        self, variant: str, server_binary: Path, host: str, port: int, output_dir: Path
    ) -> dict[str, Any]:
        output_dir.mkdir(parents=True, exist_ok=True)
        command = [str(server_binary), f"{host}:{port}"]
        stdout_path = output_dir / "fastpg-server.stdout"
        stderr_path = output_dir / "fastpg-server.stderr"
        stdout_file = stdout_path.open("w")
        stderr_file = stderr_path.open("w")
        started = time.monotonic()
        process = subprocess.Popen(
            command,
            cwd=self.source_root,
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

        if not wait_for_tcp_server(process, host, port):
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
    ) -> tuple[CommandResult, dict[str, Any]]:
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
            pgbench_process.wait()
        finally:
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
        return result, profiler

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
        return self.finish_profiler(profiler, output_dir)

    def finish_profiler(self, profiler: dict[str, Any], output_dir: Path) -> CommandResult:
        started = time.monotonic()
        process = profiler["process"]
        command = ["wait-profile", f"pid={process.pid}"]
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
        default="raw-parser",
        help="Postgres C components to link into the Rust server",
    )
    parser.add_argument(
        "--fastpg-engine",
        choices=["rust-server", "postgres-wrapper"],
        default="postgres-wrapper",
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
    args = parser.parse_args(argv)
    if args.runs < 1:
        parser.error("--runs must be at least 1")
    if args.profile_fastpg_rust_server and args.fastpg_engine != "rust-server":
        parser.error("--profile-fastpg-rust-server requires --fastpg-engine=rust-server")
    if args.profile_normal_postgres and args.clients != 1:
        parser.error("--profile-normal-postgres currently requires --clients=1")
    if args.profile_warmup_seconds < 0:
        parser.error("--profile-warmup-seconds must be non-negative")
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


def read_text(path: Path) -> str:
    try:
        return path.read_text()
    except FileNotFoundError:
        return ""


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


def profile_hotspots(profile_path: Path, limit: int = 40) -> list[dict[str, Any]]:
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
            "samples": int(match.group(2)),
            "percent": float(match.group(3)),
        }
        previous = by_name.get(name)
        if previous is None or hotspot["percent"] > previous["percent"]:
            by_name[name] = hotspot
    return sorted(by_name.values(), key=lambda value: value["percent"], reverse=True)[:limit]


def is_profile_noise(name: str) -> bool:
    if name in {"all", "main", "start", "<deduplicated_symbol>"}:
        return True
    noisy_prefixes = (
        "std::rt::",
        "std::sys::backtrace",
        "tokio::runtime::context::",
        "tokio::runtime::scheduler::current_thread::CoreGuard",
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


def profile_records_by_variant(results: dict[str, Any]) -> dict[str, dict[str, Any]]:
    records: dict[str, dict[str, Any]] = {}
    for variant_name, variant in results.get("variants", {}).items():
        for run in variant.get("runs", []):
            profile = run.get("profile")
            if profile is not None:
                records[variant_name] = profile
                break
    return records


def render_markdown(results: dict[str, Any], result_root: Path) -> str:
    lines = [
        "# pgbench comparison",
        "",
        f"Status: `{results['status']}`",
        "",
        "## Config",
        "",
    ]
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

    profiles = profile_records_by_variant(results)
    if profiles:
        lines.extend(["## Profiles", ""])
        lines.append(f"- side-by-side HTML: `{result_root / 'profile-side-by-side.html'}`")
        for variant_name in ("normal", "fastpg"):
            profile = profiles.get(variant_name)
            if profile is None:
                continue
            lines.append(f"- {variant_name} flamegraph: `{profile.get('path')}`")
        lines.append("")
        if all(name in profiles for name in ("normal", "fastpg")):
            lines.extend(["### Hotspot Comparison", ""])
            lines.append("| rank | normal Postgres | fastpg Rust server |")
            lines.append("| --- | --- | --- |")
            normal_hotspots = profiles["normal"].get("hotspots", [])
            fastpg_hotspots = profiles["fastpg"].get("hotspots", [])
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
    code {{ white-space: normal; }}
  </style>
</head>
<body>
  <h1>pgbench profile comparison</h1>
  <div class=\"profiles\">
    <section class=\"profile\">
      <h2>normal Postgres</h2>
      {profile_object(normal_path)}
    </section>
    <section class=\"profile\">
      <h2>fastpg Rust server</h2>
      {profile_object(fastpg_path)}
    </section>
  </div>
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


def render_hotspot_table_html(
    normal: dict[str, Any] | None,
    fastpg: dict[str, Any] | None,
) -> str:
    normal_hotspots = normal.get("hotspots", []) if normal else []
    fastpg_hotspots = fastpg.get("hotspots", []) if fastpg else []
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
        "<h2>Hot frames</h2>"
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


def main(argv: list[str]) -> int:
    runner = PgBenchCompare(parse_args(argv))
    try:
        return runner.run()
    except BenchmarkFailure as failure:
        runner.results["status"] = "failed"
        runner.results["failure"] = {
            "variant": failure.variant,
            "phase": failure.phase,
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
        print(f"command: {' '.join(failure.result.command)}", file=sys.stderr)
        print(f"exit code: {failure.result.returncode}", file=sys.stderr)
        print(f"stdout tail:\n{tail(failure.result.stdout)}", file=sys.stderr)
        print(f"stderr tail:\n{tail(failure.result.stderr)}", file=sys.stderr)
        print(f"results: {runner.result_root}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
