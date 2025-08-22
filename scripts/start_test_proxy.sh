#!/bin/bash

# Simple HTTP proxy server for testing
# Starts a basic HTTP proxy on port 8080

PORT=8080
PROXY_LOG="proxy.log"

echo "🌐 Starting test HTTP proxy on port $PORT"
echo "=========================================="

# Check if port is already in use
if lsof -Pi :$PORT -sTCP:LISTEN -t >/dev/null 2>&1; then
    echo "⚠️  Port $PORT is already in use"
    echo "   Current process using port $PORT:"
    lsof -Pi :$PORT -sTCP:LISTEN
    echo ""
    read -p "Kill existing process and continue? (y/N): " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        echo "Killing process on port $PORT..."
        lsof -Pi :$PORT -sTCP:LISTEN -t | xargs kill -9 2>/dev/null || true
        sleep 2
    else
        echo "Exiting..."
        exit 1
    fi
fi

# Method 1: Try using Python's http.server with proxy capabilities
if command -v python3 > /dev/null; then
    echo "🐍 Starting Python HTTP proxy server..."
    cat > proxy_server.py << 'EOF'
#!/usr/bin/env python3
import http.server
import socketserver
import urllib.request
import urllib.parse
import ssl
from http.client import HTTPConnection, HTTPSConnection
import socket

class ProxyHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.proxy_request()
    
    def do_POST(self):
        self.proxy_request()
    
    def do_CONNECT(self):
        # Handle HTTPS CONNECT method
        host, port = self.path.split(':')
        port = int(port)
        
        try:
            # Create connection to target
            target_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            target_sock.connect((host, port))
            
            # Send success response
            self.send_response(200, 'Connection Established')
            self.end_headers()
            
            # Relay data between client and target
            self.relay_data(self.connection, target_sock)
            
        except Exception as e:
            self.send_error(500, f'Connection failed: {e}')
    
    def proxy_request(self):
        try:
            # Parse the request URL
            url = self.path
            if not url.startswith('http'):
                url = 'http://' + self.headers.get('Host', '') + url
            
            # Create the request
            req = urllib.request.Request(url, data=None)
            
            # Copy headers
            for header, value in self.headers.items():
                if header.lower() not in ['host', 'connection']:
                    req.add_header(header, value)
            
            # Make the request
            response = urllib.request.urlopen(req, timeout=30)
            
            # Send response
            self.send_response(response.getcode())
            for header, value in response.headers.items():
                if header.lower() not in ['connection', 'transfer-encoding']:
                    self.send_header(header, value)
            self.end_headers()
            
            # Copy response body
            self.wfile.write(response.read())
            
        except Exception as e:
            self.send_error(500, f'Proxy error: {e}')
    
    def relay_data(self, client_sock, target_sock):
        import select
        sockets = [client_sock, target_sock]
        
        while True:
            ready, _, _ = select.select(sockets, [], [], 1)
            if not ready:
                break
                
            for sock in ready:
                try:
                    data = sock.recv(4096)
                    if not data:
                        return
                    
                    if sock is client_sock:
                        target_sock.send(data)
                    else:
                        client_sock.send(data)
                except:
                    return
    
    def log_message(self, format, *args):
        print(f"[{self.address_string()}] {format % args}")

if __name__ == "__main__":
    PORT = 8080
    with socketserver.TCPServer(("127.0.0.1", PORT), ProxyHandler) as httpd:
        print(f"HTTP Proxy serving on port {PORT}")
        print(f"Test with: curl --proxy http://127.0.0.1:{PORT} https://ipinfo.io/ip")
        print("Press Ctrl+C to stop")
        try:
            httpd.serve_forever()
        except KeyboardInterrupt:
            print("\nShutting down proxy server...")
EOF

    echo "Starting Python proxy server on port $PORT..."
    echo "Log will be saved to $PROXY_LOG"
    python3 proxy_server.py > "$PROXY_LOG" 2>&1 &
    PROXY_PID=$!
    
    # Wait a moment and check if it started successfully
    sleep 2
    if kill -0 $PROXY_PID 2>/dev/null; then
        echo "✅ Python proxy server started successfully (PID: $PROXY_PID)"
        echo "   Test with: curl --proxy http://127.0.0.1:$PORT https://ipinfo.io/ip"
        echo "   Stop with: kill $PROXY_PID"
        
        # Save PID for easy cleanup
        echo $PROXY_PID > proxy.pid
        
        echo ""
        echo "🔍 Testing proxy..."
        if curl --connect-timeout 5 --proxy "http://127.0.0.1:$PORT" --silent --head "https://ipinfo.io/ip" > /dev/null 2>&1; then
            echo "✅ Proxy is working correctly!"
        else
            echo "⚠️  Proxy test failed, but it might still work for basic HTTP requests"
        fi
        
        echo ""
        echo "📋 Proxy is ready for testing with takeover"
        echo "   Run: sudo ./scripts/test_curl_proxy.sh"
        
    else
        echo "❌ Failed to start Python proxy server"
        cat "$PROXY_LOG"
    fi

# Method 2: Try using socat if available
elif command -v socat > /dev/null; then
    echo "🔧 Starting socat HTTP proxy..."
    echo "Note: This is a simple forwarder, not a full HTTP proxy"
    
    socat TCP-LISTEN:$PORT,fork,reuseaddr TCP:ipinfo.io:80 > "$PROXY_LOG" 2>&1 &
    PROXY_PID=$!
    
    sleep 1
    if kill -0 $PROXY_PID 2>/dev/null; then
        echo "✅ socat proxy started (PID: $PROXY_PID)"
        echo $PROXY_PID > proxy.pid
    else
        echo "❌ Failed to start socat proxy"
    fi

# Method 3: Try using netcat
elif command -v nc > /dev/null; then
    echo "🔧 Starting netcat proxy (basic)..."
    echo "Note: This is very limited, mainly for demonstration"
    
    # This is very basic and won't work for HTTPS
    while true; do
        echo "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nProxy response from port $PORT" | nc -l $PORT
    done > "$PROXY_LOG" 2>&1 &
    PROXY_PID=$!
    
    echo "✅ Basic netcat responder started (PID: $PROXY_PID)"
    echo $PROXY_PID > proxy.pid

else
    echo "❌ No suitable proxy tools found"
    echo "   Please install one of: python3, socat, nc"
    echo "   Or manually start an HTTP proxy on port $PORT"
    echo ""
    echo "Alternative suggestions:"
    echo "   • Install mitmproxy: pip install mitmproxy && mitmproxy --listen-port $PORT"
    echo "   • Install squid proxy"
    echo "   • Use any HTTP proxy software"
    exit 1
fi

echo ""
echo "🏁 Proxy setup completed!"
echo ""
echo "📝 Usage:"
echo "   • Test proxy: curl --proxy http://127.0.0.1:$PORT https://ipinfo.io/ip"
echo "   • Run takeover test: sudo ./scripts/test_curl_proxy.sh"
echo "   • Stop proxy: ./scripts/stop_test_proxy.sh"
echo "   • View proxy logs: tail -f $PROXY_LOG"

# Cleanup function for graceful shutdown
cleanup() {
    echo ""
    echo "🧹 Stopping proxy server..."
    if [ -n "$PROXY_PID" ] && kill -0 $PROXY_PID 2>/dev/null; then
        kill $PROXY_PID
        echo "✅ Proxy server stopped"
    fi
    rm -f proxy_server.py proxy.pid
    exit 0
}

# Set up signal handlers
trap cleanup SIGINT SIGTERM

# If running interactively, wait for Ctrl+C
if [ -t 0 ]; then
    echo ""
    echo "Press Ctrl+C to stop the proxy server..."
    wait
else
    echo ""
    echo "Running in background. Stop with: ./scripts/stop_test_proxy.sh"
fi
