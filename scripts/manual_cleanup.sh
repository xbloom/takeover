#!/bin/bash

# Manual cleanup script for Takeover network rules
# Use this if the application didn't clean up properly

echo "🧹 Takeover Manual Cleanup Script"
echo "================================="

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    echo "❌ This script requires root privileges. Please run with sudo."
    exit 1
fi

CLEANED=false

# macOS cleanup (pfctl)
if [[ "$OSTYPE" == "darwin"* ]] && command -v pfctl > /dev/null; then
    echo "🍎 Detected macOS, cleaning up pfctl rules..."
    
    # Check for existing rules
    EXISTING_RULES=$(pfctl -a takeover -s rules 2>/dev/null | wc -l | tr -d ' ')
    
    if [ "$EXISTING_RULES" -gt 0 ]; then
        echo "   Found $EXISTING_RULES rules in takeover anchor"
        
        # Flush all rules in takeover anchor
        if pfctl -a takeover -F all 2>/dev/null; then
            echo "   ✅ Successfully flushed pfctl rules"
            CLEANED=true
        else
            echo "   ❌ Failed to flush pfctl rules"
        fi
        
        # Also try to flush specific rule types
        pfctl -a takeover -F nat 2>/dev/null
        pfctl -a takeover -F rules 2>/dev/null
        
    else
        echo "   ℹ️  No pfctl rules found in takeover anchor"
    fi
    
    # Show current status
    echo "   Current rules in takeover anchor:"
    pfctl -a takeover -s all 2>/dev/null | head -10 | sed 's/^/   /'
    
fi

# Linux cleanup (iptables)
if [[ "$OSTYPE" == "linux-gnu"* ]] && command -v iptables > /dev/null; then
    echo "🐧 Detected Linux, cleaning up iptables rules..."
    
    # Check for existing rules with takeover comment
    EXISTING_RULES=$(iptables -t nat -L OUTPUT 2>/dev/null | grep -c takeover || echo "0")
    
    if [ "$EXISTING_RULES" -gt 0 ]; then
        echo "   Found $EXISTING_RULES iptables rules with takeover comment"
        
        # Get line numbers of rules containing "takeover" and remove them
        iptables -t nat -L OUTPUT --line-numbers 2>/dev/null | grep takeover | while read line; do
            RULE_NUM=$(echo "$line" | awk '{print $1}')
            if [ -n "$RULE_NUM" ] && [ "$RULE_NUM" -gt 0 ] 2>/dev/null; then
                if iptables -t nat -D OUTPUT "$RULE_NUM" 2>/dev/null; then
                    echo "   ✅ Removed iptables rule $RULE_NUM"
                    CLEANED=true
                else
                    echo "   ❌ Failed to remove iptables rule $RULE_NUM"
                fi
            fi
        done
        
        # Alternative approach: remove by rule specification
        echo "   Attempting alternative cleanup method..."
        
        # List all OUTPUT rules and find takeover ones
        iptables -t nat -S OUTPUT 2>/dev/null | grep takeover | while read rule; do
            # Convert -A to -D to delete the rule
            DELETE_RULE=$(echo "$rule" | sed 's/-A OUTPUT/-D OUTPUT/')
            if iptables -t nat $DELETE_RULE 2>/dev/null; then
                echo "   ✅ Removed rule: $DELETE_RULE"
                CLEANED=true
            fi
        done
        
    else
        echo "   ℹ️  No iptables rules found with takeover comment"
    fi
    
    # Show current status
    echo "   Current NAT OUTPUT rules:"
    iptables -t nat -L OUTPUT 2>/dev/null | head -10 | sed 's/^/   /'
    
fi

# Generic cleanup - look for takeover processes
echo ""
echo "🔍 Checking for running takeover processes..."
TAKEOVER_PIDS=$(pgrep -f takeover || echo "")

if [ -n "$TAKEOVER_PIDS" ]; then
    echo "   Found running takeover processes: $TAKEOVER_PIDS"
    echo "   Terminating processes..."
    
    for pid in $TAKEOVER_PIDS; do
        if kill -TERM "$pid" 2>/dev/null; then
            echo "   ✅ Sent SIGTERM to PID $pid"
            sleep 2
            
            # Check if still running
            if kill -0 "$pid" 2>/dev/null; then
                echo "   ⚠️  Process $pid still running, force killing..."
                kill -9 "$pid" 2>/dev/null
            fi
        else
            echo "   ❌ Failed to terminate PID $pid"
        fi
    done
    
    CLEANED=true
else
    echo "   ℹ️  No running takeover processes found"
fi

# Final status
echo ""
if [ "$CLEANED" = true ]; then
    echo "✅ Cleanup completed! Some rules/processes were found and removed."
else
    echo "ℹ️  No cleanup was necessary - system appears clean."
fi

echo ""
echo "🔍 Verification:"
echo "   To verify cleanup was successful, check:"

if [[ "$OSTYPE" == "darwin"* ]]; then
    echo "   • pfctl rules: sudo pfctl -a takeover -s all"
elif [[ "$OSTYPE" == "linux-gnu"* ]]; then
    echo "   • iptables rules: sudo iptables -t nat -L OUTPUT | grep takeover"
fi

echo "   • running processes: pgrep -f takeover"

echo ""
echo "🏁 Manual cleanup script completed!"
