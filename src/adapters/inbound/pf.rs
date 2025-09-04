//! macOS pfctl入站适配器
//! 
//! 这个适配器将现有的pfctl.rs和transparent_proxy_manager.rs功能
//! 重构为符合六边形架构的入站端口实现。

use std::sync::Arc;
use std::net::SocketAddr;
use std::pin::Pin;
use tokio::sync::{RwLock, mpsc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use futures::Stream;
use tracing::{info, warn, error, debug, instrument};
use async_trait::async_trait;

use crate::core::ports::{InboundPort, InboundConfig, AdapterStatus, AppError};
use crate::core::model::{Connection, ConnectionMetadata};
use crate::transparent_proxy_manager::TransparentProxyManager;
use crate::protocol_handler::TransparentConnectionParser;

/// macOS pfctl入站适配器
/// 
/// 负责：
/// 1. 设置pfctl规则进行流量拦截
/// 2. 监听本地端口接收被拦截的连接
/// 3. 将TCP连接转换为Connection对象
/// 4. 管理透明代理的生命周期
pub struct PfInboundAdapter {
    /// 透明代理管理器
    proxy_manager: Arc<RwLock<TransparentProxyManager>>,
    /// 本地监听器
    listener: Arc<Mutex<Option<TcpListener>>>,
    /// 配置信息
    config: Arc<RwLock<Option<InboundConfig>>>,
    /// 适配器状态
    status: Arc<RwLock<AdapterStatus>>,
    /// 连接统计
    connection_count: Arc<RwLock<usize>>,
    /// 连接发送通道
    connection_sender: Arc<RwLock<Option<mpsc::UnboundedSender<Result<Connection, AppError>>>>>,
}

impl PfInboundAdapter {
    /// 创建新的pfctl入站适配器
    pub fn new() -> Self {
        Self {
            proxy_manager: Arc::new(RwLock::new(TransparentProxyManager::new())),
            listener: Arc::new(Mutex::new(None)),
            config: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(AdapterStatus {
                name: "pfctl".to_string(),
                active: false,
                connections: 0,
                last_activity: None,
                errors: 0,
            })),
            connection_count: Arc::new(RwLock::new(0)),
            connection_sender: Arc::new(RwLock::new(None)),
        }
    }
    
    /// 使用自定义锚点名称创建适配器
    pub fn with_anchor_name(anchor_name: &str) -> Self {
        Self {
            proxy_manager: Arc::new(RwLock::new(TransparentProxyManager::with_anchor_name(anchor_name))),
            listener: Arc::new(Mutex::new(None)),
            config: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(AdapterStatus {
                name: format!("pfctl({})", anchor_name),
                active: false,
                connections: 0,
                last_activity: None,
                errors: 0,
            })),
            connection_count: Arc::new(RwLock::new(0)),
            connection_sender: Arc::new(RwLock::new(None)),
        }
    }
    
    /// 启动透明代理监听
    #[instrument(skip(self, config))]
    async fn start_transparent_proxy(&self, config: &InboundConfig) -> Result<(), AppError> {
        info!("Starting transparent proxy with pfctl");
        
        // 获取绕过用户（通常是nobody或当前用户）
        let bypass_user = config.bypass_user.as_deref().or(Some("nobody"));
        
        // 启动透明代理管理器
        let mut proxy_manager = self.proxy_manager.write().await;
        proxy_manager.start(
            config.listen_port,
            &config.intercepted_ports,
            bypass_user
        ).await.map_err(|e| AppError::Config(format!("Failed to start transparent proxy: {}", e)))?;
        
        info!("✅ Transparent proxy started successfully");
        Ok(())
    }
    
    /// 启动本地监听器
    #[instrument(skip(self, config))]
    async fn start_listener(&self, config: &InboundConfig) -> Result<(), AppError> {
        let bind_addr = format!("127.0.0.1:{}", config.listen_port);
        debug!("Binding TCP listener to {}", bind_addr);
        
        let listener = TcpListener::bind(&bind_addr)
            .await
            .map_err(|e| AppError::Config(format!("Failed to bind to {}: {}", bind_addr, e)))?;
        
        info!("✅ TCP listener started on {}", bind_addr);
        
        // 存储监听器
        {
            let mut listener_guard = self.listener.lock().await;
            *listener_guard = Some(listener);
        }
        
        Ok(())
    }
    
    /// 从TCP流创建Connection
    async fn create_connection_from_stream(&self, stream: TcpStream) -> Result<Connection, AppError> {
        let source_addr = stream.peer_addr()
            .map_err(|e| AppError::Config(format!("Failed to get source address: {}", e)))?;
        
        // 获取进程信息（如果可能）
        let process_info = self.get_process_info(&source_addr).await;
        
        // 一次性解析协议信息，同时获取目标地址和协议类型
        // [TODO] 是否需要改成diocnatlook系统调用来解析ip？
        let protocol_parser = TransparentConnectionParser::new();
        let (original_dest, detected_protocol, target_host, full_url) = match protocol_parser.parse_from_stream(&stream).await {
            Ok(protocol_info) => {
                let target_host = &protocol_info.target_host;
                let target_port = protocol_info.target_port;
                
                debug!("Parsed target from stream: {}:{} (protocol: {:?})", 
                       target_host, target_port, protocol_info.protocol_type);
                
                // 将主机名解析为IP地址
                let resolved_addr = match tokio::net::lookup_host(format!("{}:{}", target_host, target_port)).await {
                    Ok(mut addrs) => {
                        if let Some(addr) = addrs.next() {
                            debug!("Resolved {}:{} to {}", target_host, target_port, addr);
                            addr
                        } else {
                            debug!("DNS resolution failed for {}:{}, using fallback", target_host, target_port);
                            SocketAddr::new(source_addr.ip(), target_port)
                        }
                    }
                    Err(e) => {
                        debug!("DNS lookup failed for {}:{}: {}, using fallback", target_host, target_port, e);
                        SocketAddr::new(source_addr.ip(), target_port)
                    }
                };
                
                let protocol_str = match protocol_info.protocol_type {
                    crate::protocol_handler::ProtocolType::Http => "http",
                    crate::protocol_handler::ProtocolType::Https => "https",
                    crate::protocol_handler::ProtocolType::Unknown => "tcp",
                };
                
                (resolved_addr, protocol_str, Some(protocol_info.target_host), protocol_info.full_url)
            }
            Err(e) => {
                debug!("Failed to parse protocol from stream: {}, using fallback", e);
                // 解析失败时的回退策略：假设是HTTP（80端口）
                let fallback_addr = SocketAddr::new(source_addr.ip(), 80);
                (fallback_addr, "tcp", None, None)
            }
        };
        
        // 创建连接元数据
        let mut metadata = ConnectionMetadata::default();
        metadata.protocol = detected_protocol.to_string();
        metadata.process_name = process_info.process_name;
        metadata.custom_data.insert("adapter".to_string(), serde_json::json!("pfctl"));
        metadata.custom_data.insert("intercepted".to_string(), serde_json::json!(true));
        metadata.custom_data.insert("detected_protocol".to_string(), serde_json::json!(detected_protocol));
        
        // 添加解析到的额外信息
        if let Some(ref host) = target_host {
            metadata.custom_data.insert("target_host".to_string(), serde_json::json!(host));
        }
        if let Some(ref url) = full_url {
            metadata.custom_data.insert("full_url".to_string(), serde_json::json!(url));
        }
        
        debug!("检测到协议: {} (端口: {}, 目标: {:?})", 
               detected_protocol, original_dest.port(), target_host);
        
        let connection = Connection {
            id: uuid::Uuid::new_v4(),
            source_addr,
            original_dest,
            metadata,
            stream: Some(stream), // 直接包含TCP流
            inbound_tls_stream: None, // TLS流初始化为None
            outbound_tls_stream: None, // TLS流初始化为None
        };
        
        debug!("Created connection {} from {} to {} (protocol: {})", 
               connection.id, source_addr, original_dest, detected_protocol);
        
        Ok(connection)
    }
    
    /// 获取进程信息
    async fn get_process_info(&self, _source_addr: &SocketAddr) -> ProcessInfo {
        // 简化实现，实际应该通过系统调用获取进程信息
        ProcessInfo {
            process_name: None,
            user_id: None,
        }
    }
    
    /// 更新适配器状态
    async fn update_status(&self, active: bool, error_occurred: bool) {
        let mut status = self.status.write().await;
        status.active = active;
        status.connections = *self.connection_count.read().await;
        status.last_activity = Some(std::time::SystemTime::now());
        
        if error_occurred {
            status.errors += 1;
        }
    }
    
    /// 处理连接接收循环
    async fn connection_accept_loop(
        self: Arc<Self>,
        sender: mpsc::UnboundedSender<Result<Connection, AppError>>
    ) {
        debug!("Starting connection accept loop");
        
        // 等待监听器可用并取出来使用
        let listener = loop {
            let mut listener_guard = self.listener.lock().await;
            if let Some(listener) = listener_guard.take() {
                debug!("Got listener, starting to accept connections");
                break listener;
            } else {
                drop(listener_guard);
                debug!("Waiting for listener to be available...");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        };
        
        debug!("✅ Connection accept loop ready, waiting for connections...");
        
        loop {
            // 检查是否应该停止
            {
                let status = self.status.read().await;
                if !status.active {
                    debug!("Adapter not active, stopping accept loop");
                    break;
                }
            }
            
            // 接受新连接
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
                    debug!("🎯 New connection accepted from {}", peer_addr);
                    
                    // 更新连接计数
                    {
                        let mut count = self.connection_count.write().await;
                        *count += 1;
                    }
                    
                    // 创建连接对象
                    match self.create_connection_from_stream(stream).await {
                        Ok(connection) => {
                            debug!("Created connection: {}", connection.id);
                            if let Err(_) = sender.send(Ok(connection)) {
                                warn!("Failed to send connection through channel");
                                break;
                            }
                        }
                        Err(e) => {
                            error!("Failed to create connection: {}", e);
                            if let Err(_) = sender.send(Err(e)) {
                                warn!("Failed to send error through channel");
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                    self.update_status(true, true).await;
                    
                    // 发送错误到流
                    if let Err(_) = sender.send(Err(AppError::Config(format!("Accept failed: {}", e)))) {
                        warn!("Failed to send accept error through channel");
                        break;
                    }
                    
                    // 短暂延迟后继续
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
        
        info!("Connection accept loop stopped");
    }
}

/// 进程信息结构
#[derive(Debug, Default)]
struct ProcessInfo {
    process_name: Option<String>,
    #[allow(dead_code)]
    user_id: Option<u32>,
}

#[async_trait]
impl InboundPort for PfInboundAdapter {
    #[instrument(skip(self, config))]
    async fn listen(
        &self,
        config: &InboundConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Connection, AppError>> + Send>>, AppError> {
        info!("🚀 Starting pfctl inbound adapter");
        
        // 保存配置
        {
            let mut config_guard = self.config.write().await;
            *config_guard = Some(config.clone());
        }
        
        // 启动透明代理
        self.start_transparent_proxy(config).await?;
        
        // 启动本地监听器
        self.start_listener(config).await?;
        
        // 创建连接流
        let (sender, receiver) = mpsc::unbounded_channel();
        
        // 存储发送者
        {
            let mut sender_guard = self.connection_sender.write().await;
            *sender_guard = Some(sender.clone());
        }
        
        // 更新状态
        self.update_status(true, false).await;
        
        // 启动连接接收循环
        let self_clone = Arc::new(self.clone());
        tokio::spawn(async move {
            self_clone.connection_accept_loop(sender).await;
        });
        
        // 返回接收器流
        let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(receiver);
        Ok(Box::pin(stream))
    }
    
    #[instrument(skip(self))]
    async fn stop(&self) -> Result<(), AppError> {
        info!("🛑 Stopping pfctl inbound adapter");
        
        // 更新状态为非活跃
        self.update_status(false, false).await;
        
        // 停止透明代理
        {
            let mut proxy_manager = self.proxy_manager.write().await;
            if let Err(e) = proxy_manager.stop().await {
                warn!("Failed to stop transparent proxy: {}", e);
            }
        }
        
        // 关闭监听器
        {
            let mut listener_guard = self.listener.lock().await;
            *listener_guard = None;
        }
        
        // 关闭连接发送通道
        {
            let mut sender_guard = self.connection_sender.write().await;
            *sender_guard = None;
        }
        
        info!("✅ pfctl inbound adapter stopped");
        Ok(())
    }
    
    async fn status(&self) -> AdapterStatus {
        self.status.read().await.clone()
    }
    
    fn name(&self) -> &str {
        "pfctl"
    }
}

// 实现Clone以支持在异步任务中使用
impl Clone for PfInboundAdapter {
    fn clone(&self) -> Self {
        Self {
            proxy_manager: Arc::clone(&self.proxy_manager),
            listener: Arc::clone(&self.listener),
            config: Arc::clone(&self.config),
            status: Arc::clone(&self.status),
            connection_count: Arc::clone(&self.connection_count),
            connection_sender: Arc::clone(&self.connection_sender),
        }
    }
}

impl Default for PfInboundAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_adapter_creation() {
        let adapter = PfInboundAdapter::new();
        assert_eq!(adapter.name(), "pfctl");
        
        let status = adapter.status().await;
        assert!(!status.active);
        assert_eq!(status.connections, 0);
    }
    
    #[tokio::test]
    async fn test_adapter_with_custom_anchor() {
        let adapter = PfInboundAdapter::with_anchor_name("test_anchor");
        assert_eq!(adapter.name(), "pfctl");
        
        let status = adapter.status().await;
        assert_eq!(status.name, "pfctl(test_anchor)");
    }
    
    #[test]
    fn test_process_info_default() {
        let process_info = ProcessInfo::default();
        assert!(process_info.process_name.is_none());
        assert!(process_info.user_id.is_none());
    }
}

