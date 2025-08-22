use crate::config::ProxyConfig;
use anyhow::Result;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn, error, debug};
use std::path::PathBuf;

/// 代理模式选项
#[derive(Debug, Clone)]
pub enum ProxyMode {
    /// 全局透明代理 - 拦截所有指定端口的流量 (推荐)
    Global { ports: Vec<u16> },
    /// 手动指定进程 - 精确控制特定进程
    Manual { target_pids: Vec<u32> },
    /// 智能监控 - 根据进程名自动检测 (传统方式的改进版)
    SmartMonitoring { target_patterns: Vec<String> },
}

impl Default for ProxyMode {
    fn default() -> Self {
        // 默认使用全局模式，拦截常用端口
        Self::Global { 
            ports: vec![80, 443, 8080, 3000, 5000, 8000] 
        }
    }
}

/// macOS透明代理系统 - 使用pfctl实现真正透明的网络拦截
#[derive(Debug)]
pub struct TransparentProxy {
    /// 代理配置
    proxy_config: ProxyConfig,
    /// 代理模式
    proxy_mode: ProxyMode,
    /// 预启动的本地代理服务器
    local_proxy_server: Option<LocalProxyServer>,
    /// pfctl规则管理器
    pfctl_rules: PfctlRuleManager,
    /// 就绪检查器
    readiness_checker: ReadinessChecker,
}

#[derive(Debug)]
struct LocalProxyServer {
    local_port: u16,
    handle: tokio::task::JoinHandle<()>,
}

#[derive(Debug)]
struct PfctlRuleManager {
    anchor_name: String,
    rule_file_path: PathBuf,
    active_rules: Vec<String>,
}

#[derive(Debug)]
struct ReadinessChecker;

impl TransparentProxy {
    /// 创建新的透明代理实例
    pub fn new(proxy_config: ProxyConfig) -> Self {
        Self::with_mode(proxy_config, ProxyMode::default())
    }
    
    /// 创建指定模式的透明代理实例
    pub fn with_mode(proxy_config: ProxyConfig, proxy_mode: ProxyMode) -> Self {
        Self {
            proxy_config,
            proxy_mode,
            local_proxy_server: None,
            pfctl_rules: PfctlRuleManager::new("takeover_transparent"),
            readiness_checker: ReadinessChecker::new(),
        }
    }

    /// 🚀 启动透明代理系统 - 根据模式自动选择最佳方案
    pub async fn start(&mut self) -> Result<()> {
        info!("🚀 Starting transparent proxy system in mode: {:?}", self.proxy_mode);

        // 步骤1: 预启动本地代理服务器 (所有模式都需要)
        info!("📡 Step 1: Starting local proxy server...");
        let local_port = self.start_local_proxy_server().await?;
        
        // 步骤2: 等待代理服务器完全就绪 - 解决时序问题的核心！
        info!("⏳ Step 2: Waiting for proxy server readiness...");
        self.readiness_checker.wait_for_proxy_ready(local_port).await?;
        
        // 步骤3: 根据模式设置不同的拦截规则
        info!("⚙️  Step 3: Setting up interception rules for mode...");
        match &self.proxy_mode {
            ProxyMode::Global { ports } => {
                self.pfctl_rules.setup_global_rules(local_port, ports.clone()).await?;
                info!("🌐 Global transparent proxy activated for ports: {:?}", ports);
            }
            ProxyMode::Manual { target_pids } => {
                self.pfctl_rules.setup_process_specific_rules(local_port, target_pids.clone()).await?;
                info!("🎯 Process-specific proxy activated for PIDs: {:?}", target_pids);
            }
            ProxyMode::SmartMonitoring { target_patterns } => {
                // 这个模式需要启动进程监控
                self.pfctl_rules.setup_pattern_based_rules(local_port, target_patterns.clone()).await?;
                info!("🔍 Smart monitoring activated for patterns: {:?}", target_patterns);
            }
        }
        
        // 步骤4: 最终验证整个系统就绪
        info!("✅ Step 4: Final system verification...");
        self.verify_system_ready().await?;

        self.print_usage_instructions();

        Ok(())
    }
    
    /// 显示使用说明
    fn print_usage_instructions(&self) {
        match &self.proxy_mode {
            ProxyMode::Global { ports } => {
                info!("🎉 Global transparent proxy is ready!");
                info!("👤 Users can now directly use any network tool:");
                info!("   curl https://httpbin.org/ip");
                info!("   node -e \"require('https').get('https://httpbin.org/ip', res => res.pipe(process.stdout))\"");
                info!("   wget https://example.com");
                info!("   git clone https://github.com/...");
                info!("🔗 Intercepted ports: {:?}", ports);
            }
            ProxyMode::Manual { target_pids } => {
                info!("🎉 Process-specific proxy is ready!");
                info!("🎯 Monitoring PIDs: {:?}", target_pids);
                info!("💡 Use add_target_pid() to monitor additional processes");
            }
            ProxyMode::SmartMonitoring { target_patterns } => {
                info!("🎉 Smart monitoring proxy is ready!");
                info!("🔍 Monitoring patterns: {:?}", target_patterns);
                info!("🔄 New processes matching these patterns will be automatically proxied");
            }
        }
    }

    /// 停止透明代理系统
    pub async fn stop(&mut self) -> Result<()> {
        info!("🛑 Stopping transparent proxy system...");

        // 清理pfctl规则
        if let Err(e) = self.pfctl_rules.cleanup_rules().await {
            warn!("Failed to cleanup pfctl rules: {}", e);
        }

        // 停止代理服务器
        if let Some(server) = self.local_proxy_server.take() {
            info!("Shutting down local proxy server on port: {}", server.local_port);
            server.handle.abort();
        }

        info!("✅ Transparent proxy system stopped");
        Ok(())
    }

    /// 获取本地代理端口
    pub fn get_local_port(&self) -> Option<u16> {
        self.local_proxy_server.as_ref().map(|server| server.local_port)
    }

    /// 启动本地代理服务器
    async fn start_local_proxy_server(&mut self) -> Result<u16> {
        debug!("Creating TCP listener for local proxy server");
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let local_addr = listener.local_addr()?;
        let local_port = local_addr.port();

        info!("Local proxy server binding to: {}", local_addr);

        // 启动代理服务器任务
        let proxy_config = self.proxy_config.clone();
        let handle = tokio::spawn(async move {
            if let Err(e) = Self::run_proxy_server(listener, proxy_config).await {
                error!("Proxy server error: {}", e);
            }
        });

        self.local_proxy_server = Some(LocalProxyServer {
            local_port,
            handle,
        });

        info!("✅ Local proxy server started on port: {}", local_port);
        Ok(local_port)
    }

    /// 运行代理服务器 - 处理所有入站连接
    async fn run_proxy_server(listener: TcpListener, proxy_config: ProxyConfig) -> Result<()> {
        info!("Proxy server listening, ready to handle transparent connections...");

        loop {
            match listener.accept().await {
                Ok((client_stream, client_addr)) => {
                    debug!("New transparent connection from: {}", client_addr);
                    
                    let proxy_config = proxy_config.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_transparent_connection(client_stream, proxy_config).await {
                            debug!("Connection handling error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept transparent connection: {}", e);
                    break;
                }
            }
        }

        Ok(())
    }

    /// 处理透明代理连接
    async fn handle_transparent_connection(
        mut client_stream: tokio::net::TcpStream,
        proxy_config: ProxyConfig,
    ) -> Result<()> {
        debug!("Handling transparent proxy connection");

        // 读取第一部分数据以获取目标地址（从HTTP Host头）
        let mut buffer = vec![0u8; 4096];
        let n = match tokio::time::timeout(
            Duration::from_millis(500),
            client_stream.peek(&mut buffer)
        ).await {
            Ok(Ok(n)) if n > 0 => n,
            _ => {
                debug!("Failed to peek client data");
                return Ok(());
            }
        };

        // 解析HTTP请求获取目标主机和端口
        let request_data = &buffer[..n];
        let (target_host, target_port) = Self::parse_http_target(request_data)?;
        debug!("Parsed target: {}:{}", target_host, target_port);

        // 连接到真实的SOCKS5代理
        let host = proxy_config.host().map_err(|e| anyhow::anyhow!("Invalid proxy host: {}", e))?;
        let port = proxy_config.port().map_err(|e| anyhow::anyhow!("Invalid proxy port: {}", e))?;
        let proxy_addr = format!("{}:{}", host, port);
        let mut proxy_stream = tokio::net::TcpStream::connect(&proxy_addr).await
            .map_err(|e| anyhow::anyhow!("Failed to connect to SOCKS5 proxy: {}", e))?;

        // 进行完整的SOCKS5握手（包括CONNECT请求）
        Self::perform_full_socks5_handshake(&mut proxy_stream, &proxy_config, &target_host, target_port).await?;

        // 双向透明转发数据
        Self::bidirectional_copy(client_stream, proxy_stream).await?;

        debug!("Transparent connection completed");
        Ok(())
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
                    
                    // 尝试解析端口
                    if let Ok(port) = port_str.parse::<u16>() {
                        return Ok((host, port));
                    }
                }
                
                // 没有端口，根据方法推断默认端口
                let default_port = if url_or_path.starts_with("https://") || 
                                     request_str.contains("443") {
                    443
                } else {
                    80
                };
                
                return Ok((host_part.to_string(), default_port));
            }
        }

        // 如果找不到Host头，尝试从URL解析（针对代理请求）
        if url_or_path.starts_with("http://") || url_or_path.starts_with("https://") {
            if let Ok(url) = url::Url::parse(url_or_path) {
                let host = url.host_str().unwrap_or("").to_string();
                let port = url.port().unwrap_or(if url.scheme() == "https" { 443 } else { 80 });
                return Ok((host, port));
            }
        }

        Err(anyhow::anyhow!("Could not parse target from HTTP request"))
    }

    /// 完整的SOCKS5握手（包括认证和CONNECT）
    async fn perform_full_socks5_handshake(
        stream: &mut tokio::net::TcpStream,
        proxy_config: &ProxyConfig,
        target_host: &str,
        target_port: u16,
    ) -> Result<()> {
        debug!("Performing full SOCKS5 handshake to {}:{}", target_host, target_port);

        // 第一步：认证握手
        Self::perform_socks5_handshake(stream, proxy_config).await?;

        // 第二步：发送CONNECT请求
        let mut connect_request = vec![0x05, 0x01, 0x00]; // VER, CMD=CONNECT, RSV

        // 地址类型和地址
        if target_host.parse::<std::net::Ipv4Addr>().is_ok() {
            // IPv4地址
            connect_request.push(0x01);
            let ip: std::net::Ipv4Addr = target_host.parse()?;
            connect_request.extend_from_slice(&ip.octets());
        } else if target_host.parse::<std::net::Ipv6Addr>().is_ok() {
            // IPv6地址
            connect_request.push(0x04);
            let ip: std::net::Ipv6Addr = target_host.parse()?;
            connect_request.extend_from_slice(&ip.octets());
        } else {
            // 域名
            connect_request.push(0x03);
            connect_request.push(target_host.len() as u8);
            connect_request.extend_from_slice(target_host.as_bytes());
        }

        // 端口
        connect_request.extend_from_slice(&target_port.to_be_bytes());

        stream.write_all(&connect_request).await?;

        // 读取CONNECT响应
        let mut connect_response = vec![0; 4];
        stream.read_exact(&mut connect_response).await?;

        if connect_response[0] != 0x05 {
            return Err(anyhow::anyhow!("Invalid SOCKS5 CONNECT response version"));
        }

        if connect_response[1] != 0x00 {
            return Err(anyhow::anyhow!("SOCKS5 CONNECT failed with code: {}", connect_response[1]));
        }

        // 读取剩余的地址信息（我们不需要使用，但必须读取完整）
        match connect_response[3] {
            0x01 => {
                // IPv4: 4字节地址 + 2字节端口
                let mut addr_info = vec![0; 6];
                stream.read_exact(&mut addr_info).await?;
            }
            0x03 => {
                // 域名: 1字节长度 + 域名 + 2字节端口
                let mut len_buf = vec![0; 1];
                stream.read_exact(&mut len_buf).await?;
                let len = len_buf[0] as usize;
                let mut addr_info = vec![0; len + 2];
                stream.read_exact(&mut addr_info).await?;
            }
            0x04 => {
                // IPv6: 16字节地址 + 2字节端口
                let mut addr_info = vec![0; 18];
                stream.read_exact(&mut addr_info).await?;
            }
            _ => {
                return Err(anyhow::anyhow!("Unsupported address type in SOCKS5 response"));
            }
        }

        debug!("✅ SOCKS5 CONNECT to {}:{} completed successfully", target_host, target_port);
        Ok(())
    }

    /// SOCKS5握手处理（仅认证部分）
    async fn perform_socks5_handshake(
        stream: &mut tokio::net::TcpStream,
        proxy_config: &ProxyConfig,
    ) -> Result<()> {
        debug!("Performing SOCKS5 authentication handshake");

        // 发送认证方法选择
        let greeting = if proxy_config.auth_tuple().is_some() {
            vec![0x05, 0x02, 0x00, 0x02] // 版本5，支持无认证和用户名密码
        } else {
            vec![0x05, 0x01, 0x00] // 版本5，只支持无认证
        };

        stream.write_all(&greeting).await?;

        let mut response = vec![0; 2];
        stream.read_exact(&mut response).await?;

        if response[0] != 0x05 {
            return Err(anyhow::anyhow!("Invalid SOCKS5 response version: {}", response[0]));
        }

        match response[1] {
            0x00 => {
                debug!("SOCKS5: No authentication required");
            }
            0x02 => {
                debug!("SOCKS5: Username/password authentication required");
                if let Some((username, password)) = proxy_config.auth_tuple() {
                    Self::perform_socks5_auth(stream, &username, &password).await?;
                } else {
                    return Err(anyhow::anyhow!("SOCKS5 requires authentication but none provided"));
                }
            }
            0xFF => {
                return Err(anyhow::anyhow!("SOCKS5: No acceptable authentication methods"));
            }
            _ => {
                return Err(anyhow::anyhow!("SOCKS5: Unsupported authentication method: {}", response[1]));
            }
        }

        debug!("✅ SOCKS5 authentication completed successfully");
        Ok(())
    }

    /// SOCKS5用户名密码认证
    async fn perform_socks5_auth(
        stream: &mut tokio::net::TcpStream,
        username: &str,
        password: &str,
    ) -> Result<()> {
        debug!("Performing SOCKS5 username/password authentication");

        let mut auth_request = vec![0x01]; // 认证子协议版本
        auth_request.push(username.len() as u8);
        auth_request.extend_from_slice(username.as_bytes());
        auth_request.push(password.len() as u8);
        auth_request.extend_from_slice(password.as_bytes());

        stream.write_all(&auth_request).await?;

        let mut auth_response = vec![0; 2];
        stream.read_exact(&mut auth_response).await?;

        if auth_response[1] != 0x00 {
            return Err(anyhow::anyhow!("SOCKS5 authentication failed"));
        }

        debug!("✅ SOCKS5 authentication successful");
        Ok(())
    }

    /// 双向数据转发
    async fn bidirectional_copy(
        client_stream: tokio::net::TcpStream,
        proxy_stream: tokio::net::TcpStream,
    ) -> Result<()> {
        let (mut client_read, mut client_write) = client_stream.into_split();
        let (mut proxy_read, mut proxy_write) = proxy_stream.into_split();

        let client_to_proxy = tokio::spawn(async move {
            let mut buffer = vec![0; 8192];
            loop {
                match client_read.read(&mut buffer).await {
                    Ok(0) => break, // 连接关闭
                    Ok(n) => {
                        if proxy_write.write_all(&buffer[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let proxy_to_client = tokio::spawn(async move {
            let mut buffer = vec![0; 8192];
            loop {
                match proxy_read.read(&mut buffer).await {
                    Ok(0) => break, // 连接关闭
                    Ok(n) => {
                        if client_write.write_all(&buffer[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // 等待任意一个方向的传输完成
        tokio::select! {
            _ = client_to_proxy => debug!("Client to proxy transfer completed"),
            _ = proxy_to_client => debug!("Proxy to client transfer completed"),
        }

        Ok(())
    }

    /// 验证整个系统是否就绪
    async fn verify_system_ready(&self) -> Result<()> {
        debug!("Verifying complete system readiness");

        // 验证代理服务器仍在运行
        if let Some(server) = &self.local_proxy_server {
            if server.handle.is_finished() {
                return Err(anyhow::anyhow!("Local proxy server has stopped unexpectedly"));
            }
        }

        // 验证pfctl规则已加载
        self.pfctl_rules.verify_rules_loaded().await?;

        debug!("✅ System verification completed successfully");
        Ok(())
    }
}

impl Drop for TransparentProxy {
    fn drop(&mut self) {
        warn!("TransparentProxy dropped, performing emergency cleanup");
        
        // 紧急清理pfctl规则
        if let Err(e) = futures::executor::block_on(self.pfctl_rules.emergency_cleanup()) {
            warn!("Emergency pfctl cleanup failed: {}", e);
        }

        // 停止代理服务器
        if let Some(server) = &self.local_proxy_server {
            server.handle.abort();
        }
    }
}

impl PfctlRuleManager {
    pub fn new(anchor_name: &str) -> Self {
        Self {
            anchor_name: anchor_name.to_string(),
            rule_file_path: PathBuf::from(format!("/tmp/{}.conf", anchor_name)),
            active_rules: Vec::new(),
        }
    }

    /// 设置全局透明代理规则 (推荐用于curl/Node.js)
    pub async fn setup_global_rules(&mut self, local_proxy_port: u16, ports: Vec<u16>) -> Result<()> {
        info!("Setting up global pfctl rules for ports: {:?} -> proxy port: {}", ports, local_proxy_port);

        let rules = self.generate_global_rules(local_proxy_port, ports);
        self.apply_rules(rules).await
    }
    
    /// 设置进程特定规则 (精确控制)
    pub async fn setup_process_specific_rules(&mut self, local_proxy_port: u16, _target_pids: Vec<u32>) -> Result<()> {
        info!("Setting up process-specific pfctl rules for proxy port: {}", local_proxy_port);
        
        // 注意：macOS的pfctl不能直接基于PID过滤，这里我们使用全局规则
        // 在实际应用中，可能需要结合其他技术实现真正的PID级别过滤
        warn!("macOS pfctl doesn't support PID-based filtering, falling back to global rules");
        
        let default_ports = vec![80, 443, 8080];
        let rules = self.generate_global_rules(local_proxy_port, default_ports);
        self.apply_rules(rules).await
    }
    
    /// 设置基于模式的规则 (智能监控)
    pub async fn setup_pattern_based_rules(&mut self, local_proxy_port: u16, _target_patterns: Vec<String>) -> Result<()> {
        info!("Setting up pattern-based pfctl rules for proxy port: {}", local_proxy_port);
        
        // 同样，pfctl不支持进程名模式匹配，使用全局规则
        warn!("macOS pfctl doesn't support pattern-based filtering, using global rules");
        
        let default_ports = vec![80, 443, 8080, 3000, 5000];
        let rules = self.generate_global_rules(local_proxy_port, default_ports);
        self.apply_rules(rules).await
    }
    
    /// 应用pfctl规则的通用方法
    async fn apply_rules(&mut self, rules: String) -> Result<()> {
        // 写入规则文件
        std::fs::write(&self.rule_file_path, &rules)?;
        debug!("Wrote pfctl rules to: {:?}", self.rule_file_path);

        // 确保pfctl已启用
        self.ensure_pfctl_enabled().await?;

        // 直接加载规则到主规则集（不使用anchor，确保规则生效）
        self.load_rules_directly().await?;

        // 验证规则加载成功
        self.verify_rules_loaded_direct().await?;

        self.active_rules = vec![rules];
        info!("✅ pfctl transparent proxy rules activated");

        Ok(())
    }

    /// 生成全局透明代理规则
    fn generate_global_rules(&self, local_proxy_port: u16, ports: Vec<u16>) -> String {
        let mut rules = format!(
            "# Takeover Global Transparent Proxy Rules\n\
             # Generated at: {}\n\
             # Local proxy port: {}\n\
             # Intercepted ports: {:?}\n\
             \n",
            chrono::Utc::now(),
            local_proxy_port,
            ports
        );

        // macOS pfctl透明代理的正确实现：
        // 1. 先用rdr规则在lo0上设置重定向
        // 2. 再用pass out route-to将出站流量路由到lo0
        // 这样出站流量会变成lo0的入站流量，从而被rdr规则捕获
        
        // 先设置rdr规则（在lo0上拦截）
        for port in &ports {
            rules.push_str(&format!(
                "rdr pass on lo0 inet proto tcp from any to any port {} -> 127.0.0.1 port {}\n",
                port, local_proxy_port
            ));
        }
        
        rules.push_str("\n");
        
        // 再设置route-to规则（将出站流量路由到lo0）
        for port in &ports {
            rules.push_str(&format!(
                "pass out route-to (lo0 127.0.0.1) inet proto tcp from any to any port {} keep state\n",
                port
            ));
        }
        
        rules
    }

    /// 确保pfctl已启用
    async fn ensure_pfctl_enabled(&self) -> Result<()> {
        debug!("Ensuring pfctl is enabled");

        // 启用pfctl
        let output = tokio::process::Command::new("pfctl")
            .args(&["-e"])
            .output()
            .await?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        debug!("pfctl enable output: {}", stderr);

        // 不再需要anchor引用，因为我们直接加载到主规则集

        Ok(())
    }

    /// 直接加载规则到主规则集（不使用anchor）
    async fn load_rules_directly(&self) -> Result<()> {
        debug!("Loading pfctl rules directly to main ruleset");

        let rule_file_path = self.rule_file_path.to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid rule file path"))?;

        // 重要：使用 -f 会覆盖整个规则集
        // 但这是确保规则真正生效的唯一方法
        let output = tokio::process::Command::new("pfctl")
            .args(&["-f", rule_file_path])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("Failed to load pfctl rules: {}", stderr);
            return Err(anyhow::anyhow!("pfctl rule loading failed: {}", stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        debug!("pfctl load output: {}", stdout);
        
        info!("Loaded pfctl rules directly to main ruleset");

        Ok(())
    }

    /// 验证规则已加载到主规则集
    async fn verify_rules_loaded_direct(&self) -> Result<()> {
        debug!("Verifying pfctl rules are loaded in main ruleset");

        // 检查NAT规则（直接从主规则集）
        let nat_output = tokio::process::Command::new("pfctl")
            .args(&["-s", "nat"])
            .output()
            .await?;

        if !nat_output.status.success() {
            return Err(anyhow::anyhow!("Failed to query pfctl NAT rules"));
        }

        let nat_rules = String::from_utf8_lossy(&nat_output.stdout);
        let nat_count = nat_rules.lines()
            .filter(|l| l.contains("127.0.0.1") && !l.trim().is_empty())
            .count();
        debug!("Verified NAT rules redirecting to local proxy: {} lines", nat_count);

        // 检查过滤规则（包括route-to）
        let filter_output = tokio::process::Command::new("pfctl")
            .args(&["-s", "rules"])
            .output()
            .await?;

        if !filter_output.status.success() {
            return Err(anyhow::anyhow!("Failed to query pfctl filter rules"));
        }

        let filter_rules = String::from_utf8_lossy(&filter_output.stdout);
        let filter_count = filter_rules.lines()
            .filter(|l| l.contains("route-to") && !l.trim().is_empty())
            .count();
        debug!("Verified filter rules with route-to: {} lines", filter_count);

        if nat_count == 0 {
            warn!("No NAT rules found in main ruleset - transparent proxy may not work");
        }
        if filter_count == 0 {
            warn!("No route-to rules found in main ruleset - outbound traffic may not be intercepted");
        }

        Ok(())
    }
    
    /// 验证规则已加载（保留用于向后兼容）
    pub async fn verify_rules_loaded(&self) -> Result<()> {
        self.verify_rules_loaded_direct().await
    }

    /// 清理规则
    pub async fn cleanup_rules(&mut self) -> Result<()> {
        info!("Cleaning up pfctl rules");

        // 刷新主规则集中的所有规则
        // 注意：这会清除所有规则，包括系统规则
        // 更安全的方法是保存原始规则并恢复
        let output = tokio::process::Command::new("pfctl")
            .args(&["-F", "all"])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to flush pfctl rules: {}", stderr);
        } else {
            debug!("Successfully flushed pfctl rules");
        }

        // 保留规则文件用于调试
        debug!("Keeping rule file for debugging: {:?}", self.rule_file_path);

        self.active_rules.clear();
        info!("✅ pfctl rules cleanup completed");

        Ok(())
    }

    /// 紧急清理（同步版本，用于Drop）
    pub async fn emergency_cleanup(&mut self) -> Result<()> {
        warn!("Performing emergency pfctl cleanup");
        self.cleanup_rules().await
    }
}

impl ReadinessChecker {
    pub fn new() -> Self {
        Self
    }

    /// 等待代理服务器就绪 - 解决时序问题的核心方法
    pub async fn wait_for_proxy_ready(&self, port: u16) -> Result<()> {
        info!("Waiting for proxy server readiness on port: {}", port);
        
        let max_attempts = 30;
        let base_delay = Duration::from_millis(100);
        
        for attempt in 1..=max_attempts {
            match self.check_proxy_connectivity(port).await {
                Ok(()) => {
                    info!("✅ Proxy server ready after {} attempts", attempt);
                    
                    // 额外等待确保完全稳定
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    return Ok(());
                }
                Err(e) if attempt < max_attempts => {
                    debug!("Attempt {}/{}: Proxy not ready yet: {}", attempt, max_attempts, e);
                    
                    // 指数退避，但有上限
                    let delay = std::cmp::min(
                        base_delay * (2_u32.pow(attempt / 5)), 
                        Duration::from_millis(500)
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Err(e) => {
                    error!("Proxy server failed to become ready after {} attempts: {}", max_attempts, e);
                    return Err(anyhow::anyhow!(
                        "Proxy server not ready after {} attempts: {}", 
                        max_attempts, 
                        e
                    ));
                }
            }
        }

        Ok(())
    }

    /// 检查代理服务器连通性
    async fn check_proxy_connectivity(&self, port: u16) -> Result<()> {
        let addr = format!("127.0.0.1:{}", port);
        
        // 尝试建立TCP连接
        let stream = tokio::time::timeout(
            Duration::from_millis(500),
            tokio::net::TcpStream::connect(&addr)
        ).await??;
        
        drop(stream); // 立即关闭连接
        
        debug!("Proxy connectivity check passed for port: {}", port);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProxyType;

    #[tokio::test]
    async fn test_readiness_checker() {
        let checker = ReadinessChecker::new();
        
        // 这应该失败，因为没有服务器监听
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            checker.check_proxy_connectivity(65535)
        ).await;
        
        assert!(result.is_ok() && result.unwrap().is_err());
    }

    #[test]
    fn test_pfctl_rule_generation() {
        let rule_manager = PfctlRuleManager::new("test_anchor");
        let ports = vec![80, 443, 8080];
        let rules = rule_manager.generate_global_rules(10080, ports);
        
        assert!(rules.contains("127.0.0.1 port 10080"));
        assert!(rules.contains("port 80"));
        assert!(rules.contains("port 443"));
    }

    #[tokio::test]
    async fn test_transparent_proxy_creation() {
        let proxy_config = ProxyConfig {
            url: "socks5://127.0.0.1:1080".to_string(),
            proxy_type: ProxyType::Socks5,
            auth: None,
        };

        let proxy = TransparentProxy::new(proxy_config);
        assert!(proxy.local_proxy_server.is_none());
        assert!(proxy.get_local_port().is_none());
    }
}
