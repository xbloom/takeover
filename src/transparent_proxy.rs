use crate::config::ProxyConfig;
use crate::socks5::{Socks5Client, DataForwarder};
use crate::utils;

use anyhow::Result;
use std::time::Duration;
use tokio::net::TcpListener;

use tracing::{info, warn, error, debug};
use std::path::PathBuf;

// Application constants
const DEFAULT_PORTS: &[u16] = &[80, 443, 8080, 3000, 5000, 8000];
const COMMON_WEB_PORTS: &[u16] = &[80, 443, 8080];
const EXTENDED_WEB_PORTS: &[u16] = &[80, 443, 8080, 3000, 5000];
const MAX_READINESS_ATTEMPTS: u32 = 30;
const BASE_READINESS_DELAY_MS: u64 = 100;
const MAX_READINESS_DELAY_MS: u64 = 500;
const PEEK_TIMEOUT_MS: u64 = 500;
const PEEK_BUFFER_SIZE: usize = 4096;
const PROXY_BUFFER_SIZE: usize = 8192;
const DEFAULT_HTTP_PORT: u16 = 80;
const DEFAULT_HTTPS_PORT: u16 = 443;
const ANCHOR_NAME: &str = "takeover_transparent";
const TEMP_DIR: &str = "/tmp";

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
        Self::Global { 
            ports: DEFAULT_PORTS.to_vec()
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
    using_alternative_method: bool,
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
            pfctl_rules: PfctlRuleManager::new(ANCHOR_NAME),
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
        client_stream: tokio::net::TcpStream,
        proxy_config: ProxyConfig,
    ) -> Result<()> {
        debug!("Handling transparent proxy connection");

        // 读取第一部分数据以获取目标地址（从HTTP Host头）
        let mut buffer = vec![0u8; PEEK_BUFFER_SIZE];
        let n = match tokio::time::timeout(
            Duration::from_millis(PEEK_TIMEOUT_MS),
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
        Socks5Client::connect(&mut proxy_stream, &proxy_config, &target_host, target_port).await?;

        // 双向透明转发数据
        DataForwarder::bidirectional_copy(client_stream, proxy_stream, PROXY_BUFFER_SIZE).await?;

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
                    DEFAULT_HTTPS_PORT
                } else {
                    DEFAULT_HTTP_PORT
                };
                
                return Ok((host_part.to_string(), default_port));
            }
        }

        // 如果找不到Host头，尝试从URL解析（针对代理请求）
        if url_or_path.starts_with("http://") || url_or_path.starts_with("https://") {
            if let Ok(url) = url::Url::parse(url_or_path) {
                let host = url.host_str().unwrap_or("").to_string();
                let port = url.port().unwrap_or(if url.scheme() == "https" { DEFAULT_HTTPS_PORT } else { DEFAULT_HTTP_PORT });
                return Ok((host, port));
            }
        }

        Err(anyhow::anyhow!("Could not parse target from HTTP request"))
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
            rule_file_path: PathBuf::from(format!("{}/{}.conf", TEMP_DIR, anchor_name)),
            active_rules: Vec::new(),
            using_alternative_method: false,
        }
    }

    /// 设置全局透明代理规则 (推荐用于curl/Node.js)
    pub async fn setup_global_rules(&mut self, local_proxy_port: u16, ports: Vec<u16>) -> Result<()> {
        info!("Setting up global pfctl rules for ports: {:?} -> proxy port: {}", ports, local_proxy_port);

        let rules = self.generate_global_rules(local_proxy_port, ports);
        self.apply_rules(rules).await
    }
    
    /// Sets up process-specific rules (falls back to global rules on macOS)
    pub async fn setup_process_specific_rules(&mut self, local_proxy_port: u16, _target_pids: Vec<u32>) -> Result<()> {
        info!("Setting up process-specific pfctl rules for proxy port: {}", local_proxy_port);
        warn!("macOS pfctl doesn't support PID-based filtering, falling back to global rules");
        
        self.setup_fallback_global_rules(local_proxy_port, COMMON_WEB_PORTS).await
    }
    
    /// Sets up pattern-based rules (falls back to global rules on macOS)
    pub async fn setup_pattern_based_rules(&mut self, local_proxy_port: u16, _target_patterns: Vec<String>) -> Result<()> {
        info!("Setting up pattern-based pfctl rules for proxy port: {}", local_proxy_port);
        warn!("macOS pfctl doesn't support pattern-based filtering, using global rules");
        
        self.setup_fallback_global_rules(local_proxy_port, EXTENDED_WEB_PORTS).await
    }
    
    /// Common fallback to global rules when process-specific features are not supported
    async fn setup_fallback_global_rules(&mut self, local_proxy_port: u16, ports: &[u16]) -> Result<()> {
        let rules = self.generate_global_rules(local_proxy_port, ports.to_vec());
        self.apply_rules(rules).await
    }
    
    /// 应用pfctl规则的通用方法
    async fn apply_rules(&mut self, rules: String) -> Result<()> {
        // 写入规则文件
        std::fs::write(&self.rule_file_path, &rules)?;
        debug!("Wrote pfctl rules to: {:?}", self.rule_file_path);

        // 确保pfctl已启用并创建anchor
        self.ensure_pfctl_enabled().await?;

        // 使用anchor加载规则（安全方式）
        self.load_rules_with_anchor().await?;

        // 验证规则加载成功
        self.verify_rules_loaded().await?;

        self.active_rules = vec![rules];
        info!("✅ pfctl transparent proxy rules activated with anchor: {}", self.anchor_name);

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

    /// 确保pfctl已启用并创建anchor
    async fn ensure_pfctl_enabled(&self) -> Result<()> {
        debug!("Ensuring pfctl is enabled and anchor exists");

        // 启用pfctl - 使用管理员权限
        let output = utils::execute_admin_command("pfctl", &["-e"])?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        debug!("pfctl enable output: {}", stderr);

        // 检查anchor是否存在，如果不存在则创建
        self.ensure_anchor_exists().await?;

        Ok(())
    }

    /// 确保anchor存在并在主配置中引用
    async fn ensure_anchor_exists(&self) -> Result<()> {
        debug!("Ensuring anchor '{}' exists and is referenced", self.anchor_name);

        // 创建一个临时的主配置文件，包含对我们anchor的引用
        let temp_main_config = format!("{}/pf_main_{}.conf", TEMP_DIR, self.anchor_name);
        let anchor_reference = format!(
            "# Temporary pfctl configuration with anchor reference\n\
             # This allows our anchor to be loaded and take effect\n\
             \n\
             # Load anchor for takeover transparent proxy\n\
             load anchor \"{}\" from \"{}\"\n",
            self.anchor_name,
            self.rule_file_path.display()
        );

        std::fs::write(&temp_main_config, &anchor_reference)?;
        debug!("Created temporary main config with anchor reference: {}", temp_main_config);

        Ok(())
    }

    /// 使用正确的方法加载pfctl规则（支持anchor）
    async fn load_rules_with_anchor(&mut self) -> Result<()> {
        debug!("Loading pfctl rules with proper anchor support");

        // 方法1：尝试使用anchor + 主配置引用
        if let Ok(()) = self.try_anchor_with_main_config().await {
            info!("Successfully loaded rules using anchor method");
            return Ok(());
        }

        // 方法2：使用管道方式直接加载（类似您的测试脚本）
        warn!("Anchor method failed, trying pipe method");
        self.load_rules_with_pipe().await
    }

    /// 尝试使用anchor + 主配置引用的方式
    async fn try_anchor_with_main_config(&mut self) -> Result<()> {

        // 创建主配置文件，引用我们的anchor
        let main_config_path = format!("{}/main_{}.conf", TEMP_DIR, self.anchor_name);
        let our_rules = std::fs::read_to_string(&self.rule_file_path)?;
        
        // 创建包含anchor引用的主配置
        let main_config = format!(
            "# Takeover pfctl configuration with anchor\n\
             # Generated automatically\n\
             \n\
             # Define anchor\n\
             anchor \"{}\" {{\n\
             {}\n\
             }}\n",
            self.anchor_name,
            our_rules
        );

        std::fs::write(&main_config_path, main_config)?;
        debug!("Created main config with embedded anchor: {}", main_config_path);

        // 加载主配置 - 使用管理员权限
        let output = utils::execute_admin_command("pfctl", &["-f", &main_config_path])?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            debug!("Main config loading failed: {}", stderr);
            return Err(anyhow::anyhow!("Main config loading failed"));
        }

        // 验证规则是否生效
        self.verify_rules_active().await?;
        
        info!("✅ Successfully loaded pfctl rules with anchor");
        Ok(())
    }

    /// 使用管道方式加载规则（类似测试脚本的方法）
    async fn load_rules_with_pipe(&mut self) -> Result<()> {
        debug!("Loading pfctl rules using pipe method");

        let our_rules = std::fs::read_to_string(&self.rule_file_path)?;
        
        // 使用 sudo 管道方式加载规则（使用缓存的权限）
        let mut child = tokio::process::Command::new("sudo")
            .args(&["pfctl", "-f", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        if let Some(stdin) = child.stdin.as_mut() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(our_rules.as_bytes()).await?;
            stdin.flush().await?;
        }

        let output = child.wait_with_output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("Pipe method failed: {}", stderr);
            return Err(anyhow::anyhow!("Pipe method failed: {}", stderr));
        }

        // 标记使用了备用方法
        self.using_alternative_method = true;
        
        // 验证规则是否生效
        self.verify_rules_active().await?;
        
        info!("✅ Successfully loaded pfctl rules using pipe method");
        Ok(())
    }

    /// 验证规则是否真正生效
    async fn verify_rules_active(&self) -> Result<()> {
        // 检查NAT规则 - 使用管理员权限
        let nat_output = utils::execute_admin_command("pfctl", &["-s", "nat"])?;

        if nat_output.status.success() {
            let nat_rules = String::from_utf8_lossy(&nat_output.stdout);
            let nat_count = nat_rules.lines()
                .filter(|l| l.contains("127.0.0.1") && !l.trim().is_empty())
                .count();

            if nat_count > 0 {
                debug!("Verified {} active NAT rules", nat_count);
                return Ok(());
            }
        }

        Err(anyhow::anyhow!("No active NAT rules found"))
    }

    /// 验证规则已加载（智能检测加载方式）
    async fn verify_rules_loaded_smart(&self) -> Result<()> {
        debug!("Verifying pfctl rules are loaded (smart detection)");

        // 先尝试检查anchor中的规则
        let anchor_result = self.check_anchor_rules().await;
        if anchor_result.is_ok() {
            return anchor_result;
        }

        // 如果anchor没有规则，检查主规则集
        debug!("No rules in anchor, checking main ruleset");
        self.check_main_ruleset_rules().await
    }

    /// 检查anchor中的规则
    async fn check_anchor_rules(&self) -> Result<()> {
        let nat_output = utils::execute_admin_command("pfctl", &["-a", &self.anchor_name, "-s", "nat"])?;

        if !nat_output.status.success() {
            return Err(anyhow::anyhow!("Failed to query anchor NAT rules"));
        }

        let nat_rules = String::from_utf8_lossy(&nat_output.stdout);
        let nat_count = nat_rules.lines()
            .filter(|l| l.contains("127.0.0.1") && !l.trim().is_empty())
            .count();

        if nat_count > 0 {
            debug!("Found {} NAT rules in anchor '{}'", nat_count, self.anchor_name);
            return Ok(());
        }

        Err(anyhow::anyhow!("No rules found in anchor"))
    }

    /// 检查主规则集中的规则
    async fn check_main_ruleset_rules(&self) -> Result<()> {
        let nat_output = utils::execute_admin_command("pfctl", &["-s", "nat"])?;

        if !nat_output.status.success() {
            return Err(anyhow::anyhow!("Failed to query main NAT rules"));
        }

        let nat_rules = String::from_utf8_lossy(&nat_output.stdout);
        let nat_count = nat_rules.lines()
            .filter(|l| l.contains("127.0.0.1") && !l.trim().is_empty())
            .count();

        // 检查过滤规则 - 使用管理员权限
        let filter_output = utils::execute_admin_command("pfctl", &["-s", "rules"])?;

        if !filter_output.status.success() {
            return Err(anyhow::anyhow!("Failed to query main filter rules"));
        }

        let filter_rules = String::from_utf8_lossy(&filter_output.stdout);
        let filter_count = filter_rules.lines()
            .filter(|l| l.contains("route-to") && !l.trim().is_empty())
            .count();

        debug!("Found {} NAT rules and {} filter rules in main ruleset", nat_count, filter_count);

        if nat_count == 0 {
            warn!("No NAT rules found - transparent proxy may not work");
        }
        if filter_count == 0 {
            warn!("No route-to rules found - outbound traffic may not be intercepted");
        }

        if nat_count > 0 || filter_count > 0 {
            Ok(())
        } else {
            Err(anyhow::anyhow!("No relevant rules found in main ruleset"))
        }
    }
    
    /// 验证规则已加载（公共接口）
    pub async fn verify_rules_loaded(&self) -> Result<()> {
        self.verify_rules_loaded_smart().await
    }

    /// 清理规则（智能清理方式）
    pub async fn cleanup_rules(&mut self) -> Result<()> {
        if self.using_alternative_method {
            info!("Cleaning up pfctl rules using alternative cleanup method");
            self.cleanup_alternative_rules().await
        } else {
            info!("Cleaning up pfctl rules from anchor: {}", self.anchor_name);
            self.cleanup_anchor_rules().await
        }
    }

    /// 清理anchor规则
    async fn cleanup_anchor_rules(&mut self) -> Result<()> {
        // 只清理我们的anchor，不影响系统规则 - 使用管理员权限
        let output = utils::execute_admin_command("pfctl", &["-a", &self.anchor_name, "-F", "all"])?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to flush pfctl rules from anchor '{}': {}", self.anchor_name, stderr);
        } else {
            debug!("Successfully flushed pfctl rules from anchor: {}", self.anchor_name);
        }

        self.cleanup_common_files().await?;
        info!("✅ pfctl anchor rules cleanup completed");
        Ok(())
    }

    /// 清理备用方法的规则
    async fn cleanup_alternative_rules(&mut self) -> Result<()> {
        // 尝试恢复备份的规则
        let backup_file = format!("{}/pf_backup_{}.conf", TEMP_DIR, self.anchor_name);
        
        if std::path::Path::new(&backup_file).exists() {
            debug!("Restoring original pfctl rules from backup");
            let output = tokio::process::Command::new("pfctl")
                .args(&["-f", &backup_file])
                .output()
                .await?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!("Failed to restore original rules: {}", stderr);
                // 如果恢复失败，至少清空规则
                self.emergency_clear_rules().await?;
            } else {
                info!("Successfully restored original pfctl rules");
            }

            // 删除备份文件
            std::fs::remove_file(&backup_file).ok();
        } else {
            warn!("No backup found, performing emergency cleanup");
            self.emergency_clear_rules().await?;
        }

        self.cleanup_common_files().await?;
        self.using_alternative_method = false;
        info!("✅ pfctl alternative rules cleanup completed");
        Ok(())
    }

    /// 紧急清空规则
    async fn emergency_clear_rules(&self) -> Result<()> {
        warn!("Performing emergency pfctl rules clear");
        let output = tokio::process::Command::new("pfctl")
            .args(&["-F", "all"])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("Emergency clear failed: {}", stderr);
        }

        Ok(())
    }

    /// 清理通用文件
    async fn cleanup_common_files(&mut self) -> Result<()> {
        // 清理规则文件
        if self.rule_file_path.exists() {
            if let Err(e) = std::fs::remove_file(&self.rule_file_path) {
                warn!("Failed to remove rule file {:?}: {}", self.rule_file_path, e);
            } else {
                debug!("Removed rule file: {:?}", self.rule_file_path);
            }
        }

        // 清理临时文件
        let combined_config = format!("{}/combined_{}.conf", TEMP_DIR, self.anchor_name);
        std::fs::remove_file(&combined_config).ok();

        let temp_main_config = format!("{}/pf_main_{}.conf", TEMP_DIR, self.anchor_name);
        std::fs::remove_file(&temp_main_config).ok();

        self.active_rules.clear();
        Ok(())
    }

    /// 紧急清理（异步版本，用于Drop trait）
    pub async fn emergency_cleanup(&mut self) -> Result<()> {
        warn!("Performing emergency pfctl cleanup for anchor: {}", self.anchor_name);
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
        
        let max_attempts = MAX_READINESS_ATTEMPTS;
        let base_delay = Duration::from_millis(BASE_READINESS_DELAY_MS);
        
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
                        Duration::from_millis(MAX_READINESS_DELAY_MS)
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
