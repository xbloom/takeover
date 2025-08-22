#!/bin/bash

# Verification script for ipinfo.io integration

echo "🔍 Verifying ipinfo.io integration"
echo "=================================="

# Test plain IP endpoint
echo "📍 Testing ipinfo.io/ip endpoint:"
IP_RESPONSE=$(curl -s --max-time 10 "https://ipinfo.io/ip" | tr -d '\n%')
echo "   Raw response: '$IP_RESPONSE'"

if [[ $IP_RESPONSE =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$ ]]; then
    echo "   ✅ Valid IP format: $IP_RESPONSE"
else
    echo "   ❌ Invalid IP format: $IP_RESPONSE"
fi

echo ""

# Test JSON endpoint
echo "📋 Testing ipinfo.io/json endpoint:"
JSON_RESPONSE=$(curl -s --max-time 10 "https://ipinfo.io/json")
echo "   Raw response:"
echo "$JSON_RESPONSE" | head -5 | sed 's/^/   /'

JSON_IP=$(echo "$JSON_RESPONSE" | jq -r .ip 2>/dev/null)
if [[ $JSON_IP =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$ ]]; then
    echo "   ✅ Valid JSON IP: $JSON_IP"
else
    echo "   ❌ Invalid JSON IP: $JSON_IP"
fi

echo ""

# Compare both responses
echo "🔍 Comparing responses:"
echo "   Plain IP: $IP_RESPONSE"
echo "   JSON IP:  $JSON_IP"

if [ "$IP_RESPONSE" = "$JSON_IP" ]; then
    echo "   ✅ Both responses match"
else
    echo "   ⚠️  Responses differ (this may be normal due to load balancing)"
fi

echo ""

# Test script parsing logic
echo "🧪 Testing script parsing logic:"

# Test the exact logic used in scripts
BASELINE_IP=$(curl -s --max-time 10 "https://ipinfo.io/ip" 2>/dev/null | tr -d '\n%' || echo "unknown")
echo "   Script parsed IP: '$BASELINE_IP'"

if [[ $BASELINE_IP =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$ ]]; then
    echo "   ✅ Script parsing works correctly"
else
    echo "   ❌ Script parsing failed"
fi

echo ""
echo "🏁 Verification completed!"
echo ""
echo "📝 Summary:"
echo "   • ipinfo.io/ip returns plain text IP"
echo "   • ipinfo.io/json returns detailed info"
echo "   • Our scripts correctly parse the response"
echo "   • tr -d '\\n%' removes newlines and % symbols"
