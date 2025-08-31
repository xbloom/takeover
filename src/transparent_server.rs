use crate::config::ProxyConfig;
use crate::socks5::{Socks5Client, DataForwarder};
// TrafficStats 通过方法返回值使用，不需要直接导入
use crate::routing_engine::{RoutingEngine, RoutingDecision, TlsInterceptionMode};
use crate::transparent_proxy_manager::TransparentProxyManager;
use crate::user_manager::UserManager;
use crate::protocol_handler::TransparentConnectionParser;
use crate::tls_interception::{TlsInterceptionManager, RuntimeTlsConfig, TlsStream};
use crate::protocol_handler::{ProtocolInfo, ProtocolType};
use anyhow::Result;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use std::sync::Arc;

use tracing::{info, warn, error, debug};

const PROXY_BUFFER_SIZE: usize = 8192;

/// 透明代理服务器 - 核心的连接处理器
pub struct TransparentServer {
    proxy_config: ProxyConfig,
    routing_engine: RoutingEngine,
    proxy_manager: TransparentProxyManager,
    user_manager: UserManager,
    #[allow(dead_code)]
    protocol_parser: TransparentConnectionParser,
    /// TLS拦截管理器
    tls_manager: Option<TlsInterceptionManager>,
    local_listener: Option<TcpListener>,
    local_port: Option<u16>,
    /// 连接数限制信号量
    connection_semaphore: Arc<Semaphore>,
    /// 最大并发连接数
    max_connections: usize,
    /// 是否应该停止服务
    should_stop: Arc<std::sync::atomic::AtomicBool>,
}

impl TransparentServer {
    pub fn new(proxy_config: ProxyConfig, routing_engine: RoutingEngine) -> Self {
        const DEFAULT_MAX_CONNECTIONS: usize = 1000;
        Self {
            proxy_config,
            routing_engine,
            proxy_manager: TransparentProxyManager::new(),
            user_manager: UserManager::new(),
            protocol_parser: TransparentConnectionParser::new(),
            tls_manager: None,
            local_listener: None,
            local_port: None,
            connection_semaphore: Arc::new(Semaphore::new(DEFAULT_MAX_CONNECTIONS)),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            should_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// 使用自定义用户管理器创建透明代理服务器
    pub fn with_user_manager(proxy_config: ProxyConfig, routing_engine: RoutingEngine, user_manager: UserManager) -> Self {
        const DEFAULT_MAX_CONNECTIONS: usize = 1000;
        Self {
            proxy_config,
            routing_engine,
            proxy_manager: TransparentProxyManager::new(),
            user_manager,
            protocol_parser: TransparentConnectionParser::new(),
            tls_manager: None,
            local_listener: None,
            local_port: None,
            connection_semaphore: Arc::new(Semaphore::new(DEFAULT_MAX_CONNECTIONS)),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            should_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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

    /// 启用TLS拦截功能
    /// 
    /// # 参数
    /// - `cert_store_path`: 可选的证书存储路径。如果为 None，则使用默认路径 `~/.config/takeover/`
    pub fn enable_tls_interception(&mut self, cert_store_path: Option<&str>) -> Result<()> {
        let tls_manager = match cert_store_path {
            Some(path) => {
                info!("✅ TLS拦截功能已启用，证书存储路径: {}", path);
                TlsInterceptionManager::new_real(path)
            },
            None => {
                info!("✅ TLS拦截功能已启用，使用默认证书存储路径");
                TlsInterceptionManager::new_real_with_default_path()?
            }
        };
        
        // 设置默认运行时配置
        let mut runtime_config = RuntimeTlsConfig::default();
        runtime_config.enabled = true;
        runtime_config.mode = TlsInterceptionMode::Selective;
        
        let mut tls_manager = tls_manager;
        tls_manager.set_runtime_config(runtime_config);
        self.tls_manager = Some(tls_manager);
        
        Ok(())
    }

    /// 禁用TLS拦截功能
    pub fn disable_tls_interception(&mut self) {
        self.tls_manager = None;
        info!("❌ TLS拦截功能已禁用");
    }

    /// 配置TLS拦截设置
    pub fn configure_tls_interception(&mut self, config: RuntimeTlsConfig) -> Result<()> {
        if let Some(ref mut tls_manager) = self.tls_manager {
            tls_manager.set_runtime_config(config);
            info!("✅ TLS拦截配置已更新");
            Ok(())
        } else {
            Err(anyhow::anyhow!("TLS拦截功能未启用，请先调用 enable_tls_interception()"))
        }
    }

    /// 获取TLS拦截管理器的引用
    pub fn tls_manager(&self) -> Option<&TlsInterceptionManager> {
        self.tls_manager.as_ref()
    }

    /// 获取TLS拦截管理器的可变引用
    pub fn tls_manager_mut(&mut self) -> Option<&mut TlsInterceptionManager> {
        self.tls_manager.as_mut()
    }

    /// 初始化透明代理服务器（不进入运行循环）
    pub async fn initialize(&mut self, intercepted_ports: &[u16]) -> Result<()> {
        info!("🚀 Initializing transparent proxy server");
        
        // 确保代理用户存在
        self.user_manager.ensure_proxy_user().await?;

        // 如果启用了TLS拦截，初始化TLS环境
        if let Some(ref mut tls_manager) = self.tls_manager {
            info!("🔐 初始化TLS拦截环境...");
            if let Err(e) = tls_manager.initialize().await {
                warn!("TLS拦截初始化失败，但继续启动透明代理: {}", e);
            } else {
                info!("✅ TLS拦截环境初始化完成");
            }
        }
        
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
        
        // 3. 保存监听器和端口信息
        self.local_listener = Some(listener);
        self.local_port = Some(local_port);
        
        info!("✅ Transparent proxy server initialized on port: {}", local_port);
        info!("🌐 Intercepting traffic on ports: {:?}", intercepted_ports);
        
        Ok(())
    }

    /// 启动透明代理服务器（包含初始化和运行）
    pub async fn start(&mut self, intercepted_ports: &[u16]) -> Result<()> {
        self.initialize(intercepted_ports).await?;
        self.run_server().await
    }

    /// 运行服务器（必须先调用 initialize）
    pub async fn serve(&mut self) -> Result<()> {
        self.run_server().await
    }

    /// 停止服务器
    pub async fn stop(&mut self) -> Result<()> {
        info!("🛑 Stopping transparent proxy server");
        
        // 设置停止标志
        self.should_stop.store(true, std::sync::atomic::Ordering::Relaxed);
        
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
        info!("📊 Max concurrent connections: {}", self.max_connections);

        let mut consecutive_errors = 0u32;
        const MAX_CONSECUTIVE_ERRORS: u32 = 10;
        const ERROR_BACKOFF_MS: u64 = 100;

        loop {
            // 检查是否应该停止
            if self.should_stop.load(std::sync::atomic::Ordering::Relaxed) {
                info!("🛑 Received stop signal, exiting server loop");
                break;
            }

            match listener.accept().await {
                Ok((client_stream, client_addr)) => {
                    consecutive_errors = 0; // 重置错误计数
                    debug!("New transparent connection from: {}", client_addr);
                    
                    // 检查连接数限制
                    match self.connection_semaphore.clone().try_acquire_owned() {
                        Ok(permit) => {
                            let proxy_config = self.proxy_config.clone();
                            let routing_engine = self.routing_engine.clone();
                            let user_manager = self.user_manager.clone();
                            let protocol_parser = TransparentConnectionParser::new();
                            let tls_manager = self.tls_manager.clone();
                            
                            // 为每个连接启动处理任务，传递许可证
                            tokio::spawn(async move {
                                let _permit = permit; // 任务结束时自动释放许可
                                if let Err(e) = Self::handle_connection(
                                    client_stream,
                                    client_addr,
                                    proxy_config,
                                    routing_engine,
                                    user_manager,
                                    protocol_parser,
                                    tls_manager,
                                ).await {
                                    debug!("Connection {} handling error: {}", client_addr, e);
                                }
                            });
                        }
                        Err(_) => {
                            warn!("⚠️ Maximum connections ({}) reached, rejecting connection from {}", 
                                  self.max_connections, client_addr);
                            // 直接关闭连接
                            drop(client_stream);
                        }
                    }
                }
                Err(e) => {
                    consecutive_errors += 1;
                    
                    // 区分错误类型
                    let is_temporary = matches!(e.kind(), 
                        std::io::ErrorKind::WouldBlock |
                        std::io::ErrorKind::Interrupted |
                        std::io::ErrorKind::TimedOut
                    );
                    
                    if is_temporary || consecutive_errors < MAX_CONSECUTIVE_ERRORS {
                        warn!("⚠️ Temporary accept error #{}: {} (retrying)", consecutive_errors, e);
                        
                        // 指数退避
                        let backoff_ms = ERROR_BACKOFF_MS * (1u64 << (consecutive_errors.min(5) as u64));
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    } else {
                        error!("❌ Too many consecutive accept errors ({}), stopping server: {}", consecutive_errors, e);
                        break;
                    }
                }
            }
        }

        info!("📊 Server loop ended. Waiting for active connections to finish...");
        
        // 等待所有连接完成
        let timeout = Duration::from_secs(30);
        match tokio::time::timeout(timeout, async {
            // 获取所有许可证，意味着所有连接都已结束
            let _all_permits = self.connection_semaphore.clone().acquire_many_owned(self.max_connections as u32).await;
        }).await {
            Ok(_) => info!("✅ All connections completed gracefully"),
            Err(_) => warn!("⚠️ Timeout waiting for connections to finish"),
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
        protocol_parser: TransparentConnectionParser,
        tls_manager: Option<TlsInterceptionManager>,
    ) -> Result<()> {
        debug!("Handling transparent connection from: {}", client_addr);

        // 1. 使用新的协议解析器解析连接信息
        let protocol_info = protocol_parser.parse_from_stream(&client_stream).await?;
        debug!("Parsed protocol info: {:?}", protocol_info);

        let target_host = &protocol_info.target_host;
        let target_port = protocol_info.target_port;

        // 🛡️ 防循环检查：避免拦截到SOCKS5代理的连接
        if Self::is_loop_connection(&proxy_config, target_host, target_port) {
            debug!("🛡️ Detected potential loop connection to SOCKS5 proxy, using direct connection");
            return Self::handle_direct_connection(client_stream, target_host, target_port, None, &user_manager).await;
        }

        let process_path = None; // TODO: 可以通过系统调用获取进程路径
        
        // 2. 路由决策 - 这是核心！
        let decision = routing_engine.should_proxy(
            target_host, 
            target_port, 
            protocol_info.full_url.as_deref(),
            process_path,
            &client_addr
        );
        
        match decision {
            RoutingDecision::Proxy { rewritten_url, tls_interception } => {
                if let Some(ref rewritten) = rewritten_url {
                    info!("🔀 Proxying {}:{} via SOCKS5 (URL rewritten to: {})", target_host, target_port, rewritten);
                } else {
                    info!("🔀 Proxying {}:{} via SOCKS5", target_host, target_port);
                }
                
                // 检查是否需要TLS拦截
                if Self::should_intercept_tls(&protocol_info, &tls_interception, &tls_manager).await {
                    info!("🔐 启用TLS拦截代理模式：{}:{}", target_host, target_port);
                    Self::handle_tls_intercept_proxy_connection(
                        client_stream, proxy_config, target_host, target_port, 
                        rewritten_url, &user_manager, tls_manager.as_ref().unwrap()
                    ).await
                } else {
                    Self::handle_proxy_connection(client_stream, proxy_config, target_host, target_port, rewritten_url, &user_manager).await
                }
            }
            RoutingDecision::Direct { rewritten_url, tls_interception } => {
                if let Some(ref rewritten) = rewritten_url {
                    info!("🔗 Direct connection to {}:{} (URL rewritten to: {})", target_host, target_port, rewritten);
                } else {
                    info!("🔗 Direct connection to {}:{}", target_host, target_port);
                }
                
                // 检查是否需要TLS拦截
                if Self::should_intercept_tls(&protocol_info, &tls_interception, &tls_manager).await {
                    info!("🔐 启用TLS拦截直连模式：{}:{}", target_host, target_port);
                    Self::handle_tls_intercept_direct_connection(
                        client_stream, target_host, target_port, 
                        rewritten_url, &user_manager, tls_manager.as_ref().unwrap()
                    ).await
                } else {
                    Self::handle_direct_connection(client_stream, target_host, target_port, rewritten_url, &user_manager).await
                }
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

    /// 判断是否应该进行TLS拦截
    async fn should_intercept_tls(
        protocol_info: &ProtocolInfo,
        tls_interception_mode: &TlsInterceptionMode,
        tls_manager: &Option<TlsInterceptionManager>,
    ) -> bool {
        // 只有HTTPS连接才考虑TLS拦截
        if !matches!(protocol_info.protocol_type, ProtocolType::Https) {
            return false;
        }
        
        // 没有TLS管理器则不拦截
        let tls_manager = match tls_manager.as_ref() {
            Some(manager) => manager,
            None => return false,
        };
        
        // 检查TLS管理器是否启用
        if !tls_manager.is_enabled() {
            return false;
        }
        
        // 根据路由决策的TLS拦截模式判断
        match tls_interception_mode {
            TlsInterceptionMode::None => false,
            TlsInterceptionMode::Selective | TlsInterceptionMode::Full => {
                // 使用TLS管理器的拦截决策
                tls_manager.should_intercept(&protocol_info.target_host).await
            }
        }
    }

    /// 使用TLS拦截的代理连接处理
    async fn handle_tls_intercept_proxy_connection(
        client_stream: TcpStream,
        proxy_config: ProxyConfig,
        target_host: &str,
        target_port: u16,
        _rewritten_url: Option<String>,
        user_manager: &UserManager,
        tls_manager: &TlsInterceptionManager,
    ) -> Result<()> {
        info!("🔐 处理TLS拦截代理连接: {}:{}", target_host, target_port);
        
        // 1. 获取证书管理器并生成证书
        let cert_manager = tls_manager.cert_manager();
        let cert = cert_manager.get_or_generate_cert(target_host).await?;
        
        // 2. 创建服务器TLS配置（用我们的证书）
        let server_config = crate::tls_interception::create_server_tls_config(&cert)?;
        let acceptor = tokio_rustls::TlsAcceptor::from(server_config);
        
        // 3. 与客户端建立TLS连接
        let client_tls_stream = acceptor.accept(client_stream).await?;
        debug!("✅ 与客户端建立TLS连接成功: {}", target_host);
        
        // 4. 连接到SOCKS5代理
        let host = proxy_config.host().map_err(|e| anyhow::anyhow!("Invalid proxy host: {}", e))?;
        let port = proxy_config.port().map_err(|e| anyhow::anyhow!("Invalid proxy port: {}", e))?;
        
        debug!("🔗 通过SOCKS5代理连接到目标服务器 {}:{} -> {}:{}", host, port, target_host, target_port);
        
        let mut proxy_stream = Self::create_bypass_connection(&host, port, user_manager).await
            .map_err(|e| anyhow::anyhow!("Failed to connect to SOCKS5 proxy: {}", e))?;
        
        // 5. 进行SOCKS5握手
        crate::socks5::Socks5Client::connect(&mut proxy_stream, &proxy_config, target_host, target_port).await?;
        debug!("✅ SOCKS5握手完成，已连接到目标服务器");
        
        // 6. 与真实目标服务器建立TLS连接（通过代理）
        let client_config = crate::tls_interception::create_client_tls_config()?;
        let connector = tokio_rustls::TlsConnector::from(client_config);
        let domain_name = rustls::pki_types::ServerName::try_from(target_host.to_string())?;
        let server_tls_stream = connector.connect(domain_name, proxy_stream).await?;
        debug!("✅ 与目标服务器建立TLS连接成功（通过代理）: {}:{}", target_host, target_port);
        
        // 7. 包装TLS流并双向转发数据
        let mut client_wrapper = crate::tls_interception::RustlsTlsStreamWrapper::Server(client_tls_stream);
        let mut server_wrapper = crate::tls_interception::RustlsTlsStreamWrapper::Client(server_tls_stream);
        
        info!("✅ TLS拦截代理连接建立成功，开始双向数据转发");
        
        // 8. 双向转发TLS数据
        Self::forward_tls_data(&mut client_wrapper, &mut server_wrapper).await?;
        
        info!("TLS拦截代理连接完成: {}:{}", target_host, target_port);
        Ok(())
    }

    /// 使用TLS拦截的直连处理
    async fn handle_tls_intercept_direct_connection(
        client_stream: TcpStream,
        target_host: &str,
        target_port: u16,
        _rewritten_url: Option<String>,
        _user_manager: &UserManager,
        tls_manager: &TlsInterceptionManager,
    ) -> Result<()> {
        info!("🔐 处理TLS拦截直连: {}:{}", target_host, target_port);
        
        // 1. 执行TLS拦截，获取双向TLS流
        let (mut client_tls_stream, mut server_tls_stream) = tls_manager
            .intercept(client_stream, target_host, target_port)
            .await?;
        
        info!("✅ TLS拦截建立成功，开始双向数据转发");
        
        // 2. 双向转发TLS数据
        Self::forward_tls_data(client_tls_stream.as_mut(), server_tls_stream.as_mut()).await?;
        
        info!("TLS拦截直连完成: {}:{}", target_host, target_port);
        Ok(())
    }



    /// TLS数据双向转发
    async fn forward_tls_data(
        client_stream: &mut dyn TlsStream,
        server_stream: &mut dyn TlsStream,
    ) -> Result<()> {
        let mut client_buffer = vec![0u8; PROXY_BUFFER_SIZE];
        let mut server_buffer = vec![0u8; PROXY_BUFFER_SIZE];
        
        let mut client_to_server_bytes = 0u64;
        let mut server_to_client_bytes = 0u64;
        
        loop {
            tokio::select! {
                // 客户端到服务器的数据传输
                result = client_stream.read(&mut client_buffer) => {
                    match result {
                        Ok(0) => {
                            debug!("客户端连接关闭");
                            break;
                        }
                        Ok(n) => {
                            if let Err(e) = server_stream.write(&client_buffer[..n]).await {
                                debug!("向服务器写入失败: {}", e);
                                break;
                            }
                            if let Err(e) = server_stream.flush().await {
                                debug!("服务器流刷新失败: {}", e);
                                break;
                            }
                            client_to_server_bytes += n as u64;
                        }
                        Err(e) => {
                            debug!("从客户端读取失败: {}", e);
                            break;
                        }
                    }
                }
                
                // 服务器到客户端的数据传输
                result = server_stream.read(&mut server_buffer) => {
                    match result {
                        Ok(0) => {
                            debug!("服务器连接关闭");
                            break;
                        }
                        Ok(n) => {
                            if let Err(e) = client_stream.write(&server_buffer[..n]).await {
                                debug!("向客户端写入失败: {}", e);
                                break;
                            }
                            if let Err(e) = client_stream.flush().await {
                                debug!("客户端流刷新失败: {}", e);
                                break;
                            }
                            server_to_client_bytes += n as u64;
                        }
                        Err(e) => {
                            debug!("从服务器读取失败: {}", e);
                            break;
                        }
                    }
                }
            }
        }
        
        info!("TLS数据转发完成 - 客户端→服务器: {} bytes, 服务器→客户端: {} bytes", 
              client_to_server_bytes, server_to_client_bytes);
        
        Ok(())
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



    pub fn get_local_port(&self) -> Option<u16> {
        self.local_port
    }

    /// 设置最大并发连接数
    pub fn set_max_connections(&mut self, max_connections: usize) {
        self.max_connections = max_connections;
        self.connection_semaphore = Arc::new(Semaphore::new(max_connections));
        info!("📊 Updated max concurrent connections to: {}", max_connections);
    }

    /// 获取当前活跃连接数
    pub fn get_active_connections(&self) -> usize {
        self.max_connections - self.connection_semaphore.available_permits()
    }

    /// 获取服务器状态
    pub fn get_server_stats(&self) -> (usize, usize, bool) {
        (
            self.get_active_connections(),
            self.max_connections,
            self.should_stop.load(std::sync::atomic::Ordering::Relaxed)
        )
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
    // Tests moved to protocol_handler module
}

