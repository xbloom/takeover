use crate::config::{ProxyConfig, ProxyType};
use crate::error::TakeoverError;
use crate::utils::parse_proxy_url;
use anyhow::Result;
use std::collections::HashMap;
use std::net::SocketAddr;
// use std::io::{Read, Write}; // Commented out unused imports
use tokio::net::TcpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt, split};
use tracing::{info, warn, error, debug};

#[derive(Debug, Clone)]
pub struct ProxyEndpoint {
    pub local_port: u16,
    pub proxy_config: ProxyConfig,
    pub target_pid: u32,
}

pub struct ProxyManager {
    process_proxies: HashMap<u32, ProxyEndpoint>,
    active_listeners: HashMap<u16, tokio::task::JoinHandle<()>>,
    next_port: u16,
}

impl ProxyManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            process_proxies: HashMap::new(),
            active_listeners: HashMap::new(),
            next_port: 10000, // Starting port for local proxy listeners
        })
    }

    pub fn add_process_proxy(&mut self, pid: u32, proxy_config: &ProxyConfig) -> Result<()> {
        info!("Adding proxy for process PID: {}", pid);
        
        let local_port = self.get_next_available_port()?;
        
        let endpoint = ProxyEndpoint {
            local_port,
            proxy_config: proxy_config.clone(),
            target_pid: pid,
        };
        
        // Start local proxy server
        self.start_local_proxy_server(&endpoint)?;
        
        self.process_proxies.insert(pid, endpoint);
        
        Ok(())
    }

    pub fn remove_process_proxy(&mut self, pid: u32) -> Result<()> {
        if let Some(endpoint) = self.process_proxies.remove(&pid) {
            info!("Removing proxy for process PID: {}", pid);
            
            // Stop the local proxy server
            if let Some(handle) = self.active_listeners.remove(&endpoint.local_port) {
                handle.abort();
            }
        }
        
        Ok(())
    }

    pub fn get_local_proxy_port(&self, pid: u32) -> Option<u16> {
        self.process_proxies.get(&pid).map(|endpoint| endpoint.local_port)
    }

    fn get_next_available_port(&mut self) -> Result<u16> {
        let mut port = self.next_port;
        
        // Try to find an available port
        for _ in 0..1000 {
            if !self.active_listeners.contains_key(&port) {
                if self.is_port_available(port) {
                    self.next_port = port + 1;
                    return Ok(port);
                }
            }
            port += 1;
        }
        
        Err(TakeoverError::Proxy("Could not find available port".to_string()).into())
    }

    fn is_port_available(&self, port: u16) -> bool {
        match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(_) => true,
            Err(_) => false,
        }
    }

    fn start_local_proxy_server(&mut self, endpoint: &ProxyEndpoint) -> Result<()> {
        let endpoint_clone = endpoint.clone();
        
        let handle = tokio::spawn(async move {
            if let Err(e) = Self::run_proxy_server(endpoint_clone).await {
                error!("Proxy server error: {}", e);
            }
        });
        
        self.active_listeners.insert(endpoint.local_port, handle);
        
        Ok(())
    }

    async fn run_proxy_server(endpoint: ProxyEndpoint) -> Result<()> {
        let addr = SocketAddr::from(([127, 0, 0, 1], endpoint.local_port));
        let listener = TcpListener::bind(addr).await?;
        
        info!("Proxy server listening on {}", addr);
        
        loop {
            match listener.accept().await {
                Ok((inbound, _)) => {
                    let endpoint_clone = endpoint.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_connection(inbound, endpoint_clone).await {
                            debug!("Connection handling error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                    break;
                }
            }
        }
        
        Ok(())
    }

    async fn handle_connection(
        inbound: tokio::net::TcpStream,
        endpoint: ProxyEndpoint,
    ) -> Result<()> {
        debug!("Handling new connection for PID: {}", endpoint.target_pid);
        
        // Connect to the actual proxy
        let outbound = Self::connect_to_proxy(&endpoint.proxy_config).await?;
        
        // Start bidirectional data transfer
        let (mut ri, mut wi) = split(inbound);
        let (mut ro, mut wo) = split(outbound);
        
        let client_to_proxy = tokio::spawn(async move {
            let mut buffer = vec![0; 4096];
            loop {
                match ri.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if wo.write_all(&buffer[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        
        let proxy_to_client = tokio::spawn(async move {
            let mut buffer = vec![0; 4096];
            loop {
                match ro.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if wi.write_all(&buffer[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        
        // Wait for either direction to close
        tokio::select! {
            _ = client_to_proxy => {},
            _ = proxy_to_client => {},
        }
        
        debug!("Connection closed for PID: {}", endpoint.target_pid);
        Ok(())
    }

    async fn connect_to_proxy(proxy_config: &ProxyConfig) -> Result<tokio::net::TcpStream> {
        let (_scheme, host, port, auth) = parse_proxy_url(&proxy_config.url)?;
        
        let addr = format!("{}:{}", host, port);
        let mut stream = tokio::net::TcpStream::connect(&addr).await?;
        
        match proxy_config.proxy_type {
            ProxyType::Socks5 => {
                Self::socks5_handshake(&mut stream, auth.as_ref()).await?;
            }
            ProxyType::Socks4 => {
                Self::socks4_handshake(&mut stream).await?;
            }
            ProxyType::Http | ProxyType::Https => {
                // For HTTP proxies, we'll handle the CONNECT method
                // This is more complex and would require parsing HTTP requests
                warn!("HTTP proxy support not fully implemented yet");
            }
        }
        
        Ok(stream)
    }

    async fn socks5_handshake(
        stream: &mut tokio::net::TcpStream,
        auth: Option<&(String, String)>,
    ) -> Result<()> {
        // SOCKS5 greeting
        let greeting = if auth.is_some() {
            vec![0x05, 0x02, 0x00, 0x02] // Version 5, 2 methods: no auth + username/password
        } else {
            vec![0x05, 0x01, 0x00] // Version 5, 1 method: no auth
        };
        
        stream.write_all(&greeting).await?;
        
        let mut response = vec![0; 2];
        stream.read_exact(&mut response).await?;
        
        if response[0] != 0x05 {
            return Err(TakeoverError::Proxy("Invalid SOCKS5 response".to_string()).into());
        }
        
        match response[1] {
            0x00 => {
                // No authentication required
            }
            0x02 => {
                // Username/password authentication
                if let Some((username, password)) = auth {
                    Self::socks5_auth(stream, username, password).await?;
                } else {
                    return Err(TakeoverError::Proxy("SOCKS5 server requires authentication".to_string()).into());
                }
            }
            0xFF => {
                return Err(TakeoverError::Proxy("SOCKS5 server rejected all authentication methods".to_string()).into());
            }
            _ => {
                return Err(TakeoverError::Proxy("Unsupported SOCKS5 authentication method".to_string()).into());
            }
        }
        
        Ok(())
    }

    async fn socks5_auth(
        stream: &mut tokio::net::TcpStream,
        username: &str,
        password: &str,
    ) -> Result<()> {
        let mut auth_request = vec![0x01]; // Version 1
        auth_request.push(username.len() as u8);
        auth_request.extend_from_slice(username.as_bytes());
        auth_request.push(password.len() as u8);
        auth_request.extend_from_slice(password.as_bytes());
        
        stream.write_all(&auth_request).await?;
        
        let mut response = vec![0; 2];
        stream.read_exact(&mut response).await?;
        
        if response[1] != 0x00 {
            return Err(TakeoverError::Proxy("SOCKS5 authentication failed".to_string()).into());
        }
        
        Ok(())
    }

    async fn socks4_handshake(_stream: &mut tokio::net::TcpStream) -> Result<()> {
        // SOCKS4 is simpler but less commonly used
        // Implementation would go here
        warn!("SOCKS4 handshake not fully implemented yet");
        Ok(())
    }
}
