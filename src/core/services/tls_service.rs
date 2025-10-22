//! TLS服务 - 统一的TLS连接管理
//! 
//! 负责：
//! - 客户端TLS拦截（SSL Bump）
//! - 服务器TLS连接建立
//! - 证书管理和生成
//! - TLS配置统一管理

use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::{TlsAcceptor, TlsConnector, server::TlsStream as ServerTlsStream, client::TlsStream as ClientTlsStream};
use rustls::{ServerConfig, ClientConfig};
use rustls_pemfile;
use tracing::{debug, info, warn};

use crate::core::model::TlsConfig;
use crate::tls_interception::TlsCertificate;
use crate::core::ports::AppError;
use crate::tls_interception::{TlsInterceptionManager, CertificateManager};

/// TLS服务配置
#[derive(Debug, Clone)]
pub struct TlsServiceConfig {
    /// 是否启用TLS功能
    pub enabled: bool,
    /// 客户端拦截模式
    pub interception_mode: TlsInterceptionMode,
    /// 拦截域名列表
    pub intercept_domains: Vec<String>,
    /// 证书存储路径
    pub cert_store_path: Option<String>,
    /// 证书缓存时长
    pub cert_cache_duration: Duration,
    /// 最大并发握手数
    pub max_concurrent_handshakes: usize,
    /// 连接超时时间
    pub connect_timeout: Duration,
    /// 是否在开发模式下跳过服务器证书验证
    pub skip_server_verification: bool,
}

/// TLS拦截模式
#[derive(Debug, Clone, PartialEq)]
pub enum TlsInterceptionMode {
    /// 不拦截
    None,
    /// 选择性拦截（基于域名白名单）
    Selective,
    /// 全面拦截
    Full,
}

/// TLS服务统计
#[derive(Debug, Default, Clone)]
pub struct TlsServiceStats {
    /// 客户端拦截总数
    pub client_interceptions: u64,
    /// 服务器连接总数  
    pub server_connections: u64,
    /// 证书生成次数
    pub certificates_generated: u64,
    /// 握手失败次数
    pub handshake_failures: u64,
    /// 验证跳过次数
    pub verification_skipped: u64,
}

/// 统一的TLS服务
/// 
/// 这个服务集中管理所有TLS相关功能，避免在多个组件中重复实现
pub struct TlsService {
    /// 配置
    config: TlsServiceConfig,
    /// TLS拦截管理器（用于客户端拦截）
    tls_manager: Option<TlsInterceptionManager>,
    /// 客户端TLS配置（用于服务器连接）
    client_tls_config: Arc<ClientConfig>,
    /// 统计信息
    stats: Arc<tokio::sync::RwLock<TlsServiceStats>>,
}

/// 跳过服务器证书验证（开发模式）
#[derive(Debug)]
pub struct SkipServerVerification;

impl SkipServerVerification {
    pub fn new() -> Self {
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
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ED448,
        ]
    }
}

impl TlsService {
    /// 创建新的TLS服务
    pub fn new(config: TlsServiceConfig) -> Result<Self, AppError> {
        // 创建客户端TLS配置（支持ALPN）
        let mut client_config = if config.skip_server_verification {
            debug!("🔓 创建客户端TLS配置（跳过证书验证）");
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(SkipServerVerification::new()))
                .with_no_client_auth()
        } else {
            debug!("🔒 创建客户端TLS配置（启用证书验证）");
            ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth()
        };

        // 🌐 客户端ALPN协商支持（Outbound连接）
        // 注意：Outbound连接只使用HTTP/1.1，因为Forwarding中间件只支持HTTP/1.1响应解析
        // 客户端到代理的HTTP/2由HTTP/2中间件处理
        client_config.alpn_protocols = vec![
            b"http/1.1".to_vec(),  // 仅HTTP/1.1
        ];

        debug!("🔧 客户端TLS配置ALPN支持: http/1.1 (Outbound连接强制HTTP/1.1)");

        // 初始化TLS拦截管理器（如果启用）
        let tls_manager = if config.enabled && config.interception_mode != TlsInterceptionMode::None {
            info!("🔐 初始化TLS拦截管理器...");
            match &config.cert_store_path {
                Some(path) => Some(TlsInterceptionManager::new_real(path)),
                None => Some(TlsInterceptionManager::new_real_with_default_path()
                    .map_err(|e| AppError::Config(format!("Failed to create TLS manager: {}", e)))?),
            }
        } else {
            None
        };

        Ok(Self {
            config,
            tls_manager,
            client_tls_config: Arc::new(client_config),
            stats: Arc::new(tokio::sync::RwLock::new(TlsServiceStats::default())),
        })
    }

    /// 从AppCore配置创建TLS服务
    pub fn from_config(tls_config: &TlsConfig) -> Result<Self, AppError> {
        let interception_mode = match tls_config.interception_mode.as_str() {
            "none" => TlsInterceptionMode::None,
            "full" => TlsInterceptionMode::Full,
            "selective" => TlsInterceptionMode::Selective,
            _ => {
                warn!("Unknown TLS interception mode '{}', falling back to selective", tls_config.interception_mode);
                TlsInterceptionMode::Selective
            }
        };

        let config = TlsServiceConfig {
            enabled: tls_config.enabled,
            interception_mode,
            intercept_domains: tls_config.intercept_domains.clone(),
            cert_store_path: tls_config.cert_store_path.clone(),
            cert_cache_duration: Duration::from_secs(tls_config.cert_cache_duration),
            max_concurrent_handshakes: tls_config.max_concurrent_handshakes,
            connect_timeout: Duration::from_secs(30),
            skip_server_verification: true, // 开发模式
        };

        Self::new(config)
    }

    /// 初始化TLS服务（安装根CA等）
    pub async fn initialize(&self) -> Result<(), AppError> {
        if let Some(ref tls_manager) = self.tls_manager {
            info!("🔐 初始化TLS拦截环境（安装根CA证书）...");
            tls_manager.initialize().await
                .map_err(|e| AppError::Config(format!("TLS initialization failed: {}", e)))?;
            info!("✅ TLS拦截环境初始化完成");
        }
        Ok(())
    }

    /// 客户端TLS拦截（SSL Bump）
    /// 
    /// 用于拦截客户端发起的HTTPS连接，建立与客户端的TLS连接
    // #[instrument(skip(self, client_stream), fields(domain = %target_domain))]
    pub async fn intercept_client_tls(
        &self,
        client_stream: TcpStream,
        target_domain: &str,
    ) -> Result<(ServerTlsStream<TcpStream>, Option<String>), AppError> {
        if !self.config.enabled {
            return Err(AppError::Config("TLS service not enabled".to_string()));
        }

        let tls_manager = self.tls_manager.as_ref()
            .ok_or_else(|| AppError::Config("TLS manager not initialized".to_string()))?;

        debug!("🔐 开始客户端TLS拦截: {}", target_domain);

        // 获取或生成证书
        let certificate = tls_manager.cert_manager()
            .get_or_generate_cert(target_domain).await
            .map_err(|e| AppError::Config(format!("Certificate generation failed: {}", e)))?;

        // 创建服务器配置（支持ALPN协商）
        let server_config = self.create_server_config_from_cert(&certificate)?;
        let acceptor = TlsAcceptor::from(Arc::new(server_config));

        // 执行TLS握手
        let client_tls_stream = tokio::time::timeout(
            self.config.connect_timeout,
            acceptor.accept(client_stream)
        ).await
        .map_err(|_| AppError::Config(format!("Client TLS handshake timeout: {}", target_domain)))?
        .map_err(|e| AppError::Handshake(format!("Client TLS handshake failed: {}", e)))?;

        // 获取ALPN协议
        let alpn_protocol = client_tls_stream.get_ref().1.alpn_protocol()
            .map(|protocol| String::from_utf8_lossy(protocol).to_string());

        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.client_interceptions += 1;
            if let Some(ref protocol) = alpn_protocol {
                debug!("ALPN协商成功: {} ({})", protocol, target_domain);
            }
        }

        debug!("✅ 客户端TLS拦截完成: {}", target_domain);
        Ok((client_tls_stream, alpn_protocol))
    }

    /// 服务器TLS连接建立
    /// 
    /// 用于与真实服务器建立TLS连接（通过TCP隧道或直连）
    // #[instrument(skip(self, server_stream), fields(domain = %target_domain))]
    pub async fn connect_server_tls(
        &self,
        server_stream: TcpStream,
        target_domain: &str,
    ) -> Result<ClientTlsStream<TcpStream>, AppError> {
        debug!("🔗 建立服务器TLS连接: {}", target_domain);

        let connector = TlsConnector::from(self.client_tls_config.clone());

        // 创建服务器名称
        let server_name = target_domain.to_string().try_into()
            .map_err(|e| AppError::Config(format!("Invalid server name '{}': {:?}", target_domain, e)))?;

        // 执行TLS握手
        let server_tls_stream = tokio::time::timeout(
            self.config.connect_timeout,
            connector.connect(server_name, server_stream)
        ).await
        .map_err(|_| AppError::Config(format!("Server TLS handshake timeout: {}", target_domain)))?
        .map_err(|e| AppError::Config(format!("Server TLS handshake failed: {}", e)))?;

        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.server_connections += 1;
            if self.config.skip_server_verification {
                stats.verification_skipped += 1;
            }
        }

        debug!("✅ 服务器TLS连接建立完成: {}", target_domain);
        Ok(server_tls_stream)
    }

    /// 检查是否应该拦截指定域名
    pub async fn should_intercept_domain(&self, domain: &str) -> bool {
        if !self.config.enabled {
            return false;
        }

        match self.config.interception_mode {
            TlsInterceptionMode::None => false,
            TlsInterceptionMode::Full => true,
            TlsInterceptionMode::Selective => {
                self.config.intercept_domains.iter().any(|pattern| {
                    if pattern == "*" {
                        return true;
                    }
                    if pattern.starts_with("*.") {
                        let suffix = &pattern[2..];
                        return domain.ends_with(suffix) || domain == suffix;
                    }
                    domain == pattern
                })
            }
        }
    }

    /// 获取统计信息
    pub async fn get_stats(&self) -> TlsServiceStats {
        self.stats.read().await.clone()
    }

    /// 获取证书管理器引用（如果有）
    pub fn cert_manager(&self) -> Option<&dyn CertificateManager> {
        self.tls_manager.as_ref().map(|tm| tm.cert_manager()).map(|v| &**v)
    }

    /// 检查服务是否启用
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// 从证书创建服务器配置
    fn create_server_config_from_cert(&self, certificate: &TlsCertificate) -> Result<ServerConfig, AppError> {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        
        // 🔍 解析证书链（PEM格式 → DER格式）
        let cert_chain: Vec<CertificateDer> = rustls_pemfile::certs(&mut certificate.cert_pem.as_slice())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Config(format!("Failed to parse certificate chain: {}", e)))?;

        if cert_chain.is_empty() {
            return Err(AppError::Config("No certificates found in PEM data".to_string()));
        }

        // 🔑 解析私钥（PEM格式 → DER格式）
        let private_key = rustls_pemfile::pkcs8_private_keys(&mut certificate.key_pem.as_slice())
            .next()
            .ok_or_else(|| AppError::Config("No private key found in PEM data".to_string()))?
            .map_err(|e| AppError::Config(format!("Failed to parse private key: {}", e)))?;

        let key_der = PrivateKeyDer::Pkcs8(private_key);

        // 🏗️ 创建服务器配置（支持ALPN协商）
        let mut server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key_der)
            .map_err(|e| AppError::Config(format!("Failed to create server config: {}", e)))?;

        // 🌐 配置ALPN协商 - 支持HTTP/1.1和HTTP/2
        server_config.alpn_protocols = vec![
            b"h2".to_vec(),        // HTTP/2
            b"http/1.1".to_vec(),  // HTTP/1.1
        ];

        debug!("🔧 TLS服务器配置ALPN支持: h2, http/1.1");
        Ok(server_config)
    }

}

impl Default for TlsServiceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interception_mode: TlsInterceptionMode::Selective,
            intercept_domains: vec!["*".to_string()],
            cert_store_path: None,
            cert_cache_duration: Duration::from_secs(24 * 60 * 60),
            max_concurrent_handshakes: 100,
            connect_timeout: Duration::from_secs(30),
            skip_server_verification: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tls_service_config_default() {
        let config = TlsServiceConfig::default();
        assert!(config.enabled);
        assert_eq!(config.interception_mode, TlsInterceptionMode::Selective);
        assert!(config.skip_server_verification);
    }

    #[tokio::test]
    async fn test_domain_matching() {
        let mut config = TlsServiceConfig::default();
        config.intercept_domains = vec![
            "example.com".to_string(),
            "*.test.org".to_string(),
        ];
        
        let service = TlsService::new(config).unwrap();
        
        assert!(service.should_intercept_domain("example.com").await);
        assert!(service.should_intercept_domain("api.test.org").await);
        assert!(!service.should_intercept_domain("other.com").await);
    }
}
