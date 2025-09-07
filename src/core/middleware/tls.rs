//! TLS中间件
//! 
//! 将现有的TLS拦截功能重构为中间件架构，负责：
//! - TLS连接检测和拦截
//! - SSL Bump实现
//! - 证书管理和动态生成
//! - TLS流的透明代理

use std::sync::Arc;
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::TcpStream;
use tracing::{warn, error, debug, instrument};
use async_trait::async_trait;

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{ConnectionContext, ProcessingState, TlsConfig};
use crate::core::ports::AppError;
use crate::tls_interception::{
    TlsInterceptionManager, RuntimeTlsConfig, TlsCertificate, TlsInterceptionConfig
};
use tokio_rustls::{TlsAcceptor, server::TlsStream as ServerTlsStream};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls_pemfile;
use crate::routing_engine::TlsInterceptionMode;

/// TLS中间件
/// 
/// 负责处理TLS连接的拦截和SSL Bump功能
pub struct TlsMiddleware {
    /// TLS拦截管理器
    tls_manager: Option<Arc<TlsInterceptionManager>>,
    /// 拦截配置
    config: TlsMiddlewareConfig,
    /// 证书缓存
    cert_cache: Arc<tokio::sync::RwLock<HashMap<String, CachedCertificate>>>,
    /// 统计信息
    stats: Arc<tokio::sync::RwLock<TlsStats>>,
}

/// TLS中间件配置
#[derive(Debug, Clone)]
pub struct TlsMiddlewareConfig {
    /// 是否启用TLS拦截
    pub enabled: bool,
    /// 拦截模式
    pub interception_mode: TlsInterceptionMode,
    /// 需要拦截的域名模式
    pub intercept_domains: Vec<String>,
    /// 证书存储路径
    pub cert_store_path: Option<String>,
    /// 证书缓存时间
    pub cert_cache_duration: Duration,
    /// 最大并发TLS握手数
    pub max_concurrent_handshakes: usize,
}

/// 缓存的证书信息
#[derive(Debug, Clone)]
struct CachedCertificate {
    certificate: TlsCertificate,
    cached_at: std::time::SystemTime,
    #[allow(dead_code)]
    hit_count: usize,
}

/// TLS统计信息
#[derive(Debug, Default, Clone)]
pub struct TlsStats {
    /// 总TLS连接数
    pub total_tls_connections: u64,
    /// 拦截的连接数
    pub intercepted_connections: u64,
    /// 跳过的连接数
    pub bypassed_connections: u64,
    /// 生成的证书数
    pub certificates_generated: u64,
    /// 缓存命中数
    pub cache_hits: u64,
    /// 握手失败数
    pub handshake_failures: u64,
}

impl TlsMiddleware {
    /// 创建新的TLS中间件
    pub fn new() -> Self {
        let mut middleware = Self {
            tls_manager: None,
            config: TlsMiddlewareConfig::default(),
            cert_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            stats: Arc::new(tokio::sync::RwLock::new(TlsStats::default())),
        };
        
        // 如果配置启用，自动初始化TLS拦截（使用默认路径）
        if middleware.config.enabled {
            if let Err(e) = middleware.enable_tls_interception(None) {
                warn!("Failed to auto-enable TLS interception: {}, falling back to mock mode", e);
            }
        }
        
        middleware
    }
    
    /// 使用配置创建TLS中间件
    pub fn with_config(config: TlsMiddlewareConfig) -> Self {
        let mut middleware = Self {
            tls_manager: None,
            config,
            cert_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            stats: Arc::new(tokio::sync::RwLock::new(TlsStats::default())),
        };
        
        // 如果配置启用，自动初始化TLS拦截
        if middleware.config.enabled {
            let cert_store_path = middleware.config.cert_store_path.clone();
            if let Err(e) = middleware.enable_tls_interception(cert_store_path.as_deref()) {
                warn!("Failed to auto-enable TLS interception with config: {}, falling back to mock mode", e);
            }
        }
        
        middleware
    }
    
    /// 启用TLS拦截功能
    pub fn enable_tls_interception(&mut self, cert_store_path: Option<&str>) -> Result<(), AppError> {
        if !self.config.enabled {
            return Ok(());
        }
        
        // 🎯 关键修复：从TlsMiddlewareConfig创建TlsInterceptionConfig
        let tls_config = TlsConfig {
            enabled: self.config.enabled,
            interception_mode: match self.config.interception_mode {
                TlsInterceptionMode::None => "none".to_string(),
                TlsInterceptionMode::Full => "full".to_string(),
                TlsInterceptionMode::Selective => "selective".to_string(),
            },
            intercept_domains: self.config.intercept_domains.clone(),
            cert_store_path: self.config.cert_store_path.clone(),
            cert_cache_duration: self.config.cert_cache_duration.as_secs(),
            max_concurrent_handshakes: self.config.max_concurrent_handshakes,
            auto_install_ca: true, // 默认启用自动安装
        };
        let tls_interception_config = TlsInterceptionConfig::from_app_config(&tls_config, cert_store_path);
        
        let tls_manager = match cert_store_path {
            Some(path) => TlsInterceptionManager::new_real_with_config(path, tls_interception_config),
            None => {
                // 使用默认路径创建
                let home_dir = std::env::var("HOME")
                    .map_err(|_| AppError::Config("Cannot find HOME directory".to_string()))?;
                let default_path = format!("{}/.config/takeover", home_dir);
                std::fs::create_dir_all(&default_path)
                    .map_err(|e| AppError::Config(format!("Failed to create config directory: {}", e)))?;
                TlsInterceptionManager::new_real_with_config(&default_path, tls_interception_config)
            }
        };
        
        // 设置运行时配置
        let mut runtime_config = RuntimeTlsConfig::default();
        runtime_config.enabled = true;
        runtime_config.mode = self.config.interception_mode.clone();
        runtime_config.intercept_domains = self.config.intercept_domains.clone();
        
        let mut tls_manager = tls_manager;
        tls_manager.set_runtime_config(runtime_config);
        let tls_manager_arc = Arc::new(tls_manager);
        
        // 🎯 关键修复：在后台立即启动根CA安装
        let tls_manager_clone = Arc::clone(&tls_manager_arc);
        tokio::spawn(async move {
            if let Err(e) = tls_manager_clone.initialize().await {
                error!("❌ 根CA安装失败: {}", e);
                error!("💡 请确保以sudo权限运行程序以安装系统证书");
            }
        });
        
        self.tls_manager = Some(tls_manager_arc);
        
        debug!("✅ TLS拦截功能已启用，根CA安装正在后台进行");
        Ok(())
    }
    
    /// 检查是否应该拦截此连接
    async fn should_intercept_connection(&self, context: &ConnectionContext) -> bool {
        if !self.config.enabled {
            return false;
        }
        
        // 检查拦截模式
        match self.config.interception_mode {
            TlsInterceptionMode::None => false,
            TlsInterceptionMode::Full => true,
            TlsInterceptionMode::Selective => {
                // 检查域名是否匹配拦截列表
                self.config.intercept_domains.iter().any(|pattern| {
                    self.domain_matches_pattern(&context.target_host, pattern)
                })
            }
        }
    }
    
    /// 检查域名是否匹配模式
    fn domain_matches_pattern(&self, domain: &str, pattern: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        
        if pattern.starts_with("*.") {
            let suffix = &pattern[2..];
            domain.ends_with(suffix) || domain == suffix
        } else {
            domain == pattern
        }
    }
    
    /// 检测TLS握手
    /// 
    /// 使用多种方法检测TLS连接：
    /// 1. 端口检测（443, 8443等）
    /// 2. 数据包检测（TLS握手特征）
    /// 3. 透明代理元数据
    async fn detect_tls_handshake(&self, context: &ConnectionContext) -> bool {
        // 方法1：检查常见的HTTPS端口
        if matches!(context.target_port, 443 | 8443 | 9443) {
            debug!("TLS检测：端口{}是HTTPS端口", context.target_port);
            return true;
        }
        
        // 方法2：检查透明代理的元数据
        if let Some(protocol) = context.connection.metadata.custom_data.get("detected_protocol") {
            if protocol.as_str() == Some("https") || protocol.as_str() == Some("tls") {
                debug!("TLS检测：透明代理元数据指示HTTPS");
                return true;
            }
        }
        
        // 方法3：如果有TCP流，使用peek进行TLS握手检测（非侵入式）
        if let Some(ref stream) = context.connection.stream {
            debug!("TLS检测：使用peek方法检测TLS握手");
            match self.peek_tls_handshake(stream).await {
                Ok(true) => {
                    debug!("TLS检测：peek检测到TLS握手特征");
                    return true;
                }
                Ok(false) => {
                    debug!("TLS检测：peek未检测到TLS握手特征");
                }
                Err(e) => {
                    debug!("TLS检测：peek检测失败: {}", e);
                }
            }
        }
        
        // 方法4：基于目标主机的启发式判断
        if context.target_host.contains("https") || 
           context.target_host.starts_with("secure.") ||
           context.target_host.starts_with("ssl.") {
            debug!("TLS检测：主机名暗示HTTPS: {}", context.target_host);
            return true;
        }
        
        debug!("TLS检测：未检测到TLS连接特征");
        false
    }
    
    /// 使用peek方法检测TLS握手（非侵入式）
    /// 
    /// 检测TLS握手包的特征：
    /// - 第一个字节：0x16 (TLS Handshake)
    /// - 第二第三字节：版本号 (0x03, 0x01-0x04)  
    /// - 第五个字节：0x01 (Client Hello)
    async fn peek_tls_handshake(&self, stream: &tokio::net::TcpStream) -> Result<bool, std::io::Error> {
        let mut buf = [0u8; 6];
        
        // 使用peek读取前6个字节，不消耗数据
        let n = stream.peek(&mut buf).await?;
        
        if n < 6 {
            debug!("TLS检测：数据不足6字节 ({}字节)", n);
            return Ok(false);
        }
        
        // 检测TLS握手特征
        let is_tls = buf[0] == 0x16 &&                    // TLS Handshake
                     buf[1] == 0x03 &&                    // TLS Version Major  
                     (buf[2] >= 0x01 && buf[2] <= 0x04) && // TLS Version Minor (1.0-1.3)
                     buf[5] == 0x01;                      // Client Hello
        
        if is_tls {
            debug!("TLS检测：检测到TLS握手 - 版本: 3.{}, 类型: Client Hello", buf[2]);
            debug!("TLS检测：原始数据: {:02x?}", &buf);
        } else {
            debug!("TLS检测：非TLS握手数据: {:02x?}", &buf);
        }
        
        Ok(is_tls)
    }
    
    /// 执行TLS拦截 - 现代化实现
    // #[instrument(skip(self, context), fields(connection_id = %context.id))]
    #[instrument(skip(self, context))]
    async fn perform_tls_interception(&self, context: &mut ConnectionContext) -> Result<(), AppError> {
        let domain = &context.target_host;
        let port = context.target_port;
        
        debug!("🔐 开始TLS拦截: {}:{}", domain, port);
        
        // 获取TCP流
        let tcp_stream = context.connection.stream.take()
            .ok_or_else(|| AppError::Config("No TCP stream available for TLS interception".to_string()))?;
        
        // 第一步：获取或生成证书
        let certificate = self.get_or_generate_certificate(domain).await?;
        
        // 第二步：执行TLS拦截（只处理客户端侧）
        match self.perform_tls_interception_client_only(tcp_stream, domain, &certificate).await {
            Ok((client_tls_stream, alpn_protocol)) => {
                // 只存储客户端TLS流，服务器连接由Outbound适配器处理
                context.connection.set_client_tls_stream(client_tls_stream);
                // server_tls_stream 由outbound适配器负责建立
                
                debug!("TLS拦截成功: {}:{}", domain, port);
                
                // 🌐 存储ALPN协商结果到上下文（重要！）
                if let Some(protocol) = &alpn_protocol {
                    context.custom_data.insert(
                        "alpn_protocol".to_string(),
                        serde_json::json!(protocol)
                    );
                    debug!("ALPN协商结果: {}", protocol);
                }
                
                // 存储拦截信息到上下文
                context.custom_data.insert(
                    "tls_intercepted".to_string(),
                    serde_json::json!({
                        "success": true,
                        "domain": domain,
                        "port": port,
                        "certificate_subject": certificate.subject,
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                        "tls_version": "rustls-tokio",
                        "client_side_only": true,
                        "alpn_protocol": alpn_protocol
                    })
                );
                
                // 更新统计
                {
                    let mut stats = self.stats.write().await;
                    stats.intercepted_connections += 1;
                }
                
                Ok(())
            }
            Err(e) => {
                debug!("❌ TLS拦截失败: {}:{} - {}", domain, port, e);
                
                // 更新失败统计
                {
                    let mut stats = self.stats.write().await;
                    stats.handshake_failures += 1;
                }
                
                Err(AppError::Config(format!("TLS interception failed: {}", e)))
            }
        }
    }
    
    /// TLS拦截实现 - 只处理客户端侧TLS拦截
    /// 
    /// **架构原则**: TLS中间件只负责拦截客户端的TLS连接
    /// 到服务器的连接由Outbound适配器负责
    async fn perform_tls_interception_client_only(
        &self,
        client_stream: TcpStream,
        target_domain: &str,
        certificate: &TlsCertificate,
    ) -> Result<(ServerTlsStream<TcpStream>, Option<String>), AppError> {
        debug!("🔐 执行客户端TLS拦截: {}", target_domain);
        
        // 创建服务器配置（用我们的证书与客户端建立TLS连接）
        let server_config = self.create_server_config_from_cert(certificate)
            .map_err(|e| AppError::Config(format!("Failed to create server config: {}", e)))?;
        let acceptor = TlsAcceptor::from(std::sync::Arc::new(server_config));
        
        // 与客户端建立TLS连接
        let client_tls_stream = acceptor.accept(client_stream).await
            .map_err(|e| AppError::Handshake(format!("🔐 Failed to accept TLS connection: {}", e)))?;
        
        // 🌐 获取ALPN协商结果
        let alpn_protocol = client_tls_stream.get_ref().1.alpn_protocol()
            .map(|protocol| String::from_utf8_lossy(protocol).to_string());
        
        if let Some(ref protocol) = alpn_protocol {
            debug!("ALPN协商成功: {} ({})", protocol, target_domain);
        } else {
            debug!("⚠️ 未协商ALPN协议 ({})", target_domain);
        }
        
        debug!("TLS拦截完成（客户端侧）: {}", target_domain);
        Ok((client_tls_stream, alpn_protocol))
    }
    
    /// 从证书创建服务器配置
    fn create_server_config_from_cert(&self, certificate: &TlsCertificate) -> Result<ServerConfig, rustls::Error> {
        // 解析证书和私钥 - 使用正确的PEM解析方式
        let cert_chain: Vec<CertificateDer> = rustls_pemfile::certs(&mut certificate.cert_pem.as_slice())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| rustls::Error::General(format!("Failed to parse certificate: {}", e)))?;
        
        let key = rustls_pemfile::pkcs8_private_keys(&mut certificate.key_pem.as_slice())
            .next()
            .ok_or_else(|| rustls::Error::General("No private key found".to_string()))?
            .map_err(|e| rustls::Error::General(format!("Failed to parse private key: {}", e)))?;
        
        // 🌐 创建服务器配置，支持HTTP/2 ALPN协商
        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, PrivateKeyDer::Pkcs8(key))?;
        
        // 🔧 配置ALPN协议：优先HTTP/2，回退到HTTP/1.1
        config.alpn_protocols = vec![
            b"h2".to_vec(),      // HTTP/2
            b"http/1.1".to_vec(), // HTTP/1.1
        ];
        
        debug!("🔧 TLS服务器配置已启用ALPN: h2, http/1.1");
        Ok(config)
    }
    

    
    /// 获取或生成证书
    async fn get_or_generate_certificate(&self, domain: &str) -> Result<TlsCertificate, AppError> {
        // 首先检查缓存
        {
            let cache = self.cert_cache.read().await;
            if let Some(cached) = cache.get(domain) {
                let age = std::time::SystemTime::now()
                    .duration_since(cached.cached_at)
                    .unwrap_or(Duration::from_secs(0));
                
                if age < self.config.cert_cache_duration {
                    // 更新缓存命中统计
                    {
                        let mut stats = self.stats.write().await;
                        stats.cache_hits += 1;
                    }
                    
                    debug!("Certificate cache hit for domain: {}", domain);
                    return Ok(cached.certificate.clone());
                }
            }
        }
        
        // 缓存未命中，需要生成新证书
        let certificate = if let Some(ref tls_manager) = self.tls_manager {
            tls_manager.cert_manager()
                .get_or_generate_cert(domain)
                .await
                .map_err(|e| AppError::Config(format!("Failed to generate certificate: {}", e)))?
        } else {
            return Err(AppError::Config("请配置证书管理器：tls_manager".to_string()));
        };
        
        // 更新缓存
        {
            let mut cache = self.cert_cache.write().await;
            cache.insert(domain.to_string(), CachedCertificate {
                certificate: certificate.clone(),
                cached_at: std::time::SystemTime::now(),
                hit_count: 0,
            });
        }
        
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.certificates_generated += 1;
        }
        
        debug!("Generated new certificate for domain: {}", domain);
        Ok(certificate)
    }
    
    /// 创建模拟证书（用于测试）
    #[allow(dead_code)]
    fn create_mock_certificate(&self, domain: &str) -> TlsCertificate {
        let now = chrono::Utc::now();
        TlsCertificate {
            cert_pem: format!("-----BEGIN CERTIFICATE-----\nMOCK CERT FOR {}\n-----END CERTIFICATE-----", domain).into_bytes(),
            key_pem: b"-----BEGIN PRIVATE KEY-----\nMOCK KEY\n-----END PRIVATE KEY-----".to_vec(),
            subject: domain.to_string(),
            not_before: now,
            not_after: now + chrono::Duration::days(365),
        }
    }
    
    /// 清理过期的证书缓存
    pub async fn cleanup_expired_certificates(&self) {
        let mut cache = self.cert_cache.write().await;
        let now = std::time::SystemTime::now();
        
        cache.retain(|domain, cached| {
            let age = now.duration_since(cached.cached_at).unwrap_or(Duration::from_secs(0));
            if age >= self.config.cert_cache_duration {
                debug!("Removing expired certificate for domain: {}", domain);
                false
            } else {
                true
            }
        });
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> TlsStats {
        self.stats.read().await.clone()
    }
    
    /// 更新统计信息
    async fn update_connection_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.total_tls_connections += 1;
    }
    
    /// 更新绕过统计
    async fn update_bypass_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.bypassed_connections += 1;
    }
    
    /// 更新握手失败统计
    async fn update_handshake_failure_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.handshake_failures += 1;
    }
}

impl Default for TlsMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for TlsMiddlewareConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interception_mode: TlsInterceptionMode::Selective,
            intercept_domains: vec!["*".to_string()],
            cert_store_path: None,
            cert_cache_duration: Duration::from_secs(24 * 60 * 60), // 24 hours
            max_concurrent_handshakes: 100,
        }
    }
}

impl From<&TlsConfig> for TlsMiddlewareConfig {
    fn from(config: &TlsConfig) -> Self {
        let interception_mode = match config.interception_mode.as_str() {
            "none" => TlsInterceptionMode::None,
            "full" => TlsInterceptionMode::Full,
            "selective" => TlsInterceptionMode::Selective,
            _ => {
                warn!("Unknown TLS interception mode '{}', falling back to selective", config.interception_mode);
                TlsInterceptionMode::Selective
            }
        };
        
        Self {
            enabled: config.enabled,
            interception_mode,
            intercept_domains: config.intercept_domains.clone(),
            cert_store_path: config.cert_store_path.clone(),
            cert_cache_duration: Duration::from_secs(config.cert_cache_duration),
            max_concurrent_handshakes: config.max_concurrent_handshakes,
        }
    }
}

#[async_trait]
impl Middleware for TlsMiddleware {
    fn name(&self) -> &str {
        "tls"
    }
    
    // #[instrument(skip(self, context, _core), fields(connection_id = %context.id))]
    #[instrument(skip(self, context, _core))]
    async fn process(
        &self,
        context: &mut ConnectionContext,
        _core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, crate::error::TakeoverError> {
        debug!("TLS中间件开始处理");
        
        // 更新处理状态
        context.set_processing_state(ProcessingState::TlsHandshake);
        
        // 更新连接统计
        self.update_connection_stats().await;
        
        // 检查是否是TLS连接
        let is_tls = self.detect_tls_handshake(context).await;
        
        if !is_tls {
            debug!("Not a TLS connection, skipping TLS middleware");
            return Ok(MiddlewareResult::Continue);
        }
        
        debug!("Detected TLS connection to {}:{}", context.target_host, context.target_port);
        
        // 检查是否应该拦截
        if !self.should_intercept_connection(context).await {
            debug!("TLS connection not intercepted based on configuration");
            self.update_bypass_stats().await;
            
            // 标记为TLS但不拦截
            context.custom_data.insert(
                "tls_processed".to_string(),
                serde_json::json!({"intercepted": false, "reason": "not_in_intercept_list"})
            );
            
            return Ok(MiddlewareResult::Continue);
        }
        
        // 执行TLS拦截
        match self.perform_tls_interception(context).await {
            Ok(()) => {
                debug!("✅ TLS interception successful for {}:{}", context.target_host, context.target_port);
                
                // 标记为已拦截
                context.custom_data.insert(
                    "tls_processed".to_string(),
                    serde_json::json!({"intercepted": true, "success": true})
                );
                
                Ok(MiddlewareResult::Continue)
            }
            Err(e) => {
                debug!("❌ TLS interception failed for {}:{}: {}", context.target_host, context.target_port, e);
                
                self.update_handshake_failure_stats().await;
                
                // 标记为拦截失败
                context.custom_data.insert(
                    "tls_processed".to_string(),
                    serde_json::json!({"intercepted": false, "success": false, "error": e.to_string()})
                );
                
                // 决定是否继续处理还是拒绝连接
                // if self.config.interception_mode == TlsInterceptionMode::Full {
                    // 如果是完全拦截模式，拦截失败则拒绝连接
                    Err(crate::error::TakeoverError::Config(format!("TLS interception failed: {}", e)))
                // } else {
                    // 否则继续处理，但记录错误
                //     warn!("TLS interception failed but continuing: {}", e);
                //     Ok(MiddlewareResult::Continue)
                // }
            }
        }
    }
    
    fn priority(&self) -> i32 {
        10 // 高优先级，在HTTP解析之前执行
    }
    
    fn enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::Connection;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    
    #[tokio::test]
    async fn test_middleware_creation() {
        let middleware = TlsMiddleware::new();
        assert_eq!(middleware.name(), "tls");
        assert!(!middleware.enabled());
    }
    
    #[tokio::test]
    async fn test_domain_pattern_matching() {
        let middleware = TlsMiddleware::new();
        
        // 测试通配符匹配
        assert!(middleware.domain_matches_pattern("api.example.com", "*.example.com"));
        assert!(middleware.domain_matches_pattern("example.com", "*.example.com"));
        assert!(!middleware.domain_matches_pattern("example.org", "*.example.com"));
        
        // 测试精确匹配
        assert!(middleware.domain_matches_pattern("example.com", "example.com"));
        assert!(!middleware.domain_matches_pattern("api.example.com", "example.com"));
        
        // 测试全通配符
        assert!(middleware.domain_matches_pattern("anything.com", "*"));
    }
    
    #[tokio::test]
    async fn test_tls_detection() {
        let middleware = TlsMiddleware::new();
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443)
        );
        
        let context = ConnectionContext::new(connection, "example.com".to_string(), 443);
        
        // HTTPS端口应该被检测为TLS
        assert!(middleware.detect_tls_handshake(&context).await);
        
        let context_http = ConnectionContext::new(
            Connection::new(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
            ),
            "example.com".to_string(),
            80
        );
        
        // HTTP端口不应该被检测为TLS
        assert!(!middleware.detect_tls_handshake(&context_http).await);
    }
    
    #[tokio::test]
    async fn test_certificate_caching() {
        let config = TlsMiddlewareConfig {
            enabled: true,
            interception_mode: TlsInterceptionMode::Selective,
            intercept_domains: vec!["example.com".to_string()],
            cert_cache_duration: Duration::from_secs(60),
            ..Default::default()
        };
        
        let middleware = TlsMiddleware::with_config(config);
        
        // 第一次获取证书
        let cert1 = middleware.get_or_generate_certificate("example.com").await.unwrap();
        
        // 第二次应该从缓存获取
        let cert2 = middleware.get_or_generate_certificate("example.com").await.unwrap();
        
        assert_eq!(cert1.subject, cert2.subject);
        
        let stats = middleware.get_stats().await;
        assert_eq!(stats.cache_hits, 1);
        assert_eq!(stats.certificates_generated, 1);
    }
    
    #[tokio::test]
    async fn test_stats_tracking() {
        let middleware = TlsMiddleware::new();
        
        let initial_stats = middleware.get_stats().await;
        assert_eq!(initial_stats.total_tls_connections, 0);
        
        middleware.update_connection_stats().await;
        middleware.update_bypass_stats().await;
        
        let updated_stats = middleware.get_stats().await;
        assert_eq!(updated_stats.total_tls_connections, 1);
        assert_eq!(updated_stats.bypassed_connections, 1);
    }
    
    #[tokio::test] 
    async fn test_tls_handshake_detection_logic() {
        // 测试TLS检测逻辑，使用模拟数据
        let middleware = TlsMiddleware::new();
        
        // 测试端口检测
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443)
        );
        let context = ConnectionContext::new(connection, "example.com".to_string(), 443);
        
        let is_tls = middleware.detect_tls_handshake(&context).await;
        assert!(is_tls, "端口443应该被检测为TLS");
        
        // 测试非HTTPS端口
        let connection_http = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        let context_http = ConnectionContext::new(connection_http, "example.com".to_string(), 80);
        
        let is_tls_http = middleware.detect_tls_handshake(&context_http).await;
        assert!(!is_tls_http, "端口80不应该被检测为TLS");
        
        println!("✅ TLS检测逻辑测试通过");
    }
    
    #[tokio::test]
    async fn test_tls_metadata_detection() {
        let middleware = TlsMiddleware::new();
        
        // 测试元数据检测
        let mut connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 8080)
        );
        
        // 添加HTTPS协议元数据
        connection.metadata.custom_data.insert(
            "detected_protocol".to_string(), 
            serde_json::json!("https")
        );
        
        let context = ConnectionContext::new(connection, "example.com".to_string(), 8080);
        let is_tls = middleware.detect_tls_handshake(&context).await;
        assert!(is_tls, "元数据指示HTTPS应该被检测为TLS");
        
        println!("✅ TLS元数据检测测试通过");
    }
}
