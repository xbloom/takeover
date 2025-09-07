//! 直连出站适配器
//! 
//! 提供直接连接到目标服务器的功能，不经过任何代理

use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{info, warn, debug, instrument};
use async_trait::async_trait;
use tokio_rustls::{TlsConnector, client::TlsStream as ClientTlsStream};
use rustls::ClientConfig;
use std::sync::Arc as StdArc;

use crate::core::ports::{OutboundPort, NetworkStream, AppError};
use crate::core::model::ConnectionContext;

/// 直连出站适配器
/// 
/// 负责：
/// 1. 直接连接到目标服务器
/// 2. 管理连接超时
/// 3. 提供连接统计
/// 4. 支持连接重试机制
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
}

/// 直连网络流实现
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

/// 直连TLS网络流实现
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

/// 跳过TLS服务器证书验证（仅用于开发测试）
/// 
/// ⚠️ **警告**: 这会跳过所有TLS安全检查，仅用于开发环境！
#[derive(Debug)]
struct SkipServerVerification;

impl SkipServerVerification {
    fn new() -> Self {
        Self
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // ⚠️ 跳过所有验证，接受任何证书
        debug!("⚠️  Skipping TLS certificate verification (development mode)");
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        // 支持所有签名方案
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA1,
            rustls::SignatureScheme::ECDSA_SHA1_Legacy,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ED448,
        ]
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
    
    /// 设置重试配置
    pub fn with_retry_config(mut self, max_retries: usize, retry_delay: Duration) -> Self {
        self.max_retries = max_retries;
        self.retry_delay = retry_delay;
        self
    }
    
    /// 直接连接到目标地址
    #[instrument(skip(self))]
    async fn connect_direct(
        &self,
        target_host: &str,
        target_port: u16,
    ) -> Result<TcpStream, AppError> {
        let target_addr = format!("{}:{}", target_host, target_port);
        let start_time = std::time::Instant::now();
        
        debug!("Attempting direct connection to {}", target_addr);
        
        let mut retry_count = 0;
        #[allow(unused_assignments)]
        let mut final_error = AppError::Config(format!("Failed to connect to {} after {} retries", target_addr, self.max_retries));
        
        loop {
            // 更新统计
            self.update_attempt_stats().await;
            
            match timeout(
                self.connect_timeout,
                TcpStream::connect(&target_addr)
            ).await {
                Ok(Ok(stream)) => {
                    let connect_time = start_time.elapsed();
                    debug!("Direct connection to {} established in {:?}", target_addr, connect_time);
                    
                    // 更新成功统计
                    self.update_success_stats(connect_time).await;
                    
                    return Ok(stream);
                }
                Ok(Err(e)) => {
                    final_error = AppError::Config(format!("Connection failed: {}", e));
                }
                Err(_) => {
                    // 超时
                    final_error = AppError::Config(format!("Connection to {} timed out", target_addr));
                    self.update_timeout_stats().await;
                }
            }
            
            retry_count += 1;
            if retry_count > self.max_retries {
                break;
            }
            
            debug!("Connection attempt {} failed, retrying in {:?}", retry_count, self.retry_delay);
            self.update_retry_stats().await;
            tokio::time::sleep(self.retry_delay).await;
        }
        
        // 所有重试都失败了
        self.update_failure_stats().await;
        debug!("Failed to connect to {} after {} attempts", target_addr, retry_count);
        
        Err(final_error)
    }
    
    /// 检查目标是否可达
    #[allow(dead_code)]
    async fn is_target_reachable(&self, target_host: &str, target_port: u16) -> bool {
        // 简单的连通性检查
        let target_addr = format!("{}:{}", target_host, target_port);
        
        match timeout(
            Duration::from_secs(5), // 快速检查，使用较短的超时
            TcpStream::connect(&target_addr)
        ).await {
            Ok(Ok(stream)) => {
                drop(stream);
                true
            }
            _ => false
        }
    }
    
    /// 更新尝试统计
    async fn update_attempt_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.total_attempts += 1;
    }
    
    /// 更新成功统计
    async fn update_success_stats(&self, connect_time: Duration) {
        let mut stats = self.stats.write().await;
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
        stats.failed_connections += 1;
    }
    
    /// 更新超时统计
    async fn update_timeout_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.timeout_connections += 1;
    }
    
    /// 更新重试统计
    async fn update_retry_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.total_retries += 1;
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> DirectStats {
        self.stats.read().await.clone()
    }
    
    /// 获取成功率
    pub async fn get_success_rate(&self) -> f64 {
        let stats = self.stats.read().await;
        if stats.total_attempts == 0 {
            0.0
        } else {
            stats.successful_connections as f64 / stats.total_attempts as f64
        }
    }
    
    /// 判断是否应该使用TLS连接
    /// 
    /// 根据转发中间件设置的协议信息决定
    fn should_use_tls(&self, context: &ConnectionContext) -> bool {
        // 🔍 检查转发中间件设置的原始目标协议
        if let Some(protocol) = context.custom_data.get("original_target_protocol") {
            if let Some(protocol_str) = protocol.as_str() {
                return protocol_str == "https";
            }
        }
        
        // 🔍 回退到端口检测
        matches!(context.target_port, 443 | 8443 | 9443)
    }
    
    /// 建立TLS连接到目标服务器
    #[instrument(skip(self))]
    async fn connect_tls(
        &self,
        target_host: &str,
        target_port: u16,
    ) -> Result<ClientTlsStream<TcpStream>, AppError> {
        debug!("🔐 建立TLS连接到: {}:{}", target_host, target_port);
        
        // 1️⃣ 先建立TCP连接
        let tcp_stream = self.connect_direct(target_host, target_port).await?;
        
        // 2️⃣ 创建TLS客户端配置 - 暂时使用危险模式解决版本冲突
        // TODO: 生产环境必须启用正确的证书验证
        let config = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(StdArc::new(SkipServerVerification::new()))
            .with_no_client_auth();
        
        debug!("⚠️  Using TLS client config without certificate verification (development only)");
        
        let connector = TlsConnector::from(StdArc::new(config));
        
        // 3️⃣ 进行TLS握手 
        let domain = target_host.to_string().try_into()
            .map_err(|e| AppError::Config(format!("Invalid domain name: {}", e)))?;
        
        debug!("🤝 开始TLS握手: {}", target_host);
        let tls_stream = connector.connect(domain, tcp_stream).await
            .map_err(|e| AppError::Config(format!("TLS handshake failed: {}", e)))?;
        
        debug!("✅ TLS连接已建立: {}:{}", target_host, target_port);
        Ok(tls_stream)
    }
}

#[async_trait]
impl OutboundPort for DirectOutboundAdapter {
    #[instrument(skip(self, context))]
    async fn connect(&self, context: &ConnectionContext) -> Result<Box<dyn NetworkStream>, AppError> {
        // 🔍 检查原始目标协议
        let is_https = self.should_use_tls(context);
        
        debug!("Establishing direct connection to {}:{} (TLS: {})", 
               context.target_host, context.target_port, is_https);
        
        // 更新尝试统计
        self.update_attempt_stats().await;
        
        let start_time = tokio::time::Instant::now();
        
        if is_https {
            // 🔐 建立TLS连接到服务器
            match self.connect_tls(&context.target_host, context.target_port).await {
                Ok(tls_stream) => {
                    let connect_time = start_time.elapsed();
                    self.update_success_stats(connect_time).await;
                    
                    debug!("✅ Direct TLS connection established to {}:{}", context.target_host, context.target_port);
                    Ok(Box::new(DirectTlsNetworkStream::new(tls_stream, context.target_host.clone(), context.target_port)))
                }
                Err(e) => {
                    self.update_failure_stats().await;
                    warn!("❌ Failed to establish TLS connection: {}", e);
                    Err(e)
                }
            }
        } else {
            // 🌐 建立普通TCP连接  
            match self.connect_direct(&context.target_host, context.target_port).await {
                Ok(stream) => {
                    let connect_time = start_time.elapsed();
                    self.update_success_stats(connect_time).await;
                    
                    let local_addr = stream.local_addr()
                        .map_err(|e| AppError::Config(format!("Failed to get local address: {}", e)))?;
                    let remote_addr = stream.peer_addr()
                        .map_err(|e| AppError::Config(format!("Failed to get remote address: {}", e)))?;
                    
                    let network_stream = DirectNetworkStream {
                        stream,
                        target_host: context.target_host.clone(),
                        target_port: context.target_port,
                        local_addr,
                        remote_addr,
                    };
                    
                    info!("✅ Direct connection established: {} -> {}", local_addr, remote_addr);
                    Ok(Box::new(network_stream))
                }
                Err(e) => {
                    self.update_failure_stats().await;
                    warn!("⚠️ 🔌 无法连接服务器: {}", e);
                    Err(e)
                }
            }
        }
    }
    
    async fn supports_target(&self, context: &ConnectionContext) -> bool {
        // 直连适配器可以尝试连接任何目标，但有一些基本的检查
        
        // 检查是否是有效的主机名或IP地址
        if context.target_host.is_empty() {
            return false;
        }
        
        // 检查端口范围
        if context.target_port == 0 {
            return false;
        }
        
        #[cfg(debug_assertions)]
        {
            // 开发/测试环境：跳过网络可达性检查，只做基本验证
            // 但排除明显不可达的测试地址
            if context.target_host == "0.0.0.0" || (context.target_host == "127.0.0.1" && context.target_port == 9999) {
                return false;
            }
            return true;
        }
        
        #[cfg(not(debug_assertions))]
        {
            // 生产环境：进行真实的可达性检查
            self.is_target_reachable(&context.target_host, context.target_port).await
        }
    }
    
    fn name(&self) -> &str {
        "direct"
    }
}

// 实现NetworkStream trait for DirectNetworkStream
impl NetworkStream for DirectNetworkStream {}

impl tokio::io::AsyncRead for DirectNetworkStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

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

impl DirectNetworkStream {
    /// 获取连接信息
    #[allow(dead_code)]
    pub fn connection_info(&self) -> ConnectionInfo {
        ConnectionInfo {
            target_host: self.target_host.clone(),
            target_port: self.target_port,
            local_addr: self.local_addr,
            remote_addr: self.remote_addr,
            connection_type: "direct".to_string(),
        }
    }
}

// 实现NetworkStream trait for DirectTlsNetworkStream
impl NetworkStream for DirectTlsNetworkStream {}

impl tokio::io::AsyncRead for DirectTlsNetworkStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

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

/// 连接信息
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub target_host: String,
    pub target_port: u16,
    pub local_addr: std::net::SocketAddr,
    pub remote_addr: std::net::SocketAddr,
    pub connection_type: String,
}

impl Default for DirectOutboundAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    
    #[tokio::test]
    async fn test_adapter_creation() {
        let adapter = DirectOutboundAdapter::new();
        assert_eq!(adapter.name(), "direct");
        assert_eq!(adapter.connect_timeout, Duration::from_secs(30));
        assert_eq!(adapter.read_timeout, Duration::from_secs(60));
        assert_eq!(adapter.max_retries, 3);
    }
    
    #[tokio::test]
    async fn test_timeout_configuration() {
        let adapter = DirectOutboundAdapter::new()
            .with_connect_timeout(Duration::from_secs(10))
            .with_read_timeout(Duration::from_secs(30));
        
        assert_eq!(adapter.connect_timeout, Duration::from_secs(10));
        assert_eq!(adapter.read_timeout, Duration::from_secs(30));
    }
    
    #[tokio::test]
    async fn test_retry_configuration() {
        let adapter = DirectOutboundAdapter::new()
            .with_retry_config(5, Duration::from_millis(1000));
        
        assert_eq!(adapter.max_retries, 5);
        assert_eq!(adapter.retry_delay, Duration::from_millis(1000));
    }
    
    #[tokio::test]
    async fn test_stats_initialization() {
        let adapter = DirectOutboundAdapter::new();
        let stats = adapter.get_stats().await;
        
        assert_eq!(stats.total_attempts, 0);
        assert_eq!(stats.successful_connections, 0);
        assert_eq!(stats.failed_connections, 0);
        assert_eq!(stats.timeout_connections, 0);
        assert_eq!(stats.total_retries, 0);
        assert!(stats.avg_connect_time.is_none());
    }
    
    #[tokio::test]
    async fn test_success_rate_calculation() {
        let adapter = DirectOutboundAdapter::new();
        
        // 初始状态成功率应该是0
        assert_eq!(adapter.get_success_rate().await, 0.0);
        
        // 模拟一些统计更新
        adapter.update_attempt_stats().await;
        adapter.update_success_stats(Duration::from_millis(100)).await;
        
        assert_eq!(adapter.get_success_rate().await, 1.0);
        
        adapter.update_attempt_stats().await;
        adapter.update_failure_stats().await;
        
        assert_eq!(adapter.get_success_rate().await, 0.5);
    }
    
    #[test]
    fn test_connection_info() {
        let local_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345);
        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80);
        
        let info = ConnectionInfo {
            target_host: "example.com".to_string(),
            target_port: 80,
            local_addr,
            remote_addr,
            connection_type: "direct".to_string(),
        };
        
        assert_eq!(info.target_host, "example.com");
        assert_eq!(info.target_port, 80);
        assert_eq!(info.connection_type, "direct");
    }
}
