# Troubleshooting Guide

## macOS pfctl Issues

### Problem: pfctl Syntax Error

**Error Message:**
```
pfctl: Use of -f option, could result in flushing of rules present in the main ruleset added by the system at startup.
pfctl: Syntax error in config file: pf rules not loaded
```

**Root Cause:**
The original pfctl rule syntax was incorrect and not properly formatted for macOS's Packet Filter.

**Solution Applied:**

1. **Fixed pfctl Rule Syntax**: Updated the rule format to be compliant with pfctl syntax:
   ```
   # Old (incorrect):
   rdr pass on lo0 inet proto tcp from any to any port 1:65535 -> 127.0.0.1 port 10001 # takeover_pid_12345
   
   # New (correct):
   rdr pass on lo0 inet proto tcp from any to any port 80 -> 127.0.0.1 port 10001
   rdr pass on lo0 inet proto tcp from any to any port 443 -> 127.0.0.1 port 10001
   rdr pass on lo0 inet proto tcp from any to any port 8080 -> 127.0.0.1 port 10001
   ```

2. **Improved Anchor Management**: Added proper anchor existence checking and safer rule loading.

3. **Better Error Handling**: Enhanced error reporting and cleanup on failures.

4. **Safer Rule Removal**: Updated rule removal to use specific flush commands (`-F nat` and `-F rules`) instead of `F all`.

### Additional macOS Considerations

#### System Integrity Protection (SIP)
Some applications may be protected by SIP and cannot be intercepted. To check SIP status:
```bash
csrutil status
```

#### Firewall Permissions
The application requires administrator privileges to modify pfctl rules:
```bash
sudo ./target/release/takeover -t firefox -p socks5://127.0.0.1:1080
```

#### Checking Active Rules
To verify if pfctl rules are active:
```bash
sudo pfctl -s all | grep takeover
sudo pfctl -a takeover -s rules
```

#### Manual Cleanup
If rules persist after the application exits:
```bash
sudo pfctl -a takeover -F all
```

## Linux iptables Issues

### Problem: iptables Permission Denied

**Error Message:**
```
iptables: Permission denied
```

**Solution:**
Ensure the application is run with root privileges:
```bash
sudo ./target/release/takeover -t firefox -p socks5://127.0.0.1:1080
```

### Problem: iptables Module Not Available

**Error Message:**
```
iptables: No chain/target/match by that name
```

**Solution:**
Ensure the `owner` module is available:
```bash
sudo modprobe xt_owner
```

### Checking iptables Rules
To verify active rules:
```bash
sudo iptables -t nat -L OUTPUT | grep takeover
```

### Manual Cleanup
To remove orphaned rules:
```bash
sudo iptables -t nat -D OUTPUT -m owner --pid-owner <PID> -p tcp -j REDIRECT --to-port <PORT>
```

## General Issues

### Issue: Process Not Found

**Symptoms:**
- Target application starts but no interception occurs
- No "Found existing target process" messages in logs

**Solutions:**
1. Check process name matching:
   ```bash
   ps aux | grep <target_application>
   ```

2. Verify path patterns in configuration:
   ```json
   {
     "path_patterns": [
       "**/firefox*",
       "**/Firefox*",
       "/Applications/Firefox.app/**"
     ]
   }
   ```

3. Increase log verbosity:
   ```bash
   RUST_LOG=debug sudo ./target/release/takeover -t firefox -p socks5://127.0.0.1:1080
   ```

### Issue: Proxy Connection Failed

**Symptoms:**
- Application cannot connect to internet
- "Failed to connect to proxy" errors

**Solutions:**
1. Verify proxy is running:
   ```bash
   curl --socks5 127.0.0.1:1080 https://ipinfo.io/ip
   ```

2. Check proxy URL format:
   - SOCKS5: `socks5://127.0.0.1:1080`
   - HTTP: `http://127.0.0.1:8080`

3. Test with simple application first:
   ```bash
   sudo ./target/release/takeover -t curl -p socks5://127.0.0.1:1080
   curl https://ipinfo.io/ip
   ```

### Issue: High CPU Usage

**Symptoms:**
- System becomes slow when takeover is running
- High CPU usage from takeover process

**Solutions:**
1. Reduce monitoring frequency by modifying the interval in `process_monitor.rs`
2. Use more specific process patterns to reduce false matches
3. Consider implementing kqueue (macOS) or inotify (Linux) for event-based monitoring

### Issue: Application Detects Proxy

**Symptoms:**
- Application shows proxy-aware behavior
- Application refuses to connect

**Solutions:**
1. Some applications detect transparent proxies. This is expected behavior.
2. Try different applications known to work with transparent proxies
3. Consider implementing DNS interception for better transparency

## Debugging Commands

### View All Active Network Rules
```bash
# macOS
sudo pfctl -s all

# Linux  
sudo iptables -t nat -L
sudo iptables -L
```

### Monitor Network Connections
```bash
# View connections from specific process
lsof -p <PID> -a -i

# Monitor network activity
sudo tcpdump -i lo0 port 1080
```

### Check Process Information
```bash
# Get process details
ps -p <PID> -o pid,ppid,user,comm,args

# Get executable path
ls -la /proc/<PID>/exe  # Linux
lsof -p <PID> | head -2  # macOS
```

## Known Limitations

1. **Process-specific targeting**: Current implementation affects all traffic from the interface, not just from specific processes
2. **DNS interception**: DNS queries may bypass the proxy depending on application behavior
3. **Application compatibility**: Some applications may detect or refuse transparent proxying
4. **Performance**: Polling-based process monitoring has overhead
5. **Privilege requirements**: Root access required for network interception

## Getting Help

If you encounter issues not covered here:

1. Enable debug logging: `RUST_LOG=debug`
2. Check system logs for additional error messages
3. Verify that both the target application and proxy are working independently
4. Test with simple applications like `curl` first
5. Check platform-specific documentation for pfctl (macOS) or iptables (Linux)
