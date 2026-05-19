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
RUST_BUILD_PROFILE ?= release
RUST_PGCORE ?= full
REGRESSION_SUITE ?= core
REGRESSION_CASES_DIR ?=
REGRESSION_ALLOW_FAILURES ?= 0
PROFILE_TRANSACTIONS ?= 500
PROFILE_RUNS ?= 1
PROFILE_TOOL ?= flamegraph
PROFILE_PHASE ?= run
PROFILE_OPEN ?= 0
PROFILE_WARMUP_SECONDS ?= 1.0
CRITERION_ARGS ?=
CLIPPY_ARGS ?= --workspace --all-targets
PROFILE_OPEN_FLAG := $(if $(filter 1 true yes,$(PROFILE_OPEN)),--profile-open,)
REGRESSION_CASES_ARG := $(if $(REGRESSION_CASES_DIR),--cases-dir "$(REGRESSION_CASES_DIR)",)
REGRESSION_ALLOW_FAILURES_ARG := $(if $(filter 1 true yes,$(REGRESSION_ALLOW_FAILURES)),--allow-fastpg-failures,)

PGBENCH_COMMON_ARGS = \
	--scale "$(SCALE)" \
	--transactions "$(TRANSACTIONS)" \
	--clients "$(CLIENTS)" \
	--jobs "$(JOBS)" \
	--runs "$(RUNS)" \
	--protocol "$(PROTOCOL)" \
	--meson-buildtype "$(MESON_BUILDTYPE)" \
	--fastpg-engine "rust-server" \
	--rust-build-profile "$(RUST_BUILD_PROFILE)" \
	--rust-pgcore "$(RUST_PGCORE)"

PGBENCH_PROFILE_ARGS = \
	--scale "$(SCALE)" \
	--transactions "$(PROFILE_TRANSACTIONS)" \
	--clients "$(CLIENTS)" \
	--jobs "$(JOBS)" \
	--runs "$(PROFILE_RUNS)" \
	--protocol "$(PROTOCOL)" \
	--meson-buildtype "$(MESON_BUILDTYPE)" \
	--fastpg-engine "rust-server" \
	--rust-build-profile "release" \
	--rust-pgcore "$(RUST_PGCORE)" \
	--profile-fastpg-rust-server \
	--profile-tool "$(PROFILE_TOOL)" \
	--profile-phase "$(PROFILE_PHASE)" \
	--profile-warmup-seconds "$(PROFILE_WARMUP_SECONDS)" \
	$(PROFILE_OPEN_FLAG)

.PHONY: pgbench pgbench-tpcb profile profile-open profile-side-by-side profile-open-latest
.PHONY: regression
.PHONY: bench bench-smoke lint validate

pgbench:
	$(PYTHON) pgbench_compare.py \
		--builtin "$(BUILTIN)" \
		--init-steps "$(INIT_STEPS)" \
		$(PGBENCH_COMMON_ARGS)

pgbench-tpcb: RUST_BUILD_PROFILE = release
pgbench-tpcb: RUST_PGCORE = full
pgbench-tpcb:
	$(PYTHON) pgbench_compare.py \
		--builtin "tpcb-like" \
		--init-steps "default" \
		$(PGBENCH_COMMON_ARGS)

profile:
	$(PYTHON) pgbench_compare.py \
		--builtin "$(BUILTIN)" \
		--init-steps "$(INIT_STEPS)" \
		$(PGBENCH_PROFILE_ARGS)

profile-open:
	$(MAKE) profile PROFILE_OPEN=1

profile-side-by-side:
	$(PYTHON) pgbench_compare.py \
		--builtin "tpcb-like" \
		--init-steps "default" \
		--scale "$(SCALE)" \
		--transactions "$(PROFILE_TRANSACTIONS)" \
		--clients "1" \
		--jobs "1" \
		--runs "1" \
		--protocol "$(PROTOCOL)" \
		--meson-buildtype "$(MESON_BUILDTYPE)" \
		--fastpg-engine "rust-server" \
		--rust-build-profile "release" \
		--rust-pgcore "full" \
		--profile-normal-postgres \
		--profile-fastpg-rust-server \
		--profile-tool "$(PROFILE_TOOL)" \
		--profile-phase "run" \
		--profile-warmup-seconds "$(PROFILE_WARMUP_SECONDS)" $(PROFILE_OPEN_FLAG)

profile-open-latest:
	$(PYTHON) open_latest_profile.py

regression:
	$(PYTHON) regression_compare.py \
		--suite "$(REGRESSION_SUITE)" \
		$(REGRESSION_CASES_ARG) \
		--meson-buildtype "$(MESON_BUILDTYPE)" \
		--rust-build-profile "release" \
		--rust-pgcore "full" \
		$(REGRESSION_ALLOW_FAILURES_ARG)

bench:
	cargo bench -p fastpg-storage --bench storage_engine -- $(CRITERION_ARGS)

bench-smoke:
	$(MAKE) bench CRITERION_ARGS="--sample-size 10 --warm-up-time 0.1 --measurement-time 0.2"

lint:
	cargo fmt --all --check
	cargo clippy $(CLIPPY_ARGS) -- -D warnings

validate:
	$(PYTHON) -m py_compile pgbench_compare.py open_latest_profile.py regression_compare.py
	cargo test -p fastpg-storage
	$(MAKE) pgbench-tpcb
	$(MAKE) regression
