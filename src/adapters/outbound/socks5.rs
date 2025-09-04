//! SOCKS5出站适配器
//! 
//! 将现有的socks5.rs功能重构为符合六边形架构的出站端口实现

use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{info, warn, error, debug, instrument};
use async_trait::async_trait;

use crate::core::ports::{OutboundPort, NetworkStream, AppError};
use crate::core::model::ConnectionContext;
use crate::core::ports::ProxyConfig;
use crate::socks5::{Socks5Client, DataForwarder};

/// SOCKS5出站适配器
/// 
/// 负责：
/// 1. 通过SOCKS5代理建立连接
/// 2. 处理SOCKS5认证
/// 3. 管理连接池
/// 4. 提供统一的NetworkStream接口
pub struct Socks5OutboundAdapter {
    /// 默认代理配置
    default_proxy: Option<ProxyConfig>,
    /// 连接超时
    connect_timeout: Duration,
    /// 连接统计
    stats: Arc<tokio::sync::RwLock<Socks5Stats>>,
}

/// SOCKS5统计信息
#[derive(Debug, Default)]
pub struct Socks5Stats {
    /// 总连接数
    pub total_connections: u64,
    /// 成功连接数
    pub successful_connections: u64,
    /// 失败连接数
    pub failed_connections: u64,
    /// 认证失败次数
    pub auth_failures: u64,
    /// 平均连接时间
    pub avg_connect_time: Option<Duration>,
}

/// SOCKS5网络流实现
struct Socks5NetworkStream {
    /// 底层TCP流
    stream: TcpStream,
    /// 目标地址信息
    target_host: String,
    target_port: u16,
    /// 代理信息
    proxy_addr: String,
}

impl Socks5OutboundAdapter {
    /// 创建新的SOCKS5出站适配器
    pub fn new() -> Self {
        Self {
            default_proxy: None,
            connect_timeout: Duration::from_secs(30),
            stats: Arc::new(tokio::sync::RwLock::new(Socks5Stats::default())),
        }
    }
    
    /// 使用默认代理配置创建适配器
    pub fn with_proxy(proxy: ProxyConfig) -> Self {
        Self {
            default_proxy: Some(proxy),
            connect_timeout: Duration::from_secs(30),
            stats: Arc::new(tokio::sync::RwLock::new(Socks5Stats::default())),
        }
    }
    
    /// 设置连接超时
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }
    
    /// 建立SOCKS5连接
    #[instrument(skip(self, proxy_config))]
    async fn connect_via_socks5(
        &self,
        proxy_config: &ProxyConfig,
        target_host: &str,
        target_port: u16,
    ) -> Result<TcpStream, AppError> {
        let start_time = std::time::Instant::now();
        
        // 解析代理地址
        let (proxy_host, proxy_port) = proxy_config.parse_host_port()
            .map_err(|e| AppError::Config(format!("Invalid proxy config: {}", e)))?;
        
        debug!("Connecting to SOCKS5 proxy {}:{}", proxy_host, proxy_port);
        
        // 连接到SOCKS5代理
        let proxy_addr = format!("{}:{}", proxy_host, proxy_port);
        let mut stream = timeout(
            self.connect_timeout,
            TcpStream::connect(&proxy_addr)
        ).await
        .map_err(|_| AppError::Config(format!("Connection to proxy {} timed out", proxy_addr)))?
        .map_err(|e| AppError::Config(format!("Failed to connect to proxy {}: {}", proxy_addr, e)))?;
        
        debug!("Connected to SOCKS5 proxy, performing handshake");
        
        // 执行SOCKS5握手
        Socks5Client::connect(&mut stream, proxy_config, target_host, target_port)
            .await
            .map_err(|e| {
                // 更新认证失败统计
                tokio::spawn({
                    let stats = Arc::clone(&self.stats);
                    async move {
                        let mut stats = stats.write().await;
                        stats.auth_failures += 1;
                    }
                });
                AppError::Config(format!("SOCKS5 handshake failed: {}", e))
            })?;
        
        let connect_time = start_time.elapsed();
        debug!("SOCKS5 connection established in {:?}", connect_time);
        
        // 更新统计信息
        {
            let mut stats = self.stats.write().await;
            stats.total_connections += 1;
            stats.successful_connections += 1;
            
            // 更新平均连接时间
            stats.avg_connect_time = match stats.avg_connect_time {
                Some(avg) => Some((avg + connect_time) / 2),
                None => Some(connect_time),
            };
        }
        
        Ok(stream)
    }
    
    /// 获取要使用的代理配置
    fn get_proxy_config(&self, context: &ConnectionContext) -> Option<&ProxyConfig> {
        // 优先使用连接上下文中的代理配置
        // 如果没有，使用默认配置
        self.default_proxy.as_ref()
    }
    
    /// 更新尝试统计
    async fn update_attempt_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.total_connections += 1;
    }
    
    /// 更新成功统计
    async fn update_success_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.successful_connections += 1;
    }
    
    /// 更新失败统计
    async fn update_failure_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.failed_connections += 1;
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> Socks5Stats {
        self.stats.read().await.clone()
    }
}

impl Clone for Socks5Stats {
    fn clone(&self) -> Self {
        Self {
            total_connections: self.total_connections,
            successful_connections: self.successful_connections,
            failed_connections: self.failed_connections,
            auth_failures: self.auth_failures,
            avg_connect_time: self.avg_connect_time,
        }
    }
}

#[async_trait]
impl OutboundPort for Socks5OutboundAdapter {
    #[instrument(skip(self, context))]
    async fn connect(&self, context: &ConnectionContext) -> Result<Box<dyn NetworkStream>, AppError> {
        debug!("Connecting to {}:{} via SOCKS5", context.target_host, context.target_port);
        
        // 更新尝试统计
        self.update_attempt_stats().await;
        
        let proxy_config = self.get_proxy_config(context)
            .ok_or_else(|| {
                // 如果没有代理配置，更新失败统计
                let stats = self.stats.clone();
                tokio::spawn(async move {
                    let mut stats = stats.write().await;
                    stats.failed_connections += 1;
                });
                AppError::Config("No SOCKS5 proxy configuration available".to_string())
            })?;
        
        match self.connect_via_socks5(proxy_config, &context.target_host, context.target_port).await {
            Ok(stream) => {
                // 更新成功统计
                self.update_success_stats().await;
                
                let network_stream = Socks5NetworkStream {
                    stream,
                    target_host: context.target_host.clone(),
                    target_port: context.target_port,
                    proxy_addr: format!("{}:{}", 
                        proxy_config.host().unwrap_or_default(),
                        proxy_config.port().unwrap_or_default()
                    ),
                };
                
                Ok(Box::new(network_stream))
            }
            Err(e) => {
                // 更新失败统计
                self.update_failure_stats().await;
                error!("Failed to connect via SOCKS5: {}", e);
                Err(e)
            }
        }
    }
    
    async fn supports_target(&self, context: &ConnectionContext) -> bool {
        // SOCKS5适配器需要有代理配置才能工作
        if self.default_proxy.is_none() {
            return false;
        }
        
        // 检查基本的目标有效性
        if context.target_host.is_empty() || context.target_port == 0 {
            return false;
        }
        
        // SOCKS5可以连接到任何有效的目标（通过代理）
        true
    }
    
    fn name(&self) -> &str {
        "socks5"
    }
}

// 实现NetworkStream trait for Socks5NetworkStream
impl NetworkStream for Socks5NetworkStream {}

#[async_trait]
impl tokio::io::AsyncRead for Socks5NetworkStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

#[async_trait]
impl tokio::io::AsyncWrite for Socks5NetworkStream {
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

impl Default for Socks5OutboundAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Connection, ConnectionMetadata};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    
    #[tokio::test]
    async fn test_adapter_creation() {
        let adapter = Socks5OutboundAdapter::new();
        assert_eq!(adapter.name(), "socks5");
        assert!(!adapter.supports_target(&create_test_context()).await);
    }
    
    #[tokio::test]
    async fn test_adapter_with_proxy() {
        let proxy = ProxyConfig {
            url: "socks5://127.0.0.1:1080".to_string(),
            proxy_type: crate::core::ports::ProxyType::Socks5,
            auth: None,
        };
        
        let adapter = Socks5OutboundAdapter::with_proxy(proxy);
        assert!(adapter.supports_target(&create_test_context()).await);
    }
    
    #[tokio::test]
    async fn test_timeout_configuration() {
        let adapter = Socks5OutboundAdapter::new()
            .with_timeout(Duration::from_secs(10));
        
        assert_eq!(adapter.connect_timeout, Duration::from_secs(10));
    }
    
    #[tokio::test]
    async fn test_stats_initialization() {
        let adapter = Socks5OutboundAdapter::new();
        let stats = adapter.get_stats().await;
        
        assert_eq!(stats.total_connections, 0);
        assert_eq!(stats.successful_connections, 0);
        assert_eq!(stats.failed_connections, 0);
        assert_eq!(stats.auth_failures, 0);
        assert!(stats.avg_connect_time.is_none());
    }
    
    fn create_test_context() -> ConnectionContext {
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        
        ConnectionContext::new(connection, "example.com".to_string(), 80)
    }
}
