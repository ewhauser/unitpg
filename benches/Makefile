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

.PHONY: pgbench-compare pgbench-compare-strict pgbench-compare-rust-server

pgbench-compare:
	$(PYTHON) pgbench_compare.py \
		--builtin "$(BUILTIN)" \
		--init-steps "$(INIT_STEPS)" \
		--scale "$(SCALE)" \
		--transactions "$(TRANSACTIONS)" \
		--clients "$(CLIENTS)" \
		--jobs "$(JOBS)" \
		--runs "$(RUNS)" \
		--protocol "$(PROTOCOL)"

pgbench-compare-strict:
	$(PYTHON) pgbench_compare.py \
		--builtin "tpcb-like" \
		--init-steps "default" \
		--scale "$(SCALE)" \
		--transactions "$(TRANSACTIONS)" \
		--clients "$(CLIENTS)" \
		--jobs "$(JOBS)" \
		--runs "$(RUNS)" \
		--protocol "$(PROTOCOL)"

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
		--fastpg-engine "rust-server"
