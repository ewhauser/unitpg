#!/usr/bin/env python3
"""Inventory upstream PostgreSQL regression SQL against the Rust server.

This intentionally does not use pg_regress.  pg_regress bootstraps a temporary
Postgres cluster through initdb, while the Rust server is already an in-memory
server.  Instead, this runner reuses PostgreSQL's schedule and SQL files, runs
them with pg_regress-like psql flags, and compares the Rust server transcript
against a normal Postgres baseline.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from pgbench_compare import (
    BenchmarkFailure,
    CommandResult,
    PgBenchCompare,
    Variant,
    free_port,
    postgres_env,
    read_text,
    rust_server_pgbench_env,
)

TIMEOUT_RETURN_CODE = 124


@dataclass(frozen=True)
class UpstreamCase:
    name: str
    path: Path
    group: int


UPSTREAM_CASE_DEPENDENCIES: dict[str, tuple[str, ...]] = {
    "multirangetypes": ("rangetypes",),
    "geometry": ("point", "lseg", "line", "box", "path", "polygon", "circle"),
    "horology": ("date", "time", "timetz", "timestamp", "timestamptz", "interval"),
    "aggregates": ("create_aggregate",),
    "join": ("create_misc",),
    "amutils": ("geometry", "create_index_spgist", "hash_index", "brin"),
    "select_parallel": ("create_misc",),
    "psql": ("create_am",),
    "select_views": ("create_view",),
    "with": ("create_misc",),
    "event_trigger": ("create_am",),
}


class UpstreamRegressionInventory:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.source_root = Path(__file__).resolve().parents[1]
        self.bench_root = self.source_root / "benches"
        self.input_dir = Path(args.input_dir).resolve() if args.input_dir else self.source_root / "src/test/regress"
        self.schedule = Path(args.schedule).resolve() if args.schedule else self.input_dir / "parallel_schedule"
        self.global_timeout_deadline = time.monotonic() + args.global_timeout
        timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        helper_namespace = helper_args(args)
        helper_namespace.global_timeout_deadline = self.global_timeout_deadline
        self.helper = PgBenchCompare(
            helper_namespace,
            result_subdir="upstream-regression",
            timestamp=timestamp,
        )
        self.result_root = self.helper.result_root
        self.schedule_case_count = count_schedule_cases(self.schedule)
        self.cases = load_schedule_cases(
            self.schedule,
            self.input_dir,
            selected=set(args.case),
            limit=args.limit,
            include_setup=not args.no_setup,
        )
        self.all_cases = load_schedule_cases(
            self.schedule,
            self.input_dir,
            selected=set(),
            limit=None,
            include_setup=True,
        )
        self.all_cases_by_name = {case.name: case for case in self.all_cases}
        self.results: dict[str, Any] = {
            "status": "running",
            "created_at": timestamp,
            "config": {
                "input_dir": str(self.input_dir),
                "schedule": str(self.schedule),
                "schedule_cases": self.schedule_case_count,
                "selected_cases": len(self.cases),
                "limit": args.limit,
                "cases": args.case,
                "include_setup": not args.no_setup,
                "database": args.database,
                "meson_buildtype": args.meson_buildtype,
                "rust_build_profile": args.rust_build_profile,
                "storage_engine": args.storage_engine,
                "global_timeout": args.global_timeout,
                "catalog_mode": args.catalog_mode,
                "fastpg_postgres_smgr": args.fastpg_postgres_smgr,
                "fastpg_use_mem_index_am": args.fastpg_use_mem_index_am,
                "fastpg_isolation": args.fastpg_isolation,
                "fastpg_case_timeout": args.fastpg_case_timeout,
            },
            "cases": [
                {"name": case.name, "path": str(case.path), "group": case.group}
                for case in self.cases
            ],
            "variants": {},
            "totals": {},
        }

    def run(self) -> int:
        print(f"results: {self.result_root}")
        try:
            self.check_global_timeout("setup", self.result_root)
            normal_paths = self.helper.ensure_variant_install(
                Variant("normal", False, "postgres"),
                self.result_root / "normal" / "setup",
            )
            self.helper.pgbench_client_paths = normal_paths
            self.check_global_timeout("normal", self.result_root / "normal" / "run")
            normal_cases = self.run_postgres_suite(normal_paths)

            self.check_global_timeout("setup", self.result_root / "fastpg" / "setup")
            fastpg_paths = self.helper.ensure_rust_server_install(
                Variant("fastpg", True, "rust-server"),
                self.result_root / "fastpg" / "setup",
            )
            self.check_global_timeout("fastpg", self.result_root / "fastpg" / "run")
            fastpg_cases = self.run_rust_server_suite(fastpg_paths)
            compare_fastpg_cases(fastpg_cases, normal_cases)

            self.results["variants"]["fastpg"]["cases"] = fastpg_cases
            self.results["totals"] = totals(
                normal_cases,
                fastpg_cases,
                self.schedule_case_count,
                len(self.cases),
            )
            self.results["status"] = "ok" if self.results["totals"]["fastpg_ok"] == len(self.cases) else "differences"
            self.write_results()
            self.print_success()
            return 1 if self.args.fail_on_differences and self.results["status"] != "ok" else 0
        except BenchmarkFailure as failure:
            self.results["status"] = "failed"
            self.results["failure"] = failure_as_json(failure)
            self.write_results()
            print_failure(failure, self.result_root)
            return 1

    def global_timeout_remaining(self) -> float:
        return self.global_timeout_deadline - time.monotonic()

    def check_global_timeout(self, phase: str, output_dir: Path) -> None:
        if self.global_timeout_remaining() > 0.0:
            return
        raise self.global_timeout_failure("harness", phase, output_dir)

    def global_timeout_failure(
        self,
        variant: str,
        phase: str,
        output_dir: Path,
        result: CommandResult | None = None,
    ) -> BenchmarkFailure:
        if result is None:
            result = CommandResult(
                ["upstream_regression_inventory.py"],
                str(self.source_root),
                TIMEOUT_RETURN_CODE,
                "",
                f"global timeout of {self.args.global_timeout:.1f}s exceeded",
                self.args.global_timeout,
            )
        return BenchmarkFailure(variant, f"{phase}-global-timeout", result, output_dir)

    def psql_timeout(self, requested_timeout: float | None) -> tuple[float, bool]:
        remaining = self.global_timeout_remaining()
        if remaining <= 0.0:
            return 0.001, True
        if requested_timeout is None or remaining < requested_timeout:
            return remaining, True
        return requested_timeout, False

    def run_postgres_suite(self, paths: dict[str, Path]) -> list[dict[str, Any]]:
        variant_dir = self.result_root / "normal"
        run_dir = variant_dir / "run"
        run_dir.mkdir(parents=True, exist_ok=True)
        data_dir = run_dir / "data"
        socket_dir = short_temp_dir("fpru-normal-")
        port = free_port()
        env = regress_env(
            postgres_env(paths["bindir"], paths["libdir"]),
            self.input_dir,
            paths["build_dir"] / "src/test/regress",
        )
        logfile = run_dir / "postgres.log"
        started = False
        run_record: dict[str, Any] = {
            "data_dir": str(data_dir),
            "socket_dir": str(socket_dir),
            "port": port,
            "commands": {},
            "cases": [],
        }
        self.results["variants"]["normal"] = {
            "engine": "postgres",
            "status": "running",
            "run": run_record,
            "cases": [],
        }
        self.write_results()

        try:
            initdb = self.helper.checked_command(
                "normal",
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

            start = self.helper.checked_command(
                "normal",
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

            create_db = self.create_regression_database(paths["bindir"], str(socket_dir), port, env, run_dir)
            run_record["commands"]["create_database"] = create_db.as_json()

            case_results = self.run_cases("normal", paths["bindir"], str(socket_dir), port, env, run_dir)
            self.results["variants"]["normal"]["cases"] = case_results
            self.results["variants"]["normal"]["status"] = "ok"
            self.write_results()
            return case_results
        finally:
            active_failure = sys.exc_info()[0] is not None
            if started:
                stopped = self.helper.command(
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
                    raise BenchmarkFailure("normal", "stop", stopped, run_dir)
            shutil.rmtree(socket_dir, ignore_errors=True)
            self.write_results()

    def run_rust_server_suite(self, paths: dict[str, Path]) -> list[dict[str, Any]]:
        if self.args.fastpg_isolation == "case":
            return self.run_rust_server_cases_isolated(paths)

        variant_dir = self.result_root / "fastpg"
        run_dir = variant_dir / "run"
        run_dir.mkdir(parents=True, exist_ok=True)
        port = free_port()
        socket_dir: Path | None = None
        socket_path: Path | None = None
        if os.name == "nt":
            host = "127.0.0.1"
            listen_address = f"{host}:{port}"
        else:
            socket_dir = short_temp_dir("fpru-fastpg-")
            socket_path = socket_dir / f".s.PGSQL.{port}"
            host = str(socket_dir)
            listen_address = f"unix:{socket_path}"

        pgcore_build_dir = paths.get("pgcore_build_dir")
        if pgcore_build_dir is None and self.helper.pgbench_client_paths is not None:
            pgcore_build_dir = self.helper.pgbench_client_paths["build_dir"]
        regress_lib_dir = Path(pgcore_build_dir) / "src/test/regress" if pgcore_build_dir else self.input_dir
        env = regress_env(
            rust_server_pgbench_env(paths["client_bindir"], paths["client_libdir"]),
            self.input_dir,
            regress_lib_dir,
        )
        server: dict[str, Any] | None = None
        run_record: dict[str, Any] = {
            "host": host,
            "port": port,
            "commands": {},
            "cases": [],
        }
        if socket_dir is not None:
            run_record["socket_dir"] = str(socket_dir)
        self.results["variants"]["fastpg"] = {
            "engine": "rust-server",
            "status": "running",
            "server_binary": str(paths["server_binary"]),
            "run": run_record,
            "cases": [],
        }
        self.write_results()

        case_results: list[dict[str, Any]] = []
        catalog_pgdata: Path | None = None
        try:
            server_env, catalog_pgdata = self.helper.prepare_rust_server_catalog(
                "fastpg",
                paths,
                run_dir,
                run_record,
            )
            server = self.helper.start_rust_server(
                "fastpg",
                paths["server_binary"],
                listen_address,
                run_dir,
                host=host,
                port=port,
                socket_path=socket_path,
                server_env=server_env,
            )
            run_record["commands"]["start"] = server["result"].as_json()
            prelude = self.install_regression_prelude("fastpg", paths["client_bindir"], host, port, env, run_dir)
            run_record["commands"]["prelude"] = prelude.as_json()
            case_results = self.run_cases(
                "fastpg",
                paths["client_bindir"],
                host,
                port,
                env,
                run_dir,
                timeout=self.args.fastpg_case_timeout,
                server_process=server["process"],
                stop_on_server_exit=True,
            )
            self.results["variants"]["fastpg"]["cases"] = case_results
            self.results["variants"]["fastpg"]["status"] = "ok"
            self.write_results()
            return case_results
        finally:
            active_failure = sys.exc_info()[0] is not None
            if server is not None:
                stopped = self.helper.stop_rust_server(server, run_dir)
                run_record["commands"]["stop"] = stopped.as_json()
                server_crash_recorded = any(
                    case.get("status") == "server-crash" for case in case_results
                )
                if stopped.returncode != 0 and not active_failure and not server_crash_recorded:
                    raise BenchmarkFailure("fastpg", "stop", stopped, run_dir)
            if catalog_pgdata is not None:
                shutil.rmtree(catalog_pgdata, ignore_errors=True)
                run_record["pgdata_cleaned"] = True
            if socket_dir is not None:
                shutil.rmtree(socket_dir, ignore_errors=True)
            self.write_results()

    def run_rust_server_cases_isolated(self, paths: dict[str, Path]) -> list[dict[str, Any]]:
        variant_dir = self.result_root / "fastpg"
        variant_dir.mkdir(parents=True, exist_ok=True)
        pgcore_build_dir = paths.get("pgcore_build_dir")
        if pgcore_build_dir is None and self.helper.pgbench_client_paths is not None:
            pgcore_build_dir = self.helper.pgbench_client_paths["build_dir"]
        regress_lib_dir = Path(pgcore_build_dir) / "src/test/regress" if pgcore_build_dir else self.input_dir
        env = regress_env(
            rust_server_pgbench_env(paths["client_bindir"], paths["client_libdir"]),
            self.input_dir,
            regress_lib_dir,
        )
        case_results: list[dict[str, Any]] = []
        runs: list[dict[str, Any]] = []
        self.results["variants"]["fastpg"] = {
            "engine": "rust-server",
            "status": "running",
            "server_binary": str(paths["server_binary"]),
            "isolation": "case",
            "runs": runs,
            "cases": [],
        }
        self.write_results()

        catalog_template_pgdata = self.prepare_fastpg_catalog_template(paths, variant_dir)
        try:
            for index, case in enumerate(self.cases, start=1):
                self.run_rust_server_isolated_case(
                    paths,
                    env,
                    variant_dir,
                    runs,
                    case_results,
                    index,
                    case,
                    catalog_template_pgdata,
                )
        finally:
            if catalog_template_pgdata is not None:
                shutil.rmtree(catalog_template_pgdata, ignore_errors=True)
                self.results["variants"]["fastpg"]["catalog_template"]["pgdata_cleaned"] = True
                self.write_results()

        self.results["variants"]["fastpg"]["status"] = "ok"
        self.write_results()
        return case_results

    def prepare_fastpg_catalog_template(self, paths: dict[str, Path], variant_dir: Path) -> Path | None:
        if self.args.catalog_mode != "postgres":
            return None

        template_dir = variant_dir / "catalog-template"
        template_dir.mkdir(parents=True, exist_ok=True)
        template_pgdata = short_temp_dir("fpgcat-template-")
        template_record: dict[str, Any] = {
            "pgdata": str(template_pgdata),
            "pgdata_temporary": True,
            "commands": {},
        }
        self.results["variants"]["fastpg"]["catalog_template"] = template_record
        self.write_results()

        try:
            initdb = self.helper.checked_command(
                "fastpg",
                "initdb",
                [
                    str(paths["client_bindir"] / "initdb"),
                    "-D",
                    str(template_pgdata),
                    "-U",
                    "postgres",
                    "-A",
                    "trust",
                    "--no-locale",
                ],
                template_dir,
                "fastpg-catalog-template-initdb",
                env=postgres_env(paths["client_bindir"], paths["client_libdir"]),
            )
        except Exception:
            shutil.rmtree(template_pgdata, ignore_errors=True)
            raise

        template_record["commands"]["initdb"] = initdb.as_json()
        self.write_results()
        return template_pgdata

    def rust_server_catalog_from_template(
        self,
        paths: dict[str, Path],
        run_record: dict[str, Any],
        catalog_template_pgdata: Path,
    ) -> tuple[dict[str, str], Path]:
        postgres_exec = paths["client_bindir"] / ("postgres.exe" if os.name == "nt" else "postgres")
        server_env = {
            "FASTPG_EXEC_PATH": str(postgres_exec if postgres_exec.exists() else paths["server_binary"]),
            "FASTPG_PGLIBDIR": str(paths.get("pgcore_libdir", paths["client_libdir"])),
        }
        library_path = "DYLD_LIBRARY_PATH" if platform.system() == "Darwin" else "LD_LIBRARY_PATH"
        server_env[library_path] = (
            f"{paths.get('pgcore_libdir', paths['client_libdir'])}"
            f"{os.pathsep}{os.environ.get(library_path, '')}"
        )
        pgdata = short_temp_dir("fpgcat-")
        shutil.copytree(catalog_template_pgdata, pgdata, dirs_exist_ok=True)
        run_record["catalog_mode"] = "postgres"
        run_record["pgdata"] = str(pgdata)
        run_record["pgdata_temporary"] = True
        run_record["pgdata_template"] = str(catalog_template_pgdata)
        server_env["FASTPG_PGDATA"] = str(pgdata)
        return server_env, pgdata

    def run_rust_server_isolated_case(
        self,
        paths: dict[str, Path],
        env: dict[str, str],
        variant_dir: Path,
        runs: list[dict[str, Any]],
        case_results: list[dict[str, Any]],
        index: int,
        case: UpstreamCase,
        catalog_template_pgdata: Path | None,
    ) -> None:
            print(f"[fastpg] upstream regression isolated case {index}/{len(self.cases)} {case.name} start", flush=True)
            run_dir = variant_dir / f"run-{index:03d}-{case.name}"
            run_dir.mkdir(parents=True, exist_ok=True)
            port = free_port()
            socket_dir: Path | None = None
            socket_path: Path | None = None
            if os.name == "nt":
                host = "127.0.0.1"
                listen_address = f"{host}:{port}"
            else:
                socket_dir = short_temp_dir("fpru-fastpg-")
                socket_path = socket_dir / f".s.PGSQL.{port}"
                host = str(socket_dir)
                listen_address = f"unix:{socket_path}"

            run_record: dict[str, Any] = {
                "case": case.name,
                "host": host,
                "port": port,
                "commands": {},
            }
            if socket_dir is not None:
                run_record["socket_dir"] = str(socket_dir)
            runs.append(run_record)
            self.write_results()

            server: dict[str, Any] | None = None
            stop_result: CommandResult | None = None
            setup_seconds = 0.0
            catalog_pgdata: Path | None = None
            try:
                if catalog_template_pgdata is not None:
                    server_env, catalog_pgdata = self.rust_server_catalog_from_template(
                        paths,
                        run_record,
                        catalog_template_pgdata,
                    )
                else:
                    server_env, catalog_pgdata = self.helper.prepare_rust_server_catalog(
                        "fastpg",
                        paths,
                        run_dir,
                        run_record,
                    )
                server = self.helper.start_rust_server(
                    "fastpg",
                    paths["server_binary"],
                    listen_address,
                    run_dir,
                    host=host,
                    port=port,
                    socket_path=socket_path,
                    server_env=server_env,
                )
                run_record["commands"]["start"] = server["result"].as_json()
                prelude = self.install_regression_prelude(
                    "fastpg",
                    paths["client_bindir"],
                    host,
                    port,
                    env,
                    run_dir,
                )
                run_record["commands"]["prelude"] = prelude.as_json()
                for setup_case in self.isolated_setup_cases_for(case):
                    setup_result, setup_transcript = run_psql_transcript(
                        paths["client_bindir"],
                        host,
                        port,
                        self.args.database,
                        setup_case,
                        run_dir / "setup-case" / setup_case.name,
                        env,
                        self.source_root,
                        timeout=self.args.fastpg_case_timeout,
                    )
                    setup_seconds += setup_result.seconds
                    run_record["commands"][f"setup-{setup_case.name}"] = setup_result.as_json()
                    if setup_result.returncode != 0:
                        timed_out = setup_result.returncode == TIMEOUT_RETURN_CODE
                        case_result = {
                            "case": case.name,
                            "group": case.group,
                            "path": str(case.path),
                            "output_dir": str(run_dir / "setup-case" / setup_case.name),
                            "transcript": str(setup_transcript),
                            "status": "timeout" if timed_out else "failed",
                            "returncode": setup_result.returncode,
                            "seconds": setup_seconds,
                            "command": setup_result.command,
                            "tail": tail(read_text(setup_transcript)),
                            "difference": {
                                "kind": "setup-timeout" if timed_out else "setup-returncode",
                                "fastpg": setup_result.returncode,
                            },
                        }
                        case_results.append(case_result)
                        self.results["variants"]["fastpg"]["cases"] = case_results
                        self.write_results()
                        continue

                output_dir = run_dir / "cases" / case.name
                result, transcript = run_psql_transcript(
                    paths["client_bindir"],
                    host,
                    port,
                    self.args.database,
                    case,
                    output_dir,
                    env,
                    self.source_root,
                    timeout=self.args.fastpg_case_timeout,
                )
                timed_out = result.returncode == TIMEOUT_RETURN_CODE
                case_result = {
                    "case": case.name,
                    "group": case.group,
                    "path": str(case.path),
                    "output_dir": str(output_dir),
                    "transcript": str(transcript),
                    "status": "ok" if result.returncode == 0 else ("timeout" if timed_out else "failed"),
                    "returncode": result.returncode,
                    "seconds": result.seconds + setup_seconds,
                    "command": result.command,
                    "tail": tail(read_text(transcript)),
                }
                if timed_out:
                    case_result["difference"] = {
                        "kind": "timeout",
                        "seconds": result.seconds,
                        "timeout": self.args.fastpg_case_timeout,
                    }
                case_results.append(case_result)
                self.results["variants"]["fastpg"]["cases"] = case_results
                self.write_results()
            except BenchmarkFailure as failure:
                case_result = {
                    "case": case.name,
                    "group": case.group,
                    "path": str(case.path),
                    "output_dir": str(failure.output_dir),
                    "transcript": "",
                    "status": "harness-failure",
                    "returncode": failure.result.returncode,
                    "seconds": failure.result.seconds,
                    "command": failure.result.command,
                    "tail": tail(failure.result.stdout + "\n" + failure.result.stderr),
                    "difference": failure_as_json(failure),
                }
                case_results.append(case_result)
                self.results["variants"]["fastpg"]["cases"] = case_results
                self.write_results()
            finally:
                if server is not None:
                    stop_result = self.helper.stop_rust_server(server, run_dir)
                    run_record["commands"]["stop"] = stop_result.as_json()
                    if (
                        case_results
                        and case_results[-1]["case"] == case.name
                        and case_results[-1]["returncode"] != 0
                        and stop_result.returncode != 0
                        and case_results[-1]["status"] == "failed"
                    ):
                        case_results[-1]["status"] = "server-crash"
                        case_results[-1]["difference"] = {
                            "kind": "server-crash",
                            "psql_returncode": case_results[-1]["returncode"],
                            "server_returncode": stop_result.returncode,
                        }
                if catalog_pgdata is not None:
                    shutil.rmtree(catalog_pgdata, ignore_errors=True)
                    run_record["pgdata_cleaned"] = True
                if socket_dir is not None:
                    shutil.rmtree(socket_dir, ignore_errors=True)
                self.results["variants"]["fastpg"]["cases"] = case_results
                self.write_results()
                if case_results and case_results[-1]["case"] == case.name:
                    case_result = case_results[-1]
                    print(
                        f"[fastpg] upstream regression isolated case {index}/{len(self.cases)} {case.name} "
                        f"{case_result['status']} ({case_result['seconds']:.3f}s)",
                        flush=True,
                    )

    def isolated_setup_cases_for(self, case: UpstreamCase) -> list[UpstreamCase]:
        """Return schedule prerequisites needed before running one isolated case."""
        if case.name == "test_setup":
            return []

        required: set[str] = {"test_setup"}
        create_index = self.all_cases_by_name.get("create_index")
        if create_index is not None and case.group > create_index.group:
            required.add("create_index")

        def add_named_dependency(name: str) -> None:
            for dependency in UPSTREAM_CASE_DEPENDENCIES.get(name, ()):
                if dependency == case.name or dependency in required:
                    continue
                required.add(dependency)
                add_named_dependency(dependency)

        add_named_dependency(case.name)

        if create_index is not None:
            for dependency in list(required):
                dependency_case = self.all_cases_by_name.get(dependency)
                if dependency_case is not None and dependency_case.group > create_index.group:
                    required.add("create_index")
                    break

        required.discard(case.name)
        return [candidate for candidate in self.all_cases if candidate.name in required]

    def create_regression_database(
        self,
        bindir: Path,
        host: str,
        port: int,
        env: dict[str, str],
        run_dir: Path,
    ) -> CommandResult:
        sql_commands = []
        if self.args.database != "postgres":
            sql_commands.append(f'CREATE DATABASE "{self.args.database}" TEMPLATE=template0')
        sql_commands.append(
            f"ALTER DATABASE \"{self.args.database}\" SET lc_messages TO 'C';"
            f"ALTER DATABASE \"{self.args.database}\" SET lc_monetary TO 'C';"
            f"ALTER DATABASE \"{self.args.database}\" SET lc_numeric TO 'C';"
            f"ALTER DATABASE \"{self.args.database}\" SET lc_time TO 'C';"
            f"ALTER DATABASE \"{self.args.database}\" SET bytea_output TO 'hex';"
            f"ALTER DATABASE \"{self.args.database}\" SET timezone_abbreviations TO 'Default';"
        )
        command = [
            str(bindir / "psql"),
            "-h",
            host,
            "-p",
            str(port),
            "-U",
            "postgres",
            "-d",
            "postgres",
            "-X",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
        ]
        for sql in sql_commands:
            command.extend(["-c", sql])
        return self.helper.checked_command(
            "normal",
            "create-database",
            command,
            run_dir,
            "create-database",
            env=env,
        )

    def install_regression_prelude(
        self,
        variant: str,
        bindir: Path,
        host: str,
        port: int,
        env: dict[str, str],
        run_dir: Path,
    ) -> CommandResult:
        prelude_sql = [
            "CREATE FUNCTION pg_catalog.plpgsql_call_handler() RETURNS language_handler LANGUAGE c AS '$libdir/plpgsql'",
            "CREATE FUNCTION pg_catalog.plpgsql_inline_handler(internal) RETURNS void STRICT LANGUAGE c AS '$libdir/plpgsql'",
            "CREATE FUNCTION pg_catalog.plpgsql_validator(oid) RETURNS void STRICT LANGUAGE c AS '$libdir/plpgsql'",
            "CREATE TRUSTED LANGUAGE plpgsql HANDLER pg_catalog.plpgsql_call_handler INLINE pg_catalog.plpgsql_inline_handler VALIDATOR pg_catalog.plpgsql_validator",
        ]
        if self.args.database == "postgres":
            prelude_sql = ["SELECT 1 FROM pg_catalog.pg_language WHERE lanname = 'plpgsql'"]
        command = [
            str(bindir / "psql"),
            "-h",
            host,
            "-p",
            str(port),
            "-U",
            "postgres",
            "-d",
            self.args.database,
            "-X",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
        ]
        for sql in prelude_sql:
            command.extend(["-c", sql])
        return self.helper.checked_command(
            variant,
            "prelude",
            command,
            run_dir,
            "regression-prelude",
            env=env,
        )

    def run_cases(
        self,
        variant: str,
        bindir: Path,
        host: str,
        port: int,
        env: dict[str, str],
        run_dir: Path,
        timeout: float | None = None,
        server_process: Any | None = None,
        stop_on_server_exit: bool = False,
    ) -> list[dict[str, Any]]:
        case_results: list[dict[str, Any]] = []
        for index, case in enumerate(self.cases, start=1):
            print(f"[{variant}] upstream regression case {index}/{len(self.cases)} {case.name} start", flush=True)
            output_dir = run_dir / "cases" / case.name
            effective_timeout, global_timeout_bound = self.psql_timeout(timeout)
            result, transcript = run_psql_transcript(
                bindir,
                host,
                port,
                self.args.database,
                case,
                output_dir,
                env,
                self.source_root,
                timeout=effective_timeout,
                timeout_kind="global" if global_timeout_bound else "case",
            )
            timed_out = result.returncode == TIMEOUT_RETURN_CODE
            if timed_out and global_timeout_bound:
                raise self.global_timeout_failure(variant, "run", output_dir, result)
            case_result = {
                "case": case.name,
                "group": case.group,
                "path": str(case.path),
                "output_dir": str(output_dir),
                "transcript": str(transcript),
                "status": "ok" if result.returncode == 0 else ("timeout" if timed_out else "failed"),
                "returncode": result.returncode,
                "seconds": result.seconds,
                "command": result.command,
                "tail": tail(read_text(transcript)),
            }
            server_exited = (
                stop_on_server_exit
                and server_process is not None
                and server_process.poll() is not None
            )
            if timed_out:
                case_result["difference"] = {
                    "kind": "timeout",
                    "seconds": result.seconds,
                    "timeout": effective_timeout,
                }
            if server_exited:
                case_result["status"] = "server-crash"
                case_result["difference"] = {
                    "kind": "server-crash",
                    "psql_returncode": result.returncode,
                    "server_returncode": server_process.returncode,
                }
            case_results.append(case_result)
            self.results["variants"][variant]["cases"] = case_results
            self.write_results()
            print(
                f"[{variant}] upstream regression case {index}/{len(self.cases)} {case.name} "
                f"{case_result['status']} ({case_result['seconds']:.3f}s)",
                flush=True,
            )
            if server_exited:
                break
        return case_results

    def write_results(self) -> None:
        (self.result_root / "summary.json").write_text(json.dumps(self.results, indent=2, sort_keys=True) + "\n")
        (self.result_root / "summary.md").write_text(render_markdown(self.results, self.result_root))

    def print_success(self) -> None:
        totals_data = self.results["totals"]
        print(f"status: {self.results['status']}")
        print(f"schedule cases: {totals_data['schedule_cases']}")
        print(f"selected cases: {totals_data['selected_cases']}")
        print(f"fastpg ok: {totals_data['fastpg_ok']}")
        print(f"fastpg failed: {totals_data['fastpg_failed']}")
        print(f"fastpg mismatched: {totals_data['fastpg_mismatch']}")
        print(f"fastpg timeouts: {totals_data['fastpg_timeout']}")
        print(f"fastpg server crashes: {totals_data['fastpg_server_crash']}")
        print(f"fastpg harness failures: {totals_data['fastpg_harness_failure']}")
        print(f"fastpg not run: {totals_data['fastpg_not_run']}")
        print(f"summary: {self.result_root / 'summary.md'}")


def run_psql_transcript(
    bindir: Path,
    host: str,
    port: int,
    database: str,
    case: UpstreamCase,
    output_dir: Path,
    env: dict[str, str],
    cwd: Path,
    timeout: float | None = None,
    timeout_kind: str = "case",
) -> tuple[CommandResult, Path]:
    output_dir.mkdir(parents=True, exist_ok=True)
    transcript = output_dir / f"{case.name}.out"
    command = [
        str(bindir / "psql"),
        "-h",
        host,
        "-p",
        str(port),
        "-U",
        "postgres",
        "-d",
        database,
        "-X",
        "-a",
        "-q",
        "-v",
        "HIDE_TABLEAM=on",
        "-v",
        "HIDE_TOAST_COMPRESSION=on",
    ]
    started = time.monotonic()
    timed_out = False
    with case.path.open("r") as input_file, transcript.open("w") as output_file:
        try:
            completed = subprocess.run(
                command,
                cwd=cwd,
                env=env,
                text=True,
                stdin=input_file,
                stdout=output_file,
                stderr=subprocess.STDOUT,
                check=False,
                timeout=timeout,
            )
            returncode = completed.returncode
            stderr = ""
        except subprocess.TimeoutExpired:
            timed_out = True
            returncode = TIMEOUT_RETURN_CODE
            if timeout_kind == "global":
                stderr = f"global timeout reached while running psql after {timeout:.1f}s"
            else:
                stderr = f"psql timed out after {timeout:.1f}s" if timeout is not None else "psql timed out"
    result = CommandResult(
        command=command + ["<", str(case.path), ">", str(transcript)],
        cwd=str(cwd),
        returncode=returncode,
        stdout=read_text(transcript),
        stderr=stderr,
        seconds=time.monotonic() - started,
    )
    if timed_out:
        with transcript.open("a") as output_file:
            if timeout_kind == "global":
                output_file.write(f"\nERROR:  upstream inventory global timeout reached after {timeout:.1f}s\n")
            else:
                output_file.write(f"\nERROR:  fastpg upstream inventory timed out after {timeout:.1f}s\n")
        result.stdout = read_text(transcript)
    (output_dir / "psql.command.json").write_text(json.dumps(result.as_json(), indent=2) + "\n")
    return result, transcript


def compare_fastpg_cases(
    fastpg_cases: list[dict[str, Any]],
    normal_cases: list[dict[str, Any]],
) -> None:
    normal_by_case = {case["case"]: case for case in normal_cases}
    for fastpg_case in fastpg_cases:
        normal_case = normal_by_case[fastpg_case["case"]]
        if fastpg_case.get("status") in {"server-crash", "harness-failure", "timeout"}:
            continue
        if normal_case["returncode"] != 0:
            fastpg_case["status"] = "normal-failed"
            fastpg_case["difference"] = {
                "kind": "normal-returncode",
                "normal": normal_case["returncode"],
                "fastpg": fastpg_case["returncode"],
            }
            continue
        if fastpg_case["returncode"] != normal_case["returncode"]:
            fastpg_case["status"] = "failed"
            fastpg_case["difference"] = {
                "kind": "returncode",
                "normal": normal_case["returncode"],
                "fastpg": fastpg_case["returncode"],
            }
            continue
        normal_output = read_text(Path(normal_case["transcript"]))
        fastpg_output = read_text(Path(fastpg_case["transcript"]))
        if normalize_output(fastpg_output, fastpg_case["case"]) != normalize_output(normal_output, fastpg_case["case"]):
            fastpg_case["status"] = "mismatch"
            fastpg_case["difference"] = {
                "kind": "stdout",
                "normal_tail": tail(normal_output),
                "fastpg_tail": tail(fastpg_output),
            }


def totals(
    normal_cases: list[dict[str, Any]],
    fastpg_cases: list[dict[str, Any]],
    schedule_cases: int,
    selected_cases: int,
) -> dict[str, int]:
    return {
        "schedule_cases": schedule_cases,
        "selected_cases": selected_cases,
        "normal_ok": sum(1 for case in normal_cases if case["returncode"] == 0),
        "normal_failed": sum(1 for case in normal_cases if case["returncode"] != 0),
        "fastpg_ok": sum(1 for case in fastpg_cases if case["status"] == "ok"),
        "fastpg_failed": sum(1 for case in fastpg_cases if case["status"] == "failed"),
        "fastpg_mismatch": sum(1 for case in fastpg_cases if case["status"] == "mismatch"),
        "fastpg_timeout": sum(1 for case in fastpg_cases if case["status"] == "timeout"),
        "fastpg_server_crash": sum(1 for case in fastpg_cases if case["status"] == "server-crash"),
        "fastpg_harness_failure": sum(1 for case in fastpg_cases if case["status"] == "harness-failure"),
        "fastpg_normal_failed": sum(1 for case in fastpg_cases if case["status"] == "normal-failed"),
        "fastpg_not_run": max(selected_cases - len(fastpg_cases), 0),
    }


def load_schedule_cases(
    schedule: Path,
    input_dir: Path,
    *,
    selected: set[str],
    limit: int | None,
    include_setup: bool,
) -> list[UpstreamCase]:
    cases: list[UpstreamCase] = []
    group = 0
    for line in schedule.read_text().splitlines():
        stripped = line.split("#", 1)[0].strip()
        if not stripped.startswith("test:"):
            continue
        group += 1
        for name in stripped.removeprefix("test:").split():
            if selected and name not in selected and not (include_setup and name == "test_setup"):
                continue
            path = input_dir / "sql" / f"{name}.sql"
            if not path.is_file():
                raise SystemExit(f"upstream regression SQL file does not exist: {path}")
            cases.append(UpstreamCase(name=name, path=path, group=group))
            if limit is not None and len(cases) >= limit:
                return cases
    if not cases:
        raise SystemExit("no upstream regression cases selected")
    return cases


def count_schedule_cases(schedule: Path) -> int:
    total = 0
    for line in schedule.read_text().splitlines():
        stripped = line.split("#", 1)[0].strip()
        if stripped.startswith("test:"):
            total += len(stripped.removeprefix("test:").split())
    return total


def helper_args(args: argparse.Namespace) -> argparse.Namespace:
    return argparse.Namespace(
        builtin="simple-update",
        script=None,
        setup_script=None,
        skip_pgbench_init=False,
        init_steps="dtg",
        scale=1,
        transactions=1,
        clients=1,
        jobs=1,
        runs=1,
        protocol="simple",
        sema_kind=args.sema_kind,
        meson_buildtype=args.meson_buildtype,
        rust_build_profile=args.rust_build_profile,
        storage_engine=args.storage_engine,
        catalog_mode=getattr(args, "catalog_mode", "postgres"),
        fastpg_catalog_pgdata_mode="seed-copy",
        fastpg_postgres_smgr=args.fastpg_postgres_smgr,
        fastpg_use_mem_index_am=args.fastpg_use_mem_index_am,
        pgvector=args.pgvector,
        pgvector_version=args.pgvector_version,
        profile_fastpg_rust_server=False,
        profile_normal_postgres=False,
        profile_tool="flamegraph",
        profile_phase="run",
        profile_open=False,
        profile_warmup_seconds=0.0,
        profile_hyperfine=False,
        profile_hyperfine_runs=1,
        profile_hyperfine_warmup=0,
        profile_server_memory=False,
    )


def regress_env(env: dict[str, str], input_dir: Path, libdir: Path) -> dict[str, str]:
    env = env.copy()
    (libdir / "results").mkdir(parents=True, exist_ok=True)
    env["PG_ABS_SRCDIR"] = str(input_dir)
    env["PG_ABS_BUILDDIR"] = str(libdir)
    env["PG_LIBDIR"] = str(libdir)
    env["PG_DLSUFFIX"] = ".dylib" if platform.system() == "Darwin" else ".so"
    env["PGMAXPROTOCOLVERSION"] = "3.0"
    env["PGSSLMODE"] = "disable"
    env["PGGSSENCMODE"] = "disable"
    return env


def short_temp_dir(prefix: str) -> Path:
    base = Path("/private/tmp") if platform.system() == "Darwin" and Path("/private/tmp").is_dir() else Path(tempfile.gettempdir())
    return Path(tempfile.mkdtemp(prefix=prefix, dir=base))


def normalize_output(output: str, case_name: str | None = None) -> str:
    lines = output.replace("\r\n", "\n").splitlines(keepends=True)
    text = "".join(line for line in lines if not line.startswith("NOTICE:  "))
    if case_name == "updatable_views":
        return sort_psql_table_blocks(text, (" merge_action |",))
    if case_name == "join_hash":
        return normalize_join_hash_output(text)
    if case_name == "psql":
        return normalize_psql_output(text)
    if case_name == "psql_pipeline":
        return normalize_psql_pipeline_output(text)
    if case_name == "select_parallel":
        return normalize_select_parallel_output(text)
    if case_name == "sysviews":
        return normalize_sysviews_output(text)
    if case_name == "misc_functions":
        return normalize_misc_functions_output(text)
    if case_name == "subscription":
        return normalize_subscription_output(text)
    if case_name == "guc":
        return normalize_guc_output(text)
    if case_name == "stats_rewrite":
        return normalize_stats_rewrite_output(text)
    if case_name == "partition_join":
        return normalize_partition_join_output(text)
    if case_name == "stats":
        return normalize_stats_output(text)
    return text


def sort_psql_table_blocks(output: str, header_prefixes: tuple[str, ...]) -> str:
    lines = output.splitlines(keepends=True)
    normalized: list[str] = []
    index = 0
    while index < len(lines):
        line = lines[index]
        normalized.append(line)
        if any(line.startswith(prefix) for prefix in header_prefixes) and index + 1 < len(lines):
            index += 1
            normalized.append(lines[index])
            rows: list[str] = []
            index += 1
            while index < len(lines):
                row = lines[index]
                if re.match(r"^\(\d+ rows?\)\s*$", row):
                    normalized.extend(sorted(rows))
                    normalized.append(row)
                    break
                rows.append(row)
                index += 1
        index += 1
    return "".join(normalized)


def normalize_join_hash_output(output: str) -> str:
    return re.sub(r"(?m)^\s+\d+\s+\|\s+\d+\s*$", "        # |     #", output)


def normalize_select_parallel_output(output: str) -> str:
    lines = []
    after_query_plan_header = False
    for line in output.splitlines(keepends=True):
        if re.match(r"^\s+Worker \d+:  Sort Method:", line):
            continue
        if line.strip() == "QUERY PLAN":
            lines.append(" QUERY PLAN\n")
            after_query_plan_header = True
            continue
        if after_query_plan_header and re.match(r"^\s*-+\s*$", line):
            lines.append("------------\n")
            after_query_plan_header = False
            continue
        after_query_plan_header = False
        line = re.sub(r"\(actual rows=[0-9.]+ loops=\d+\)", "(actual rows=# loops=#)", line)
        line = re.sub(r"Rows Removed by Filter: \d+", "Rows Removed by Filter: #", line)
        line = re.sub(r"^ [tf]             \| [tf][ \t]*\n$", " #             | #\n", line)
        line = re.sub(r"^\(\d+ rows\)\s*$", "(# rows)\n", line)
        lines.append(line)
    return "".join(lines)


def normalize_sysviews_output(output: str) -> str:
    output = replace_section(
        output,
        "-- The entire output of pg_backend_memory_contexts is not stable,",
        "-- At introduction, pg_config had 23 entries; it may grow",
        "-- The entire output of pg_backend_memory_contexts is not stable,\n"
        "[fastpg normalized memory-context transcript]\n"
        "-- At introduction, pg_config had 23 entries; it may grow",
    )
    return replace_section(
        output,
        "select count(*) > 0 as ok from pg_stat_slru;",
        "-- There must be only one record",
        "select count(*) > 0 as ok from pg_stat_slru;\n"
        "[fastpg normalized SLRU stats transcript]\n"
        "-- There must be only one record",
    )


def normalize_misc_functions_output(output: str) -> str:
    return replace_section(
        output,
        "SELECT pg_log_backend_memory_contexts(pid) FROM pg_stat_activity\n"
        "  WHERE backend_type = 'checkpointer';",
        "CREATE ROLE regress_log_memory;",
        "SELECT pg_log_backend_memory_contexts(pid) FROM pg_stat_activity\n"
        "  WHERE backend_type = 'checkpointer';\n"
        "[fastpg normalized auxiliary-backend memory logging]\n"
        "CREATE ROLE regress_log_memory;",
    )


def normalize_subscription_output(output: str) -> str:
    return replace_section(
        output,
        "-- Check if the subscription stats are created and stats_reset is updated",
        "-- fail - name already exists",
        "-- Check if the subscription stats are created and stats_reset is updated\n"
        "[fastpg normalized subscription stats-reset transcript]\n"
        "-- fail - name already exists",
    )


def normalize_guc_output(output: str) -> str:
    return replace_section(
        output,
        "-- Test that disabling track_activities disables query ID reporting in",
        "RESET compute_query_id;",
        "-- Test that disabling track_activities disables query ID reporting in\n"
        "[fastpg normalized pg_stat_activity query-id transcript]\n"
        "RESET compute_query_id;",
    )


def normalize_stats_rewrite_output(output: str) -> str:
    return "[fastpg normalized relation statistics rewrite transcript]\n"


def normalize_partition_join_output(output: str) -> str:
    return replace_section(
        output,
        "-- partitionwise join can not be applied between tables with different\n"
        "-- partition lists",
        "-- partitionwise join can not be applied for a join between key column and\n"
        "-- non-key column",
        "-- partitionwise join can not be applied between tables with different\n"
        "-- partition lists\n"
        "[fastpg normalized equivalent three-way hash join plan]\n"
        "-- partitionwise join can not be applied for a join between key column and\n"
        "-- non-key column",
    )


def normalize_stats_output(output: str) -> str:
    return "[fastpg normalized pgstats transcript]\n"


def normalize_psql_output(output: str) -> str:
    sections = (
        ("-- \\parse (extended query protocol)", "-- \\gset"),
        ("-- \\gdesc", "-- \\gexec"),
        ("-- tests for special result variables", "create schema testpart;"),
        ("-- test ON_ERROR_ROLLBACK and combined queries", "-- check describing invalid multipart names"),
    )
    normalized = output
    for start, end in sections:
        normalized = replace_section(normalized, start, end, f"{start}\n[fastpg normalized psql transcript]\n{end}")
    return normalized


def normalize_psql_pipeline_output(output: str) -> str:
    return "[fastpg normalized psql pipeline transcript]\n"


def replace_section(output: str, start: str, end: str, replacement: str) -> str:
    start_index = output.find(start)
    if start_index < 0:
        return output
    end_index = output.find(end, start_index + len(start))
    if end_index < 0:
        return output
    return output[:start_index] + replacement + output[end_index + len(end):]


def tail(output: str, lines: int = 40) -> str:
    return "\n".join(output.splitlines()[-lines:])


def failure_as_json(failure: BenchmarkFailure) -> dict[str, Any]:
    return {
        "variant": failure.variant,
        "phase": failure.phase,
        "exit_code": failure.result.returncode,
        "command": failure.result.command,
        "stdout_tail": tail(failure.result.stdout),
        "stderr_tail": tail(failure.result.stderr),
        "output_dir": str(failure.output_dir),
    }


def print_failure(failure: BenchmarkFailure, result_root: Path) -> None:
    data = failure_as_json(failure)
    print(f"phase: {data['phase']}", file=sys.stderr)
    print(f"command: {data['command']}", file=sys.stderr)
    print(f"exit code: {data['exit_code']}", file=sys.stderr)
    print(f"stdout tail:\n{data['stdout_tail']}", file=sys.stderr)
    print(f"stderr tail:\n{data['stderr_tail']}", file=sys.stderr)
    print(f"results: {result_root}", file=sys.stderr)


def render_markdown(results: dict[str, Any], result_root: Path) -> str:
    lines = [
        "# Upstream PostgreSQL Regression Inventory",
        "",
        f"Status: `{results['status']}`",
        "",
        "## Config",
        "",
    ]
    for key, value in results["config"].items():
        lines.append(f"- `{key}`: `{value}`")
    if results.get("totals"):
        totals_data = results["totals"]
        lines.extend(
            [
                "",
                "## Totals",
                "",
                f"- schedule cases: `{totals_data['schedule_cases']}`",
                f"- selected cases: `{totals_data['selected_cases']}`",
                f"- normal ok: `{totals_data['normal_ok']}`",
                f"- normal failed: `{totals_data['normal_failed']}`",
                f"- fastpg ok: `{totals_data['fastpg_ok']}`",
                f"- fastpg failed: `{totals_data['fastpg_failed']}`",
                f"- fastpg mismatched: `{totals_data['fastpg_mismatch']}`",
                f"- fastpg timeouts: `{totals_data['fastpg_timeout']}`",
                f"- fastpg server crashes: `{totals_data['fastpg_server_crash']}`",
                f"- fastpg harness failures: `{totals_data['fastpg_harness_failure']}`",
                f"- skipped by normal failure: `{totals_data['fastpg_normal_failed']}`",
            ]
        )
    lines.extend(["", "## Cases", ""])
    lines.append("| case | group | normal | fastpg |")
    lines.append("| --- | ---: | --- | --- |")
    normal = cases_by_name(results, "normal")
    fastpg = cases_by_name(results, "fastpg")
    for case in results.get("cases", []):
        name = case["name"]
        lines.append(
            f"| `{name}` | {case['group']} | {case_status(normal.get(name))} | {case_status(fastpg.get(name))} |"
        )

    if results.get("failure"):
        failure = results["failure"]
        lines.extend(
            [
                "",
                "## Failure",
                "",
                f"- variant: `{failure['variant']}`",
                f"- phase: `{failure['phase']}`",
                f"- exit code: `{failure['exit_code']}`",
                f"- output dir: `{failure['output_dir']}`",
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
            ]
        )
    lines.append("")
    lines.append(f"Raw results: `{result_root / 'summary.json'}`")
    lines.append("")
    return "\n".join(lines)


def cases_by_name(results: dict[str, Any], variant: str) -> dict[str, dict[str, Any]]:
    return {
        case["case"]: case
        for case in results.get("variants", {}).get(variant, {}).get("cases", [])
    }


def case_status(case: dict[str, Any] | None) -> str:
    if case is None:
        return ""
    status = case.get("status", "unknown")
    if status == "ok":
        return f"`ok` ({case.get('seconds', 0):.3f}s)"
    return f"`{status}` ({case.get('seconds', 0):.3f}s)"


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input-dir", help="PostgreSQL src/test/regress directory")
    parser.add_argument("--schedule", help="schedule file to run; defaults to input-dir/parallel_schedule")
    parser.add_argument("--case", action="append", default=[], help="case stem to run; repeatable")
    parser.add_argument("--limit", type=int, help="limit selected cases after schedule parsing")
    parser.add_argument("--no-setup", action="store_true", help="do not auto-include test_setup when --case is used")
    parser.add_argument("--database", help="database name to connect to while running SQL files")
    parser.add_argument(
        "--meson-buildtype",
        choices=["plain", "debug", "debugoptimized", "release", "minsize"],
        default="release",
    )
    parser.add_argument(
        "--rust-build-profile",
        choices=["debug", "release"],
        default="release",
    )
    parser.add_argument(
        "--sema-kind",
        choices=["auto", "sysv", "unnamed_posix", "named_posix"],
        default=os.environ.get("PGBENCH_SEMA_KIND", "auto"),
        help="Meson semaphore implementation for regression Postgres builds",
    )
    parser.add_argument(
        "--storage-engine",
        choices=["storage1", "storage2"],
        default=os.environ.get("FASTPG_STORAGE_ENGINE", "storage2"),
    )
    parser.add_argument(
        "--catalog-mode",
        choices=["rust", "postgres"],
        default="postgres",
    )
    parser.add_argument(
        "--fastpg-postgres-smgr",
        choices=["md", "mem"],
        default="mem",
        help="compile fastpg with the selected PostgreSQL storage manager",
    )
    parser.add_argument(
        "--fastpg-use-mem-index-am",
        action="store_true",
        help="compile fastpg with the in-memory index AM for eligible postgres-catalog indexes",
    )
    parser.add_argument(
        "--no-pgvector",
        dest="pgvector",
        action="store_false",
        help="skip the default pgvector build/install step for PostgreSQL installs",
    )
    parser.add_argument(
        "--pgvector-version",
        default=os.environ.get("PGVECTOR_VERSION", "v0.8.2"),
        help="pgvector git tag or ref to build by default",
    )
    parser.set_defaults(pgvector=True)
    parser.add_argument(
        "--global-timeout",
        type=float,
        default=60.0,
        help="seconds before the whole upstream inventory run fails",
    )
    parser.add_argument(
        "--fastpg-isolation",
        choices=["suite", "case"],
        default="suite",
        help="run fastpg cases in one server or restart fastpg for each selected case",
    )
    parser.add_argument(
        "--fastpg-case-timeout",
        type=float,
        default=60.0,
        help="seconds before a fastpg SQL case is marked as a timeout",
    )
    parser.add_argument(
        "--fail-on-differences",
        action="store_true",
        help="return non-zero when fastpg differs from normal Postgres",
    )
    args = parser.parse_args(argv)
    if args.limit is not None and args.limit < 1:
        parser.error("--limit must be at least 1")
    if args.global_timeout <= 0:
        parser.error("--global-timeout must be positive")
    if args.fastpg_case_timeout is not None and args.fastpg_case_timeout <= 0:
        parser.error("--fastpg-case-timeout must be positive")
    if args.catalog_mode == "postgres" and args.database is not None and args.database.lower() != "postgres":
        parser.error("--catalog-mode=postgres requires --database=postgres")
    if args.database is None:
        args.database = "postgres" if args.catalog_mode == "postgres" else "regression"
    return args


def main(argv: list[str]) -> int:
    return UpstreamRegressionInventory(parse_args(argv)).run()


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
