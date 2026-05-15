#!/usr/bin/env python3
"""Build two PostgreSQL variants and compare the rollback pgbench workload."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shutil
import statistics
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RUN_PGBENCH = ROOT / "bench" / "run_pgbench.py"
REQUIRED_BINARIES = ("initdb", "pg_ctl", "createdb", "pgbench", "postgres")

MESON_BASE_OPTIONS = [
    "-Dauto_features=disabled",
    "-Dssl=none",
    "-Dicu=disabled",
    "-Dreadline=disabled",
    "-Dzlib=disabled",
    "-Dlz4=disabled",
    "-Dzstd=disabled",
]

CONFIGURE_BASE_OPTIONS = [
    "--without-bonjour",
    "--without-gssapi",
    "--without-icu",
    "--without-ldap",
    "--without-libxml",
    "--without-libxslt",
    "--without-lz4",
    "--without-pam",
    "--without-readline",
    "--without-ssl",
    "--without-systemd",
    "--without-tcl",
    "--without-perl",
    "--without-python",
    "--without-zlib",
    "--without-zstd",
]


def run_logged(cmd: list[str], *, cwd: Path | None = None, log: Path) -> None:
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open("a", encoding="utf-8") as handle:
        handle.write("$ " + " ".join(cmd) + "\n")
        handle.flush()
        subprocess.run(
            cmd,
            cwd=cwd,
            stdout=handle,
            stderr=subprocess.STDOUT,
            text=True,
            check=True,
        )


def run_json(cmd: list[str], *, log: Path) -> dict[str, object]:
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open("a", encoding="utf-8") as handle:
        handle.write("$ " + " ".join(cmd) + "\n")
        handle.flush()
        proc = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        handle.write(proc.stdout)
        if proc.stderr:
            handle.write(proc.stderr)
        proc.check_returncode()
    return json.loads(proc.stdout)


def git_value(source: Path, args: list[str]) -> str | None:
    try:
        proc = subprocess.run(
            ["git", *args],
            cwd=source,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            check=True,
        )
    except subprocess.CalledProcessError:
        return None
    return proc.stdout.strip()


def install_is_usable(bin_dir: Path) -> bool:
    return all((bin_dir / binary).exists() for binary in REQUIRED_BINARIES)


def log_reused_install(*, bin_dir: Path, log: Path) -> None:
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open("a", encoding="utf-8") as handle:
        handle.write(f"Reusing existing install at {bin_dir}\n")


def build_with_meson(
    *,
    source: Path,
    build_dir: Path,
    prefix: Path,
    fake_wal: bool,
    no_bg_jobs: bool,
    mem_smgr: bool,
    jobs: int,
    reuse: bool,
    skip_if_installed: bool,
    log: Path,
) -> Path:
    bin_dir = prefix / "bin"
    if skip_if_installed and install_is_usable(bin_dir):
        log_reused_install(bin_dir=bin_dir, log=log)
        return bin_dir

    if not reuse:
        shutil.rmtree(build_dir, ignore_errors=True)
        shutil.rmtree(prefix, ignore_errors=True)

    setup_cmd = [
        "meson",
        "setup",
        str(build_dir),
        str(source),
        f"--prefix={prefix}",
        *MESON_BASE_OPTIONS,
        f"-Dtest_fake_wal={'true' if fake_wal else 'false'}",
        f"-Dtest_no_bg_jobs={'true' if no_bg_jobs else 'false'}",
        f"-Dtest_mem_smgr={'true' if mem_smgr else 'false'}",
    ]
    if reuse and (build_dir / "build.ninja").exists():
        setup_cmd.insert(2, "--reconfigure")

    run_logged(setup_cmd, log=log)
    run_logged(["meson", "compile", "-C", str(build_dir), "-j", str(jobs)], log=log)
    run_logged(["meson", "install", "-C", str(build_dir)], log=log)
    return bin_dir


def build_with_configure(
    *,
    source: Path,
    build_dir: Path,
    prefix: Path,
    fake_wal: bool,
    no_bg_jobs: bool,
    mem_smgr: bool,
    jobs: int,
    reuse: bool,
    skip_if_installed: bool,
    log: Path,
) -> Path:
    bin_dir = prefix / "bin"
    if skip_if_installed and install_is_usable(bin_dir):
        log_reused_install(bin_dir=bin_dir, log=log)
        return bin_dir

    if not reuse:
        shutil.rmtree(build_dir, ignore_errors=True)
        shutil.rmtree(prefix, ignore_errors=True)

    build_dir.mkdir(parents=True, exist_ok=True)
    configure = source / "configure"
    configure_cmd = [
        str(configure),
        f"--prefix={prefix}",
        *CONFIGURE_BASE_OPTIONS,
    ]
    if fake_wal:
        configure_cmd.append("--enable-test-fake-wal")
    if no_bg_jobs:
        configure_cmd.append("--enable-test-no-bg-jobs")
    if mem_smgr:
        configure_cmd.append("--enable-test-mem-smgr")

    if not reuse or not (build_dir / "Makefile").exists():
        run_logged(configure_cmd, cwd=build_dir, log=log)

    run_logged(["make", "-j", str(jobs)], cwd=build_dir, log=log)
    run_logged(["make", "install"], cwd=build_dir, log=log)
    return bin_dir


def build_variant(
    *,
    build_system: str,
    source: Path,
    build_root: Path,
    output_dir: Path,
    label: str,
    fake_wal: bool,
    no_bg_jobs: bool,
    mem_smgr: bool,
    jobs: int,
    reuse: bool,
    skip_if_installed: bool,
) -> Path:
    build_dir = build_root / "builds" / f"{label}-{build_system}"
    prefix = build_root / "installs" / f"{label}-{build_system}"
    log = output_dir / "logs" / f"{label}-build.log"

    if build_system == "meson":
        return build_with_meson(
            source=source,
            build_dir=build_dir,
            prefix=prefix,
            fake_wal=fake_wal,
            no_bg_jobs=no_bg_jobs,
            mem_smgr=mem_smgr,
            jobs=jobs,
            reuse=reuse,
            skip_if_installed=skip_if_installed,
            log=log,
        )
    return build_with_configure(
        source=source,
        build_dir=build_dir,
        prefix=prefix,
        fake_wal=fake_wal,
        no_bg_jobs=no_bg_jobs,
        mem_smgr=mem_smgr,
        jobs=jobs,
        reuse=reuse,
        skip_if_installed=skip_if_installed,
        log=log,
    )


def pgbench_cmd(args: argparse.Namespace, bin_dir: Path, label: str, output: Path) -> list[str]:
    cmd = [
        sys.executable,
        "-B",
        str(RUN_PGBENCH),
        "--bin",
        str(bin_dir),
        "--label",
        label,
        "--output",
        str(output),
        "--clients",
        str(args.clients),
        "--jobs",
        str(args.pgbench_jobs),
        "--transactions",
        str(args.transactions),
        "--rows",
        str(args.rows),
        "--warmup-transactions",
        str(args.warmup_transactions),
        "--random-seed",
        str(args.random_seed),
    ]
    for config in args.config:
        cmd.extend(["--config", config])
    return cmd


def summarize_variant(runs: list[dict[str, object]]) -> dict[str, object]:
    tps = [float(run["result"]["tps"]) for run in runs]
    latency = [float(run["result"]["latency_average_ms"]) for run in runs]
    return {
        "rounds": len(runs),
        "tps": {
            "min": min(tps),
            "max": max(tps),
            "mean": statistics.fmean(tps),
            "median": statistics.median(tps),
        },
        "latency_average_ms": {
            "min": min(latency),
            "max": max(latency),
            "mean": statistics.fmean(latency),
            "median": statistics.median(latency),
        },
    }


def write_summary_markdown(payload: dict[str, object], path: Path) -> None:
    variants = payload["variants"]
    comparison = payload["comparison"]
    lines = [
        "# pgbench Comparison",
        "",
        f"Generated: `{payload['generated_at']}`",
        f"Build system: `{payload['build_system']}`",
        "",
        "| Variant | Rounds | Median TPS | Mean TPS | Median latency (ms) |",
        "| --- | ---: | ---: | ---: | ---: |",
    ]
    for name in ("baseline", "fakewal"):
        summary = variants[name]["summary"]
        lines.append(
            "| {name} | {rounds} | {median_tps:.3f} | {mean_tps:.3f} | {median_latency:.3f} |".format(
                name=name,
                rounds=summary["rounds"],
                median_tps=summary["tps"]["median"],
                mean_tps=summary["tps"]["mean"],
                median_latency=summary["latency_average_ms"]["median"],
            )
        )
    lines.extend(
        [
            "",
            "Fake-WAL median TPS speedup: `{:.3f}x`".format(comparison["fakewal_vs_baseline_tps_median_ratio"]),
            "Fake-WAL median latency ratio: `{:.3f}x`".format(
                comparison["fakewal_vs_baseline_latency_median_ratio"]
            ),
            "",
            "Individual run JSON files are in `runs/`; build and pgbench logs are in `logs/`.",
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
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--clients", type=int, default=1)
    parser.add_argument("--pgbench-jobs", type=int, default=1)
    parser.add_argument("--transactions", type=int, default=100)
    parser.add_argument("--warmup-transactions", type=int, default=10)
    parser.add_argument("--rows", type=int, default=200)
    parser.add_argument("--random-seed", default="1")
    parser.add_argument(
        "--config",
        action="append",
        default=[],
        help="extra postgresql.conf line forwarded to bench/run_pgbench.py",
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
            jobs=args.build_jobs,
            reuse=args.reuse_builds,
            skip_if_installed=False,
        )

    runs: dict[str, list[dict[str, object]]] = {"baseline": [], "fakewal": []}
    for round_index in range(1, args.rounds + 1):
        for name in ("baseline", "fakewal"):
            run_path = output_dir / "runs" / f"{round_index:02d}-{name}.json"
            label = f"{name}-round-{round_index:02d}"
            cmd = pgbench_cmd(args, bins[name], label, run_path)
            result = run_json(cmd, log=output_dir / "logs" / f"{round_index:02d}-{name}-pgbench.log")
            result["round"] = round_index
            result["result_path"] = str(run_path)
            runs[name].append(result)

    baseline_summary = summarize_variant(runs["baseline"])
    fakewal_summary = summarize_variant(runs["fakewal"])
    comparison = {
        "fakewal_vs_baseline_tps_median_ratio": (
            fakewal_summary["tps"]["median"] / baseline_summary["tps"]["median"]
        ),
        "fakewal_vs_baseline_latency_median_ratio": (
            fakewal_summary["latency_average_ms"]["median"]
            / baseline_summary["latency_average_ms"]["median"]
        ),
    }

    payload: dict[str, object] = {
        "generated_at": dt.datetime.now(dt.UTC).isoformat(),
        "source": str(source),
        "git_head": git_value(source, ["rev-parse", "HEAD"]),
        "git_status_short": git_value(source, ["status", "--short"]),
        "build_system": args.build_system,
        "build_root": str(build_root),
        "parameters": {
            "rounds": args.rounds,
            "clients": args.clients,
            "pgbench_jobs": args.pgbench_jobs,
            "transactions": args.transactions,
            "warmup_transactions": args.warmup_transactions,
            "rows": args.rows,
            "random_seed": args.random_seed,
            "config": args.config,
        },
        "variants": {
            "baseline": {
                "fake_wal": False,
                "no_bg_jobs": False,
                "mem_smgr": False,
                "bin_dir": str(bins["baseline"]),
                "summary": baseline_summary,
                "runs": runs["baseline"],
            },
            "fakewal": {
                "fake_wal": True,
                "no_bg_jobs": not args.keep_bg_jobs,
                "mem_smgr": not args.disable_mem_smgr,
                "bin_dir": str(bins["fakewal"]),
                "summary": fakewal_summary,
                "runs": runs["fakewal"],
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
