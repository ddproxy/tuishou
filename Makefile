.PHONY: build test lint docs release-check validate-workflows

build:
	cargo build --locked

validate-workflows:
	bash scripts/validate-workflows.sh
	bash scripts/test-validate-workflows.sh

test: validate-workflows
	cargo test --workspace --locked --features test-hooks

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --locked -- -D warnings

docs:
	@if command -v vale >/dev/null 2>&1; then vale README.md docs/*.md CHANGELOG.md SECURITY.md; else echo "vale not installed; skipping"; fi

release-check: build
	bash scripts/release-check.sh
