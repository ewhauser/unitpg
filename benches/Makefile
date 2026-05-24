PYTHON ?= python3

BUILTIN ?= simple-update
# Built-in pgbench scripts derive :scale from generated rows, so keep g in the
# default init path. Use INIT_STEPS=default for pgbench's full default setup.
INIT_STEPS ?= dtg
SCALE ?= 1
TRANSACTIONS ?= 20
CLIENTS ?= 1
JOBS ?= 1
RUNS ?= 3
PROTOCOL ?= simple
MESON_BUILDTYPE ?= release
SEMA_KIND ?= auto
RUST_BUILD_PROFILE ?= release
STORAGE_ENGINE ?= storage2
CATALOG_MODE ?= postgres
CATALOG_PGDATA_MODE ?= seed-skeleton
FASTPG_POSTGRES_SMGR ?= mem
FASTPG_USE_MEM_INDEX_AM ?= 0
PGVECTOR ?= 1
PGVECTOR_VERSION ?= v0.8.2
UPSTREAM_REGRESSION_CASES ?=
UPSTREAM_REGRESSION_LIMIT ?=
REGRESSION_GLOBAL_TIMEOUT ?= 300
FASTPG_CASE_TIMEOUT ?= 60
REGRESSION_FAIL_ON_DIFFERENCES ?= 0
PROFILE_TRANSACTIONS ?= 50000
PROFILE_RUNS ?= 1
PROFILE_TOOL ?= flamegraph
PROFILE_PHASE ?= run
PROFILE_OPEN ?= 0
PROFILE_WARMUP_SECONDS ?= 0.1
PROFILE_HYPERFINE ?= 1
PROFILE_HYPERFINE_RUNS ?= 1
PROFILE_HYPERFINE_WARMUP ?= 0
PROFILE_SERVER_MEMORY ?= 1
CRITERION_ARGS ?=
CLIPPY_ARGS ?= --workspace --all-targets
UNIT_TEST_WORKLOAD_SCRIPT ?= workloads/unit-test-workload.sql
PROFILE_OPEN_FLAG := $(if $(filter 1 true yes,$(PROFILE_OPEN)),--profile-open,)
PROFILE_HYPERFINE_FLAG := $(if $(filter 1 true yes,$(PROFILE_HYPERFINE)),--profile-hyperfine,)
PROFILE_SERVER_MEMORY_FLAG := $(if $(filter 1 true yes,$(PROFILE_SERVER_MEMORY)),--profile-server-memory,)
UPSTREAM_REGRESSION_CASE_ARGS := $(foreach case,$(UPSTREAM_REGRESSION_CASES),--case "$(case)")
UPSTREAM_REGRESSION_LIMIT_ARG := $(if $(UPSTREAM_REGRESSION_LIMIT),--limit "$(UPSTREAM_REGRESSION_LIMIT)",)
REGRESSION_FAIL_ON_DIFFERENCES_ARG := $(if $(filter 1 true yes,$(REGRESSION_FAIL_ON_DIFFERENCES)),--fail-on-differences,)
FASTPG_USE_MEM_INDEX_AM_ARG := $(if $(filter 1 true yes,$(FASTPG_USE_MEM_INDEX_AM)),--fastpg-use-mem-index-am,)
PGVECTOR_ARG := $(if $(filter 0 false no,$(PGVECTOR)),--no-pgvector,)
PGVECTOR_VERSION_ARG := --pgvector-version "$(PGVECTOR_VERSION)"
PGVECTOR_TEST_CMD := $(if $(filter 0 false no,$(PGVECTOR)),@true,$(MAKE) pgvector-smoke)
FASTPG_PGVECTOR_PREFIX := $(CURDIR)/.build/pgbench/fastpg/tmp_install/usr/local/pgsql
FASTPG_PGVECTOR_SOURCE_DIR := $(CURDIR)/.build/pgbench/pgvector/fastpg-smoke

PGBENCH_COMMON_ARGS = \
	--scale "$(SCALE)" \
	--transactions "$(TRANSACTIONS)" \
	--clients "$(CLIENTS)" \
	--jobs "$(JOBS)" \
	--runs "$(RUNS)" \
	--protocol "$(PROTOCOL)" \
	--meson-buildtype "$(MESON_BUILDTYPE)" \
	--sema-kind "$(SEMA_KIND)" \
	--storage-engine "$(STORAGE_ENGINE)" \
	--catalog-mode "$(CATALOG_MODE)" \
	--fastpg-catalog-pgdata-mode "$(CATALOG_PGDATA_MODE)" \
	--fastpg-postgres-smgr "$(FASTPG_POSTGRES_SMGR)" \
	--rust-build-profile "$(RUST_BUILD_PROFILE)" \
	$(PGVECTOR_ARG) \
	$(PGVECTOR_VERSION_ARG) \
	$(FASTPG_USE_MEM_INDEX_AM_ARG)

PGBENCH_PROFILE_ARGS = \
	--scale "$(SCALE)" \
	--transactions "$(PROFILE_TRANSACTIONS)" \
	--clients "$(CLIENTS)" \
	--jobs "$(JOBS)" \
	--runs "$(PROFILE_RUNS)" \
	--protocol "$(PROTOCOL)" \
	--meson-buildtype "$(MESON_BUILDTYPE)" \
	--sema-kind "$(SEMA_KIND)" \
	--storage-engine "$(STORAGE_ENGINE)" \
	--catalog-mode "$(CATALOG_MODE)" \
	--fastpg-catalog-pgdata-mode "$(CATALOG_PGDATA_MODE)" \
	--fastpg-postgres-smgr "$(FASTPG_POSTGRES_SMGR)" \
	--rust-build-profile "release" \
	$(PGVECTOR_ARG) \
	$(PGVECTOR_VERSION_ARG) \
	--profile-fastpg-rust-server \
	--profile-tool "$(PROFILE_TOOL)" \
	--profile-phase "$(PROFILE_PHASE)" \
	--profile-warmup-seconds "$(PROFILE_WARMUP_SECONDS)" \
	--profile-hyperfine-runs "$(PROFILE_HYPERFINE_RUNS)" \
	--profile-hyperfine-warmup "$(PROFILE_HYPERFINE_WARMUP)" \
	$(FASTPG_USE_MEM_INDEX_AM_ARG) \
	$(PROFILE_HYPERFINE_FLAG) \
	$(PROFILE_SERVER_MEMORY_FLAG) \
	$(PROFILE_OPEN_FLAG)

.PHONY: pgbench pgbench-simple-indexed pgbench-tpcb pgbench-unit-test-workload profile profile-unit-test-workload profile-open profile-side-by-side profile-open-latest
.PHONY: regression
.PHONY: bench bench-smoke lint validate

pgbench:
	$(PYTHON) pgbench_compare.py \
		--builtin "$(BUILTIN)" \
		--init-steps "$(INIT_STEPS)" \
		$(PGBENCH_COMMON_ARGS)

pgbench-simple-indexed: RUST_BUILD_PROFILE = release
pgbench-simple-indexed:
	$(PYTHON) pgbench_compare.py \
		--builtin "simple-update" \
		--init-steps "dtgp" \
		$(PGBENCH_COMMON_ARGS)

pgbench-tpcb: RUST_BUILD_PROFILE = release
pgbench-tpcb:
	$(PYTHON) pgbench_compare.py \
		--builtin "tpcb-like" \
		--init-steps "default" \
		$(PGBENCH_COMMON_ARGS)

pgbench-unit-test-workload: TRANSACTIONS = 200
pgbench-unit-test-workload: CLIENTS = 10
pgbench-unit-test-workload: JOBS = 10
pgbench-unit-test-workload:
	$(PYTHON) pgbench_compare.py \
		--script "$(UNIT_TEST_WORKLOAD_SCRIPT)" \
		--skip-pgbench-init \
		$(PGBENCH_COMMON_ARGS)

profile:
	$(PYTHON) pgbench_compare.py \
		--builtin "$(BUILTIN)" \
		--init-steps "$(INIT_STEPS)" \
		$(PGBENCH_PROFILE_ARGS)

profile-unit-test-workload: PROFILE_TRANSACTIONS = 500
profile-unit-test-workload: PROFILE_RUNS = 1
profile-unit-test-workload: CLIENTS = 10
profile-unit-test-workload: JOBS = 10
profile-unit-test-workload:
	$(PYTHON) pgbench_compare.py \
		--script "$(UNIT_TEST_WORKLOAD_SCRIPT)" \
		--skip-pgbench-init \
		$(PGBENCH_PROFILE_ARGS)

profile-open:
	$(MAKE) profile PROFILE_OPEN=1

profile-side-by-side:
	$(PYTHON) pgbench_compare.py \
		--builtin "tpcb-like" \
		--init-steps "default" \
		--scale "$(SCALE)" \
		--transactions "$(PROFILE_TRANSACTIONS)" \
		--clients "$(CLIENTS)" \
		--jobs "$(JOBS)" \
		--runs "1" \
		--protocol "$(PROTOCOL)" \
		--meson-buildtype "$(MESON_BUILDTYPE)" \
		--sema-kind "$(SEMA_KIND)" \
		--storage-engine "$(STORAGE_ENGINE)" \
		--catalog-mode "$(CATALOG_MODE)" \
		--rust-build-profile "release" \
		$(PGVECTOR_ARG) \
		$(PGVECTOR_VERSION_ARG) \
		--profile-normal-postgres \
		--profile-fastpg-rust-server \
		--profile-tool "$(PROFILE_TOOL)" \
		--profile-phase "run" \
		--profile-warmup-seconds "$(PROFILE_WARMUP_SECONDS)" \
		--profile-hyperfine-runs "$(PROFILE_HYPERFINE_RUNS)" \
		--profile-hyperfine-warmup "$(PROFILE_HYPERFINE_WARMUP)" \
		$(FASTPG_USE_MEM_INDEX_AM_ARG) \
		$(PROFILE_HYPERFINE_FLAG) \
		$(PROFILE_SERVER_MEMORY_FLAG) \
		$(PROFILE_OPEN_FLAG)

profile-open-latest:
	$(PYTHON) open_latest_profile.py

regression:
	$(PYTHON) upstream_regression_inventory.py \
		$(UPSTREAM_REGRESSION_CASE_ARGS) \
		$(UPSTREAM_REGRESSION_LIMIT_ARG) \
		--meson-buildtype "$(MESON_BUILDTYPE)" \
	--rust-build-profile "$(RUST_BUILD_PROFILE)" \
	--storage-engine "$(STORAGE_ENGINE)" \
	--catalog-mode "$(CATALOG_MODE)" \
	--fastpg-postgres-smgr "$(FASTPG_POSTGRES_SMGR)" \
	--sema-kind "$(SEMA_KIND)" \
	$(PGVECTOR_ARG) \
	$(PGVECTOR_VERSION_ARG) \
	--global-timeout "$(REGRESSION_GLOBAL_TIMEOUT)" \
	--fastpg-case-timeout "$(FASTPG_CASE_TIMEOUT)" \
	$(FASTPG_USE_MEM_INDEX_AM_ARG) \
	$(REGRESSION_FAIL_ON_DIFFERENCES_ARG)

pgvector-smoke:
	meson test -C "$(CURDIR)/.build/pgbench/fastpg" --print-errorlogs postgresql:tmp_install
	../ci/build-pgvector.sh "$(FASTPG_PGVECTOR_PREFIX)" "$(FASTPG_PGVECTOR_SOURCE_DIR)" "$(PGVECTOR_VERSION)"
	FASTPG_POSTGRES_BUILD_DIR="$(CURDIR)/.build/pgbench/fastpg" cargo test -p fastpg-testkit --features postgres-execution --test pgvector_smoke

bench:
	cargo bench -p fastpg-storage --bench storage_engine -- $(CRITERION_ARGS)

bench-smoke:
	$(MAKE) bench CRITERION_ARGS="--sample-size 10 --warm-up-time 0.1 --measurement-time 0.2"

lint:
	cargo fmt --all --check
	cargo clippy $(CLIPPY_ARGS) -- -D warnings

validate:
	$(PYTHON) -m py_compile pgbench_compare.py open_latest_profile.py upstream_regression_inventory.py
	cargo test -p fastpg-storage
	$(MAKE) pgbench-tpcb
	$(PGVECTOR_TEST_CMD)
	$(MAKE) regression
