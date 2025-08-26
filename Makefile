.PHONY: build clean install test run help

# Default target
help:
	@echo "Available targets:"
	@echo "  build    - Build the project in release mode"
	@echo "  debug    - Build the project in debug mode"
	@echo "  clean    - Clean build artifacts"
	@echo "  install  - Install the binary to system"
	@echo "  test     - Run tests"
	@echo "  run      - Run with example configuration"
	@echo "  check    - Check code without building"
	@echo "  fmt      - Format code"
	@echo "  clippy   - Run clippy linter"

build:
	cargo build --release

debug:
	cargo build

clean:
	cargo clean

install: build
	cargo install --path .

test:
	cargo test

run:
	@echo "Note: This requires sudo privileges"
	@echo "Example usage:"
	@echo "sudo ./target/release/takeover -t firefox -p socks5://127.0.0.1:1080"

check:
	cargo check

fmt:
	cargo fmt

clippy:
	cargo clippy -- -D warnings

# Development helpers
dev-setup:
	rustup component add rustfmt clippy

# Create example config if it doesn't exist
config:
	@if [ ! -f config.json ]; then \
		cp config.json.example config.json; \
		echo "Created config.json from example"; \
	else \
		echo "config.json already exists"; \
	fi

demo:
	sudo cargo run --example test_proxy_manager