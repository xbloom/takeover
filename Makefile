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
	sudo pfctl -a takeover_transparent -F all
	sudo pkill -9 takeover
	sudo pkill -9 simple_proxy_server
	sudo pkill -9 transparent_proxy_demo

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

server:
	sudo cargo run --example simple_proxy_server

rel:
	cargo build --release --example simple_proxy_server
	sudo chown root target/release/examples/simple_proxy_server
	sudo chmod u+s target/release/examples/simple_proxy_server 
	./target/release/examples/simple_proxy_server

tls:
	cargo build --release --example complete_tls_proxy_demo
	sudo chown root target/release/examples/complete_tls_proxy_demo
	sudo chmod u+s target/release/examples/complete_tls_proxy_demo 
	./target/release/examples/complete_tls_proxy_demo