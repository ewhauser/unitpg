"""Helpers for fast-fork startup seed images."""

from __future__ import annotations

import os
import re
import shutil
from dataclasses import dataclass
from pathlib import Path


RELATION_STORAGE_RE = re.compile(r"^\d+(?:_(?:fsm|vm|init))?(?:\.\d+)?$")
WAL_SEGMENT_RE = re.compile(r"^[0-9A-F]{24}(?:\.[A-Za-z0-9._-]+)?$")


@dataclass(frozen=True)
class RuntimeSkeletonStats:
    directories: int
    files: int
    bytes: int

    def as_dict(self) -> dict[str, int]:
        return {
            "directories": self.directories,
            "files": self.files,
            "bytes": self.bytes,
        }


def _is_relation_storage_file(path: Path, seed_dir: Path) -> bool:
    rel = path.relative_to(seed_dir)
    parts = rel.parts
    if not parts:
        return False

    name = parts[-1]
    if parts[0] == "global" and len(parts) == 2:
        return bool(RELATION_STORAGE_RE.match(name))
    if parts[0] == "base" and len(parts) == 3:
        return bool(RELATION_STORAGE_RE.match(name))
    if parts[0] == "pg_tblspc" and len(parts) >= 5:
        return bool(RELATION_STORAGE_RE.match(name))
    return False


def _is_wal_payload_file(path: Path, seed_dir: Path) -> bool:
    rel = path.relative_to(seed_dir)
    parts = rel.parts
    if len(parts) == 2 and parts[0] == "pg_wal":
        return bool(WAL_SEGMENT_RE.match(parts[-1]))
    if len(parts) >= 2 and parts[0] == "pg_wal" and parts[1] == "summaries":
        return path.is_file()
    return False


def should_copy_runtime_file(path: Path, seed_dir: Path) -> bool:
    if path.name in {"postmaster.pid", "postmaster.opts", "current_logfiles"}:
        return False
    if _is_relation_storage_file(path, seed_dir):
        return False
    if _is_wal_payload_file(path, seed_dir):
        return False
    return True


def copy_runtime_skeleton(seed_dir: Path, runtime_dir: Path) -> RuntimeSkeletonStats:
    """Copy a PGDATA-shaped runtime skeleton without relation storage files."""

    seed_dir = seed_dir.resolve()
    if runtime_dir.exists():
        raise FileExistsError(runtime_dir)

    directory_count = 0
    file_count = 0
    byte_count = 0

    for root, dirs, files in os.walk(seed_dir):
        source_root = Path(root)
        rel_root = source_root.relative_to(seed_dir)
        target_root = runtime_dir / rel_root
        target_root.mkdir(parents=True, exist_ok=True)
        shutil.copystat(source_root, target_root, follow_symlinks=False)
        directory_count += 1

        for dirname in dirs:
            source_dir = source_root / dirname
            target_dir = target_root / dirname
            if source_dir.is_symlink():
                target_dir.symlink_to(os.readlink(source_dir))
                directory_count += 1

        for filename in files:
            source_file = source_root / filename
            if not should_copy_runtime_file(source_file, seed_dir):
                continue

            target_file = target_root / filename
            if source_file.is_symlink():
                target_file.symlink_to(os.readlink(source_file))
                file_count += 1
                continue

            shutil.copy2(source_file, target_file)
            file_count += 1
            byte_count += source_file.stat().st_size

    return RuntimeSkeletonStats(
        directories=directory_count,
        files=file_count,
        bytes=byte_count,
    )
