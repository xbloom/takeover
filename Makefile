.PHONY: build clean install test run help

# Default target
help:
	@echo "Available targets:"
	@echo ""
	@echo "🏗️  Build:"
	@echo "  build           - Build the project in release mode"
	@echo "  debug           - Build the project in debug mode"
	@echo "  clean           - Clean build artifacts and processes"
	@echo "  install         - Install the binary to system"
	@echo ""
	@echo "🚀 Run (New Architecture):"
	@echo "  main            - Run main program (release, requires sudo)"
	@echo "  main-debug      - Run main program with debug logs"
	@echo "  dev             - Quick dev run (debug build)"
	@echo "  main-full-log   - Run with full logging to file"
	@echo ""
	@echo "🧪 Examples (POC):"
	@echo "  lv5             - Run HTTP/2 direct demo"
	@echo "  rel             - Run simple proxy server"
	@echo "  tls             - Run complete TLS proxy demo"
	@echo ""
	@echo "🧪 Testing:"
	@echo "  test            - Run tests"
	@echo "  check           - Check code without building"
	@echo "  fmt             - Format code"
	@echo "  clippy          - Run clippy linter"
	@echo ""
	@echo "🔧 Utilities:"
	@echo "  socks5          - Start SSH SOCKS5 tunnel"
	@echo "  curl            - Test HTTP/HTTPS requests"
	@echo "  status          - Check pfctl status"
	@echo "  config          - Create example config"

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

socks5:
	ssh -D 1080 -f -C -q -N hz1

.SILENT: curl

curl:
	echo "HTTP request:"
	curl -k  -s 'http://httpbin.org/ip'
	curl -k  -s 'http://ipinfo.io/json'
	echo "\n-----------------------------------"
	echo "HTTPS request:"
	curl -k  -s 'https://httpbin.org/ip'
	curl -k  -s 'https://ipinfo.io/json'

status:
	sudo pfctl -s info
	sudo pfctl -s nat
	sudo pfctl -s rules
	sudo pfctl -s states

cpf:
	sudo pfctl -F all
	sudo pfctl -a takeover_transparent -F all

lv5:
	cargo build --example http2_direct_demo
	sudo chown root target/debug/examples/http2_direct_demo
	sudo chmod u+s target/debug/examples/http2_direct_demo 
	echo $$RUST_LOG
	./target/debug/examples/http2_direct_demo

rel:
	cargo build --release --example simple_proxy_server
	sudo chown root target/release/examples/simple_proxy_server
	sudo chmod u+s target/release/examples/simple_proxy_server 
	./target/release/examples/simple_proxy_server

tls:
	cargo build --release --example complete_tls_proxy_demo
	sudo chown root target/release/examples/complete_tls_proxy_demo
	sudo chmod u+s target/release/examples/complete_tls_proxy_demo 
	RUST_LOG=takeover=debug,main=debug ./target/release/examples/complete_tls_proxy_demo

# 🌐 新架构主程序运行
main:
	@echo "🚀 Building and running Takeover main program..."
	cargo build --release
	sudo ./target/release/takeover

# 快速运行开发版本（debug编译速度快）
dev:
	@echo "⚡ Quick dev run..."
	cargo build
	sudo RUST_LOG=takeover=debug ./target/debug/takeover 2>&1
