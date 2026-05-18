#!/usr/bin/env python3
"""Compare curated SQL regression cases against normal Postgres and fastpg."""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
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
    classify_failure,
    free_port,
    postgres_env,
    rust_server_pgbench_env,
)


@dataclass(frozen=True)
class RegressionCase:
    name: str
    path: Path


class RegressionFailure(Exception):
    def __init__(
        self,
        variant: str,
        phase: str,
        result: CommandResult,
        output_dir: Path,
        *,
        case: str | None = None,
        expected_stdout: str | None = None,
        actual_stdout: str | None = None,
    ):
        self.variant = variant
        self.phase = phase
        self.result = result
        self.output_dir = output_dir
        self.case = case
        self.expected_stdout = expected_stdout
        self.actual_stdout = actual_stdout
        super().__init__(f"{variant} failed during {phase}")


class RegressionCompare:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.source_root = Path(__file__).resolve().parents[1]
        self.bench_root = self.source_root / "benches"
        timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        self.result_root = self.bench_root / "results" / "regression" / timestamp
        self.result_root.mkdir(parents=True, exist_ok=True)
        self.helper = PgBenchCompare(
            helper_args(args),
            result_subdir="regression",
            timestamp=timestamp,
        )
        self.cases = load_cases(self.bench_root, args)
        self.results: dict[str, Any] = {
            "status": "running",
            "created_at": timestamp,
            "config": {
                "suite": args.suite,
                "cases_dir": str(cases_dir(self.bench_root, args)),
                "allow_fastpg_failures": args.allow_fastpg_failures,
                "meson_buildtype": args.meson_buildtype,
                "rust_build_profile": args.rust_build_profile,
                "rust_pgcore": args.rust_pgcore,
                "fastpg_no_internal_ipc": args.fastpg_no_internal_ipc,
            },
            "cases": [{"name": case.name, "path": str(case.path)} for case in self.cases],
            "variants": {},
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
            self.results["variants"]["normal"] = {
                "engine": "postgres",
                "status": "ok",
                "cases": normal_cases,
            }
            self.write_results()

            fastpg_paths = self.helper.ensure_rust_server_install(
                Variant("fastpg", True, "rust-server"),
                self.result_root / "fastpg" / "setup",
            )
            fastpg_cases = self.run_rust_server_suite(fastpg_paths, normal_cases)
            fastpg_status = "ok" if all(case["status"] == "ok" for case in fastpg_cases) else "differences"
            self.results["variants"]["fastpg"] = {
                "engine": "rust-server",
                "status": fastpg_status,
                "server_binary": str(fastpg_paths["server_binary"]),
                "cases": fastpg_cases,
            }
            self.results["status"] = "ok" if fastpg_status == "ok" else "differences"
            self.write_results()
            self.print_success()
            return 0
        except BenchmarkFailure as failure:
            regression_failure = RegressionFailure(
                failure.variant,
                failure.phase,
                failure.result,
                failure.output_dir,
            )
            self.record_failure(regression_failure)
            return 1
        except RegressionFailure as failure:
            self.record_failure(failure)
            return 1

    def run_postgres_suite(self, paths: dict[str, Path]) -> list[dict[str, Any]]:
        variant_dir = self.result_root / "normal"
        run_dir = variant_dir / "run"
        run_dir.mkdir(parents=True, exist_ok=True)
        data_dir = run_dir / "data"
        socket_dir = short_temp_dir("fprg-normal-")
        port = free_port()
        env = postgres_env(paths["bindir"], paths["libdir"])
        logfile = run_dir / "postgres.log"
        started = False
        run_record = {
            "data_dir": str(data_dir),
            "socket_dir": str(socket_dir),
            "port": port,
            "commands": {},
        }
        self.results["variants"]["normal"] = {
            "engine": "postgres",
            "status": "running",
            "run": run_record,
            "cases": [],
        }
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

            case_results = self.run_cases("normal", paths["bindir"], str(socket_dir), port, env, run_dir)
            self.results["variants"]["normal"]["cases"] = case_results
            for case_result in case_results:
                if case_result["returncode"] != 0:
                    raise RegressionFailure(
                        "normal",
                        "psql-case",
                        command_result_from_case(case_result),
                        Path(case_result["output_dir"]),
                        case=case_result["case"],
                    )
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
                    raise RegressionFailure("normal", "stop", stopped, run_dir)
            shutil.rmtree(socket_dir, ignore_errors=True)
            self.write_results()

    def run_rust_server_suite(
        self,
        paths: dict[str, Path],
        normal_cases: list[dict[str, Any]],
    ) -> list[dict[str, Any]]:
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
            socket_dir = short_temp_dir("fprg-fastpg-")
            socket_path = socket_dir / f".s.PGSQL.{port}"
            host = str(socket_dir)
            listen_address = f"unix:{socket_path}"
        env = rust_server_pgbench_env(paths["client_bindir"], paths["client_libdir"])
        server: dict[str, Any] | None = None
        run_record: dict[str, Any] = {
            "host": host,
            "port": port,
            "commands": {},
        }
        if socket_dir is not None:
            run_record["socket_dir"] = str(socket_dir)
        self.results["variants"]["fastpg"] = {
            "engine": "rust-server",
            "status": "running",
            "run": run_record,
            "cases": [],
        }
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
            fastpg_cases = self.run_cases("fastpg", paths["client_bindir"], host, port, env, run_dir)
            self.compare_fastpg_cases(fastpg_cases, normal_cases)
            self.results["variants"]["fastpg"]["cases"] = fastpg_cases
            return fastpg_cases
        finally:
            active_failure = sys.exc_info()[0] is not None
            if server is not None:
                stopped = self.helper.stop_rust_server(server, run_dir)
                run_record["commands"]["stop"] = stopped.as_json()
                if stopped.returncode != 0 and not active_failure:
                    raise RegressionFailure("fastpg", "stop", stopped, run_dir)
            if socket_dir is not None:
                shutil.rmtree(socket_dir, ignore_errors=True)
            self.write_results()

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
        for case in self.cases:
            print(f"[{variant}] regression case {case.name}")
            output_dir = run_dir / "cases" / case.name
            result = self.helper.command(
                psql_case_command(bindir, host, port, case.path),
                output_dir,
                "psql",
                env=env,
            )
            case_result = {
                "case": case.name,
                "path": str(case.path),
                "output_dir": str(output_dir),
                "status": "ok" if result.returncode == 0 else "failed",
                "returncode": result.returncode,
                "seconds": result.seconds,
                "command": result.command,
                "stdout": normalize_output(result.stdout),
                "stderr": normalize_output(result.stderr),
            }
            case_results.append(case_result)
            self.results["variants"][variant]["cases"] = case_results
            self.write_results()
            if result.returncode != 0 and (variant == "normal" or not self.args.allow_fastpg_failures):
                raise RegressionFailure(variant, "psql-case", result, output_dir, case=case.name)
        return case_results

    def compare_fastpg_cases(
        self,
        fastpg_cases: list[dict[str, Any]],
        normal_cases: list[dict[str, Any]],
    ) -> None:
        normal_by_case = {case["case"]: case for case in normal_cases}
        for fastpg_case in fastpg_cases:
            normal_case = normal_by_case[fastpg_case["case"]]
            if fastpg_case["returncode"] != normal_case["returncode"]:
                fastpg_case["status"] = "failed"
                fastpg_case["difference"] = {
                    "kind": "returncode",
                    "normal": normal_case["returncode"],
                    "fastpg": fastpg_case["returncode"],
                }
                if not self.args.allow_fastpg_failures:
                    raise RegressionFailure(
                        "fastpg",
                        "psql-case",
                        command_result_from_case(fastpg_case),
                        Path(fastpg_case["output_dir"]),
                        case=fastpg_case["case"],
                    )
                continue
            if fastpg_case["stdout"] != normal_case["stdout"]:
                fastpg_case["status"] = "mismatch"
                fastpg_case["difference"] = {
                    "kind": "stdout",
                    "normal_tail": tail(normal_case["stdout"]),
                    "fastpg_tail": tail(fastpg_case["stdout"]),
                }
                if not self.args.allow_fastpg_failures:
                    raise RegressionFailure(
                        "fastpg",
                        "compare",
                        command_result_from_case(fastpg_case),
                        Path(fastpg_case["output_dir"]),
                        case=fastpg_case["case"],
                        expected_stdout=normal_case["stdout"],
                        actual_stdout=fastpg_case["stdout"],
                    )

    def record_failure(self, failure: RegressionFailure) -> None:
        self.results["status"] = "failed"
        self.results["failure"] = failure_as_json(failure)
        self.write_results()
        print_failure(failure, self.result_root)

    def write_results(self) -> None:
        (self.result_root / "summary.json").write_text(
            json.dumps(self.results, indent=2, sort_keys=True) + "\n"
        )
        (self.result_root / "summary.md").write_text(render_markdown(self.results, self.result_root))

    def print_success(self) -> None:
        print(f"status: {self.results['status']}")
        print(f"cases: {len(self.cases)}")
        print(f"summary: {self.result_root / 'summary.md'}")


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
        fastpg_no_internal_ipc=args.fastpg_no_internal_ipc,
    )


def psql_case_command(bindir: Path, host: str, port: int, case_path: Path) -> list[str]:
    return [
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
        "-v",
        "ON_ERROR_STOP=1",
        "-qAt",
        "-F",
        "\t",
        "-f",
        str(case_path),
    ]


def load_cases(bench_root: Path, args: argparse.Namespace) -> list[RegressionCase]:
    root = cases_dir(bench_root, args)
    if not root.is_dir():
        raise SystemExit(f"regression cases directory does not exist: {root}")
    selected = set(args.case)
    cases = []
    for path in sorted(root.glob("*.sql")):
        name = path.stem
        if selected and name not in selected:
            continue
        cases.append(RegressionCase(name, path))
    if not cases:
        raise SystemExit(f"no regression cases selected under {root}")
    return cases


def cases_dir(bench_root: Path, args: argparse.Namespace) -> Path:
    if args.cases_dir is not None:
        return Path(args.cases_dir).resolve()
    return bench_root / "regression" / args.suite


def short_temp_dir(prefix: str) -> Path:
    base = Path("/private/tmp") if platform.system() == "Darwin" and Path("/private/tmp").is_dir() else Path(tempfile.gettempdir())
    return Path(tempfile.mkdtemp(prefix=prefix, dir=base))


def normalize_output(output: str) -> str:
    return output.replace("\r\n", "\n")


def tail(output: str, lines: int = 40) -> str:
    return "\n".join(output.splitlines()[-lines:])


def command_result_from_case(case_result: dict[str, Any]) -> CommandResult:
    return CommandResult(
        command=case_result["command"],
        cwd=str(Path(__file__).resolve().parents[1]),
        returncode=int(case_result["returncode"]),
        stdout=case_result["stdout"],
        stderr=case_result["stderr"],
        seconds=float(case_result["seconds"]),
    )


def failure_as_json(failure: RegressionFailure) -> dict[str, Any]:
    data = {
        "variant": failure.variant,
        "phase": failure.phase,
        "case": failure.case,
        "classification": classify_failure(failure.result),
        "exit_code": failure.result.returncode,
        "command": failure.result.command,
        "stdout_tail": tail(failure.result.stdout),
        "stderr_tail": tail(failure.result.stderr),
        "output_dir": str(failure.output_dir),
    }
    if failure.expected_stdout is not None:
        data["expected_stdout_tail"] = tail(failure.expected_stdout)
    if failure.actual_stdout is not None:
        data["actual_stdout_tail"] = tail(failure.actual_stdout)
    return data


def print_failure(failure: RegressionFailure, result_root: Path) -> None:
    data = failure_as_json(failure)
    print(f"phase: {data['phase']}", file=sys.stderr)
    if data["classification"] is not None:
        print(f"classification: {data['classification']}", file=sys.stderr)
    if data["case"] is not None:
        print(f"case: {data['case']}", file=sys.stderr)
    print(f"command: {data['command']}", file=sys.stderr)
    print(f"exit code: {data['exit_code']}", file=sys.stderr)
    print(f"stdout tail:\n{data['stdout_tail']}", file=sys.stderr)
    print(f"stderr tail:\n{data['stderr_tail']}", file=sys.stderr)
    if "expected_stdout_tail" in data:
        print(f"expected stdout tail:\n{data['expected_stdout_tail']}", file=sys.stderr)
    if "actual_stdout_tail" in data:
        print(f"actual stdout tail:\n{data['actual_stdout_tail']}", file=sys.stderr)
    print(f"results: {result_root}", file=sys.stderr)


def render_markdown(results: dict[str, Any], result_root: Path) -> str:
    lines = [
        "# SQL regression comparison",
        "",
        f"Status: `{results['status']}`",
        "",
        "## Config",
        "",
    ]
    for key, value in results["config"].items():
        lines.append(f"- `{key}`: `{value}`")
    lines.extend(["", "## Cases", ""])
    lines.append("| case | normal | fastpg |")
    lines.append("| --- | --- | --- |")
    normal = cases_by_name(results, "normal")
    fastpg = cases_by_name(results, "fastpg")
    for case in results.get("cases", []):
        name = case["name"]
        normal_status = case_status(normal.get(name))
        fastpg_status = case_status(fastpg.get(name))
        lines.append(f"| `{name}` | {normal_status} | {fastpg_status} |")

    if results.get("failure"):
        failure = results["failure"]
        lines.extend(
            [
                "",
                "## Failure",
                "",
                f"- variant: `{failure['variant']}`",
                f"- phase: `{failure['phase']}`",
                f"- classification: `{failure.get('classification')}`",
                f"- case: `{failure.get('case')}`",
                f"- exit code: `{failure['exit_code']}`",
                f"- command: `{failure['command']}`",
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
                "",
            ]
        )
        if "expected_stdout_tail" in failure:
            lines.extend(
                [
                    "### expected stdout tail",
                    "",
                    "```text",
                    failure["expected_stdout_tail"],
                    "```",
                    "",
                    "### actual stdout tail",
                    "",
                    "```text",
                    failure["actual_stdout_tail"],
                    "```",
                    "",
                ]
            )
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
    return f"`{status}`"


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", default="core")
    parser.add_argument("--cases-dir")
    parser.add_argument("--case", action="append", default=[], help="case stem to run; repeatable")
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
        choices=["raw-parser", "full"],
        default="full",
    )
    parser.add_argument(
        "--allow-fastpg-failures",
        action="store_true",
        help="record fastpg failures and stdout mismatches without returning nonzero",
    )
    parser.add_argument(
        "--fastpg-no-internal-ipc",
        action="store_true",
        help="set FASTPG_NO_INTERNAL_IPC=1 for the fastpg Rust server process",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    return RegressionCompare(args).run()


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
