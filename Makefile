.PHONY: help build build-minimal test installer-test lint docs docs-check check install release-check validate-workflows

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
		'release-check    local release rehearsal'

build:
	cargo build --locked

validate-workflows:
	bash scripts/validate-workflows.sh
	bash scripts/test-validate-workflows.sh

build-minimal:
	cargo build --locked --no-default-features

test: validate-workflows installer-test
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
