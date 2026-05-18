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
RUST_PGCORE ?= raw-parser
REGRESSION_SUITE ?= core
REGRESSION_INVENTORY_SUITE ?= inventory
REGRESSION_CASES_DIR ?=
PROFILE_TRANSACTIONS ?= 500
PROFILE_RUNS ?= 1
PROFILE_TOOL ?= flamegraph
PROFILE_PHASE ?= run
PROFILE_OPEN ?= 0
PROFILE_WARMUP_SECONDS ?= 1.0
PROFILE_OPEN_FLAG := $(if $(filter 1 true yes,$(PROFILE_OPEN)),--profile-open,)
REGRESSION_CASES_ARG := $(if $(REGRESSION_CASES_DIR),--cases-dir "$(REGRESSION_CASES_DIR)",)

.PHONY: pgbench-compare pgbench-compare-strict pgbench-compare-rust-server pgbench-compare-rust-server-strict pgbench-profile-rust-server pgbench-profile-side-by-side pgbench-profile-rust-server-open pgbench-open-latest-flamegraph regression-compare-rust-server regression-compare-rust-server-inventory validate-rust-server

pgbench-compare:
	$(PYTHON) pgbench_compare.py \
		--builtin "$(BUILTIN)" \
		--init-steps "$(INIT_STEPS)" \
		--scale "$(SCALE)" \
		--transactions "$(TRANSACTIONS)" \
		--clients "$(CLIENTS)" \
		--jobs "$(JOBS)" \
		--runs "$(RUNS)" \
		--protocol "$(PROTOCOL)" \
		--meson-buildtype "$(MESON_BUILDTYPE)"

pgbench-compare-strict:
	$(PYTHON) pgbench_compare.py \
		--builtin "tpcb-like" \
		--init-steps "default" \
		--scale "$(SCALE)" \
		--transactions "$(TRANSACTIONS)" \
		--clients "$(CLIENTS)" \
		--jobs "$(JOBS)" \
		--runs "$(RUNS)" \
		--protocol "$(PROTOCOL)" \
		--meson-buildtype "$(MESON_BUILDTYPE)"

pgbench-compare-rust-server:
	$(PYTHON) pgbench_compare.py \
		--builtin "$(BUILTIN)" \
		--init-steps "$(INIT_STEPS)" \
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

pgbench-compare-rust-server-strict:
	$(PYTHON) pgbench_compare.py \
		--builtin "tpcb-like" \
		--init-steps "default" \
		--scale "$(SCALE)" \
		--transactions "$(TRANSACTIONS)" \
		--clients "$(CLIENTS)" \
		--jobs "$(JOBS)" \
		--runs "$(RUNS)" \
		--protocol "$(PROTOCOL)" \
		--meson-buildtype "$(MESON_BUILDTYPE)" \
		--fastpg-engine "rust-server" \
		--rust-build-profile "release" \
		--rust-pgcore "full"

pgbench-profile-rust-server:
	$(PYTHON) pgbench_compare.py \
		--builtin "$(BUILTIN)" \
		--init-steps "$(INIT_STEPS)" \
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
		--profile-warmup-seconds "$(PROFILE_WARMUP_SECONDS)" $(PROFILE_OPEN_FLAG)

pgbench-profile-side-by-side:
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

pgbench-profile-rust-server-open:
	$(MAKE) pgbench-profile-rust-server PROFILE_OPEN=1

pgbench-open-latest-flamegraph:
	$(PYTHON) open_latest_profile.py

regression-compare-rust-server:
	$(PYTHON) regression_compare.py \
		--suite "$(REGRESSION_SUITE)" \
		$(REGRESSION_CASES_ARG) \
		--meson-buildtype "$(MESON_BUILDTYPE)" \
		--rust-build-profile "release" \
		--rust-pgcore "full"

regression-compare-rust-server-inventory:
	$(PYTHON) regression_compare.py \
		--suite "$(REGRESSION_INVENTORY_SUITE)" \
		$(REGRESSION_CASES_ARG) \
		--meson-buildtype "$(MESON_BUILDTYPE)" \
		--rust-build-profile "release" \
		--rust-pgcore "full" \
		--allow-fastpg-failures

validate-rust-server:
	$(PYTHON) -m py_compile pgbench_compare.py open_latest_profile.py regression_compare.py
	cargo test -p fastpg-storage
	$(MAKE) pgbench-compare-rust-server-strict
	$(MAKE) regression-compare-rust-server
	meson test -C .build/pgbench/fastpg --suite fastpg_parser_probe --print-errorlogs
