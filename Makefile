.PHONY: build test lint docs

build:
	cargo build --locked

test:
	cargo test --workspace --locked

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --locked -- -D warnings

docs:
	@if command -v vale >/dev/null 2>&1; then vale README.md docs/*.md CHANGELOG.md SECURITY.md; else echo "vale not installed; skipping"; fi
