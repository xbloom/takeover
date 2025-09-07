//! 测试证书生成功能
//! 
//! 这个例子测试新的TLS证书生成功能，验证Chrome信任要求的扩展字段

use anyhow::Result;
use takeover::tls_interception::{TlsInterceptionManager};
use tracing::{info, Level};
use tracing_subscriber;

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    info!("🧪 Testing TLS certificate generation with Chrome-compatible extensions");

    // 创建TLS拦截管理器（使用真实实现）
    let manager = TlsInterceptionManager::new_real_with_default_path()?;
    
    info!("📋 Testing root CA generation...");
    
    // 测试根CA安装
    let cert_manager = manager.cert_manager();
    if !cert_manager.is_root_ca_installed().await {
        info!("🔐 Installing root CA certificate...");
        cert_manager.install_root_ca().await?;
    } else {
        info!("✅ Root CA already installed");
    }
    
    info!("📋 Testing domain certificate generation...");
    
    // 测试生成几个不同类型的证书
    let test_domains = vec![
        "example.com",
        "test.local", 
        "127.0.0.1",
        "localhost",
        "sub.domain.example.org",
    ];
    
    for domain in test_domains {
        info!("🔐 Generating certificate for: {}", domain);
        match cert_manager.get_or_generate_cert(domain).await {
            Ok(cert) => {
                info!("✅ Successfully generated certificate for {}", domain);
                info!("   📄 Certificate size: {} bytes", cert.cert_pem.len());
                info!("   🔑 Private key size: {} bytes", cert.key_pem.len());
                info!("   📅 Valid from: {} to {}", cert.not_before, cert.not_after);
                
                // 保存证书文件用于检查
                let cert_path = format!("/tmp/takeover_test_{}.pem", domain.replace(".", "_"));
                std::fs::write(&cert_path, &cert.cert_pem)?;
                info!("   💾 Saved certificate to: {}", cert_path);
                info!("   🔍 Inspect with: openssl x509 -in {} -text -noout", cert_path);
            }
            Err(e) => {
                info!("❌ Failed to generate certificate for {}: {}", domain, e);
            }
        }
    }
    
    info!("🎯 Test completed!");
    info!("📌 Next steps:");
    info!("   1. Inspect generated certificates with OpenSSL");
    info!("   2. Install root CA in browser trust store");
    info!("   3. Test HTTPS connections with generated certificates");
    
    Ok(())
}
