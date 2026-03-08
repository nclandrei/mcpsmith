SHELL := /bin/bash

.PHONY: help fmt fmt-check clippy test check local-checks local-checks-fix

help:
	@printf '%s\n' \
	  'Targets:' \
	  '  make fmt               Run cargo fmt --all' \
	  '  make fmt-check         Run cargo fmt --all --check' \
	  '  make clippy            Run cargo clippy --workspace --all-targets -- -D warnings' \
	  '  make test              Run cargo test --workspace' \
	  '  make check             Run fmt-check, clippy, and test' \
	  '  make local-checks      Run scripts/local-checks.sh' \
	  '  make local-checks-fix  Run scripts/local-checks.sh --fix'

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

check: fmt-check clippy test

local-checks:
	./scripts/local-checks.sh

local-checks-fix:
	./scripts/local-checks.sh --fix
