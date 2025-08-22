#!/bin/bash

# Quick test script for takeover functionality
# This is a simplified version for rapid testing

set -e

BINARY="./target/release/takeover"
TEST_TARGET="curl"
TEST_PROXY="http://127.0.0.1:8080"
TEST_URL="https://ipinfo.io/ip"

echo "⚡ Quick Takeover Test"
echo "===================="

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

print_status() {
    echo -e "${1}${2}${NC}"
}

# Basic checks
if [ ! -f "$BINARY" ]; then
    print_status $RED "❌ Binary not found. Run: cargo build --release"
    exit 1
fi

if [ "$EUID" -ne 0 ]; then
    print_status $RED "❌ Run with sudo"
    exit 1
fi

print_status $GREEN "✅ Ready to test"

# Get baseline
print_status $BLUE "📍 Getting baseline IP..."
BASELINE_IP=$(curl -s --max-time 10 "$TEST_URL" 2>/dev/null | tr -d '\n%' || echo "unknown")
echo "   Baseline: $BASELINE_IP"

# Check if proxy is available
print_status $BLUE "🔍 Checking proxy..."
if curl -s --connect-timeout 3 --proxy "$TEST_PROXY" "$TEST_URL" >/dev/null 2>&1; then
    PROXY_AVAILABLE=true
    print_status $GREEN "   ✅ Proxy working"
else
    PROXY_AVAILABLE=false
    print_status $YELLOW "   ⚠️  Proxy not available (will test interception anyway)"
fi

# Start takeover
print_status $BLUE "🚀 Starting takeover..."
$BINARY -t "$TEST_TARGET" -p "$TEST_PROXY" >/dev/null 2>&1 &
TAKEOVER_PID=$!

# Quick startup check
sleep 2
if ! kill -0 $TAKEOVER_PID 2>/dev/null; then
    print_status $RED "❌ Takeover failed to start"
    exit 1
fi

print_status $GREEN "   ✅ Started (PID: $TAKEOVER_PID)"

# Test curl
print_status $BLUE "🧪 Testing curl..."
RESULT_IP=$(curl -s --max-time 10 "$TEST_URL" 2>/dev/null | tr -d '\n%' || echo "failed")

if [ "$RESULT_IP" = "failed" ]; then
    print_status $RED "   ❌ Curl failed"
    TEST_RESULT="FAILED"
elif [ "$RESULT_IP" = "$BASELINE_IP" ]; then
    print_status $YELLOW "   📊 Same IP as baseline (no interception detected)"
    TEST_RESULT="NO_CHANGE"
else
    print_status $GREEN "   📊 Different IP detected (interception working!)"
    TEST_RESULT="SUCCESS"
fi

echo "   Result: $RESULT_IP"

# Quick cleanup
print_status $BLUE "🧹 Cleaning up..."
kill -TERM $TAKEOVER_PID 2>/dev/null
sleep 2
kill -9 $TAKEOVER_PID 2>/dev/null || true

# Verify cleanup
print_status $BLUE "✅ Verifying cleanup..."
FINAL_IP=$(curl -s --max-time 10 "$TEST_URL" 2>/dev/null | tr -d '\n%' || echo "unknown")

if [ "$FINAL_IP" = "$BASELINE_IP" ]; then
    print_status $GREEN "   ✅ Back to baseline"
else
    print_status $YELLOW "   ⚠️  Different from baseline: $FINAL_IP"
fi

# Summary
echo ""
print_status $BLUE "📊 Quick Test Results"
print_status $BLUE "===================="
echo "Baseline IP:   $BASELINE_IP"
echo "Test IP:       $RESULT_IP"
echo "Final IP:      $FINAL_IP"
echo "Proxy Status:  $([ "$PROXY_AVAILABLE" = true ] && echo "Available" || echo "Not Available")"

case $TEST_RESULT in
    "SUCCESS")
        print_status $GREEN "🎉 SUCCESS: Takeover appears to be working!"
        ;;
    "NO_CHANGE")
        print_status $YELLOW "⚠️  NO CHANGE: Takeover didn't intercept traffic"
        echo "   This could be due to:"
        echo "   • Proxy not available"
        echo "   • Network rules not applied correctly"
        echo "   • Platform-specific issues"
        ;;
    "FAILED")
        print_status $RED "❌ FAILED: Curl couldn't connect"
        echo "   Check network connectivity and proxy availability"
        ;;
esac

echo ""
print_status $BLUE "💡 Next steps:"
echo "   • Full test: sudo ./scripts/test_curl_proxy.sh"
echo "   • Start proxy: ./scripts/start_test_proxy.sh"
echo "   • Manual cleanup: sudo ./scripts/manual_cleanup.sh"
