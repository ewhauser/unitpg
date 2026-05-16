#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MESON="${MESON:-meson}"
BUILD_DIR="${FASTFORK_BUILD_DIR:-$ROOT/bench/.build/fastfork-validation}"
MODE="core"
LIST_ONLY=0
SETUP_ONLY=0
WIPE=0
RECONFIGURE=1
JOBS="${FASTFORK_JOBS:-}"
TIMEOUT_MULTIPLIER="${FASTFORK_TIMEOUT_MULTIPLIER:-2}"
EXTRA_TEST_ARGS=()
TAP_AVAILABLE=0

usage() {
	cat <<EOF
Usage: ./test-fastfork.sh [quick|core|full] [options] [-- extra meson test args]

Build and validate the fast-fork Postgres configuration:
  -Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_mem_smgr=true
  -Dtest_ephemeral_buffers=true -Dtest_mem_slru=true
  -Dtest_epoch_rollback=true
  -Dtest_no_wal_assembly=true -Dtest_no_observability=true
  -Dtest_fast_memory_contexts=true -Dtest_ephemeral_catalog=true
  -Dtest_no_durable_maintenance=true -Dtest_fast_analyze=true
  -Dtest_no_recovery_startup=true -Dtest_seed_only_startup=true
  -Dtest_no_data_directory_startup=true
  -Dtest_macos_named_posix_semaphores=true on macOS
  -Dtest_no_sysv_shared_memory=true on macOS

Modes:
  quick       Fast compatible smoke tests plus async I/O tests when the local
              TAP dependencies are available.
  core        quick plus selected executor/parser/resource/index-adjacent tests.
  full        Every compatible Meson test except known unsupported durability/
              recovery/replication/backup/autovacuum/checksum/background-worker
              tests. This is slower.

Options:
  --build-dir DIR      Meson build dir. Default: bench/.build/fastfork-validation
  --jobs N            Parallel test jobs. Default: CPU count
  --list              Configure and print selected tests without running them
  --setup-only        Configure/build only; do not run tests
  --no-reconfigure    Reuse an existing build dir without reconfiguring options
  --wipe              Recreate the Meson build dir
  -h, --help          Show this help

Environment:
  FASTFORK_BUILD_DIR
  FASTFORK_JOBS
  FASTFORK_TIMEOUT_MULTIPLIER
  FASTFORK_TESTS      Space-separated explicit Meson test names to run
  MESON

Examples:
  ./test-fastfork.sh
  ./test-fastfork.sh quick
  ./test-fastfork.sh full -- --verbose
  FASTFORK_TESTS="postgresql:regress/regress" ./test-fastfork.sh
EOF
}

detect_jobs() {
	if [[ -n "$JOBS" ]]; then
		printf '%s\n' "$JOBS"
	elif command -v sysctl >/dev/null 2>&1; then
		sysctl -n hw.ncpu 2>/dev/null || printf '4\n'
	elif command -v nproc >/dev/null 2>&1; then
		nproc
	else
		printf '4\n'
	fi
}

fix_darwin_tmp_install_names() {
	local install_prefix="$BUILD_DIR/tmp_install/usr/local/pgsql"
	local path
	local deps
	local dep
	local replacement

	if [[ "$(uname -s)" != "Darwin" ]]; then
		return
	fi
	if ! command -v install_name_tool >/dev/null 2>&1 || ! command -v otool >/dev/null 2>&1; then
		return
	fi
	if [[ ! -d "$install_prefix" ]]; then
		return
	fi

	for path in "$install_prefix"/bin/* "$install_prefix"/lib/*.dylib; do
		if [[ ! -f "$path" ]]; then
			continue
		fi

		deps="$(otool -L "$path" 2>/dev/null | awk '/\/usr\/local\/pgsql\/lib\// {print $1}')"
		if [[ -z "$deps" ]]; then
			continue
		fi

		for dep in $deps; do
			replacement="$install_prefix/lib/$(basename "$dep")"
			if [[ -f "$replacement" ]]; then
				install_name_tool -change "$dep" "$replacement" "$path" 2>/dev/null || true
			fi
		done
	done
}

while [[ $# -gt 0 ]]; do
	case "$1" in
		quick|core|full)
			MODE="$1"
			shift
			;;
		--build-dir)
			BUILD_DIR="$2"
			shift 2
			;;
		--jobs)
			JOBS="$2"
			shift 2
			;;
		--list)
			LIST_ONLY=1
			shift
			;;
		--setup-only)
			SETUP_ONLY=1
			shift
			;;
		--no-reconfigure)
			RECONFIGURE=0
			shift
			;;
		--wipe)
			WIPE=1
			shift
			;;
		-h|--help)
			usage
			exit 0
			;;
		--)
			shift
			EXTRA_TEST_ARGS+=("$@")
			break
			;;
		*)
			printf 'unknown argument: %s\n\n' "$1" >&2
			usage >&2
			exit 2
			;;
	esac
done

JOBS="$(detect_jobs)"
if perl -MIPC::Run -e 1 >/dev/null 2>&1; then
	TAP_AVAILABLE=1
fi

SETUP_ARGS=(
	"-Dtest_fake_wal=true"
	"-Dtest_no_bg_jobs=true"
	"-Dtest_mem_smgr=true"
	"-Dtest_ephemeral_buffers=true"
	"-Dtest_mem_slru=true"
	"-Dtest_epoch_rollback=true"
	"-Dtest_no_wal_assembly=true"
	"-Dtest_no_observability=true"
	"-Dtest_fast_memory_contexts=true"
	"-Dtest_ephemeral_catalog=true"
	"-Dtest_no_durable_maintenance=true"
	"-Dtest_fast_analyze=true"
	"-Dtest_no_recovery_startup=true"
	"-Dtest_seed_only_startup=true"
	"-Dtest_no_data_directory_startup=true"
	"-Dtap_tests=auto"
	"-Dauto_features=disabled"
	"-Dicu=disabled"
	"-Dreadline=disabled"
	"-Dssl=none"
	"-Dzlib=disabled"
	"-Dlz4=disabled"
	"-Dzstd=disabled"
)

if [[ "$(uname -s)" == "Darwin" ]]; then
	SETUP_ARGS+=("-Dtest_macos_named_posix_semaphores=true")
	SETUP_ARGS+=("-Dtest_no_sysv_shared_memory=true")
fi

QUICK_TESTS=(
	"postgresql:test_copy_callbacks/regress"
	"postgresql:test_parser/regress"
	"postgresql:test_regex/regress"
	"postgresql:test_aio/001_aio"
	"postgresql:test_aio/002_io_workers"
	"postgresql:test_aio/003_initdb"
	"postgresql:test_aio/004_read_stream"
)

SETUP_TESTS=(
	"postgresql:tmp_install"
	"postgresql:install_test_files"
	"postgresql:initdb_cache"
)

CORE_EXTRA_TESTS=(
	"postgresql:brin/isolation"
	"postgresql:spgist_name_ops/regress"
	"postgresql:test_binaryheap/regress"
	"postgresql:test_bitmapset/regress"
	"postgresql:test_bloomfilter/regress"
	"postgresql:test_copy_callbacks/regress"
	"postgresql:test_dsa/regress"
	"postgresql:test_dsm_registry/regress"
	"postgresql:test_extensions/regress"
	"postgresql:test_ginpostinglist/regress"
	"postgresql:test_integerset/regress"
	"postgresql:test_lfind/regress"
	"postgresql:test_lwlock_tranches/regress"
	"postgresql:test_parser/regress"
	"postgresql:test_predtest/regress"
	"postgresql:test_radixtree/regress"
	"postgresql:test_rbtree/regress"
	"postgresql:test_resowner/regress"
	"postgresql:test_saslprep/regress"
	"postgresql:test_tidstore/regress"
)

# test_no_bg_jobs keeps async I/O workers but disables generic, parallel, and
# extension-launched background workers, so skip suites that intentionally
# exercise those code paths.
# The index isolation suite checks pg_statio_* heap access counters.  Those are
# intentionally disabled by test_no_observability.
# The BRIN isolation suite asserts VACUUM-driven summarization; in
# test_no_durable_maintenance, VACUUM is intentionally a no-op.
UNSUPPORTED_RE='^postgresql:(regress/regress|isolation/isolation|recovery/|subscription/|pg_upgrade/|pg_basebackup/|pg_combinebackup/|pg_rewind/|pg_verifybackup/|pg_archivecleanup/|pg_resetwal/|pg_walinspect/|pg_waldump/|pg_walsummary/|pg_logicalinspect/|test_decoding/|test_checksums/|test_autovacuum/|test_custom_rmgrs/|test_shm_mq/|worker_spi/|basic_archive/|brin/isolation|brin/02_wal_consistency|index/isolation|pg_ctl/003_promote)'

if [[ "$WIPE" -eq 1 ]]; then
	rm -rf "$BUILD_DIR"
fi

if [[ -f "$BUILD_DIR/build.ninja" ]]; then
	if [[ "$RECONFIGURE" -eq 1 ]]; then
		"$MESON" setup "$BUILD_DIR" "${SETUP_ARGS[@]}" --reconfigure
	fi
else
	"$MESON" setup "$BUILD_DIR" "${SETUP_ARGS[@]}"
fi

if [[ "$SETUP_ONLY" -eq 1 ]]; then
	"$MESON" compile -C "$BUILD_DIR" -j "$JOBS"
	exit 0
fi

AVAILABLE_TESTS="$(mktemp "${TMPDIR:-/tmp}/fastfork-tests.XXXXXX")"
SELECTED_TESTS="$(mktemp "${TMPDIR:-/tmp}/fastfork-selected.XXXXXX")"
trap 'rm -f "$AVAILABLE_TESTS" "$SELECTED_TESTS"' EXIT

"$MESON" test -C "$BUILD_DIR" --list | sed 's/^.* - //' > "$AVAILABLE_TESTS"

select_named_tests() {
	local test_name

	for test_name in "$@"; do
		if [[ "$test_name" == postgresql:test_aio/* && "$TAP_AVAILABLE" -eq 0 ]]; then
			printf 'warning: skipping TAP-backed test without IPC::Run: %s\n' "$test_name" >&2
		elif grep -Fxq "$test_name" "$AVAILABLE_TESTS"; then
			printf '%s\n' "$test_name" >> "$SELECTED_TESTS"
		else
			printf 'warning: requested test is not available in this build: %s\n' "$test_name" >&2
		fi
	done
}

if [[ -n "${FASTFORK_TESTS:-}" ]]; then
	# shellcheck disable=SC2206
	EXPLICIT_TESTS=($FASTFORK_TESTS)
	select_named_tests "${EXPLICIT_TESTS[@]}"
elif [[ "$MODE" == "quick" ]]; then
	select_named_tests "${QUICK_TESTS[@]}"
elif [[ "$MODE" == "core" ]]; then
	select_named_tests "${QUICK_TESTS[@]}" "${CORE_EXTRA_TESTS[@]}"
else
	if [[ "$TAP_AVAILABLE" -eq 1 ]]; then
		grep -Ev "$UNSUPPORTED_RE" "$AVAILABLE_TESTS" | \
			grep -Ev '^postgresql:(tmp_install|install_test_files|initdb_cache)$' > "$SELECTED_TESTS"
	else
		printf 'warning: IPC::Run is not available; full mode is limited to pg_regress/isolation-style tests\n' >&2
		grep -Ev "$UNSUPPORTED_RE" "$AVAILABLE_TESTS" | grep -E '/(regress|isolation)$' > "$SELECTED_TESTS"
	fi
fi

sort -u "$SELECTED_TESTS" -o "$SELECTED_TESTS"
grep -Ev "$UNSUPPORTED_RE" "$SELECTED_TESTS" > "$SELECTED_TESTS.filtered"
mv "$SELECTED_TESTS.filtered" "$SELECTED_TESTS"

if [[ ! -s "$SELECTED_TESTS" ]]; then
	printf 'no tests selected\n' >&2
	exit 1
fi

printf 'Fast-fork validation build: %s\n' "$BUILD_DIR"
printf 'Mode: %s\n' "$MODE"
printf 'Selected tests: %s\n' "$(wc -l < "$SELECTED_TESTS" | tr -d ' ')"

if [[ "$LIST_ONLY" -eq 1 ]]; then
	cat "$SELECTED_TESTS"
	exit 0
fi

"$MESON" compile -C "$BUILD_DIR" -j "$JOBS"

SETUP_ARGS_FOR_TEST=(
	"-C" "$BUILD_DIR"
	"--print-errorlogs"
	"--num-processes" "1"
)

for test_name in "${SETUP_TESTS[@]}"; do
	if grep -Fxq "$test_name" "$AVAILABLE_TESTS"; then
		SETUP_ARGS_FOR_TEST+=("$test_name")
	fi
done

"$MESON" test "${SETUP_ARGS_FOR_TEST[@]}"
fix_darwin_tmp_install_names
"${PYTHON:-python3}" "$ROOT/bench/test_fastfork_snapshot.py" \
	--bin "$BUILD_DIR/tmp_install/usr/local/pgsql/bin"
"${PYTHON:-python3}" "$ROOT/bench/test_fastfork_epoch_rollback.py" \
	--bin "$BUILD_DIR/tmp_install/usr/local/pgsql/bin"
"${PYTHON:-python3}" "$ROOT/bench/test_seed_only_startup.py" \
	--bin "$BUILD_DIR/tmp_install/usr/local/pgsql/bin"
"${PYTHON:-python3}" "$ROOT/bench/test_no_data_directory_startup.py" \
	--bin "$BUILD_DIR/tmp_install/usr/local/pgsql/bin"
if [[ "$(uname -s)" == "Darwin" ]]; then
	"${PYTHON:-python3}" "$ROOT/bench/test_macos_named_posix_semaphores.py" \
		--bin "$BUILD_DIR/tmp_install/usr/local/pgsql/bin"
	"${PYTHON:-python3}" "$ROOT/bench/test_macos_no_sysv_shared_memory.py" \
		--bin "$BUILD_DIR/tmp_install/usr/local/pgsql/bin"
fi

TEST_ARGS=(
	"-C" "$BUILD_DIR"
	"--print-errorlogs"
	"--num-processes" "$JOBS"
	"--timeout-multiplier" "$TIMEOUT_MULTIPLIER"
)

if [[ "${#EXTRA_TEST_ARGS[@]}" -gt 0 ]]; then
	TEST_ARGS+=("${EXTRA_TEST_ARGS[@]}")
fi

while IFS= read -r test_name; do
	TEST_ARGS+=("$test_name")
done < "$SELECTED_TESTS"

"$MESON" test "${TEST_ARGS[@]}"
