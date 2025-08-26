use crate::config::ProxyConfig;
use crate::socks5::{Socks5Client, DataForwarder};
// TrafficStats 通过方法返回值使用，不需要直接导入
use crate::routing_engine::{RoutingEngine, RoutingDecision};
use crate::transparent_proxy_manager::TransparentProxyManager;
use crate::user_manager::UserManager;
use anyhow::Result;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};

use tracing::{info, warn, error, debug};

const PEEK_TIMEOUT_MS: u64 = 500;
const PEEK_BUFFER_SIZE: usize = 4096;
const PROXY_BUFFER_SIZE: usize = 8192;
const DEFAULT_HTTP_PORT: u16 = 80;
const DEFAULT_HTTPS_PORT: u16 = 443;

/// 透明代理服务器 - 核心的连接处理器
pub struct TransparentServer {
    proxy_config: ProxyConfig,
    routing_engine: RoutingEngine,
    proxy_manager: TransparentProxyManager,
    user_manager: UserManager,
    local_listener: Option<TcpListener>,
    local_port: Option<u16>,
}

impl TransparentServer {
    pub fn new(proxy_config: ProxyConfig, routing_engine: RoutingEngine) -> Self {
        Self {
            proxy_config,
            routing_engine,
            proxy_manager: TransparentProxyManager::new(),
            user_manager: UserManager::new(),
            local_listener: None,
            local_port: None,
        }
    }

    /// 使用自定义用户管理器创建透明代理服务器
    pub fn with_user_manager(proxy_config: ProxyConfig, routing_engine: RoutingEngine, user_manager: UserManager) -> Self {
        Self {
            proxy_config,
            routing_engine,
            proxy_manager: TransparentProxyManager::new(),
            user_manager,
            local_listener: None,
            local_port: None,
        }
    }

    /// 获取用户管理器的引用
    pub fn user_manager(&self) -> &UserManager {
        &self.user_manager
    }

    /// 获取用户管理器的可变引用
    pub fn user_manager_mut(&mut self) -> &mut UserManager {
        &mut self.user_manager
    }

    /// 启动透明代理服务器
    pub async fn start(&mut self, intercepted_ports: &[u16]) -> Result<()> {
        info!("🚀 Starting transparent proxy server");
        
        // 确保代理用户存在
        self.user_manager.ensure_proxy_user().await?;
        
        // 显示当前用户信息和代理用户设置
        if let Some(current_user) = UserManager::get_current_user() {
            info!("📋 Current process user: {}", current_user);
            let proxy_user = self.user_manager.get_proxy_user();
            
            if current_user == proxy_user {
                warn!("⚠️  Warning: Current user ({}) is same as proxy user. This may cause loops!", current_user);
                warn!("💡 Consider running the proxy with a different user for better loop prevention.");
            } else {
                info!("✅ Good: Current user ({}) differs from proxy user ({})", current_user, proxy_user);
            }
            
            if !self.user_manager.is_admin() {
                warn!("⚠️  Warning: Not running with admin privileges. Some operations may fail.");
            }
        }

        // 1. 启动本地监听器
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let local_addr = listener.local_addr()?;
        let local_port = local_addr.port();
        
        info!("📡 Local proxy server binding to: {}", local_addr);
        
        // 2. 设置pfctl全局重定向
        info!("⚙️  Setting up pfctl global redirect for ports: {:?}", intercepted_ports);
        let proxy_user = self.user_manager.get_proxy_user();
        info!("🛡️ Using proxy user for bypass: {}", proxy_user);
        self.proxy_manager.start(local_port, intercepted_ports, Some(proxy_user)).await?;
        
        // 3. 启动连接处理循环
        self.local_listener = Some(listener);
        self.local_port = Some(local_port);
        
        info!("✅ Transparent proxy server started on port: {}", local_port);
        info!("🌐 Intercepting traffic on ports: {:?}", intercepted_ports);
        
        self.run_server().await
    }

    /// 停止服务器
    pub async fn stop(&mut self) -> Result<()> {
        info!("🛑 Stopping transparent proxy server");
        
        // 清理pfctl规则（由 TransparentProxyManager 统一管理）
        self.proxy_manager.stop().await?;
        
        // 关闭监听器
        self.local_listener = None;
        self.local_port = None;
        
        info!("✅ Transparent proxy server stopped");
        Ok(())
    }

    /// 运行服务器主循环
    async fn run_server(&mut self) -> Result<()> {
        let listener = self.local_listener.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Server not started"))?;

        info!("Transparent server listening, ready to handle connections...");

        loop {
            match listener.accept().await {
                Ok((client_stream, client_addr)) => {
                    debug!("New transparent connection from: {}", client_addr);
                    
                    let proxy_config = self.proxy_config.clone();
                    let routing_engine = self.routing_engine.clone();
                    let user_manager = self.user_manager.clone();
                    
                    // 为每个连接启动处理任务
                    tokio::spawn(Self::handle_connection(
                        client_stream,
                        client_addr,
                        proxy_config,
                        routing_engine,
                        user_manager,
                    ));
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                    break;
                }
            }
        }

        Ok(())
    }

    /// 处理单个透明连接 - 核心路由逻辑
    async fn handle_connection(
        client_stream: TcpStream,
        client_addr: std::net::SocketAddr,
        proxy_config: ProxyConfig,
        routing_engine: RoutingEngine,
        user_manager: UserManager,
    ) -> Result<()> {
        debug!("Handling transparent connection from: {}", client_addr);

        // 1. 解析目标地址
        let (target_host, target_port) = Self::parse_target_from_stream(&client_stream).await?;
        debug!("Parsed target: {}:{}", target_host, target_port);

        // 🛡️ 防循环检查：避免拦截到SOCKS5代理的连接
        if Self::is_loop_connection(&proxy_config, &target_host, target_port) {
            debug!("🛡️ Detected potential loop connection to SOCKS5 proxy, using direct connection");
            return Self::handle_direct_connection(client_stream, &target_host, target_port, None, &user_manager).await;
        }

        // 2. 提取完整URL（如果可能）
        let request_url = Self::extract_full_url_from_stream(&client_stream).await.ok();
        let process_path = None; // TODO: 可以通过系统调用获取进程路径
        
        // 3. 路由决策 - 这是核心！
        let decision = routing_engine.should_proxy(
            &target_host, 
            target_port, 
            request_url.as_deref(),
            process_path,
            &client_addr
        );
        
        match decision {
            RoutingDecision::Proxy { rewritten_url } => {
                if let Some(ref rewritten) = rewritten_url {
                    info!("🔀 Proxying {}:{} via SOCKS5 (URL rewritten to: {})", target_host, target_port, rewritten);
                } else {
                    info!("🔀 Proxying {}:{} via SOCKS5", target_host, target_port);
                }
                Self::handle_proxy_connection(client_stream, proxy_config, &target_host, target_port, rewritten_url, &user_manager).await
            }
            RoutingDecision::Direct { rewritten_url } => {
                if let Some(ref rewritten) = rewritten_url {
                    info!("🔗 Direct connection to {}:{} (URL rewritten to: {})", target_host, target_port, rewritten);
                } else {
                    info!("🔗 Direct connection to {}:{}", target_host, target_port);
                }
                Self::handle_direct_connection(client_stream, &target_host, target_port, rewritten_url, &user_manager).await
            }
        }
    }

    /// 通过SOCKS5代理处理连接
    async fn handle_proxy_connection(
        client_stream: TcpStream,
        proxy_config: ProxyConfig,
        target_host: &str,
        target_port: u16,
        _rewritten_url: Option<String>,
        user_manager: &UserManager,
    ) -> Result<()> {
        // 连接到SOCKS5代理
        let host = proxy_config.host().map_err(|e| anyhow::anyhow!("Invalid proxy host: {}", e))?;
        let port = proxy_config.port().map_err(|e| anyhow::anyhow!("Invalid proxy port: {}", e))?;
        
        debug!("🔗 Connecting to SOCKS5 proxy {}:{} for target {}:{}", host, port, target_host, target_port);
        
        // 连接到SOCKS5代理
        let mut proxy_stream = Self::create_bypass_connection(&host, port, user_manager).await
            .map_err(|e| anyhow::anyhow!("Failed to connect to SOCKS5 proxy: {}", e))?;

        debug!("✅ SOCKS5 connection established");

        // 进行SOCKS5握手
        Socks5Client::connect(&mut proxy_stream, &proxy_config, target_host, target_port).await?;

        // 双向转发数据
        let stats = DataForwarder::bidirectional_copy_with_stats(client_stream, proxy_stream, PROXY_BUFFER_SIZE).await?;
        stats.log_stats("SOCKS5 proxy", &format!("{}:{}", target_host, target_port));

        debug!("SOCKS5 proxy connection completed");
        Ok(())
    }

    /// 直接连接到目标服务器
    async fn handle_direct_connection(
        client_stream: TcpStream,
        target_host: &str,
        target_port: u16,
        _rewritten_url: Option<String>,
        user_manager: &UserManager,
    ) -> Result<()> {
        debug!("🔗 Creating direct connection to {}:{}", target_host, target_port);
        
        // 直接连接到目标
        let target_stream = Self::create_direct_connection(target_host, target_port, user_manager).await
            .map_err(|e| anyhow::anyhow!("Failed to connect to target: {}", e))?;

        debug!("✅ Direct connection established");

        // 双向转发数据并统计传输量
        let stats = DataForwarder::bidirectional_copy_with_stats(client_stream, target_stream, PROXY_BUFFER_SIZE).await?;

        // 输出传输统计信息
        stats.log_stats("Direct connection", &format!("{}:{}", target_host, target_port));
        
        debug!("Direct connection completed");
        Ok(())
    }

    /// 创建到SOCKS5代理的连接（使用代理用户身份）
    async fn create_bypass_connection(host: &str, port: u16, user_manager: &UserManager) -> Result<TcpStream> {
        let target_addr = format!("{}:{}", host, port);
        debug!("🔗 Connecting to SOCKS5 proxy at {}", target_addr);
        
        // 智能用户切换：只在必要时切换
        let _user_switcher = if user_manager.is_running_as_proxy_user() {
            debug!("✅ Already running as proxy user, no switching needed");
            None
        } else {
            debug!("🔄 Temporarily switching to proxy user for SOCKS5 connection");
            Some(user_manager.create_user_switcher()?)
        };
        
        // 连接到SOCKS5代理（此时socket带有代理用户身份）
        let stream = TcpStream::connect(&target_addr).await
            .map_err(|e| anyhow::anyhow!("Failed to connect to SOCKS5 proxy: {}", e))?;
        
        let local_addr = stream.local_addr()?;
        debug!("✅ Successfully connected to SOCKS5 proxy from {} to {} (using proxy user)", local_addr, target_addr);
        
        Ok(stream)
    }

    /// 创建直连到目标的连接（使用代理用户身份）
    async fn create_direct_connection(host: &str, port: u16, user_manager: &UserManager) -> Result<TcpStream> {
        let target_addr = format!("{}:{}", host, port);
        debug!("🔗 Creating direct connection to {}", target_addr);
        
        // 切换到代理用户身份创建socket
        let _user_switcher = user_manager.create_user_switcher()?;
        debug!("🔄 Switched to proxy user for direct connection");
        
        // 直接连接到目标（此时socket带有代理用户身份）
        let stream = TcpStream::connect(&target_addr).await
            .map_err(|e| anyhow::anyhow!("Failed to connect to target: {}", e))?;
        
        let local_addr = stream.local_addr()?;
        debug!("✅ Successfully connected directly from {} to {} (using proxy user)", local_addr, target_addr);
        
        Ok(stream)
    }


    /// 从透明连接中解析目标地址
    async fn parse_target_from_stream(stream: &TcpStream) -> Result<(String, u16)> {
        // 使用peek来读取第一部分数据，判断协议类型
        let mut buffer = vec![0u8; PEEK_BUFFER_SIZE];
        let n = match tokio::time::timeout(
            Duration::from_millis(PEEK_TIMEOUT_MS),
            stream.peek(&mut buffer)
        ).await {
            Ok(Ok(n)) if n > 0 => n,
            _ => {
                debug!("Failed to peek client data");
                return Err(anyhow::anyhow!("Failed to read client data"));
            }
        };

        let request_data = &buffer[..n];
        
        // 检查是否是TLS握手
        if Self::is_tls_handshake(request_data) {
            debug!("Detected TLS handshake - extracting SNI");
            Self::parse_tls_sni(request_data)
        } else {
            // 假设是HTTP请求
            Self::parse_http_target(request_data)
        }
    }

    /// 从透明连接中提取完整URL
    async fn extract_full_url_from_stream(stream: &TcpStream) -> Result<String> {
        // 使用peek来读取数据，判断协议类型
        let mut buffer = vec![0u8; PEEK_BUFFER_SIZE];
        let n = match tokio::time::timeout(
            Duration::from_millis(PEEK_TIMEOUT_MS),
            stream.peek(&mut buffer)
        ).await {
            Ok(Ok(n)) if n > 0 => n,
            _ => {
                return Err(anyhow::anyhow!("Failed to read client data"));
            }
        };

        let request_data = &buffer[..n];
        
        // 检查是否是TLS握手
        if Self::is_tls_handshake(request_data) {
            // 从TLS SNI提取hostname并构造HTTPS URL
            let (hostname, _port) = Self::parse_tls_sni(request_data)?;
            Ok(format!("https://{}/", hostname))
        } else {
            // HTTP请求
            Self::extract_url_from_http_request(request_data)
        }
    }

    /// 从HTTP请求中提取完整URL
    fn extract_url_from_http_request(request_data: &[u8]) -> Result<String> {
        let request_str = String::from_utf8_lossy(request_data);
        let lines: Vec<&str> = request_str.lines().collect();
        
        if lines.is_empty() {
            return Err(anyhow::anyhow!("Empty HTTP request"));
        }

        // 解析第一行获取方法和路径
        let first_line = lines[0];
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        
        if parts.len() < 2 {
            return Err(anyhow::anyhow!("Invalid HTTP request line"));
        }

        let method = parts[0];
        let url_or_path = parts[1];

        // 如果已经是完整URL，直接返回
        if url_or_path.starts_with("http://") || url_or_path.starts_with("https://") {
            return Ok(url_or_path.to_string());
        }

        // CONNECT方法处理
        if method == "CONNECT" {
            return Ok(format!("https://{}", url_or_path));
        }

        // 从Host头构造完整URL
        for line in &lines[1..] {
            if line.to_lowercase().starts_with("host:") {
                let host = line[5..].trim();
                
                // 判断是否为HTTPS
                let scheme = if url_or_path.starts_with("/") && 
                               (request_str.contains(":443") || 
                                request_str.to_lowercase().contains("https")) {
                    "https"
                } else {
                    "http"
                };
                
                return Ok(format!("{}://{}{}", scheme, host, url_or_path));
            }
        }

        Err(anyhow::anyhow!("Could not extract URL from HTTP request"))
    }

    /// 🛡️ 增强的循环连接检测（辅助IP别名方法）
    fn is_loop_connection(proxy_config: &ProxyConfig, target_host: &str, target_port: u16) -> bool {
        // 获取SOCKS5代理的主机和端口
        if let (Ok(proxy_host), Ok(proxy_port)) = (proxy_config.host(), proxy_config.port()) {
            // 直接匹配：目标就是SOCKS5代理
            if target_host == proxy_host && target_port == proxy_port {
                warn!("🛡️ Blocking direct loop: {}:{} == SOCKS5 proxy", target_host, target_port);
                return true;
            }
            
            // localhost变体检查
            let localhost_variants = ["127.0.0.1", "localhost", "::1"];
            let target_is_localhost = localhost_variants.contains(&target_host);
            let proxy_is_localhost = localhost_variants.contains(&proxy_host.as_str());
            
            if target_is_localhost && proxy_is_localhost && target_port == proxy_port {
                warn!("🛡️ Blocking localhost loop: {}:{} (SOCKS5 proxy port)", target_host, target_port);
                return true;
            }
            
            // 应用层循环检查
        }
        
        false
    }

    /// 检查是否是TLS握手数据包
    fn is_tls_handshake(data: &[u8]) -> bool {
        // TLS握手包的特征：
        // - 第一个字节是0x16 (TLS Handshake)
        // - 第二第三字节是版本号 (0x03, 0x01-0x04)
        // - 第五个字节是0x01 (Client Hello)
        if data.len() < 6 {
            return false;
        }
        
        data[0] == 0x16 && // TLS Handshake
        data[1] == 0x03 && // TLS Version Major
        (data[2] >= 0x01 && data[2] <= 0x04) && // TLS Version Minor (1.0-1.3)
        data[5] == 0x01 // Client Hello
    }

    /// 从TLS Client Hello中提取SNI（Server Name Indication）
    fn parse_tls_sni(data: &[u8]) -> Result<(String, u16)> {
        if data.len() < 43 {
            return Err(anyhow::anyhow!("TLS packet too short"));
        }

        // 跳过TLS记录头（5字节）和握手消息头（4字节）
        let mut offset = 9;
        
        // 跳过版本号（2字节）
        offset += 2;
        
        // 跳过随机数（32字节）
        offset += 32;
        
        if offset >= data.len() {
            return Err(anyhow::anyhow!("Invalid TLS packet structure"));
        }
        
        // 跳过Session ID
        let session_id_len = data[offset] as usize;
        offset += 1 + session_id_len;
        
        if offset + 2 >= data.len() {
            return Err(anyhow::anyhow!("Invalid TLS packet - session ID"));
        }
        
        // 跳过Cipher Suites
        let cipher_suites_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2 + cipher_suites_len;
        
        if offset >= data.len() {
            return Err(anyhow::anyhow!("Invalid TLS packet - cipher suites"));
        }
        
        // 跳过Compression Methods
        let compression_len = data[offset] as usize;
        offset += 1 + compression_len;
        
        if offset + 2 >= data.len() {
            return Err(anyhow::anyhow!("Invalid TLS packet - compression"));
        }
        
        // 读取Extensions长度
        let extensions_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        
        let extensions_end = offset + extensions_len;
        if extensions_end > data.len() {
            return Err(anyhow::anyhow!("Invalid TLS packet - extensions length"));
        }
        
        // 解析Extensions寻找SNI
        while offset < extensions_end {
            if offset + 4 > data.len() {
                break;
            }
            
            let ext_type = u16::from_be_bytes([data[offset], data[offset + 1]]);
            let ext_len = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
            offset += 4;
            
            if ext_type == 0x0000 {
                // SNI Extension
                return Self::parse_sni_extension(&data[offset..offset + ext_len]);
            }
            
            offset += ext_len;
        }
        
        Err(anyhow::anyhow!("No SNI found in TLS handshake"))
    }

    /// 解析SNI扩展
    fn parse_sni_extension(data: &[u8]) -> Result<(String, u16)> {
        if data.len() < 5 {
            return Err(anyhow::anyhow!("SNI extension too short"));
        }
        
        // 跳过SNI列表长度（2字节）
        let mut offset = 2;
        
        // 读取第一个SNI条目
        if offset >= data.len() {
            return Err(anyhow::anyhow!("Invalid SNI structure"));
        }
        
        let name_type = data[offset];
        offset += 1;
        
        if name_type != 0x00 {
            // 只处理hostname类型
            return Err(anyhow::anyhow!("Unsupported SNI name type"));
        }
        
        if offset + 2 > data.len() {
            return Err(anyhow::anyhow!("Invalid SNI name length"));
        }
        
        let name_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        
        if offset + name_len > data.len() {
            return Err(anyhow::anyhow!("Invalid SNI name"));
        }
        
        let hostname = String::from_utf8_lossy(&data[offset..offset + name_len]).to_string();
        
        // 对于HTTPS，默认使用443端口
        Ok((hostname, DEFAULT_HTTPS_PORT))
    }

    /// 解析HTTP请求获取目标主机和端口
    fn parse_http_target(request_data: &[u8]) -> Result<(String, u16)> {
        let request_str = String::from_utf8_lossy(request_data);
        let lines: Vec<&str> = request_str.lines().collect();
        
        if lines.is_empty() {
            return Err(anyhow::anyhow!("Empty HTTP request"));
        }

        // 解析第一行获取方法和路径
        let first_line = lines[0];
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        
        if parts.len() < 2 {
            return Err(anyhow::anyhow!("Invalid HTTP request line"));
        }

        let method = parts[0];
        let url_or_path = parts[1];

        // CONNECT方法直接包含host:port
        if method == "CONNECT" {
            let host_port: Vec<&str> = url_or_path.split(':').collect();
            if host_port.len() == 2 {
                let host = host_port[0].to_string();
                let port = host_port[1].parse::<u16>()
                    .map_err(|_| anyhow::anyhow!("Invalid port in CONNECT: {}", host_port[1]))?;
                return Ok((host, port));
            }
        }

        // 对于其他方法，从Host头获取主机
        for line in &lines[1..] {
            if line.to_lowercase().starts_with("host:") {
                let host_part = line[5..].trim();
                
                // 检查是否包含端口
                if let Some(colon_pos) = host_part.rfind(':') {
                    let host = host_part[..colon_pos].to_string();
                    let port_str = &host_part[colon_pos + 1..];
                    
                    if let Ok(port) = port_str.parse::<u16>() {
                        return Ok((host, port));
                    }
                }
                
                // 没有端口，根据方法推断默认端口
                let default_port = if url_or_path.starts_with("https://") || 
                                     request_str.contains("443") {
                    DEFAULT_HTTPS_PORT
                } else {
                    DEFAULT_HTTP_PORT
                };
                
                return Ok((host_part.to_string(), default_port));
            }
        }

        // 如果找不到Host头，尝试从URL解析
        if url_or_path.starts_with("http://") || url_or_path.starts_with("https://") {
            if let Ok(url) = url::Url::parse(url_or_path) {
                let host = url.host_str().unwrap_or("").to_string();
                let port = url.port().unwrap_or(if url.scheme() == "https" { DEFAULT_HTTPS_PORT } else { DEFAULT_HTTP_PORT });
                return Ok((host, port));
            }
        }

        Err(anyhow::anyhow!("Could not parse target from HTTP request"))
    }

    pub fn get_local_port(&self) -> Option<u16> {
        self.local_port
    }
}

impl Drop for TransparentServer {
    fn drop(&mut self) {
        warn!("TransparentServer dropped, performing emergency cleanup");
        
        // 注意：这里我们不能使用async，所以只是触发cleanup
        // 实际的清理会由PfctlManager的Drop trait处理
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_target_parsing() {
        let http_request = b"GET / HTTP/1.1\r\nHost: www.google.com\r\n\r\n";
        let (host, port) = TransparentServer::parse_http_target(http_request).unwrap();
        assert_eq!(host, "www.google.com");
        assert_eq!(port, 80);

        let https_request = b"CONNECT www.google.com:443 HTTP/1.1\r\n\r\n";
        let (host, port) = TransparentServer::parse_http_target(https_request).unwrap();
        assert_eq!(host, "www.google.com");
        assert_eq!(port, 443);
    }

    #[test]
    fn test_tls_handshake_detection() {
        // 模拟TLS Client Hello开头
        let tls_handshake = [
            0x16, 0x03, 0x03, // TLS Handshake, version 3.3
            0x00, 0x20,       // Length
            0x01,             // Client Hello
        ];
        
        assert!(TransparentServer::is_tls_handshake(&tls_handshake));
        
        // HTTP请求不应该被识别为TLS
        let http_request = b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";
        assert!(!TransparentServer::is_tls_handshake(http_request));
        
        // 太短的数据包
        let short_data = [0x16, 0x03];
        assert!(!TransparentServer::is_tls_handshake(&short_data));
    }

    #[test]
    fn test_url_extraction_types() {
        // HTTP请求的URL提取
        let http_request = b"GET /api/v1/users HTTP/1.1\r\nHost: api.example.com\r\n\r\n";
        let url = TransparentServer::extract_url_from_http_request(http_request).unwrap();
        assert_eq!(url, "http://api.example.com/api/v1/users");
        
        // HTTPS CONNECT请求
        let https_connect = b"CONNECT secure.example.com:443 HTTP/1.1\r\n\r\n";
        let url = TransparentServer::extract_url_from_http_request(https_connect).unwrap();
        assert_eq!(url, "https://secure.example.com:443");
    }
}

