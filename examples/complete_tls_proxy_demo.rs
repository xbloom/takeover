//! 完整的TLS透明代理演示程序
//! 
//! 展示完整的透明代理系统，包含TLS拦截功能

use takeover::config::{ProxyConfig, ProxyType, Config};
use takeover::routing_engine::{RoutingEngine, TlsInterceptionMode};
use takeover::transparent_server::TransparentServer;
use takeover::tls_interception::RuntimeTlsConfig;
use anyhow::Result;
use tokio::signal;


#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志系统
    tracing_subscriber::fmt()
        .with_env_filter("takeover=info,complete_tls_proxy_demo=info")
        .init();

    println!("🌐 完整TLS透明代理演示程序");
    println!("=====================================");

    // 1. 创建SOCKS5代理配置
    let socks5_config = ProxyConfig {
        url: "socks5://127.0.0.1:1080".to_string(),
        proxy_type: ProxyType::Socks5,
        auth: None,
    };
    println!("✅ SOCKS5代理配置: 127.0.0.1:7897");

    // 2. 创建基础配置
    let config = Config::default();
    
    // 3. 创建路由引擎
    let routing_engine = RoutingEngine::new(&config);
    
    println!("✅ 路由引擎配置完成 (使用默认规则)");

    // 3. 创建透明代理服务器
    let mut transparent_server = TransparentServer::new(socks5_config, routing_engine);
    
    // 4. 启用TLS拦截功能（使用默认证书路径）
    transparent_server.enable_tls_interception(None)?;
    
    // 5. 配置TLS拦截设置
    let mut tls_config = RuntimeTlsConfig::default();
    tls_config.enabled = true;
    tls_config.mode = TlsInterceptionMode::Selective;
    tls_config.intercept_domains = vec![
        "api.github.com".to_string(),
        "httpbin.org".to_string(),
        "example.com".to_string(),
    ];
    
    transparent_server.configure_tls_interception(tls_config)?;
    println!("✅ TLS拦截功能已启用");
    println!("   证书存储: ~/.config/takeover/");
    println!("   拦截域名: {:?}", transparent_server.tls_manager().unwrap().runtime_config().intercept_domains);

    // 6. 检查用户权限
    let user_manager = transparent_server.user_manager();
    if !user_manager.is_admin() {
        println!("⚠️  警告: 当前没有管理员权限，某些功能可能无法正常工作");
        println!("   建议使用 sudo 运行此程序");
    }

    // 7. 在启动服务器之前，先完成所有证书相关的准备工作
    if let Some(tls_manager) = transparent_server.tls_manager() {
        println!("\n📜 检查根CA证书...");
        if !tls_manager.cert_manager().is_root_ca_installed().await {
            println!("   正在安装根CA证书到系统...");
            match tls_manager.cert_manager().install_root_ca().await {
                Ok(_) => println!("   ✅ 根CA证书安装成功"),
                Err(e) => println!("   ⚠️  根CA证书安装失败: {}", e),
            }
        } else {
            println!("   ✅ 根CA证书已安装");
        }
        
        // 预生成一些测试证书
        println!("\n🔐 预生成测试证书...");
        for domain in &["api.github.com", "httpbin.org", "example.com"] {
            match tls_manager.cert_manager().get_or_generate_cert(domain).await {
                Ok(cert) => println!("   ✅ {} - 证书有效期至 {}", domain, cert.not_after),
                Err(e) => println!("   ❌ {} - 证书生成失败: {}", domain, e),
            }
        }
    }

    // 8. 初始化透明代理（但不启动监听）
    let intercepted_ports = vec![443];
    println!("\n🚀 初始化透明代理服务器...");
    
    match transparent_server.initialize(&intercepted_ports).await {
        Ok(_) => {
            println!("✅ 透明代理服务器初始化成功");
            println!("   本地代理端口: {}", transparent_server.get_local_port().unwrap_or(0));
            println!("   拦截端口: {:?}", intercepted_ports);
            
            // 显示TLS拦截状态
            if let Some(tls_manager) = transparent_server.tls_manager() {
                println!("   TLS拦截状态: {}", if tls_manager.is_enabled() { "✅ 启用" } else { "❌ 禁用" });
            }
            
            println!("\n🎯 透明代理配置完成！");
            println!("\n💡 使用说明:");
            println!("   1. 确保SOCKS5代理在 127.0.0.1:1080 运行");
            println!("   2. 安装根CA证书到系统信任存储");
            println!("   3. 配置系统流量重定向到透明代理");
            println!("   4. 访问 https://api.github.com 或 https://httpbin.org 测试TLS拦截");
            
            println!("\n📊 系统状态:");
            let (active, max, should_stop) = transparent_server.get_server_stats();
            println!("   活跃连接: {}/{}", active, max);
            println!("   运行状态: {}", if should_stop { "已停止" } else { "运行中" });
            
            // 启动服务器（使用tokio::select!来处理信号）
            println!("\n🚀 启动透明代理服务器...");
            println!("   服务器将运行直到手动停止 (Ctrl+C)");
            
            // 使用 tokio::select! 来并发运行服务器和等待信号
            let result = tokio::select! {
                // 运行透明代理服务器（这会一直运行直到出错或被取消）
                server_result = transparent_server.serve() => {
                    match server_result {
                        Ok(_) => {
                            println!("✅ 透明代理服务器正常退出");
                            Ok(())
                        }
                        Err(e) => {
                            println!("❌ 透明代理服务器运行失败: {}", e);
                            Err(e)
                        }
                    }
                }
                
                // 等待 Ctrl+C 信号
                _ = signal::ctrl_c() => {
                    println!("\n🛑 收到 Ctrl+C 信号，正在关闭...");

                    // 停止服务器并清理资源
                    println!("正在停止透明代理服务器并清理系统资源...");
                    if let Err(e) = transparent_server.stop().await {
                        println!("⚠️  停止透明代理服务器时出错: {}", e);
                        println!("可能需要手动清理pfctl规则: sudo pfctl -d");
                        Err(e)
                    } else {
                        println!("✅ 透明代理系统已完全停止，规则已清理");
                        Ok(())
                    }
                }
            };
            
            // 处理结果
            match result {
                Ok(_) => {
                    println!("程序正常退出");
                }
                Err(e) => {
                    println!("程序异常退出: {}", e);
                    return Err(e);
                }
            }
        }
        Err(e) => {
            println!("❌ 透明代理服务器初始化失败: {}", e);
            println!("\n🔧 可能的解决方案:");
            println!("   1. 使用管理员权限运行: sudo cargo run --example complete_tls_proxy_demo");
            println!("   2. 检查端口 1080 是否已被占用");
            println!("   3. 确保网络配置正确");
        }
    }



    println!("\n🎉 演示程序完成！");
    Ok(())
}
