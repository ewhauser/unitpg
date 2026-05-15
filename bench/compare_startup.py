#!/usr/bin/env python3
"""Build two PostgreSQL variants and compare startup benchmark timings."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import sys
from pathlib import Path

from compare_pgbench import build_variant, git_value, run_json


ROOT = Path(__file__).resolve().parents[1]
RUN_STARTUP = ROOT / "bench" / "run_startup.py"


def startup_cmd(args: argparse.Namespace, bin_dir: Path, label: str, output: Path) -> list[str]:
    cmd = [
        sys.executable,
        "-B",
        str(RUN_STARTUP),
        "--bin",
        str(bin_dir),
        "--label",
        label,
        "--output",
        str(output),
        "--rounds",
        str(args.rounds),
        "--mode",
        args.mode,
        "--stop-mode",
        args.stop_mode,
    ]
    for config in args.config:
        cmd.extend(["--config", config])
    return cmd


def ratio(numerator: float | None, denominator: float | None) -> float | None:
    if numerator is None or denominator in (None, 0):
        return None
    return numerator / denominator


def summary_value(payload: dict[str, object], metric: str, field: str) -> float | None:
    summary = payload["summary"]
    return summary[metric][field]


def write_summary_markdown(payload: dict[str, object], path: Path) -> None:
    variants = payload["variants"]
    comparison = payload["comparison"]
    lines = [
        "# Startup Comparison",
        "",
        f"Generated: `{payload['generated_at']}`",
        f"Build system: `{payload['build_system']}`",
        f"Mode: `{payload['mode']}`",
        f"Stop mode: `{payload['parameters']['stop_mode']}`",
        "",
        "| Variant | Rounds | Initdb (s) | Median launch (s) | Median start (s) | Mean start (s) | Median first query (s) | Median attempts | Median stop (s) |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for name in ("baseline", "fakewal"):
        run = variants[name]["run"]
        summary = run["summary"]
        lines.append(
            "| {name} | {rounds} | {initdb:.6f} | {median_launch:.6f} | {median_start:.6f} | {mean_start:.6f} | {median_query:.6f} | {median_attempts:.1f} | {median_stop:.6f} |".format(
                name=name,
                rounds=run["rounds"],
                initdb=run["initdb_seconds"],
                median_launch=summary["pg_ctl_launch_seconds"]["median"],
                median_start=summary["pg_ctl_start_seconds"]["median"],
                mean_start=summary["pg_ctl_start_seconds"]["mean"],
                median_query=summary["first_query_seconds"]["median"],
                median_attempts=summary["query_attempts"]["median"],
                median_stop=summary["pg_ctl_stop_seconds"]["median"],
            )
        )

    lines.extend(
        [
            "",
            "Fast-fork median start speedup: `{}`".format(
                "n/a"
                if comparison["fakewal_vs_baseline_start_median_ratio"] is None
                else f"{comparison['fakewal_vs_baseline_start_median_ratio']:.3f}x"
            ),
            "Fast-fork median first-query speedup: `{}`".format(
                "n/a"
                if comparison["fakewal_vs_baseline_first_query_median_ratio"] is None
                else f"{comparison['fakewal_vs_baseline_first_query_median_ratio']:.3f}x"
            ),
            "Fast-fork median stop speedup: `{}`".format(
                "n/a"
                if comparison["fakewal_vs_baseline_stop_median_ratio"] is None
                else f"{comparison['fakewal_vs_baseline_stop_median_ratio']:.3f}x"
            ),
            "",
            "Individual run JSON files are in `runs/`; build and startup logs are in `logs/`.",
            "",
        ]
    )
    path.write_text("\n".join(lines), encoding="utf-8")


def main() -> int:
    stamp = dt.datetime.now(dt.UTC).strftime("%Y%m%dT%H%M%SZ")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=ROOT, help="PostgreSQL source tree")
    parser.add_argument(
        "--build-system",
        choices=["meson", "configure"],
        default="meson",
        help="build system used for both variants",
    )
    parser.add_argument("--build-root", type=Path, default=ROOT / "bench" / ".build")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "bench" / "results" / stamp)
    parser.add_argument("--reuse-builds", action="store_true", help="reuse existing build directories")
    parser.add_argument(
        "--rebuild-baseline",
        action="store_true",
        help="force rebuilding the cached baseline install",
    )
    parser.add_argument("--build-jobs", type=int, default=os.cpu_count() or 1)
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--mode", choices=["reuse", "copy"], default="reuse")
    parser.add_argument(
        "--config",
        action="append",
        default=[],
        help="extra postgresql.conf line forwarded to bench/run_startup.py",
    )
    parser.add_argument(
        "--stop-mode",
        choices=["fast", "immediate"],
        default="fast",
        help="shutdown mode forwarded to bench/run_startup.py",
    )
    parser.add_argument("--baseline-bin", type=Path, help="use an existing baseline install bin directory")
    parser.add_argument("--fakewal-bin", type=Path, help="use an existing fake-WAL install bin directory")
    parser.add_argument(
        "--keep-bg-jobs",
        action="store_true",
        help="do not enable the no-background-jobs flag in the fake-WAL build",
    )
    parser.add_argument(
        "--disable-mem-smgr",
        action="store_true",
        help="do not enable the memory storage manager in the fast-fork build",
    )
    parser.add_argument(
        "--disable-ephemeral-buffers",
        action="store_true",
        help="do not use direct memory-backed temp buffers in the fast-fork build",
    )
    parser.add_argument(
        "--disable-mem-slru",
        action="store_true",
        help="do not enable in-memory transaction-status SLRUs in the fast-fork build",
    )
    parser.add_argument(
        "--disable-no-wal-assembly",
        action="store_true",
        help="do not skip ordinary WAL record assembly in the fast-fork build",
    )
    parser.add_argument(
        "--disable-no-observability",
        action="store_true",
        help="do not compile out hot-path statistics and wait reporting in the fast-fork build",
    )
    parser.add_argument(
        "--disable-fast-memory-contexts",
        action="store_true",
        help="do not use faster memory context choices in the fast-fork build",
    )
    parser.add_argument(
        "--disable-ephemeral-catalog",
        action="store_true",
        help="do not enable ephemeral catalog fast paths in the fast-fork build",
    )
    parser.add_argument(
        "--disable-no-durable-maintenance",
        action="store_true",
        help="do not disable durable maintenance work in the fast-fork build",
    )
    parser.add_argument(
        "--disable-fast-analyze",
        action="store_true",
        help="do not use the no-sample ANALYZE fast path in the fast-fork build",
    )
    parser.add_argument(
        "--disable-no-recovery-startup",
        action="store_true",
        help="do not skip crash recovery and WAL redo during startup in the fast-fork build",
    )
    args = parser.parse_args()

    if args.rounds < 1:
        raise SystemExit("--rounds must be at least 1")

    source = args.source.resolve()
    build_root = args.build_root.resolve()
    output_dir = args.output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "runs").mkdir(parents=True, exist_ok=True)
    (output_dir / "logs").mkdir(parents=True, exist_ok=True)

    bins: dict[str, Path] = {}
    if args.baseline_bin:
        bins["baseline"] = args.baseline_bin.resolve()
    else:
        bins["baseline"] = build_variant(
            build_system=args.build_system,
            source=source,
            build_root=build_root,
            output_dir=output_dir,
            label="baseline",
            fake_wal=False,
            no_bg_jobs=False,
            mem_smgr=False,
            ephemeral_buffers=False,
            mem_slru=False,
            no_wal_assembly=False,
            no_observability=False,
            fast_memory_contexts=False,
            ephemeral_catalog=False,
            no_durable_maintenance=False,
            fast_analyze=False,
            no_recovery_startup=False,
            jobs=args.build_jobs,
            reuse=args.reuse_builds or not args.rebuild_baseline,
            skip_if_installed=not args.rebuild_baseline,
        )

    if args.fakewal_bin:
        bins["fakewal"] = args.fakewal_bin.resolve()
    else:
        bins["fakewal"] = build_variant(
            build_system=args.build_system,
            source=source,
            build_root=build_root,
            output_dir=output_dir,
            label="fakewal",
            fake_wal=True,
            no_bg_jobs=not args.keep_bg_jobs,
            mem_smgr=not args.disable_mem_smgr,
            ephemeral_buffers=not args.disable_ephemeral_buffers,
            mem_slru=not args.disable_mem_slru,
            no_wal_assembly=not args.disable_no_wal_assembly,
            no_observability=not args.disable_no_observability,
            fast_memory_contexts=not args.disable_fast_memory_contexts,
            ephemeral_catalog=not args.disable_ephemeral_catalog,
            no_durable_maintenance=not args.disable_no_durable_maintenance,
            fast_analyze=not args.disable_fast_analyze,
            no_recovery_startup=not args.disable_no_recovery_startup,
            jobs=args.build_jobs,
            reuse=args.reuse_builds,
            skip_if_installed=False,
        )

    runs: dict[str, dict[str, object]] = {}
    for name in ("baseline", "fakewal"):
        run_path = output_dir / "runs" / f"{name}.json"
        label = f"{name}-startup"
        cmd = startup_cmd(args, bins[name], label, run_path)
        runs[name] = run_json(cmd, log=output_dir / "logs" / f"{name}-startup.log")
        runs[name]["result_path"] = str(run_path)

    baseline_start = summary_value(runs["baseline"], "pg_ctl_start_seconds", "median")
    fakewal_start = summary_value(runs["fakewal"], "pg_ctl_start_seconds", "median")
    baseline_first_query = summary_value(runs["baseline"], "first_query_seconds", "median")
    fakewal_first_query = summary_value(runs["fakewal"], "first_query_seconds", "median")
    baseline_stop = summary_value(runs["baseline"], "pg_ctl_stop_seconds", "median")
    fakewal_stop = summary_value(runs["fakewal"], "pg_ctl_stop_seconds", "median")

    comparison = {
        "fakewal_vs_baseline_start_median_ratio": ratio(baseline_start, fakewal_start),
        "fakewal_vs_baseline_first_query_median_ratio": ratio(
            baseline_first_query,
            fakewal_first_query,
        ),
        "fakewal_vs_baseline_stop_median_ratio": ratio(baseline_stop, fakewal_stop),
    }

    payload: dict[str, object] = {
        "generated_at": dt.datetime.now(dt.UTC).isoformat(),
        "source": str(source),
        "git_head": git_value(source, ["rev-parse", "HEAD"]),
        "git_status_short": git_value(source, ["-c", "core.fsmonitor=false", "status", "--short"]),
        "build_system": args.build_system,
        "build_root": str(build_root),
        "mode": args.mode,
        "parameters": {
            "rounds": args.rounds,
            "mode": args.mode,
            "stop_mode": args.stop_mode,
            "config": args.config,
        },
        "variants": {
            "baseline": {
                "bin_dir": str(bins["baseline"]),
                "run": runs["baseline"],
            },
            "fakewal": {
                "fake_wal": True,
                "no_bg_jobs": not args.keep_bg_jobs,
                "mem_smgr": not args.disable_mem_smgr,
                "ephemeral_buffers": not args.disable_ephemeral_buffers,
                "mem_slru": not args.disable_mem_slru,
                "no_wal_assembly": not args.disable_no_wal_assembly,
                "no_observability": not args.disable_no_observability,
                "fast_memory_contexts": not args.disable_fast_memory_contexts,
                "ephemeral_catalog": not args.disable_ephemeral_catalog,
                "no_durable_maintenance": not args.disable_no_durable_maintenance,
                "fast_analyze": not args.disable_fast_analyze,
                "no_recovery_startup": not args.disable_no_recovery_startup,
                "bin_dir": str(bins["fakewal"]),
                "run": runs["fakewal"],
            },
        },
        "comparison": comparison,
    }

    summary_json = output_dir / "summary.json"
    summary_md = output_dir / "summary.md"
    summary_json.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    write_summary_markdown(payload, summary_md)

    print(summary_md.read_text(encoding="utf-8"))
    print(f"Wrote {summary_json}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
