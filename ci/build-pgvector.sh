#!/usr/bin/env bash
set -euo pipefail

usage() {
	printf 'usage: %s INSTALL_PREFIX SOURCE_DIR [VERSION]\n' "$0" >&2
}

if [[ $# -lt 2 || $# -gt 3 ]]; then
	usage
	exit 2
fi

INSTALL_PREFIX="$1"
SOURCE_DIR="$2"
PGVECTOR_VERSION="${3:-${PGVECTOR_VERSION:-v0.8.2}}"
PGVECTOR_REPO="${PGVECTOR_REPO:-https://github.com/pgvector/pgvector.git}"

apply_pgvector_compatibility_patches() {
	# pgvector v0.8.2 has PG19 conditionals, but its HNSW and IVFFlat sources
	# still rely on headers and tableam call shapes that changed for PG19.
	# These compatibility edits match upstream pgvector main.
	python3 - "$SOURCE_DIR" <<'PY'
from pathlib import Path
import sys

root = Path(sys.argv[1])


def replace_once(relative_path, old, new, marker):
	path = root / relative_path
	text = path.read_text()
	if marker in text:
		return
	if old not in text:
		raise SystemExit(f"could not apply pgvector compatibility patch to {relative_path}")
	path.write_text(text.replace(old, new, 1))


port_include = '#include "port.h"\t\t\t\t/* for random() */\n'
replace_once(
	"src/hnsw.h",
	port_include + '#include "utils/relptr.h"\n',
	port_include
	+ '#include "storage/bufpage.h"\n'
	+ '#include "storage/condition_variable.h"\n'
	+ '#include "storage/lwlock.h"\n'
	+ '#include "storage/s_lock.h"\n'
	+ '#include "utils/relptr.h"\n',
	'storage/lwlock.h',
)
replace_once(
	"src/hnsw.c",
	'#include "nodes/pg_list.h"\n#include "utils/float.h"\n',
	'#include "nodes/pg_list.h"\n#include "storage/lwlock.h"\n#include "utils/float.h"\n',
	'storage/lwlock.h',
)
replace_once(
	"src/hnswbuild.c",
	'\tscan = table_beginscan_parallel(heapRel,\n'
	+ '\t\t\t\t\t\t\t\t\tParallelTableScanFromHnswShared(hnswshared));\n',
	'#if PG_VERSION_NUM >= 190000\n'
	+ '\tscan = table_beginscan_parallel(heapRel,\n'
	+ '\t\t\t\t\t\t\t\t\tParallelTableScanFromHnswShared(hnswshared),\n'
	+ '\t\t\t\t\t\t\t\t\tSO_NONE);\n'
	+ '#else\n'
	+ '\tscan = table_beginscan_parallel(heapRel,\n'
	+ '\t\t\t\t\t\t\t\t\tParallelTableScanFromHnswShared(hnswshared));\n'
	+ '#endif\n',
	'SO_NONE',
)
replace_once(
	"src/ivfflat.h",
	port_include + '#include "utils/sampling.h"\n',
	port_include + '#include "storage/condition_variable.h"\n#include "utils/sampling.h"\n',
	'storage/condition_variable.h',
)
replace_once(
	"src/ivfbuild.c",
	'#include "storage/bufmgr.h"\n#include "tcop/tcopprot.h"\n',
	'#include "storage/bufmgr.h"\n#include "storage/condition_variable.h"\n#include "tcop/tcopprot.h"\n',
	'storage/condition_variable.h',
)
replace_once(
	"src/ivfbuild.c",
	'\tscan = table_beginscan_parallel(ivfspool->heap,\n'
	+ '\t\t\t\t\t\t\t\t\tParallelTableScanFromIvfflatShared(ivfshared));\n',
	'#if PG_VERSION_NUM >= 190000\n'
	+ '\tscan = table_beginscan_parallel(ivfspool->heap,\n'
	+ '\t\t\t\t\t\t\t\t\tParallelTableScanFromIvfflatShared(ivfshared),\n'
	+ '\t\t\t\t\t\t\t\t\tSO_NONE);\n'
	+ '#else\n'
	+ '\tscan = table_beginscan_parallel(ivfspool->heap,\n'
	+ '\t\t\t\t\t\t\t\t\tParallelTableScanFromIvfflatShared(ivfshared));\n'
	+ '#endif\n',
	'SO_NONE',
)
PY
}

if [[ ! -d "$INSTALL_PREFIX" ]]; then
	printf 'install prefix does not exist: %s\n' "$INSTALL_PREFIX" >&2
	exit 1
fi

INSTALL_PREFIX="$(cd "$INSTALL_PREFIX" && pwd)"
PG_CONFIG="$INSTALL_PREFIX/bin/pg_config"

if [[ ! -x "$PG_CONFIG" ]]; then
	printf 'pg_config does not exist or is not executable: %s\n' "$PG_CONFIG" >&2
	exit 1
fi

rm -rf "$SOURCE_DIR"
git clone --depth 1 --branch "$PGVECTOR_VERSION" "$PGVECTOR_REPO" "$SOURCE_DIR"
apply_pgvector_compatibility_patches

# pgvector defaults to -march=native on some platforms. Release archives should
# run on machines beyond the GitHub runner CPU, so build the extension portably.
make -C "$SOURCE_DIR" PG_CONFIG="$PG_CONFIG" OPTFLAGS=""
make -C "$SOURCE_DIR" PG_CONFIG="$PG_CONFIG" OPTFLAGS="" install
