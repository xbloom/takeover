# Takeover - Transparent Proxy Tool

Takeover is a Rust-based transparent proxy tool similar to Proxifier. It monitors running processes and automatically redirects their network traffic through configured proxy servers without the target applications knowing about it.

## Features

- **Process Monitoring**: Monitors both existing and newly launched processes
- **Transparent Proxying**: Redirects network traffic without application awareness
- **Multiple Proxy Support**: Supports SOCKS5, SOCKS4, HTTP, and HTTPS proxies
- **Pattern Matching**: Flexible process matching using path patterns
- **Cross-Platform**: Supports macOS and Linux (with platform-specific optimizations)
- **Configuration-Driven**: JSON-based configuration for easy management

## Requirements

### System Requirements
- **Root privileges**: Required for network traffic interception
- **Rust**: 1.70 or higher

### Platform-Specific Requirements

#### macOS
- `pfctl` (Packet Filter Control) - usually pre-installed
- Administrator privileges to modify firewall rules

#### Linux
- `iptables` for traffic redirection
- Root access for netfilter rule modification

## Installation

1. Clone the repository:
```bash
git clone <repository-url>
cd takeover
```

2. Build the project:
```bash
cargo build --release
```

3. Install (optional):
```bash
cargo install --path .
```

## Usage

### Basic Usage

```bash
# Run with a specific target program and proxy
sudo ./target/release/takeover -t firefox -p socks5://127.0.0.1:1080

# Use a custom configuration file
sudo ./target/release/takeover -c my_config.json -t chrome -p http://127.0.0.1:8080

# Run as daemon
sudo ./target/release/takeover -t firefox -p socks5://127.0.0.1:1080 -d

# Stop gracefully (preserves cleanup)
sudo pkill -TERM takeover

# Force stop (may require manual cleanup)
sudo pkill -9 takeover
```

### Command Line Options

- `-t, --target <PROGRAM_NAME>`: Target program name to proxy (required)
- `-p, --proxy <PROXY_URL>`: Proxy URL (required)
- `-c, --config <FILE>`: Configuration file path (default: config.json)
- `-d, --daemon`: Run as daemon

### Proxy URL Formats

- SOCKS5: `socks5://127.0.0.1:1080`
- SOCKS5 with auth: `socks5://user:pass@127.0.0.1:1080`
- SOCKS4: `socks4://127.0.0.1:1080`
- HTTP: `http://127.0.0.1:8080`
- HTTPS: `https://127.0.0.1:8080`

## Configuration

### Configuration File Structure

The configuration file uses JSON format. See `config.json.example` for a complete example.

```json
{
  "targets": {
    "firefox": {
      "name": "firefox",
      "path_patterns": [
        "**/firefox*",
        "**/Firefox*"
      ],
      "proxy_config": {
        "url": "socks5://127.0.0.1:1080",
        "proxy_type": "Socks5",
        "auth": null
      },
      "enabled": true
    }
  },
  "global_settings": {
    "log_level": "info",
    "bind_interface": null,
    "dns_servers": ["8.8.8.8", "8.8.4.4"],
    "enable_ipv6": false,
    "connection_timeout": 30
  }
}
```

### Path Patterns

Path patterns support simple wildcard matching:
- `**` - Recursive directory matching
- `*` - Single wildcard matching
- Exact paths are also supported

Examples:
- `**/firefox*` - Matches any firefox executable in any directory
- `firefox` - Exact match
- `/usr/bin/firefox` - Absolute path match

## How It Works

1. **Process Monitoring**: Continuously scans for running processes that match configured targets
2. **Traffic Interception**: Uses platform-specific mechanisms to intercept network traffic:
   - **macOS**: Uses `pfctl` (Packet Filter) to redirect traffic
   - **Linux**: Uses `iptables` with `--pid-owner` matching
3. **Proxy Forwarding**: Establishes connections to configured proxy servers and forwards traffic bidirectionally
4. **Transparent Operation**: Target applications remain unaware of the proxying

## Platform-Specific Implementation

### macOS
- Uses `pfctl` anchor rules for traffic redirection
- Requires `sudo` privileges for firewall modification
- Process monitoring via `sysinfo` crate with periodic scanning

### Linux
- Uses `iptables` with owner module for process-specific rules
- Supports `--pid-owner` matching for precise targeting
- Future: eBPF support for more efficient interception

## Cleanup Mechanism

### Automatic Cleanup

Takeover implements multiple layers of cleanup to ensure network rules are removed when the program exits:

1. **Graceful Shutdown**: Responds to SIGTERM and SIGINT signals
2. **Normal Exit**: Cleans up when monitoring loop ends naturally
3. **Drop Trait**: Emergency cleanup when the process is dropped (best-effort)

### Signal Handling

- **SIGTERM** (kill): Graceful shutdown with full cleanup
- **SIGINT** (Ctrl+C): Graceful shutdown with full cleanup
- **SIGKILL** (kill -9): Cannot be caught, may leave rules (use manual cleanup)

### Manual Cleanup

If the application crashes or is force-killed, you may need to manually clean up:

```bash
# Run the manual cleanup script
sudo ./scripts/manual_cleanup.sh

# Or manually on macOS:
sudo pfctl -a takeover -F all

# Or manually on Linux:
sudo iptables -t nat -L OUTPUT | grep takeover
# Then remove specific rules
```

### Testing Cleanup

Test the cleanup functionality:

```bash
# Run the cleanup test script
sudo ./scripts/test_cleanup.sh
```

## Security Considerations

- **Root Privileges**: Required for system-level network interception
- **Firewall Rules**: Modifies system firewall rules (automatically cleaned up on exit)
- **Process Monitoring**: Accesses system process information
- **Network Traffic**: Handles sensitive network data
- **Cleanup**: Multiple cleanup mechanisms ensure rules don't persist after exit

## Limitations

1. **Root Required**: Must run with root/administrator privileges
2. **Platform Specific**: Implementation varies between platforms
3. **Process Granularity**: Current implementation affects all connections from target processes
4. **DNS**: DNS requests may not be proxied depending on application behavior

## Development Status

This is a proof-of-concept implementation. Production use would require:

- Enhanced error handling and recovery
- Better process-specific traffic isolation
- DNS interception and proxying
- GUI interface
- Comprehensive testing across different applications
- Performance optimizations

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests if applicable
5. Submit a pull request

## License

[License information]

## Disclaimer

This tool modifies system network configuration and should be used with caution. Always test in a safe environment before production use. The authors are not responsible for any system damage or security issues.
