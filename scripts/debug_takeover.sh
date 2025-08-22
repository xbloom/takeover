#!/bin/bash

# Debug script for takeover issues

echo "🔍 Takeover Debug Analysis"
echo "=========================="

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    echo "❌ This script requires root privileges. Please run with sudo."
    exit 1
fi

BINARY="./target/release/takeover"
TEST_TARGET="curl"
TEST_PROXY="http://127.0.0.1:8080"

echo "📋 System Information:"
echo "   OS: $(uname -s) $(uname -r)"
echo "   Platform: $(uname -m)"
echo ""

echo "🔍 Step 1: Check takeover binary"
if [ -f "$BINARY" ]; then
    echo "   ✅ Binary exists: $BINARY"
else
    echo "   ❌ Binary not found: $BINARY"
    echo "   Run: cargo build --release"
    exit 1
fi

echo ""
echo "🔍 Step 2: Check proxy availability"
if curl --connect-timeout 3 --proxy "$TEST_PROXY" --silent "https://ipinfo.io/ip" > /dev/null 2>&1; then
    echo "   ✅ Proxy is working"
    PROXY_IP=$(curl --connect-timeout 3 --proxy "$TEST_PROXY" --silent "https://ipinfo.io/ip" | tr -d '\n%')
    echo "   Proxy IP: $PROXY_IP"
else
    echo "   ⚠️  Proxy not available, starting test proxy..."
    ./scripts/start_test_proxy.sh &
    PROXY_PID=$!
    sleep 3
    echo "   Test proxy started (PID: $PROXY_PID)"
fi

echo ""
echo "🔍 Step 3: Start takeover with debug logging"
echo "   Starting takeover with RUST_LOG=debug..."

# Start takeover with debug logging
RUST_LOG=debug $BINARY -t "$TEST_TARGET" -p "$TEST_PROXY" > takeover_debug.log 2>&1 &
TAKEOVER_PID=$!

echo "   Takeover PID: $TAKEOVER_PID"
echo "   Waiting for startup..."
sleep 3

# Check if takeover is still running
if ! kill -0 $TAKEOVER_PID 2>/dev/null; then
    echo "   ❌ Takeover failed to start or crashed"
    echo "   Log output:"
    cat takeover_debug.log | head -20 | sed 's/^/   /'
    exit 1
else
    echo "   ✅ Takeover is running"
fi

echo ""
echo "🔍 Step 4: Check network rules"

# Check pfctl rules (macOS)
if command -v pfctl > /dev/null; then
    echo "   📋 pfctl rules in takeover anchor:"
    PFCTL_RULES=$(pfctl -a takeover -s rules 2>/dev/null | wc -l | tr -d ' ')
    echo "   Found $PFCTL_RULES rules"
    
    if [ "$PFCTL_RULES" -gt 0 ]; then
        echo "   Rules details:"
        pfctl -a takeover -s rules 2>/dev/null | sed 's/^/   /'
    else
        echo "   ⚠️  No pfctl rules found in takeover anchor"
    fi
    
    # Check if pfctl is enabled
    PFCTL_STATUS=$(pfctl -s info 2>/dev/null | grep "Status:" | awk '{print $2}')
    echo "   pfctl status: $PFCTL_STATUS"
fi

# Check iptables rules (Linux)
if command -v iptables > /dev/null; then
    echo "   📋 iptables rules with takeover:"
    IPTABLES_RULES=$(iptables -t nat -L OUTPUT 2>/dev/null | grep -c takeover || echo "0")
    echo "   Found $IPTABLES_RULES rules"
    
    if [ "$IPTABLES_RULES" -gt 0 ]; then
        echo "   Rules details:"
        iptables -t nat -L OUTPUT 2>/dev/null | grep takeover | sed 's/^/   /'
    fi
fi

echo ""
echo "🔍 Step 5: Test curl and monitor processes"

# Start curl in background and monitor
echo "   Starting curl test..."
curl -s "https://ipinfo.io/ip" > curl_result.txt &
CURL_PID=$!

echo "   Curl PID: $CURL_PID"

# Wait a moment for curl to start
sleep 1

# Check if takeover detected the curl process
echo "   Checking takeover logs for curl detection..."
if grep -q "curl" takeover_debug.log 2>/dev/null; then
    echo "   ✅ Takeover detected curl process"
    grep "curl" takeover_debug.log | tail -3 | sed 's/^/   /'
else
    echo "   ⚠️  No curl detection found in logs"
fi

# Wait for curl to complete
wait $CURL_PID
CURL_RESULT=$(cat curl_result.txt | tr -d '\n%')
echo "   Curl result: $CURL_RESULT"

echo ""
echo "🔍 Step 6: Analyze logs"
echo "   Recent takeover log entries:"
tail -10 takeover_debug.log | sed 's/^/   /'

echo ""
echo "🔍 Step 7: Check process detection"
echo "   Current curl processes:"
pgrep -f curl | while read pid; do
    echo "   PID: $pid"
    ps -p $pid -o pid,comm,args | tail -1 | sed 's/^/   /'
done

echo ""
echo "🔍 Step 8: Manual process path check"
if command -v lsof > /dev/null; then
    echo "   Checking curl executable paths:"
    CURL_PATH=$(which curl)
    echo "   curl location: $CURL_PATH"
    
    # Check if this matches our patterns
    echo "   Pattern matching test:"
    echo "   Path: $CURL_PATH"
    echo "   Matches **/curl*: $(echo $CURL_PATH | grep -q 'curl' && echo 'YES' || echo 'NO')"
fi

echo ""
echo "🧹 Cleanup"
echo "   Stopping takeover..."
kill -TERM $TAKEOVER_PID 2>/dev/null
sleep 2
kill -9 $TAKEOVER_PID 2>/dev/null || true

if [ -n "$PROXY_PID" ]; then
    echo "   Stopping test proxy..."
    kill $PROXY_PID 2>/dev/null || true
fi

echo ""
echo "📊 Debug Summary"
echo "================"
echo "   Takeover started: $([ -n "$TAKEOVER_PID" ] && echo 'YES' || echo 'NO')"
echo "   Network rules found: $([ "$PFCTL_RULES" -gt 0 ] 2>/dev/null && echo "$PFCTL_RULES pfctl rules" || echo 'NO')"
echo "   Process detection: $(grep -q 'curl' takeover_debug.log 2>/dev/null && echo 'YES' || echo 'NO')"
echo "   Curl result: $CURL_RESULT"

echo ""
echo "🔧 Possible Issues and Solutions:"

# Analyze common issues
if [ "$PFCTL_RULES" -eq 0 ] 2>/dev/null; then
    echo "   ❌ No pfctl rules detected"
    echo "      - Check if pfctl is enabled: sudo pfctl -e"
    echo "      - Verify rule syntax in network_interceptor.rs"
fi

if ! grep -q 'curl' takeover_debug.log 2>/dev/null; then
    echo "   ❌ Curl process not detected"
    echo "      - Check process name matching in config"
    echo "      - Verify path patterns match curl location"
    echo "      - Check sysinfo process enumeration"
fi

if [ "$PFCTL_STATUS" != "Enabled" ] 2>/dev/null; then
    echo "   ❌ pfctl not enabled"
    echo "      - Run: sudo pfctl -e"
fi

echo ""
echo "📁 Debug files created:"
echo "   - takeover_debug.log: Detailed takeover logs"
echo "   - curl_result.txt: Curl test result"
echo ""
echo "🔍 Next steps:"
echo "   - Review takeover_debug.log for detailed error messages"
echo "   - Check specific issues mentioned above"
echo "   - Run with more specific debugging if needed"

# Cleanup temp files
rm -f curl_result.txt
