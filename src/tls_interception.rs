//! TLS拦截模块
//! 
//! 此模块提供TLS拦截功能的完整接口和实现。
//! 通过运行时配置控制是否启用TLS拦截功能。
//! 
//! # 功能
//! - 证书管理和动态生成
//! - SSL Bump TLS拦截
//! - 运行时配置控制
//! - 存根实现用于测试和开发

use anyhow::{Result, Context};
use crate::routing_engine::TlsInterceptionMode;
use crate::{utils, AppError};
use async_trait::async_trait;
use tokio::net::TcpStream;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::path::Path;
use tokio::io::{AsyncRead, AsyncWrite, AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn, debug};
use rustls::{ServerConfig, ClientConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rcgen::{
    KeyPair, CertificateParams, DistinguishedName, SanType, 
    KeyUsagePurpose, ExtendedKeyUsagePurpose, IsCa, BasicConstraints, Issuer, CustomExtension
};
use time::OffsetDateTime;
use tokio_rustls::{TlsAcceptor, TlsConnector, server::TlsStream as ServerTlsStream, client::TlsStream as ClientTlsStream};
use sha1::{Sha1, Digest};

/// TLS证书结构
#[derive(Debug, Clone)]
pub struct TlsCertificate {
    /// 证书数据（PEM格式）
    pub cert_pem: Vec<u8>,
    /// 私钥数据（PEM格式）
    pub key_pem: Vec<u8>,
    /// 证书主题（域名）
    pub subject: String,
    /// 证书有效期开始时间
    pub not_before: chrono::DateTime<chrono::Utc>,
    /// 证书有效期结束时间
    pub not_after: chrono::DateTime<chrono::Utc>,
}

/// TLS拦截配置
#[derive(Debug, Clone)]
pub struct TlsInterceptionConfig {
    /// 拦截模式
    pub mode: TlsInterceptionMode,
    /// 需要拦截的域名列表
    pub intercept_domains: Vec<String>,
    /// 需要完全拦截的域名列表
    pub full_intercept_domains: Vec<String>,
    /// 证书缓存目录
    pub cert_cache_dir: String,
    /// 根CA证书路径
    pub root_ca_cert_path: Option<String>,
    /// 根CA私钥路径
    pub root_ca_key_path: Option<String>,
    /// 是否自动安装根CA
    pub auto_install_ca: bool,
}

// ========== TLS Interfaces ==========

/// 证书管理器接口
/// 
/// 负责动态生成和管理TLS证书，包括：
/// - 根CA证书的创建和安装
/// - 为特定域名动态生成证书
/// - 证书缓存和管理
#[async_trait]
pub trait CertificateManager: Send + Sync {
    /// 获取或生成指定域名的证书
    async fn get_or_generate_cert(&self, domain: &str) -> Result<TlsCertificate>;
    
    /// 安装根CA证书到系统信任存储
    async fn install_root_ca(&self) -> Result<()>;
    
    /// 检查根CA是否已安装
    async fn is_root_ca_installed(&self) -> bool;
    
    /// 清理过期证书
    async fn cleanup_expired_certs(&self) -> Result<()>;
}

/// TLS拦截器接口
/// 
/// 负责实际的TLS连接拦截和代理，包括：
/// - TLS握手拦截
/// - 双向TLS连接建立
/// - 内容检查和修改（可选）
#[async_trait]
pub trait TlsInterceptor: Send + Sync {
    /// 拦截TLS连接并建立双向代理
    /// 
    /// # 参数
    /// - `client_stream`: 客户端的TCP连接
    /// - `target_domain`: 目标域名
    /// - `target_port`: 目标端口
    /// 
    /// # 返回
    /// 返回(客户端TLS流, 服务器TLS流)的元组
    async fn intercept_connection(
        &self,
        client_stream: TcpStream,
        target_domain: &str,
        target_port: u16,
    ) -> Result<(Box<dyn TlsStream>, Box<dyn TlsStream>)>;
    
    /// 检查是否可以拦截指定域名
    async fn can_intercept(&self, domain: &str) -> bool;
    
    /// 设置拦截模式
    fn set_interception_mode(&mut self, mode: TlsInterceptionMode);
}

/// TLS流接口
/// 
/// 为拦截的TLS连接提供统一的读写接口
#[async_trait]
pub trait TlsStream: Send + Sync {
    /// 异步读取数据
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize>;
    
    /// 异步写入数据
    async fn write(&mut self, buf: &[u8]) -> Result<usize>;
    
    /// 刷新缓冲区
    async fn flush(&mut self) -> Result<()>;
    
    /// 获取对等证书信息
    fn peer_certificate(&self) -> Option<TlsCertificate>;
}

// ========== Real Implementations ==========

/// 真实的证书管理器实现
pub struct RustlsCertificateManager {
    /// 根CA颁发者（用于签名新证书）
    root_ca_issuer: Arc<Mutex<Option<Issuer<'static, KeyPair>>>>,
    /// 根CA证书PEM（用于客户端信任）
    root_ca_cert_pem: Arc<Mutex<Option<String>>>,
    /// 证书缓存 (域名 -> 证书)
    cert_cache: Arc<Mutex<HashMap<String, TlsCertificate>>>,
    /// 证书存储路径
    cert_store_path: String,
}

impl RustlsCertificateManager {
    /// 创建新的证书管理器
    pub fn new(cert_store_path: &str) -> Self {
        Self {
            root_ca_issuer: Arc::new(Mutex::new(None)),
            root_ca_cert_pem: Arc::new(Mutex::new(None)),
            cert_cache: Arc::new(Mutex::new(HashMap::new())),
            cert_store_path: cert_store_path.to_string(),
        }
    }

    /// 创建新的证书管理器（使用默认配置目录）
    pub fn new_with_default_path() -> Result<Self> {
        let home_dir = std::env::var("HOME")
            .map_err(|_| anyhow::anyhow!("Cannot find HOME directory"))?;
        let cert_store_path = format!("{}/.config/takeover", home_dir);
        
        // 创建目录（如果不存在）
        std::fs::create_dir_all(&cert_store_path)
            .map_err(|e| anyhow::anyhow!("Failed to create config directory: {}", e))?;

        Ok(Self::new(&cert_store_path))
    }
    
    /// 生成新的根CA证书和颁发者 (使用正确的rcgen API和完整扩展)
    fn generate_root_ca(&self) -> Result<(Issuer<'static, KeyPair>, String, String)> {
        
        // 创建证书参数
        let mut params = CertificateParams::new(vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
        ])?;
        
        // 设置证书主题
        let mut dn = DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, "Takeover Root CA");
        dn.push(rcgen::DnType::OrganizationName, "Takeover Proxy");
        dn.push(rcgen::DnType::CountryName, "US");
        params.distinguished_name = dn;
        
        // 设置为CA证书
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        
        // 设置Key Usage - CA需要的用途
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        
        // 设置Extended Key Usage - 虽然CA通常不需要，但加上Server Auth可以提高兼容性
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        
        // 设置证书有效期
        params.not_before = rcgen::date_time_ymd(2024, 1, 1);
        params.not_after = rcgen::date_time_ymd(2034, 1, 1);
        
        // 生成密钥对
        let key_pair = KeyPair::generate()?;
        
        // 生成证书
        let cert = params.self_signed(&key_pair)?;
        let cert_pem = cert.pem();
        let key_pem = key_pair.serialize_pem();
        
        // 创建Issuer对象用于签名其他证书
        let issuer = Issuer::from_ca_cert_pem(&cert_pem, key_pair)?;
        
        debug!("Generated root CA certificate (Subject: CN=Takeover Root CA, O=Takeover Proxy, C=US, Valid: 2024-2034)");
        
        Ok((issuer, cert_pem, key_pem))
    }


    
    /// 为指定域名生成证书 (由根CA签名，包含完整的Chrome信任扩展)
    fn generate_domain_cert(&self, domain: &str) -> Result<TlsCertificate> {
        debug!("🔐 Generating CA-signed certificate for domain: {}", domain);
        
        // 获取根CA颁发者
        let binding = self.root_ca_issuer.lock().unwrap();
        let ca_issuer = binding
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Root CA not initialized. Call ensure_root_ca() first."))?;
        
        // 创建证书参数
        let mut params = CertificateParams::new(vec![domain.to_string()])?;
        
        // 添加IP地址作为SAN（如果域名是IP地址）
        if domain.parse::<std::net::IpAddr>().is_ok() {
            params.subject_alt_names.push(SanType::IpAddress(domain.parse()?));
        }
        
        // 设置证书主题
        let mut dn = DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, domain);
        dn.push(rcgen::DnType::OrganizationName, "Takeover Proxy");
        dn.push(rcgen::DnType::CountryName, "US");
        params.distinguished_name = dn;
        
        // 设置为终端实体证书（非CA）
        params.is_ca = IsCa::NoCa;
        
        // 手动添加Basic Constraints扩展 (Chrome必需)
        // CA:FALSE = empty sequence (default)
        let basic_constraints_ext = CustomExtension::from_oid_content(
            &[2, 5, 29, 19], // Basic Constraints OID
            vec![0x30, 0x00] // DER: SEQUENCE {} = CA:FALSE
        );
        params.custom_extensions.push(basic_constraints_ext);
        
        // 设置Key Usage - 服务器证书需要的用途
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
            KeyUsagePurpose::KeyAgreement,
        ];
        
        // 设置Extended Key Usage - Chrome必需的服务器认证
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        
        // 设置证书有效期
        // 设置证书有效期为当前时间开始的1年
        let now = OffsetDateTime::now_utc();
        params.not_before = now;
        params.not_after = now + time::Duration::days(365); // 1年有效期
        
        // 生成密钥对
        let key_pair = KeyPair::generate()?;
        
        // 使用根CA签名证书 (真正的CA签名！)
        let cert = params.signed_by(&key_pair, &ca_issuer)?;
        let cert_pem = cert.pem();
        
        let key_pem = key_pair.serialize_pem();
        let now = chrono::Utc::now();
        
        let tls_cert = TlsCertificate {
            cert_pem: cert_pem.into_bytes(),
            key_pem: key_pem.into_bytes(),
            subject: domain.to_string(),
            not_before: now,
            not_after: now + chrono::Duration::days(365), // 1年有效期
        };
        
        debug!("Generated CA-signed certificate for domain: {} (Subject: CN={}, O=Takeover Proxy, C=US)", domain, domain);
        
        Ok(tls_cert)
    }
    
    /// 确保根CA已初始化
    fn ensure_root_ca(&self) -> Result<()> {
        let mut issuer_guard = self.root_ca_issuer.lock().unwrap();
        let mut cert_pem_guard = self.root_ca_cert_pem.lock().unwrap();
        
        if issuer_guard.is_some() && cert_pem_guard.is_some() {
            return Ok(());
        }
        
        let cert_path = format!("{}/takeover-root-ca.pem", self.cert_store_path);
        let key_path = format!("{}/takeover-root-ca-key.pem", self.cert_store_path);
        
        let has_cert = std::path::Path::new(&cert_path).exists();
        let has_key = std::path::Path::new(&key_path).exists();
        debug!("look for root CA files: cert={}, key={}", cert_path, key_path);
        
        // 🎯 读取已存在的证书和私钥文件
        if has_cert && has_key {
            debug!("Found existing root CA files: cert={}, key={}", cert_path, key_path);
            
            // 读取PEM文件内容
            let cert_pem = std::fs::read_to_string(&cert_path)?;
            let key_pem = std::fs::read_to_string(&key_path)?;
            
            // 从PEM文件创建Issuer对象
            let key_pair = KeyPair::from_pem(&key_pem)?;
            let issuer = Issuer::from_ca_cert_pem(&cert_pem, key_pair)?;
            
            debug!("Successfully loaded existing root CA certificate");
            
            *cert_pem_guard = Some(cert_pem);
            *issuer_guard = Some(issuer);
            
            return Ok(());
        }
        
        // 如果文件不存在，生成新的根CA
        // 一般不会执行到这里，如果到这里，就说明系统不完整，出问题了
        debug!("找不到根CA证书:{}，生成新的根CA", cert_path);
        let (issuer, cert_pem, key_pem) = self.generate_root_ca()?;
        
        // 🔥 修复：保存到文件系统（确保持久化）
        std::fs::write(&cert_path, &cert_pem)?;
        std::fs::write(&key_path, &key_pem)?;
        debug!("根CA证书和私钥已保存到文件: {}, {}", cert_path, key_path);
        
        // 存储到内存中
        *issuer_guard = Some(issuer);
        *cert_pem_guard = Some(cert_pem);
        
        drop(issuer_guard);
        drop(cert_pem_guard);
        
        debug!("✅ 根CA证书生成完成，已存储到文件和内存");
        
        Ok(())
    }
    
    /// 根CA证书安装的核心实现（同步版本）
    /// 
    /// 🎯 设计原则：同步实现作为基础，异步版本通过spawn_blocking包装
    fn install_root_ca_impl(cert_store_path: &str, cert_pem: &str) -> Result<()> {
        let ca_path = format!("{}/takeover-root-ca.pem", cert_store_path);
        
        // 创建目录（同步版本）
        if let Some(parent) = Path::new(&ca_path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        // 写入证书文件（同步版本）
        std::fs::write(&ca_path, cert_pem)?;
        
        info!("Root CA certificate saved to: {}", ca_path);
        
        // 在macOS上使用管理员权限安装到系统钥匙串
        #[cfg(target_os = "macos")]
        {
            debug!("🔐 正在将根CA证书安装到系统信任存储...");
            
            // 使用管理员权限安装证书
            match utils::execute_admin_command(
                "security", 
                &["add-trusted-cert", "-d", "-r", "trustRoot", "-k", "/Library/Keychains/System.keychain", &ca_path]
            ) {
                Ok(output) => {
                    if output.status.success() {
                        debug!("✅ 根CA证书已成功安装到系统信任存储");
                    } else {
                        let error_msg = String::from_utf8_lossy(&output.stderr);
                        info!("💡 请手动安装根CA证书: {}", ca_path);
                        info!("   使用命令: open {}", ca_path);
                        return Err(anyhow::anyhow!("❌ 根CA证书安装失败: {}", error_msg));
                    }
                },
                Err(e) => {
                    warn!("❌ 执行证书安装命令失败: {}", e);
                    info!("💡 请手动安装根CA证书: {}", ca_path);
                    info!("   使用命令: open {}", ca_path);
                    return Err(anyhow::anyhow!("❌ 根CA证书安装失败: {}", e));
                }
            }
        }
        
        #[cfg(not(target_os = "macos"))]
        {
            info!("💡 系统级证书安装仅支持macOS，请手动安装: {}", ca_path);
        }
        
        Ok(())
    }
    
}

#[async_trait]
impl CertificateManager for RustlsCertificateManager {
    async fn get_or_generate_cert(&self, domain: &str) -> Result<TlsCertificate> {
        // 先检查缓存
        {
            let cache = self.cert_cache.lock().unwrap();
            if let Some(cert) = cache.get(domain) {
                // 检查证书是否过期
                if cert.not_after > chrono::Utc::now() {
                    debug!("Using cached certificate for domain: {}", domain);
                    return Ok(cert.clone());
                }
            }
        }
        
        // 确保根CA已初始化
        self.ensure_root_ca()?;
        
        // 生成新证书
        let cert = self.generate_domain_cert(domain)?;
        
        // 缓存证书
        {
            let mut cache = self.cert_cache.lock().unwrap();
            cache.insert(domain.to_string(), cert.clone());
        }
        
        debug!("Generated and cached new certificate for domain: {}", domain);
        Ok(cert)
    }
    
    async fn install_root_ca(&self) -> Result<()> {
        self.ensure_root_ca()?;
        
        let ca_pem = {
            let cert_pem_guard = self.root_ca_cert_pem.lock().unwrap();
            if let Some(pem) = cert_pem_guard.as_ref() {
                pem.clone()
            } else {
                return Err(anyhow::anyhow!("Root CA not found"));
            }
        };
        
        // 🔄 调用同步实现（异步版本通过spawn_blocking包装同步版本）
        let cert_store_path = self.cert_store_path.clone();
        tokio::task::spawn_blocking(move || {
            Self::install_root_ca_impl(&cert_store_path, &ca_pem)
        }).await??;
        
        Ok(())
    }
    
    async fn is_root_ca_installed(&self) -> bool {
        let ca_path = format!("{}/takeover-root-ca.pem", self.cert_store_path);
        
        // 首先检查证书文件是否存在
        if !Path::new(&ca_path).exists() {
            return false;
        }
        
        // 获取我们证书的SHA-1指纹用于验证
        let cert_fingerprint = match self.get_cert_fingerprint(&ca_path).await {
            Ok(fp) => fp,
            Err(_) => return false,
        };
        
        // 在macOS上通过指纹检查证书是否安装到钥匙串
        #[cfg(target_os = "macos")]
        {
            match tokio::process::Command::new("security")
                .args(&["find-certificate", "-Z", "-c", "Takeover Root CA"])
                .output()
                .await
            {
                Ok(output) => {
                    if output.status.success() {
                        let output_str = String::from_utf8_lossy(&output.stdout);
                        // 检查输出中是否包含我们的证书指纹
                        output_str.lines()
                            .any(|line| {
                                if line.contains("SHA-1 hash:") {
                                    let hash = line.replace("SHA-1 hash:", "").trim().replace(" ", "").to_uppercase();
                                    let result = hash == cert_fingerprint.to_uppercase();
                                    debug!("证书指纹对比：{}",result);
                                    debug!("系统指纹：{}",hash);
                                    debug!("证书文件指纹：{}",cert_fingerprint);
                                    result
                                } else {
                                    false
                                }
                            })
                    } else {
                        false
                    }
                },
                Err(_) => false
            }
        }
        
        // 在其他系统上暂时只检查文件存在
        #[cfg(not(target_os = "macos"))]
        {
            true // 其他系统的检查逻辑待实现
        }
    }
    
    async fn cleanup_expired_certs(&self) -> Result<()> {
        let mut cache = self.cert_cache.lock().unwrap();
        let now = chrono::Utc::now();
        
        let expired_domains: Vec<String> = cache
            .iter()
            .filter(|(_, cert)| cert.not_after <= now)
            .map(|(domain, _)| domain.clone())
            .collect();
        
        for domain in expired_domains {
            cache.remove(&domain);
            debug!("Removed expired certificate for domain: {}", domain);
        }
        
        if !cache.is_empty() {
            info!("Cleaned up {} expired certificates", cache.len());
        }
        
        Ok(())
    }
}

impl RustlsCertificateManager {
    /// 获取证书的SHA-1指纹（纯Rust实现，不依赖系统openssl命令）
    async fn get_cert_fingerprint(&self, cert_path: &str) -> Result<String> {
        // 读取PEM文件
        let pem_data = tokio::fs::read(cert_path).await
            .context("Failed to read certificate file")?;
        
        // 解析PEM格式，提取证书的DER数据
        let mut cursor = std::io::Cursor::new(pem_data);
        let cert_der = rustls_pemfile::certs(&mut cursor)
            .next()
            .ok_or_else(|| anyhow::anyhow!("No certificate found in PEM file"))?
            .context("Failed to parse certificate from PEM")?;
        
        // 计算DER数据的SHA-1哈希
        let mut hasher = Sha1::new();
        hasher.update(&cert_der);
        let hash_result = hasher.finalize();
        
        // 格式化为十六进制字符串（大写，无分隔符）
        let fingerprint = hash_result.iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join("");
        Ok(fingerprint)
    }
}

// ========== Stub Implementations ==========

/// 存根证书管理器实现
pub struct StubCertificateManager;

#[async_trait]
impl CertificateManager for StubCertificateManager {
    async fn get_or_generate_cert(&self, _domain: &str) -> Result<TlsCertificate> {
        Err(anyhow::anyhow!("TLS interception not implemented yet"))
    }
    
    async fn install_root_ca(&self) -> Result<()> {
        Err(anyhow::anyhow!("TLS interception not implemented yet"))
    }
    
    async fn is_root_ca_installed(&self) -> bool {
        false
    }
    
    async fn cleanup_expired_certs(&self) -> Result<()> {
        Ok(()) // 存根实现，无需清理
    }
}

/// 真实的TLS拦截器实现
pub struct RustlsTlsInterceptor {
    mode: TlsInterceptionMode,
    cert_manager: Arc<dyn CertificateManager>,
}

impl RustlsTlsInterceptor {
    pub fn new(cert_manager: Arc<dyn CertificateManager>) -> Self {
        Self {
            mode: TlsInterceptionMode::None,
            cert_manager,
        }
    }
    
    /// 创建服务器TLS配置
    async fn create_server_config(&self, domain: &str) -> Result<Arc<ServerConfig>> {
        let cert = self.cert_manager.get_or_generate_cert(domain).await?;
        create_server_tls_config(&cert)
    }
    
    /// 创建客户端TLS配置（用于连接真正的服务器）
    fn create_client_config() -> Result<Arc<ClientConfig>> {
        create_client_tls_config()
    }
}

#[async_trait]
impl TlsInterceptor for RustlsTlsInterceptor {
    async fn intercept_connection(
        &self,
        client_stream: TcpStream,
        target_domain: &str,
        target_port: u16,
    ) -> Result<(Box<dyn TlsStream>, Box<dyn TlsStream>)> {
        info!("Starting TLS interception for {}:{}", target_domain, target_port);
        
        // 1. 创建服务器配置（用我们的证书）
        let server_config = self.create_server_config(target_domain).await?;
        let acceptor = TlsAcceptor::from(server_config);
        
        // 2. 与客户端建立TLS连接
        let client_tls_stream = acceptor.accept(client_stream).await?;
        debug!("Established TLS connection with client for {}", target_domain);
        
        // 3. 连接到真正的服务器
        let server_stream = TcpStream::connect((target_domain, target_port)).await?;
        
        // 4. 与真正的服务器建立TLS连接
        let client_config = Self::create_client_config()?;
        let connector = TlsConnector::from(client_config);
        let domain_name = rustls::pki_types::ServerName::try_from(target_domain.to_string())?;
        let server_tls_stream = connector.connect(domain_name, server_stream).await?;
        debug!("Established TLS connection with server {}:{}", target_domain, target_port);
        
        // 5. 包装为TlsStream
        let client_wrapped: Box<dyn TlsStream> = Box::new(RustlsTlsStreamWrapper::Server(client_tls_stream));
        let server_wrapped: Box<dyn TlsStream> = Box::new(RustlsTlsStreamWrapper::Client(server_tls_stream));
        
        info!("TLS interception established for {}:{}", target_domain, target_port);
        Ok((client_wrapped, server_wrapped))
    }
    
    async fn can_intercept(&self, domain: &str) -> bool {
        match self.mode {
            TlsInterceptionMode::None => false,
            TlsInterceptionMode::Selective | TlsInterceptionMode::Full => {
                // 简单检查：尝试为域名生成证书
                self.cert_manager.get_or_generate_cert(domain).await.is_ok()
            }
        }
    }
    
    fn set_interception_mode(&mut self, mode: TlsInterceptionMode) {
        debug!("TLS interception mode set to: {:?}", mode);
        self.mode = mode;
    }
}

/// TLS流包装器
pub enum RustlsTlsStreamWrapper<T> {
    Server(ServerTlsStream<T>),
    Client(ClientTlsStream<T>),
}

#[async_trait]
impl<T> TlsStream for RustlsTlsStreamWrapper<T>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + Sync,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self {
            Self::Server(stream) => {
                let n = stream.read(buf).await?;
                Ok(n)
            }
            Self::Client(stream) => {
                let n = stream.read(buf).await?;
                Ok(n)
            }
        }
    }
    
    async fn write(&mut self, buf: &[u8]) -> Result<usize> {
        match self {
            Self::Server(stream) => {
                stream.write_all(buf).await?;
                Ok(buf.len())
            }
            Self::Client(stream) => {
                stream.write_all(buf).await?;
                Ok(buf.len())
            }
        }
    }
    
    async fn flush(&mut self) -> Result<()> {
        match self {
            Self::Server(stream) => {
                stream.flush().await?;
                Ok(())
            }
            Self::Client(stream) => {
                stream.flush().await?;
                Ok(())
            }
        }
    }
    
    fn peer_certificate(&self) -> Option<TlsCertificate> {
        // 这里可以从TLS连接中提取证书信息
        // 暂时返回None
        None
    }
}

/// 存根TLS拦截器实现
pub struct StubTlsInterceptor {
    mode: TlsInterceptionMode,
}

impl StubTlsInterceptor {
    pub fn new() -> Self {
        Self {
            mode: TlsInterceptionMode::None,
        }
    }
}

#[async_trait]
impl TlsInterceptor for StubTlsInterceptor {
    async fn intercept_connection(
        &self,
        _client_stream: TcpStream,
        _target_domain: &str,
        _target_port: u16,
    ) -> Result<(Box<dyn TlsStream>, Box<dyn TlsStream>)> {
        Err(anyhow::anyhow!("TLS interception not implemented yet"))
    }
    
    async fn can_intercept(&self, _domain: &str) -> bool {
        false // 存根实现始终返回false
    }
    
    fn set_interception_mode(&mut self, mode: TlsInterceptionMode) {
        self.mode = mode;
    }
}

impl Default for StubTlsInterceptor {
    fn default() -> Self {
        Self::new()
    }
}

/// 存根TLS流实现
pub struct StubTlsStream;

#[async_trait]
impl TlsStream for StubTlsStream {
    async fn read(&mut self, _buf: &mut [u8]) -> Result<usize> {
        Err(anyhow::anyhow!("TLS stream not implemented yet"))
    }
    
    async fn write(&mut self, _buf: &[u8]) -> Result<usize> {
        Err(anyhow::anyhow!("TLS stream not implemented yet"))
    }
    
    async fn flush(&mut self) -> Result<()> {
        Err(anyhow::anyhow!("TLS stream not implemented yet"))
    }
    
    fn peer_certificate(&self) -> Option<TlsCertificate> {
        None
    }
}

// ========== Runtime Configuration Support ==========

/// 运行时TLS拦截配置
/// 
/// 控制是否在运行时启用TLS拦截功能
#[derive(Debug, Clone)]
pub struct RuntimeTlsConfig {
    /// 是否启用TLS拦截功能
    pub enabled: bool,
    /// TLS拦截模式
    pub mode: TlsInterceptionMode,
    /// 需要拦截的域名列表
    pub intercept_domains: Vec<String>,
}

impl Default for RuntimeTlsConfig {
    fn default() -> Self {
        Self {
            enabled: false, // 默认禁用
            mode: TlsInterceptionMode::None,
            intercept_domains: Vec::new(),
        }
    }
}

impl Default for TlsInterceptionConfig {
    fn default() -> Self {
        Self {
            mode: TlsInterceptionMode::Selective,
            intercept_domains: vec!["*".to_string()],
            full_intercept_domains: Vec::new(),
            cert_cache_dir: "/tmp/takeover_certs".to_string(),
            root_ca_cert_path: None,
            root_ca_key_path: None,
            auto_install_ca: true,
        }
    }
}

impl TlsInterceptionConfig {
    /// 从AppConfig的TlsConfig创建TlsInterceptionConfig
    pub fn from_app_config(tls_config: &crate::core::model::TlsConfig, cert_store_path: Option<&str>) -> Self {
        let mode = match tls_config.interception_mode.as_str() {
            "none" => TlsInterceptionMode::None,
            "full" => TlsInterceptionMode::Full,
            "selective" => TlsInterceptionMode::Selective,
            _ => TlsInterceptionMode::Selective,
        };
        
        let cert_cache_dir = cert_store_path
            .map(|p| p.to_string())
            .or_else(|| tls_config.cert_store_path.clone())
            .unwrap_or_else(|| "/tmp/takeover_certs".to_string());
        
        Self {
            mode,
            intercept_domains: tls_config.intercept_domains.clone(),
            full_intercept_domains: Vec::new(), // 可以后续扩展
            cert_cache_dir,
            root_ca_cert_path: None, // 使用动态生成
            root_ca_key_path: None,  // 使用动态生成
            auto_install_ca: tls_config.auto_install_ca,
        }
    }
}

// ========== TLS Configuration Utilities ==========

/// 创建服务器TLS配置（使用给定的证书）
pub fn create_server_tls_config(cert: &TlsCertificate) -> Result<Arc<ServerConfig>> {
    // 解析证书和私钥
    let cert_chain: Vec<CertificateDer> = rustls_pemfile::certs(&mut cert.cert_pem.as_slice())
        .collect::<Result<Vec<_>, _>>()?;
    
    let key = rustls_pemfile::pkcs8_private_keys(&mut cert.key_pem.as_slice())
        .next()
        .ok_or_else(|| anyhow::anyhow!("No private key found"))?
        .map_err(|e| anyhow::anyhow!("Failed to parse private key: {}", e))?;
    
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, PrivateKeyDer::Pkcs8(key))?;
    
    Ok(Arc::new(config))
}

/// 创建客户端TLS配置（接受所有证书用于代理）
pub fn create_client_tls_config() -> Result<Arc<ClientConfig>> {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerifier};
    use rustls::pki_types::{ServerName, UnixTime};
    
    #[derive(Debug)]
    struct AcceptAllCerts;
    
    impl ServerCertVerifier for AcceptAllCerts {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        
        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
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
    
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAllCerts))
        .with_no_client_auth();
    Ok(Arc::new(config))
}

// ========== TLS Interception Manager ==========

/// TLS拦截管理器
/// 
/// 协调证书管理器和TLS拦截器的工作，支持运行时配置
pub struct TlsInterceptionManager {
    cert_manager: Arc<dyn CertificateManager>,
    interceptor: Arc<dyn TlsInterceptor + Send + Sync>,
    config: TlsInterceptionConfig,
    runtime_config: RuntimeTlsConfig,
}

impl Clone for TlsInterceptionManager {
    fn clone(&self) -> Self {
        Self {
            cert_manager: self.cert_manager.clone(),
            interceptor: self.interceptor.clone(),
            config: self.config.clone(),
            runtime_config: self.runtime_config.clone(),
        }
    }
}

impl TlsInterceptionManager {
    /// 创建新的TLS拦截管理器（使用真实实现）
    pub fn new_real(cert_store_path: &str) -> Self {
        let cert_manager = RustlsCertificateManager::new(cert_store_path);
        let cert_manager_arc = Arc::new(cert_manager);
        let interceptor = Arc::new(RustlsTlsInterceptor::new(cert_manager_arc.clone()));
        
        Self {
            cert_manager: cert_manager_arc,
            interceptor,
            config: TlsInterceptionConfig::default(),
            runtime_config: RuntimeTlsConfig::default(),
        }
    }
    
    /// 创建新的TLS拦截管理器（使用真实实现和自定义配置）
    pub fn new_real_with_config(cert_store_path: &str, config: TlsInterceptionConfig) -> Self {
        let cert_manager = RustlsCertificateManager::new(cert_store_path);
        let cert_manager_arc = Arc::new(cert_manager);
        let interceptor = Arc::new(RustlsTlsInterceptor::new(cert_manager_arc.clone()));
        
        Self {
            cert_manager: cert_manager_arc,
            interceptor,
            config,
            runtime_config: RuntimeTlsConfig::default(),
        }
    }

    /// 创建新的TLS拦截管理器（使用默认配置目录）
    pub fn new_real_with_default_path() -> Result<Self> {
        let cert_manager = RustlsCertificateManager::new_with_default_path()?;
        let cert_manager_arc = Arc::new(cert_manager);
        let interceptor = Arc::new(RustlsTlsInterceptor::new(cert_manager_arc.clone()));
        
        Ok(Self {
            cert_manager: cert_manager_arc,
            interceptor,
            config: TlsInterceptionConfig::default(),
            runtime_config: RuntimeTlsConfig::default(),
        })
    }
    
    /// 创建新的TLS拦截管理器（使用存根实现）
    pub fn new_stub() -> Self {
        Self {
            cert_manager: Arc::new(StubCertificateManager),
            interceptor: Arc::new(StubTlsInterceptor::new()),
            config: TlsInterceptionConfig::default(),
            runtime_config: RuntimeTlsConfig::default(),
        }
    }
    
    /// 使用自定义实现创建TLS拦截管理器
    pub fn new(
        cert_manager: Arc<dyn CertificateManager>,
        interceptor: Arc<dyn TlsInterceptor + Send + Sync>,
        config: TlsInterceptionConfig,
    ) -> Self {
        Self {
            cert_manager,
            interceptor,
            config,
            runtime_config: RuntimeTlsConfig::default(),
        }
    }
    
    /// 初始化TLS拦截环境
    /// 
    /// 🎯 职责清晰：协调证书生成和安装的完整流程
    pub async fn initialize(&self) -> Result<()> {
        match self.config.mode {
            TlsInterceptionMode::None => {
                debug!("TLS拦截模式为None，跳过初始化");
                Ok(())
            }
            TlsInterceptionMode::Selective | TlsInterceptionMode::Full => {
                debug!("开始初始化TLS拦截环境...");
                
                // 如果配置为自动安装，则安装到系统（install_root_ca会自动调用ensure_root_ca）
                if self.config.auto_install_ca {
                    if !self.cert_manager.is_root_ca_installed().await {
                        debug!("根CA未安装，开始自动安装到系统信任存储");
                        if let Err(e) = self.cert_manager.install_root_ca().await {
                            return Err(AppError::System("根CA自动安装失败".to_string()).into());
                        } else {
                            debug!("✅ 根CA已安装到系统信任存储");
                        }
                    } else {
                        debug!("根CA已安装，跳过安装步骤");
                    }
                } else {
                    debug!("自动安装已禁用，根CA证书将在首次使用时生成");
                }
                let cert = self.cert_manager.get_or_generate_cert("www.aowuwu.com").await?;
                debug!("TLS拦截环境初始化完成: ca_cert_path={:?}", cert);
                Ok(())
            }
        }
    }
    
    /// 检查是否应该拦截指定域名
    pub async fn should_intercept(&self, domain: &str) -> bool {
        // 首先检查运行时配置是否启用
        if !self.runtime_config.enabled {
            return false;
        }
        
        match self.runtime_config.mode {
            TlsInterceptionMode::None => false,
            TlsInterceptionMode::Selective => {
                self.runtime_config.intercept_domains.iter().any(|d| domain.contains(d))
            }
            TlsInterceptionMode::Full => {
                self.interceptor.can_intercept(domain).await
            }
        }
    }
    
    /// 执行TLS拦截
    pub async fn intercept(
        &self,
        client_stream: TcpStream,
        target_domain: &str,
        target_port: u16,
    ) -> Result<(Box<dyn TlsStream>, Box<dyn TlsStream>)> {
        if !self.should_intercept(target_domain).await {
            return Err(anyhow::anyhow!("Domain {} not configured for interception", target_domain));
        }
        
        self.interceptor.intercept_connection(client_stream, target_domain, target_port).await
    }
    
    /// 获取当前配置
    pub fn config(&self) -> &TlsInterceptionConfig {
        &self.config
    }
    
    /// 更新配置
    pub fn set_config(&mut self, config: TlsInterceptionConfig) {
        // 注意：Arc不允许修改，这里只能更新本地配置
        self.config = config;
    }
    
    /// 更新运行时配置
    pub fn set_runtime_config(&mut self, runtime_config: RuntimeTlsConfig) {
        // 注意：Arc不允许修改，这里只能更新本地配置
        self.runtime_config = runtime_config;
    }
    
    /// 获取运行时配置
    pub fn runtime_config(&self) -> &RuntimeTlsConfig {
        &self.runtime_config
    }
    
    /// 启用或禁用TLS拦截
    pub fn set_enabled(&mut self, enabled: bool) {
        self.runtime_config.enabled = enabled;
    }
    
    /// 检查TLS拦截是否启用
    pub fn is_enabled(&self) -> bool {
        self.runtime_config.enabled
    }
    
    /// 获取证书管理器的引用
    pub fn cert_manager(&self) -> &Arc<dyn CertificateManager> {
        &self.cert_manager
    }
}



impl Default for TlsInterceptionManager {
    fn default() -> Self {
        Self::new_stub()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stub_implementations() {
        // 测试存根实现的默认行为
        let interceptor = StubTlsInterceptor::new();
        let _ = interceptor; // 避免unused variable警告
    }

    #[test]
    fn test_tls_manager() {
        let manager = TlsInterceptionManager::new_stub();
        
        // 测试基本创建
        assert_eq!(manager.config().mode, TlsInterceptionMode::None);
        assert!(!manager.is_enabled()); // 默认禁用
    }

    #[tokio::test]
    async fn test_async_tls_manager() {
        let mut manager = TlsInterceptionManager::new_stub();
        
        // 测试异步初始化
        assert!(manager.initialize().await.is_ok());
        
        // 测试异步拦截决策（默认禁用应该都返回false）
        assert!(!manager.should_intercept("example.com").await);
        assert!(!manager.should_intercept("google.com").await);
        
        // 测试启用后的行为
        let mut runtime_config = RuntimeTlsConfig::default();
        runtime_config.enabled = true;
        runtime_config.mode = TlsInterceptionMode::Selective;
        runtime_config.intercept_domains.push("example.com".to_string());
        
        manager.set_runtime_config(runtime_config);
        assert!(manager.is_enabled());
        assert!(manager.should_intercept("example.com").await);
        assert!(!manager.should_intercept("google.com").await);
    }

    #[test]
    fn test_config_default() {
        let config = TlsInterceptionConfig::default();
        assert_eq!(config.mode, TlsInterceptionMode::None);
        assert!(config.intercept_domains.is_empty());
        assert!(!config.auto_install_ca);
    }
}
