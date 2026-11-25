.PHONY: help build build-minimal test installer-test lint docs docs-check check install release-check validate-workflows fuzz-smoke

FUZZ_TARGETS := config_toml release_json node_json version_tag
FUZZ_TOOLCHAIN ?= +nightly
FUZZ_SECONDS ?= 5

help:
	@printf '%s\n' 'build            compile the project' \
		'build-minimal    build without the top feature' \
		'test             workflow validation, installer test and cargo tests' \
		'installer-test   test install.sh against a local fake release server' \
		'lint             rustfmt, clippy and shellcheck when available' \
		'docs             vale style pass when available' \
		'docs-check       run quickstart commands, validate config examples, check links' \
		'check            lint plus docs-check' \
		'install          run install.sh with INSTALL_ARGS' \
		'release-check    local release rehearsal' \
		'fuzz-smoke       run every fuzz target briefly with nightly libFuzzer'

build:
	cargo build --locked

validate-workflows:
	bash scripts/validate-workflows.sh
	bash scripts/test-validate-workflows.sh

build-minimal:
	cargo build --locked --no-default-features

test: validate-workflows installer-test
	@if command -v python3 >/dev/null 2>&1; then \
		python3 scripts/test_set_release_version.py; \
	else \
		echo "python3 not installed; skipping version script tests"; \
	fi
	cargo test --workspace --locked --features test-hooks

installer-test:
	bash tests/installer_test.sh

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --locked -- -D warnings
	@if command -v shellcheck >/dev/null 2>&1; then \
		shellcheck -x install.sh tests/installer_test.sh tests/fixtures/installer/*.sh scripts/*.sh; \
	else \
		echo "shellcheck not installed; skipping"; \
	fi

docs:
	@if command -v vale >/dev/null 2>&1; then vale README.md docs/*.md CHANGELOG.md SECURITY.md; else echo "vale not installed; skipping"; fi

docs-check: build
	bash scripts/docs-check.sh

check: lint docs-check

install:
	bash install.sh $(INSTALL_ARGS)

release-check: build
	bash scripts/release-check.sh

fuzz-smoke:
	@set -eu; \
	for target in $(FUZZ_TARGETS); do \
		mkdir -p "fuzz/artifacts/$$target"; \
		printf '%s\n' "fuzz: $$target ($(FUZZ_SECONDS)s)"; \
		cargo $(FUZZ_TOOLCHAIN) fuzz run "$$target" -- \
			-max_total_time=$(FUZZ_SECONDS) -timeout=5 -rss_limit_mb=2048 \
			-max_len=4096 -detect_leaks=0 -print_final_stats=1 \
			-artifact_prefix="fuzz/artifacts/$$target/"; \
	done
