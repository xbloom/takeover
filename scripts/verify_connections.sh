#!/bin/bash

# Verify if connections actually go through our proxy

echo "🔍 Verifying Real Proxy Connections"
echo "==================================="

if [ "$EUID" -ne 0 ]; then
    echo "❌ Run with sudo"
    exit 1
fi

echo "🚀 Step 1: Start takeover with verbose logging"
RUST_LOG=debug ./target/release/takeover -t curl -p http://127.0.0.1:8080 > takeover_verbose.log 2>&1 &
TAKEOVER_PID=$!
echo "   Takeover PID: $TAKEOVER_PID"

echo "⏳ Step 2: Wait for initialization"
sleep 3

echo "🔍 Step 3: Check what's actually listening on port 10000"
echo "   Checking port 10000:"
lsof -i :10000 || echo "   ❌ No service on port 10000!"

echo ""
echo "🔍 Step 4: Check takeover processes"
ps aux | grep takeover | grep -v grep

echo ""
echo "🌐 Step 5: Test curl and monitor connections"
echo "   Running curl in background..."

# Start curl and capture its PID
curl -v https://ipinfo.io/ip > curl_result.txt 2> curl_verbose.log &
CURL_PID=$!
echo "   Curl PID: $CURL_PID"

# Monitor for a few seconds
echo "   Monitoring for 5 seconds..."
for i in {1..5}; do
    echo "   ${i}s: Checking port 10000..."
    lsof -i :10000 2>/dev/null || echo "     Still no service on 10000"
    sleep 1
done

wait $CURL_PID

echo ""
echo "📋 Step 6: Analyze takeover logs"
echo "   Recent takeover log (last 20 lines):"
tail -20 takeover_verbose.log | sed 's/^/   /'

echo ""
echo "📋 Step 7: Check for proxy-related logs"
echo "   Searching for proxy/connection logs:"
grep -i "proxy\|connection\|server\|listen" takeover_verbose.log | sed 's/^/   /' || echo "   ❌ No proxy/connection logs found!"

echo ""
echo "📋 Step 8: Check curl verbose output"
echo "   Curl connection details:"
head -10 curl_verbose.log | sed 's/^/   /'

echo ""
echo "📊 Step 9: Results"
CURL_RESULT=$(cat curl_result.txt | tr -d '\n%')
echo "   Curl result IP: $CURL_RESULT"

# Check baseline
BASELINE_IP=$(curl -s --max-time 5 https://ipinfo.io/ip | tr -d '\n%')
echo "   Baseline IP: $BASELINE_IP"

if [ "$CURL_RESULT" = "$BASELINE_IP" ]; then
    echo "   📊 Result: SAME IP - No proxying detected!"
else
    echo "   📊 Result: DIFFERENT IP - But need to verify if through our proxy"
fi

echo ""
echo "🧹 Cleanup"
kill -TERM $TAKEOVER_PID 2>/dev/null
sleep 2
kill -9 $TAKEOVER_PID 2>/dev/null || true

echo ""
echo "🔍 Final Analysis"
echo "=================="

# Count proxy-related log entries
PROXY_LOGS=$(grep -c -i "proxy\|connection\|server\|listen" takeover_verbose.log 2>/dev/null || echo "0")
echo "   Proxy-related log entries: $PROXY_LOGS"

# Check if port 10000 was ever used
PORT_USAGE=$(lsof -i :10000 2>/dev/null | wc -l)
echo "   Port 10000 usage: $PORT_USAGE"

if [ "$PROXY_LOGS" -eq 0 ]; then
    echo ""
    echo "❌ PROBLEM FOUND: No proxy connection logs!"
    echo "   This means:"
    echo "   • proxy_manager may not be starting local server"
    echo "   • OR network rules are not working"
    echo "   • OR traffic is not being redirected"
    echo ""
    echo "🔧 Suggestions:"
    echo "   1. Check proxy_manager.rs start_local_proxy_server()"
    echo "   2. Verify pfctl rules are actually redirecting traffic"
    echo "   3. Test manual connection to port 10000"
fi

echo ""
echo "📁 Debug files created:"
echo "   - takeover_verbose.log: Full takeover logs"
echo "   - curl_verbose.log: Curl connection details"
echo "   - curl_result.txt: Curl result"
