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


@dataclass(frozen=True)
class UpstreamCase:
    name: str
    path: Path
    group: int


class UpstreamRegressionInventory:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.source_root = Path(__file__).resolve().parents[1]
        self.bench_root = self.source_root / "benches"
        self.input_dir = Path(args.input_dir).resolve() if args.input_dir else self.source_root / "src/test/regress"
        self.schedule = Path(args.schedule).resolve() if args.schedule else self.input_dir / "parallel_schedule"
        timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        self.helper = PgBenchCompare(
            helper_args(args),
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
                "rust_pgcore": args.rust_pgcore,
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
            normal_paths = self.helper.ensure_variant_install(
                Variant("normal", False, "postgres"),
                self.result_root / "normal" / "setup",
            )
            self.helper.pgbench_client_paths = normal_paths
            normal_cases = self.run_postgres_suite(normal_paths)

            fastpg_paths = self.helper.ensure_rust_server_install(
                Variant("fastpg", True, "rust-server"),
                self.result_root / "fastpg" / "setup",
            )
            fastpg_cases = self.run_rust_server_suite(fastpg_paths)
            compare_fastpg_cases(fastpg_cases, normal_cases)

            self.results["variants"]["fastpg"]["cases"] = fastpg_cases
            self.results["totals"] = totals(normal_cases, fastpg_cases, self.schedule_case_count)
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

        try:
            server = self.helper.start_rust_server(
                "fastpg",
                paths["server_binary"],
                listen_address,
                run_dir,
                host=host,
                port=port,
                socket_path=socket_path,
            )
            run_record["commands"]["start"] = server["result"].as_json()
            prelude = self.install_regression_prelude("fastpg", paths["client_bindir"], host, port, env, run_dir)
            run_record["commands"]["prelude"] = prelude.as_json()
            case_results = self.run_cases("fastpg", paths["client_bindir"], host, port, env, run_dir)
            self.results["variants"]["fastpg"]["cases"] = case_results
            self.results["variants"]["fastpg"]["status"] = "ok"
            self.write_results()
            return case_results
        finally:
            active_failure = sys.exc_info()[0] is not None
            if server is not None:
                stopped = self.helper.stop_rust_server(server, run_dir)
                run_record["commands"]["stop"] = stopped.as_json()
                if stopped.returncode != 0 and not active_failure:
                    raise BenchmarkFailure("fastpg", "stop", stopped, run_dir)
            if socket_dir is not None:
                shutil.rmtree(socket_dir, ignore_errors=True)
            self.write_results()

    def create_regression_database(
        self,
        bindir: Path,
        host: str,
        port: int,
        env: dict[str, str],
        run_dir: Path,
    ) -> CommandResult:
        return self.helper.checked_command(
            "normal",
            "create-database",
            [
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
                "-c",
                f'CREATE DATABASE "{self.args.database}" TEMPLATE=template0',
                "-c",
                (
                    f"ALTER DATABASE \"{self.args.database}\" SET lc_messages TO 'C';"
                    f"ALTER DATABASE \"{self.args.database}\" SET lc_monetary TO 'C';"
                    f"ALTER DATABASE \"{self.args.database}\" SET lc_numeric TO 'C';"
                    f"ALTER DATABASE \"{self.args.database}\" SET lc_time TO 'C';"
                    f"ALTER DATABASE \"{self.args.database}\" SET bytea_output TO 'hex';"
                    f"ALTER DATABASE \"{self.args.database}\" SET timezone_abbreviations TO 'Default';"
                ),
            ],
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
        return self.helper.checked_command(
            variant,
            "prelude",
            [
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
                "-c",
                "CREATE FUNCTION pg_catalog.plpgsql_call_handler() RETURNS language_handler LANGUAGE c AS '$libdir/plpgsql'",
                "-c",
                "CREATE FUNCTION pg_catalog.plpgsql_inline_handler(internal) RETURNS void STRICT LANGUAGE c AS '$libdir/plpgsql'",
                "-c",
                "CREATE FUNCTION pg_catalog.plpgsql_validator(oid) RETURNS void STRICT LANGUAGE c AS '$libdir/plpgsql'",
                "-c",
                "CREATE TRUSTED LANGUAGE plpgsql HANDLER pg_catalog.plpgsql_call_handler INLINE pg_catalog.plpgsql_inline_handler VALIDATOR pg_catalog.plpgsql_validator",
            ],
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
    ) -> list[dict[str, Any]]:
        case_results: list[dict[str, Any]] = []
        for index, case in enumerate(self.cases, start=1):
            print(f"[{variant}] upstream regression case {index}/{len(self.cases)} {case.name}")
            output_dir = run_dir / "cases" / case.name
            result, transcript = run_psql_transcript(
                bindir,
                host,
                port,
                self.args.database,
                case,
                output_dir,
                env,
                self.source_root,
            )
            case_result = {
                "case": case.name,
                "group": case.group,
                "path": str(case.path),
                "output_dir": str(output_dir),
                "transcript": str(transcript),
                "status": "ok" if result.returncode == 0 else "failed",
                "returncode": result.returncode,
                "seconds": result.seconds,
                "command": result.command,
                "tail": tail(read_text(transcript)),
            }
            case_results.append(case_result)
            self.results["variants"][variant]["cases"] = case_results
            self.write_results()
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
    with case.path.open("r") as input_file, transcript.open("w") as output_file:
        completed = subprocess.run(
            command,
            cwd=cwd,
            env=env,
            text=True,
            stdin=input_file,
            stdout=output_file,
            stderr=subprocess.STDOUT,
            check=False,
        )
    result = CommandResult(
        command=command + ["<", str(case.path), ">", str(transcript)],
        cwd=str(cwd),
        returncode=completed.returncode,
        stdout=read_text(transcript),
        stderr="",
        seconds=time.monotonic() - started,
    )
    (output_dir / "psql.command.json").write_text(json.dumps(result.as_json(), indent=2) + "\n")
    return result, transcript


def compare_fastpg_cases(
    fastpg_cases: list[dict[str, Any]],
    normal_cases: list[dict[str, Any]],
) -> None:
    normal_by_case = {case["case"]: case for case in normal_cases}
    for fastpg_case in fastpg_cases:
        normal_case = normal_by_case[fastpg_case["case"]]
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
        if normalize_output(fastpg_output) != normalize_output(normal_output):
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
) -> dict[str, int]:
    return {
        "schedule_cases": schedule_cases,
        "selected_cases": len(fastpg_cases),
        "normal_ok": sum(1 for case in normal_cases if case["returncode"] == 0),
        "normal_failed": sum(1 for case in normal_cases if case["returncode"] != 0),
        "fastpg_ok": sum(1 for case in fastpg_cases if case["status"] == "ok"),
        "fastpg_failed": sum(1 for case in fastpg_cases if case["status"] == "failed"),
        "fastpg_mismatch": sum(1 for case in fastpg_cases if case["status"] == "mismatch"),
        "fastpg_normal_failed": sum(1 for case in fastpg_cases if case["status"] == "normal-failed"),
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
        init_steps="dtg",
        scale=1,
        transactions=1,
        clients=1,
        jobs=1,
        runs=1,
        protocol="simple",
        fastpg_engine="rust-server",
        meson_buildtype=args.meson_buildtype,
        rust_build_profile=args.rust_build_profile,
        rust_pgcore=args.rust_pgcore,
        profile_fastpg_rust_server=False,
        profile_normal_postgres=False,
        profile_tool="flamegraph",
        profile_phase="run",
        profile_open=False,
        profile_warmup_seconds=0.0,
    )


def regress_env(env: dict[str, str], input_dir: Path, libdir: Path) -> dict[str, str]:
    env = env.copy()
    env["PG_ABS_SRCDIR"] = str(input_dir)
    env["PG_LIBDIR"] = str(libdir)
    env["PG_DLSUFFIX"] = ".dylib" if platform.system() == "Darwin" else ".so"
    env["PGMAXPROTOCOLVERSION"] = "3.0"
    env["PGSSLMODE"] = "disable"
    env["PGGSSENCMODE"] = "disable"
    return env


def short_temp_dir(prefix: str) -> Path:
    base = Path("/private/tmp") if platform.system() == "Darwin" and Path("/private/tmp").is_dir() else Path(tempfile.gettempdir())
    return Path(tempfile.mkdtemp(prefix=prefix, dir=base))


def normalize_output(output: str) -> str:
    lines = output.replace("\r\n", "\n").splitlines(keepends=True)
    return "".join(line for line in lines if not line.startswith("NOTICE:  "))


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
    parser.add_argument("--database", default="regression", help="database name to connect to while running SQL files")
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
        "--rust-pgcore",
        choices=["off", "raw-parser", "full"],
        default="full",
    )
    parser.add_argument(
        "--fail-on-differences",
        action="store_true",
        help="return non-zero when fastpg differs from normal Postgres",
    )
    args = parser.parse_args(argv)
    if args.limit is not None and args.limit < 1:
        parser.error("--limit must be at least 1")
    return args


def main(argv: list[str]) -> int:
    return UpstreamRegressionInventory(parse_args(argv)).run()


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
