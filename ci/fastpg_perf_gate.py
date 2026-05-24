#!/usr/bin/env python3
"""Run and gate FastPG pgbench performance against a base checkout."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from statistics import median
from typing import Any


COMMENT_MARKER = "<!-- fastpg-performance-report -->"
DEFAULT_THRESHOLD_PERCENT = 5.0


@dataclass(frozen=True)
class BenchRun:
    label: str
    repo: Path
    summary_path: Path | None
    results: dict[str, Any] | None
    returncode: int
    log_path: Path | None


@dataclass(frozen=True)
class Metric:
    name: str
    unit: str
    base: float | int | None
    head: float | int | None
    better: str
    gated: bool = True

    @property
    def delta(self) -> float | None:
        if self.base is None or self.head is None:
            return None
        return float(self.head) - float(self.base)

    @property
    def delta_percent(self) -> float | None:
        if self.base in (None, 0) or self.head is None:
            return None
        return ((float(self.head) / float(self.base)) - 1.0) * 100.0

    def regressed(self, threshold_percent: float) -> bool:
        if not self.gated:
            return False
        delta_percent = self.delta_percent
        if delta_percent is None:
            return True
        if self.better == "higher":
            return delta_percent < -threshold_percent
        if self.better == "lower":
            return delta_percent > threshold_percent
        raise ValueError(f"unknown metric direction: {self.better}")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-repo", type=Path, required=True)
    parser.add_argument("--head-repo", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--base-label", default="main")
    parser.add_argument("--head-label", default="PR")
    parser.add_argument("--base-sha", default="")
    parser.add_argument("--head-sha", default="")
    parser.add_argument("--threshold-percent", type=float, default=DEFAULT_THRESHOLD_PERCENT)
    parser.add_argument("--scale", type=int, default=1)
    parser.add_argument("--transactions", type=int, default=20)
    parser.add_argument("--clients", type=int, default=1)
    parser.add_argument("--jobs", type=int, default=1)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--protocol", choices=["simple", "extended", "prepared"], default="simple")
    parser.add_argument("--builtin", default="simple-update")
    parser.add_argument("--init-steps", default="dtgp")
    parser.add_argument("--storage-engine", choices=["storage1", "storage2"], default="storage2")
    parser.add_argument("--catalog-mode", choices=["rust", "postgres"], default="postgres")
    parser.add_argument(
        "--fastpg-catalog-pgdata-mode",
        choices=["initdb", "seed-copy", "seed-skeleton"],
        default="seed-skeleton",
    )
    parser.add_argument("--fastpg-postgres-smgr", choices=["md", "mem"], default="mem")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    args.output_dir.mkdir(parents=True, exist_ok=True)

    base = run_benchmark("base", args.base_label, args.base_repo, args, args.output_dir)
    head = run_benchmark("head", args.head_label, args.head_repo, args, args.output_dir)

    report, gate = build_report(args, base, head)
    (args.output_dir / "report.md").write_text(report)
    (args.output_dir / "gate.json").write_text(json.dumps(gate, indent=2, sort_keys=True) + "\n")

    if base.returncode != 0 or head.returncode != 0:
        return 1
    return 0 if gate["status"] == "pass" else 1


def run_benchmark(
    prefix: str,
    label: str,
    repo: Path,
    args: argparse.Namespace,
    output_dir: Path,
) -> BenchRun:
    repo = repo.resolve()
    bench_dir = repo / "benches"
    script = bench_dir / "pgbench_compare.py"
    log_path = output_dir / f"{prefix}-pgbench.log"
    before = summary_paths(repo)
    command = [
        sys.executable,
        str(script.name),
        "--builtin",
        args.builtin,
        "--init-steps",
        args.init_steps,
        "--scale",
        str(args.scale),
        "--transactions",
        str(args.transactions),
        "--clients",
        str(args.clients),
        "--jobs",
        str(args.jobs),
        "--runs",
        str(args.runs),
        "--protocol",
        args.protocol,
        "--meson-buildtype",
        "release",
        "--rust-build-profile",
        "release",
        "--storage-engine",
        args.storage_engine,
        "--catalog-mode",
        args.catalog_mode,
        "--fastpg-catalog-pgdata-mode",
        args.fastpg_catalog_pgdata_mode,
        "--fastpg-postgres-smgr",
        args.fastpg_postgres_smgr,
        "--profile-server-memory",
    ]
    started = time.monotonic()
    print(f"::group::{label} FastPG pgbench")
    print(shlex_join(command))
    returncode = run_streaming(command, bench_dir, log_path)
    print(f"{label} benchmark seconds: {time.monotonic() - started:.1f}")
    print("::endgroup::")

    summary_path = newest_summary(repo, before)
    results = read_json(summary_path) if summary_path is not None else None
    copy_summary_artifacts(prefix, summary_path, output_dir)
    return BenchRun(label, repo, summary_path, results, returncode, log_path)


def run_streaming(command: list[str], cwd: Path, log_path: Path) -> int:
    env = os.environ.copy()
    env.setdefault("CARGO_TERM_COLOR", "always")
    with log_path.open("w") as log_file:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        assert process.stdout is not None
        for line in process.stdout:
            sys.stdout.write(line)
            log_file.write(line)
        return process.wait()


def summary_paths(repo: Path) -> set[Path]:
    root = repo / "benches" / "results" / "pgbench"
    return {path.resolve() for path in root.glob("*/summary.json")}


def newest_summary(repo: Path, before: set[Path]) -> Path | None:
    summaries = summary_paths(repo)
    new_summaries = summaries - before
    candidates = new_summaries or summaries
    if not candidates:
        return None
    return max(candidates, key=lambda path: path.stat().st_mtime)


def copy_summary_artifacts(prefix: str, summary_path: Path | None, output_dir: Path) -> None:
    if summary_path is None:
        return
    shutil.copy2(summary_path, output_dir / f"{prefix}-summary.json")
    markdown = summary_path.with_name("summary.md")
    if markdown.exists():
        shutil.copy2(markdown, output_dir / f"{prefix}-summary.md")


def build_report(
    args: argparse.Namespace,
    base: BenchRun,
    head: BenchRun,
) -> tuple[str, dict[str, Any]]:
    metrics = comparison_metrics(base.results, head.results)
    failures = benchmark_failures(base, head)
    regressed = [metric for metric in metrics if metric.regressed(args.threshold_percent)]
    status = "fail" if failures or regressed else "pass"
    gate = {
        "status": status,
        "threshold_percent": args.threshold_percent,
        "base_label": args.base_label,
        "head_label": args.head_label,
        "base_sha": args.base_sha,
        "head_sha": args.head_sha,
        "failures": failures,
        "metrics": [metric_as_json(metric, args.threshold_percent) for metric in metrics],
    }
    return render_markdown_report(args, base, head, metrics, failures, regressed, status), gate


def comparison_metrics(
    base_results: dict[str, Any] | None,
    head_results: dict[str, Any] | None,
) -> list[Metric]:
    return [
        Metric(
            "FastPG median TPS",
            "float",
            fastpg_summary_value(base_results, "median_tps"),
            fastpg_summary_value(head_results, "median_tps"),
            "higher",
        ),
        Metric(
            "FastPG median average latency",
            "milliseconds",
            fastpg_summary_value(base_results, "median_latency_average_ms"),
            fastpg_summary_value(head_results, "median_latency_average_ms"),
            "lower",
        ),
        Metric(
            "FastPG pgbench command wall time",
            "seconds",
            median_run_value(base_results, "fastpg", ("commands", "pgbench_run", "seconds")),
            median_run_value(head_results, "fastpg", ("commands", "pgbench_run", "seconds")),
            "lower",
            gated=False,
        ),
        Metric(
            "FastPG catalog PGDATA prepare",
            "seconds",
            fastpg_summary_value(base_results, "median_catalog_pgdata_prepare_seconds"),
            fastpg_summary_value(head_results, "median_catalog_pgdata_prepare_seconds"),
            "lower",
            gated=False,
        ),
        Metric(
            "FastPG max RSS during pgbench",
            "bytes",
            median_run_value(base_results, "fastpg", ("memory", "pgbench_run", "max_rss_bytes")),
            median_run_value(head_results, "fastpg", ("memory", "pgbench_run", "max_rss_bytes")),
            "lower",
            gated=False,
        ),
        Metric(
            "FastPG/native median TPS ratio",
            "ratio",
            fastpg_native_tps_ratio(base_results),
            fastpg_native_tps_ratio(head_results),
            "higher",
            gated=False,
        ),
    ]


def benchmark_failures(base: BenchRun, head: BenchRun) -> list[dict[str, Any]]:
    failures = []
    for run in (base, head):
        if run.returncode != 0:
            failures.append(
                {
                    "label": run.label,
                    "returncode": run.returncode,
                    "summary_path": str(run.summary_path) if run.summary_path else None,
                    "log_tail": tail_text(run.log_path),
                }
            )
        elif run.results is None:
            failures.append(
                {
                    "label": run.label,
                    "returncode": run.returncode,
                    "summary_path": None,
                    "log_tail": tail_text(run.log_path),
                }
            )
    return failures


def metric_as_json(metric: Metric, threshold_percent: float) -> dict[str, Any]:
    return {
        "name": metric.name,
        "unit": metric.unit,
        "base": metric.base,
        "head": metric.head,
        "delta": metric.delta,
        "delta_percent": metric.delta_percent,
        "better": metric.better,
        "gated": metric.gated,
        "regressed": metric.regressed(threshold_percent),
    }


def render_markdown_report(
    args: argparse.Namespace,
    base: BenchRun,
    head: BenchRun,
    metrics: list[Metric],
    failures: list[dict[str, Any]],
    regressed: list[Metric],
    status: str,
) -> str:
    status_label = "PASS" if status == "pass" else "FAIL"
    status_emoji = "🚀" if status == "pass" else "🐢"
    base_ref = format_ref(args.base_label, args.base_sha)
    head_ref = format_ref(args.head_label, args.head_sha)
    lines = [
        COMMENT_MARKER,
        f"## {status_emoji} FastPG Performance Gate: {status_label}",
        "",
        (
            f"Compared {head_ref} against {base_ref} with indexed pgbench "
            f"`{args.builtin}` (`init_steps={args.init_steps}`, `scale={args.scale}`, "
            f"`transactions={args.transactions}`, `runs={args.runs}`, "
            f"`storage_engine={args.storage_engine}`)."
        ),
        "",
        (
            f"Gate threshold: fail when a gated FastPG metric regresses by more than "
            f"`{args.threshold_percent:.1f}%` relative to `{args.base_label}`."
        ),
        "",
        "| Metric | Base | PR | Delta | Gate |",
        "| --- | ---: | ---: | ---: | --- |",
    ]
    for metric in metrics:
        gate = format_gate(metric, args.threshold_percent)
        lines.append(
            f"| {metric.name} | {format_metric(metric.base, metric.unit)} | "
            f"{format_metric(metric.head, metric.unit)} | "
            f"{format_delta(metric)} | {gate} |"
        )

    lines.extend(
        [
            "",
            "### Architectural Areas",
            "",
            "| Area | CI signal | Base | PR | Delta |",
            "| --- | --- | ---: | ---: | ---: |",
        ]
    )
    for area, signal, metric_name in architecture_rows():
        metric = next((candidate for candidate in metrics if candidate.name == metric_name), None)
        if metric is None:
            continue
        lines.append(
            f"| {area} | {signal} | {format_metric(metric.base, metric.unit)} | "
            f"{format_metric(metric.head, metric.unit)} | {format_delta(metric)} |"
        )

    if failures:
        lines.extend(["", "### Benchmark Failure", ""])
        for failure in failures:
            lines.append(
                f"- 🐢 `{failure['label']}` exited with `{failure['returncode']}`; "
                f"summary: `{failure['summary_path']}`"
            )
            if failure.get("log_tail"):
                lines.extend(["", "```text", failure["log_tail"], "```", ""])
    elif regressed:
        lines.extend(["", "### Gate Breaches", ""])
        for metric in regressed:
            lines.append(
                f"- 🐢 {metric.name}: {format_delta(metric)} "
                f"(allowed: +/- {args.threshold_percent:.1f}% in the worse direction)"
            )
    else:
        lines.extend(["", "🚀 No gated FastPG metric exceeded the regression threshold."])

    lines.extend(
        [
            "",
            "Artifacts include the raw base/head pgbench summaries and full benchmark logs.",
            "",
        ]
    )
    return "\n".join(lines)


def architecture_rows() -> list[tuple[str, str, str]]:
    return [
        ("Overall workload", "FastPG indexed workload throughput", "FastPG median TPS"),
        (
            "Socket / protocol + Query dispatch",
            "pgbench command wall time around the FastPG server",
            "FastPG pgbench command wall time",
        ),
        (
            "Parsing / analysis, planning / rewrite, execution",
            "pgbench average transaction latency",
            "FastPG median average latency",
        ),
        ("Storage / index AM", "indexed simple-update primary-key path", "FastPG median TPS"),
        ("Catalog / metadata", "catalog PGDATA prepare time", "FastPG catalog PGDATA prepare"),
        (
            "Transactions / WAL",
            "per-transaction UPDATE path included in TPS and latency",
            "FastPG median average latency",
        ),
        ("Runtime / memory", "FastPG process RSS sampled during pgbench", "FastPG max RSS during pgbench"),
    ]


def fastpg_summary_value(results: dict[str, Any] | None, key: str) -> float | None:
    if results is None:
        return None
    value = (
        results.get("variants", {})
        .get("fastpg", {})
        .get("summary", {})
        .get(key)
    )
    return float(value) if value is not None else None


def fastpg_native_tps_ratio(results: dict[str, Any] | None) -> float | None:
    if results is None:
        return None
    variants = results.get("variants", {})
    fastpg = variants.get("fastpg", {}).get("summary", {}).get("median_tps")
    normal = variants.get("normal", {}).get("summary", {}).get("median_tps")
    if fastpg is None or normal in (None, 0):
        return None
    return float(fastpg) / float(normal)


def median_run_value(
    results: dict[str, Any] | None,
    variant: str,
    path: tuple[str, ...],
) -> float | None:
    if results is None:
        return None
    values = []
    for run in results.get("variants", {}).get(variant, {}).get("runs", []):
        value: Any = run
        for key in path:
            if not isinstance(value, dict):
                value = None
                break
            value = value.get(key)
        if value is not None:
            values.append(float(value))
    return median(values) if values else None


def format_ref(label: str, sha: str) -> str:
    if not sha:
        return f"`{label}`"
    return f"`{label}` (`{sha[:12]}`)"


def format_metric(value: float | int | None, unit: str) -> str:
    if value is None:
        return "n/a"
    if unit == "bytes":
        return format_bytes(float(value))
    if unit == "seconds":
        return f"{float(value):.3f} s"
    if unit == "milliseconds":
        return f"{float(value):.3f} ms"
    if unit == "ratio":
        return f"{float(value):.3f}x"
    return f"{float(value):.3f}"


def format_delta(metric: Metric) -> str:
    delta = metric.delta
    delta_percent = metric.delta_percent
    if delta is None or delta_percent is None:
        return "n/a"
    sign = "+" if delta > 0 else ""
    percent_sign = "+" if delta_percent > 0 else ""
    return f"{sign}{format_metric(delta, metric.unit)} ({percent_sign}{delta_percent:.2f}%)"


def format_gate(metric: Metric, threshold_percent: float) -> str:
    if not metric.gated:
        return "⚪ report only"
    if metric.delta_percent is None:
        return "⚪ missing"
    return "🐢 fail" if metric.regressed(threshold_percent) else "🚀 pass"


def format_bytes(value: float) -> str:
    units = ["B", "KiB", "MiB", "GiB"]
    current = value
    for unit in units:
        if abs(current) < 1024 or unit == units[-1]:
            return f"{current:.1f} {unit}"
        current /= 1024
    return f"{value:.1f} B"


def read_json(path: Path | None) -> dict[str, Any] | None:
    if path is None:
        return None
    return json.loads(path.read_text())


def tail_text(path: Path | None, max_lines: int = 80) -> str:
    if path is None or not path.exists():
        return ""
    lines = path.read_text(errors="replace").splitlines()
    return "\n".join(lines[-max_lines:])


def shlex_join(command: list[str]) -> str:
    try:
        import shlex

        return shlex.join(command)
    except AttributeError:
        return " ".join(command)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
