# Basic Usage Examples

## Example 1: Proxy Firefox through SOCKS5

```bash
# Start takeover to proxy Firefox through a SOCKS5 proxy
sudo ./target/release/takeover -t firefox -p socks5://127.0.0.1:1080
```

This will:
1. Monitor for Firefox processes
2. When Firefox starts, redirect its traffic through the SOCKS5 proxy at 127.0.0.1:1080
3. Firefox will be unaware of the proxying

## Example 2: Proxy Chrome with Configuration File

First, create a configuration file:

```json
{
  "targets": {
    "chrome": {
      "name": "chrome",
      "path_patterns": [
        "**/chrome*",
        "**/Chrome*",
        "**/Google Chrome*"
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

Then run:

```bash
sudo ./target/release/takeover -c chrome_config.json -t chrome -p socks5://127.0.0.1:1080
```

## Example 3: Proxy curl with HTTP Proxy

```bash
sudo ./target/release/takeover -t curl -p http://127.0.0.1:8080
```

Now any curl command will be proxied:

```bash
curl https://ipinfo.io/ip
```

## Example 4: Running as Daemon

```bash
# Start as daemon
sudo ./target/release/takeover -t firefox -p socks5://127.0.0.1:1080 -d

# Check if it's running
ps aux | grep takeover

# Stop the daemon
sudo pkill takeover
```

## Testing the Setup

1. **Start a SOCKS5 proxy** (e.g., using ssh):
   ```bash
   ssh -D 1080 -N user@remote-server
   ```

2. **Run takeover**:
   ```bash
   sudo ./target/release/takeover -t firefox -p socks5://127.0.0.1:1080
   ```

3. **Launch Firefox** and visit https://ipinfo.io/ip to verify your IP has changed

## Troubleshooting

### Permission Issues
Make sure you're running with `sudo`:
```bash
sudo ./target/release/takeover -t firefox -p socks5://127.0.0.1:1080
```

### Process Not Found
Check if the process name matches:
```bash
ps aux | grep firefox
```

### Proxy Not Working
1. Verify your proxy is running:
   ```bash
   curl --socks5 127.0.0.1:1080 https://ipinfo.io/ip
   ```

2. Check logs for error messages (increase verbosity):
   ```bash
   RUST_LOG=debug sudo ./target/release/takeover -t firefox -p socks5://127.0.0.1:1080
   ```

### macOS Specific Issues

1. **System Integrity Protection (SIP)**: Some applications might be protected
2. **Network Extensions**: Modern versions might require additional permissions
3. **Firewall Rules**: Check if pfctl rules are being applied:
   ```bash
   sudo pfctl -s all | grep takeover
   ```

### Linux Specific Issues

1. **iptables**: Check if rules are being added:
   ```bash
   sudo iptables -t nat -L OUTPUT | grep takeover
   ```

2. **Process ownership**: Ensure the target process is running as expected user
