//! 直连出站适配器
//! 
//! 提供直接连接到目标服务器的功能，不经过任何代理
//! 使用统一的TLS Service处理TLS连接

use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{warn, debug};
use async_trait::async_trait;
use tokio_rustls::client::TlsStream as ClientTlsStream;

use crate::core::ports::{OutboundPort, NetworkStream, AppError};
use crate::core::model::ConnectionContext;
use crate::core::services::TlsService;

/// 直连出站适配器
/// 
/// 负责：
/// 1. 直接连接到目标服务器
/// 2. 管理连接超时
/// 3. 提供连接统计
/// 4. 支持连接重试机制
/// 5. 使用TLS Service处理TLS连接
pub struct DirectOutboundAdapter {
    /// 连接超时
    connect_timeout: Duration,
    /// 读取超时
    read_timeout: Duration,
    /// 最大重试次数
    max_retries: usize,
    /// 重试间隔
    retry_delay: Duration,
    /// 连接统计
    stats: Arc<tokio::sync::RwLock<DirectStats>>,
}

/// 直连统计信息
#[derive(Debug, Default, Clone)]
pub struct DirectStats {
    /// 总连接尝试数
    pub total_attempts: u64,
    /// 成功连接数
    pub successful_connections: u64,
    /// 失败连接数
    pub failed_connections: u64,
    /// 超时连接数
    pub timeout_connections: u64,
    /// 总重试次数
    pub total_retries: u64,
    /// 平均连接时间
    pub avg_connect_time: Option<Duration>,
    /// TLS连接数
    pub tls_connections: u64,
    /// TCP连接数
    pub tcp_connections: u64,
}

/// 直连网络流实现（TCP）
struct DirectNetworkStream {
    /// 底层TCP流
    stream: TcpStream,
    /// 目标地址信息
    #[allow(dead_code)]
    target_host: String,
    #[allow(dead_code)]
    target_port: u16,
    /// 本地地址
    #[allow(dead_code)]
    local_addr: std::net::SocketAddr,
    /// 远程地址
    #[allow(dead_code)]
    remote_addr: std::net::SocketAddr,
}

/// 直连TLS网络流实现（TLS）
struct DirectTlsNetworkStream {
    /// 底层TLS流
    stream: ClientTlsStream<TcpStream>,
    /// 目标地址信息
    #[allow(dead_code)]
    target_host: String,
    #[allow(dead_code)]
    target_port: u16,
}

impl DirectTlsNetworkStream {
    fn new(stream: ClientTlsStream<TcpStream>, target_host: String, target_port: u16) -> Self {
        Self {
            stream,
            target_host,
            target_port,
        }
    }
}

impl DirectOutboundAdapter {
    /// 创建新的直连出站适配器
    pub fn new() -> Self {
        Self {
            connect_timeout: Duration::from_secs(30),
            read_timeout: Duration::from_secs(60),
            max_retries: 0,
            retry_delay: Duration::from_millis(500),
            stats: Arc::new(tokio::sync::RwLock::new(DirectStats::default())),
        }
    }
    
    /// 设置连接超时
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }
    
    /// 设置读取超时
    pub fn with_read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = timeout;
        self
    }
    
    /// 设置最大重试次数
    pub fn with_max_retries(mut self, retries: usize) -> Self {
        self.max_retries = retries;
        self
    }
    
    /// 设置重试延迟
    pub fn with_retry_delay(mut self, delay: Duration) -> Self {
        self.retry_delay = delay;
        self
    }
    
    /// 设置重试配置（最大重试次数和重试延迟）
    pub fn with_retry_config(mut self, max_retries: usize, retry_delay: Duration) -> Self {
        self.max_retries = max_retries;
        self.retry_delay = retry_delay;
        self
    }
    
    /// 建立TCP连接到目标服务器
    async fn connect_tcp(&self, target_host: &str, target_port: u16) -> Result<TcpStream, AppError> {
        let target_addr = format!("{}:{}", target_host, target_port);
        let start_time = std::time::Instant::now();
        
        let mut last_error = None;
        
        for attempt in 0..=self.max_retries {
            if attempt > 0 {
                debug!("Retry attempt {} for {}", attempt, target_addr);
                tokio::time::sleep(self.retry_delay * attempt as u32).await;
                
                // 更新重试统计
                {
                    let mut stats = self.stats.write().await;
                    stats.total_retries += 1;
                }
            }
            
            match timeout(
                self.connect_timeout,
                TcpStream::connect(&target_addr)
            ).await {
                Ok(Ok(stream)) => {
                    let connect_time = start_time.elapsed();
                    debug!("✅ TCP connection established to {} in {:?}", target_addr, connect_time);
                    
                    // 更新统计
                    self.update_success_stats(connect_time).await;
                    
                    return Ok(stream);
                }
                Ok(Err(e)) => {
                    last_error = Some(AppError::Config(format!("Connection failed: {}", e)));
                    warn!("❌ TCP connection attempt {} failed to {}: {}", attempt + 1, target_addr, e);
                }
                Err(_) => {
                    last_error = Some(AppError::Config(format!("Connection timeout to {}", target_addr)));
                    warn!("❌ TCP connection attempt {} timed out to {}", attempt + 1, target_addr);
                    
                    // 更新超时统计
                    {
                        let mut stats = self.stats.write().await;
                        stats.timeout_connections += 1;
                    }
                }
            }
        }
        
        // 所有重试都失败了
        self.update_failure_stats().await;
        Err(last_error.unwrap_or_else(|| AppError::Config(format!("Failed to connect to {}", target_addr))))
    }
    
    /// 使用TLS Service建立TLS连接
    async fn connect_tls_with_service(
        &self,
        tcp_stream: TcpStream,
        target_host: &str,
        tls_service: &TlsService,
    ) -> Result<ClientTlsStream<TcpStream>, AppError> {
        debug!("🔐 使用TLS Service建立服务器TLS连接: {}", target_host);
        
        let tls_stream = tls_service
            .connect_server_tls(tcp_stream, target_host)
            .await?;
        
        debug!("✅ TLS连接建立成功: {}", target_host);
        Ok(tls_stream)
    }
    
    /// 更新成功统计
    async fn update_success_stats(&self, connect_time: Duration) {
        let mut stats = self.stats.write().await;
        stats.total_attempts += 1;
        stats.successful_connections += 1;
        
        // 更新平均连接时间
        stats.avg_connect_time = match stats.avg_connect_time {
            Some(avg) => Some((avg + connect_time) / 2),
            None => Some(connect_time),
        };
    }
    
    /// 更新失败统计
    async fn update_failure_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.total_attempts += 1;
        stats.failed_connections += 1;
    }
    
    /// 更新TLS连接统计
    async fn update_tls_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.tls_connections += 1;
    }
    
    /// 更新TCP连接统计
    async fn update_tcp_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.tcp_connections += 1;
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> DirectStats {
        self.stats.read().await.clone()
    }
    
    /// 检查是否需要TLS连接
    fn needs_tls_connection(&self, context: &ConnectionContext) -> bool {
        // 🔧 Copy Pipe模式：强制使用纯TCP连接
        if let Some(force_tcp) = context.custom_data.get("force_tcp_only") {
            if force_tcp.as_bool() == Some(true) {
                debug!("🔗 检测到force_tcp_only标记，跳过TLS连接");
                return false;
            }
        }
        
        // 检查自定义数据中的协议信息
        if let Some(protocol) = context.custom_data.get("original_target_protocol") {
            if let Some(protocol_str) = protocol.as_str() {
                return protocol_str == "https";
            }
        }
        
        // 根据端口判断
        matches!(context.target_port, 443 | 8443 | 9443)
    }
    
    /// 从全局AppCore获取TLS Service (类似Tauri的全局状态访问)
    async fn get_tls_service() -> Option<Arc<TlsService>> {
        // 通过全局单例访问AppCore，无需传递引用
        let app_core = crate::core::AppCore::global();
        app_core.state::<TlsService>().await
    }
}

#[async_trait]
impl OutboundPort for DirectOutboundAdapter {
    // #[instrument(skip(self, context))]
    async fn connect(&self, context: &ConnectionContext) -> Result<Box<dyn NetworkStream>, AppError> {
        debug!("DirectOutboundAdapter connecting to {}:{}", context.target_host, context.target_port);
        
        // 建立TCP连接
        let tcp_stream = self.connect_tcp(&context.target_host, context.target_port).await?;
        
        // 检查是否需要TLS
        if self.needs_tls_connection(context) {
            debug!("🔐 需要建立TLS连接到: {}", context.target_host);
            
            // 通过全局AppCore获取TLS Service (类似Tauri全局状态访问)
            if let Some(tls_service) = Self::get_tls_service().await {
                let tls_stream = self.connect_tls_with_service(tcp_stream, &context.target_host, &tls_service).await?;
                self.update_tls_stats().await;
                
                Ok(Box::new(DirectTlsNetworkStream::new(
                    tls_stream,
                    context.target_host.clone(),
                    context.target_port,
                )))
            } else {
                warn!("❌ TLS Service未注册到AppCore，请确保中间件服务已初始化");
                Err(AppError::Config(
                    "TLS Service not available. Please ensure middleware services are registered.".to_string()
                ))
            }
        } else {
            // 返回TCP连接
            debug!("✅ 使用TCP连接到: {}", context.target_host);
            self.update_tcp_stats().await;
            
            let local_addr = tcp_stream.local_addr()
                .map_err(|e| AppError::Config(format!("Failed to get local address: {}", e)))?;
            let remote_addr = tcp_stream.peer_addr()
                .map_err(|e| AppError::Config(format!("Failed to get remote address: {}", e)))?;
            
            Ok(Box::new(DirectNetworkStream {
                stream: tcp_stream,
                target_host: context.target_host.clone(),
                target_port: context.target_port,
                local_addr,
                remote_addr,
            }))
        }
    }
    
    async fn supports_target(&self, context: &ConnectionContext) -> bool {
        // 直连适配器理论上可以连接到任何可达的目标
        if context.target_host.is_empty() || context.target_port == 0 {
            return false;
        }
        
        // 检查是否是本地地址或保留地址
        if context.target_host == "127.0.0.1" || context.target_host == "localhost" {
            return true;
        }
        
        // 其他情况都支持
        true
    }
    
    fn name(&self) -> &str {
        "direct"
    }
}

// NetworkStream trait implementations
impl NetworkStream for DirectNetworkStream {}
impl NetworkStream for DirectTlsNetworkStream {}

// AsyncRead/AsyncWrite implementations for DirectNetworkStream (TCP)
#[async_trait]
impl tokio::io::AsyncRead for DirectNetworkStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

#[async_trait]
impl tokio::io::AsyncWrite for DirectNetworkStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        std::pin::Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        std::pin::Pin::new(&mut self.stream).poll_flush(cx)
    }
    
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

// AsyncRead/AsyncWrite implementations for DirectTlsNetworkStream (TLS)
#[async_trait]
impl tokio::io::AsyncRead for DirectTlsNetworkStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

#[async_trait]
impl tokio::io::AsyncWrite for DirectTlsNetworkStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        std::pin::Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        std::pin::Pin::new(&mut self.stream).poll_flush(cx)
    }
    
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

impl Default for DirectOutboundAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::Connection;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    
    #[tokio::test]
    async fn test_adapter_creation() {
        let adapter = DirectOutboundAdapter::new();
        assert_eq!(adapter.name(), "direct");
        assert_eq!(adapter.connect_timeout, Duration::from_secs(30));
        assert_eq!(adapter.max_retries, 3);
    }
    
    #[tokio::test]
    async fn test_configuration() {
        let adapter = DirectOutboundAdapter::new()
            .with_connect_timeout(Duration::from_secs(10))
            .with_max_retries(5)
            .with_retry_delay(Duration::from_millis(200));
        
        assert_eq!(adapter.connect_timeout, Duration::from_secs(10));
        assert_eq!(adapter.max_retries, 5);
        assert_eq!(adapter.retry_delay, Duration::from_millis(200));
    }
    
    #[tokio::test]
    async fn test_supports_target() {
        let adapter = DirectOutboundAdapter::new();
        
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        let context = ConnectionContext::new(connection, "google.com".to_string(), 80);
        
        assert!(adapter.supports_target(&context).await);
    }
    
    #[tokio::test]
    async fn test_tls_detection() {
        let adapter = DirectOutboundAdapter::new();
        
        // HTTPS端口
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443)
        );
        let context = ConnectionContext::new(connection, "google.com".to_string(), 443);
        
        assert!(adapter.needs_tls_connection(&context));
        
        // HTTP端口
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        let context = ConnectionContext::new(connection, "google.com".to_string(), 80);
        
        assert!(!adapter.needs_tls_connection(&context));
    }
}