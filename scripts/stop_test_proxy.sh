#!/bin/bash

# Stop test proxy server

echo "🛑 Stopping test proxy server"
echo "=============================="

STOPPED=false

# Check for PID file
if [ -f proxy.pid ]; then
    PROXY_PID=$(cat proxy.pid)
    echo "Found proxy PID file: $PROXY_PID"
    
    if kill -0 $PROXY_PID 2>/dev/null; then
        echo "Stopping proxy server (PID: $PROXY_PID)..."
        kill $PROXY_PID 2>/dev/null
        
        # Wait for graceful shutdown
        for i in {1..5}; do
            if ! kill -0 $PROXY_PID 2>/dev/null; then
                break
            fi
            sleep 1
        done
        
        # Force kill if still running
        if kill -0 $PROXY_PID 2>/dev/null; then
            echo "Force killing proxy server..."
            kill -9 $PROXY_PID 2>/dev/null
        fi
        
        echo "✅ Proxy server stopped"
        STOPPED=true
    else
        echo "⚠️  Proxy PID $PROXY_PID is not running"
    fi
    
    rm -f proxy.pid
fi

# Look for any processes on port 8080
PORT_PIDS=$(lsof -Pi :8080 -sTCP:LISTEN -t 2>/dev/null || echo "")

if [ -n "$PORT_PIDS" ]; then
    echo "Found processes on port 8080: $PORT_PIDS"
    
    for pid in $PORT_PIDS; do
        # Get process info
        PROCESS_INFO=$(ps -p $pid -o comm= 2>/dev/null || echo "unknown")
        echo "Stopping process $pid ($PROCESS_INFO)..."
        
        kill $pid 2>/dev/null
        sleep 1
        
        if kill -0 $pid 2>/dev/null; then
            echo "Force killing process $pid..."
            kill -9 $pid 2>/dev/null
        fi
    done
    
    STOPPED=true
fi

# Look for our proxy-related processes
PROXY_PROCESSES=$(pgrep -f "proxy_server.py\|socat.*8080" || echo "")

if [ -n "$PROXY_PROCESSES" ]; then
    echo "Found proxy-related processes: $PROXY_PROCESSES"
    
    for pid in $PROXY_PROCESSES; do
        echo "Stopping proxy process $pid..."
        kill $pid 2>/dev/null
        sleep 1
        
        if kill -0 $pid 2>/dev/null; then
            kill -9 $pid 2>/dev/null
        fi
    done
    
    STOPPED=true
fi

# Cleanup files
rm -f proxy_server.py proxy.log

# Final check
echo ""
echo "🔍 Verifying port 8080 is free..."
if lsof -Pi :8080 -sTCP:LISTEN -t >/dev/null 2>&1; then
    echo "⚠️  Port 8080 is still in use:"
    lsof -Pi :8080 -sTCP:LISTEN
    echo ""
    echo "💡 You may need to manually kill these processes:"
    echo "   sudo lsof -Pi :8080 -sTCP:LISTEN -t | xargs kill -9"
else
    echo "✅ Port 8080 is now free"
fi

if [ "$STOPPED" = true ]; then
    echo ""
    echo "🏁 Proxy cleanup completed!"
else
    echo ""
    echo "ℹ️  No proxy server was found running"
fi
