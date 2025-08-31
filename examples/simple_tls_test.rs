//! 最简单的 TLS intercept_connection 测试
//! 
//! 直接创建TCP监听器，测试TLS拦截功能
//! 使用 curl --connect-to httpbin.org:443:127.0.0.1:8443 https://httpbin.org/ip 测试

use takeover::tls_interception::{TlsInterceptionManager, RuntimeTlsConfig};
use takeover::routing_engine::TlsInterceptionMode;
use anyhow::Result;
use tokio::net::{TcpListener, TcpStream};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter("takeover=debug,simple_tls_test=info")
        .init();

    println!("🧪 简单TLS拦截测试");
    println!("===================");

    // 1. 创建TLS拦截管理器（使用默认配置目录）
    let mut tls_manager = TlsInterceptionManager::new_real_with_default_path()?;
    
    // 2. 配置允许拦截httpbin.org
    let mut tls_config = RuntimeTlsConfig::default();
    tls_config.enabled = true;
    tls_config.mode = TlsInterceptionMode::Selective;
    tls_config.intercept_domains = vec!["httpbin.org".to_string()];
    tls_manager.set_runtime_config(tls_config);
    
    println!("✅ TLS拦截管理器创建成功");
    println!("   证书路径: ~/.config/takeover/");
    println!("   拦截域名: httpbin.org");

    // 3. 初始化TLS环境
    println!("\n🔐 初始化TLS环境...");
    tls_manager.initialize().await?;
    println!("✅ TLS环境初始化完成");

    // 4. 预生成证书
    println!("\n📜 生成httpbin.org证书...");
    let cert = tls_manager.cert_manager().get_or_generate_cert("httpbin.org").await?;
    println!("✅ 证书生成成功，有效期: {} - {}", cert.not_before, cert.not_after);

    // 5. 创建TCP监听器
    let listener = TcpListener::bind("127.0.0.1:8443").await?;
    println!("\n🚀 TCP监听器启动成功: 127.0.0.1:8443");
    
    println!("\n💡 测试命令:");
    println!("   curl -k --connect-to httpbin.org:443:127.0.0.1:8443 https://httpbin.org/ip");
    println!("   (-k 参数忽略证书验证错误)");
    println!("   或者在另一个终端:");
    println!("   curl -k --resolve httpbin.org:443:127.0.0.1 https://httpbin.org/ip");
    
    println!("\n⏳ 等待连接...");

    // 6. 监听并处理连接
    loop {
        match listener.accept().await {
            Ok((client_stream, client_addr)) => {
                println!("\n🔗 接收到连接: {}", client_addr);
                
                let manager = tls_manager.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(client_stream, manager).await {
                        println!("❌ 连接处理失败: {}", e);
                    }
                });
            }
            Err(e) => {
                println!("❌ 接受连接失败: {}", e);
                break;
            }
        }
    }

    Ok(())
}

async fn handle_connection(client_stream: TcpStream, tls_manager: TlsInterceptionManager) -> Result<()> {
    println!("🔧 开始处理连接...");

    // 直接调用 intercept_connection
    println!("🚀 调用 intercept_connection...");
    match tls_manager.intercept(client_stream, "httpbin.org", 443).await {
        Ok((mut client_tls, mut server_tls)) => {
            println!("✅ TLS拦截成功！");
            
            // 简单的数据转发
            println!("📡 开始数据转发...");
            
            let mut client_buf = vec![0u8; 4096];
            let mut server_buf = vec![0u8; 4096];
            
            loop {
                tokio::select! {
                    // 客户端到服务器
                    result = client_tls.read(&mut client_buf) => {
                        match result {
                            Ok(0) => {
                                println!("🔚 客户端连接关闭");
                                break;
                            }
                            Ok(n) => {
                                println!("📤 客户端 -> 服务器: {} bytes", n);
                                if let Err(e) = server_tls.write(&client_buf[..n]).await {
                                    println!("❌ 写入服务器失败: {}", e);
                                    break;
                                }
                                if let Err(e) = server_tls.flush().await {
                                    println!("❌ 刷新服务器失败: {}", e);
                                    break;
                                }
                            }
                            Err(e) => {
                                println!("❌ 读取客户端失败: {}", e);
                                break;
                            }
                        }
                    }
                    
                    // 服务器到客户端
                    result = server_tls.read(&mut server_buf) => {
                        match result {
                            Ok(0) => {
                                println!("🔚 服务器连接关闭");
                                break;
                            }
                            Ok(n) => {
                                println!("📥 服务器 -> 客户端: {} bytes", n);
                                if let Err(e) = client_tls.write(&server_buf[..n]).await {
                                    println!("❌ 写入客户端失败: {}", e);
                                    break;
                                }
                                if let Err(e) = client_tls.flush().await {
                                    println!("❌ 刷新客户端失败: {}", e);
                                    break;
                                }
                            }
                            Err(e) => {
                                println!("❌ 读取服务器失败: {}", e);
                                break;
                            }
                        }
                    }
                    
                    // 超时保护
                    _ = tokio::time::sleep(Duration::from_secs(300)) => {
                        println!("⏰ 连接超时");
                        break;
                    }
                }
            }
            
            println!("✅ 连接处理完成");
        }
        Err(e) => {
            println!("❌ TLS拦截失败: {}", e);
        }
    }

    Ok(())
}