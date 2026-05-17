#!/usr/bin/env python3
"""Open the newest saved fastpg Rust-server flamegraph."""

from __future__ import annotations

import argparse
import os
import platform
import subprocess
import sys
from pathlib import Path


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--print-only",
        action="store_true",
        help="print the newest flamegraph path without opening it",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    bench_root = Path(__file__).resolve().parent
    candidates = sorted(
        (bench_root / "results" / "pgbench").glob("*/fastpg/run-*/profile/fastpg-server-flamegraph.svg"),
        key=lambda path: path.stat().st_mtime,
        reverse=True,
    )
    if not candidates:
        print("no fastpg Rust-server flamegraph found under benches/results/pgbench", file=sys.stderr)
        return 1

    newest = candidates[0]
    print(newest)
    if args.print_only:
        return 0

    opener = opener_command()
    if opener is None:
        print("no supported opener found; use the printed path directly", file=sys.stderr)
        return 1

    subprocess.run([opener, str(newest)], check=False)
    return 0


def opener_command() -> str | None:
    if platform.system() == "Darwin":
        return "open"
    for candidate in ("xdg-open", "gio"):
        for path_dir in os.environ.get("PATH", "").split(os.pathsep):
            path = Path(path_dir) / candidate
            if path.exists():
                return candidate
    return None


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
