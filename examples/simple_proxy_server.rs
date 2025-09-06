//! 测试 TransparentProxyManager 功能
//! 
//! 这个示例程序演示如何使用新的分层设计来设置透明代理。
//! 运行前请确保 SOCKS5 代理已经在 localhost:1080 运行。

use anyhow::Result;
use takeover::{TransparentServer, RoutingEngine, ProxyConfig, ProxyType, Config};
use tracing::{info, error, warn};
use tokio::signal;

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志 - 从环境变量 RUST_LOG 读取配置
    // 设置命令： RUST_LOG=debug cargo run --example test_proxy_manager
    // 或者： RUST_LOG=test_proxy_manager=info,takeover=debug cargo run --example test_proxy_manager
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| {
                    // 默认配置：显示 test_proxy_manager 和 takeover 的日志
                    tracing_subscriber::EnvFilter::new("test_proxy_manager=info,takeover=info")
                })
        )
        .init();
    
    // 配置
    let socks5_proxy_port = 7897;           // 外部SOCKS5代理端口
    let intercepted_ports = vec![80];  // 要拦截的端口
        
    // 配置SOCKS5代理
    let proxy_config = ProxyConfig {
        url: format!("socks5://127.0.0.1:{}", socks5_proxy_port),
        proxy_type: ProxyType::Socks5,
        auth: None,
    };
    
    // 创建路由引擎（简单配置：代理所有流量）
    let routing_config = Config::default();
    let routing_engine = RoutingEngine::new(&routing_config);
    
    // 创建透明服务器
    let mut transparent_server = TransparentServer::new(proxy_config, routing_engine);
    
    // 1. 先初始化服务器（这会立即返回）
    info!("🚀 启动透明代理服务器...");
    if let Err(e) = transparent_server.initialize(&intercepted_ports).await {
        error!("❌ 透明代理服务器初始化失败: {}", e);
        error!("提示：");
        error!("1. 确保SOCKS5代理在端口 {} 正在运行", socks5_proxy_port);
        error!("2. 确保运行权限足够（可能需要sudo）");
        error!("3. 确保端口没有被占用");
        return Err(e);
    }
    
    // 2. 获取服务器信息并显示
    let local_port = transparent_server.get_local_port()
        .ok_or_else(|| anyhow::anyhow!("Failed to get local port"))?;
        
    info!("✅ 透明代理服务器启动成功！");
    info!("");
    info!("🎉 完整的透明代理系统现在已激活！");
    info!("");
    info!("系统信息:");
    info!("  透明代理监听端口: {}", local_port);
    info!("  外部SOCKS5代理: localhost:{}", socks5_proxy_port);
    info!("  拦截端口: {:?}", intercepted_ports);
    info!("");
    info!("工作流程:");
    info!("1. HTTP/HTTPS 请求 → pfctl 拦截");
    info!("2. pfctl → 重定向到透明代理服务器 (port {})", local_port);
    info!("3. 透明代理服务器 → 解析请求");
    info!("4. 透明代理服务器 → 转发给SOCKS5代理 (port {})", socks5_proxy_port);
    info!("5. SOCKS5代理 → 发送到目标服务器");
    info!("");
    info!("测试命令:");
    info!("  curl http://httpbin.org/ip");
    info!("  curl https://httpbin.org/ip");
    info!("");
    info!("按 Ctrl+C 停止透明代理...");
    
    // 3. 使用 tokio::select! 来并发运行服务器和等待信号
    let result = tokio::select! {
        // 运行透明代理服务器（这会一直运行直到出错或被取消）
        server_result = transparent_server.serve() => {
            match server_result {
                Ok(_) => {
                    info!("透明代理服务器正常退出");
                    Ok(())
                }
                Err(e) => {
                    error!("❌ 透明代理服务器运行失败: {}", e);
                    Err(e)
                }
            }
        }
        
        // 等待 Ctrl+C 信号
        _ = signal::ctrl_c() => {
            info!("🛑 收到 Ctrl+C 信号，正在关闭...");

            // 停止服务器
            info!("正在停止透明代理服务器...");
            if let Err(e) = transparent_server.stop().await {
                error!("停止透明代理服务器时出错: {}", e);
                warn!("可能需要手动清理pfctl规则");
                warn!("可以运行: sudo pfctl -d 来禁用pfctl");
                Err(e)
            } else {
                info!("✅ 透明代理系统已完全停止，规则已清理");
                Ok(())
            }
        }
    };
    
    // 处理结果
    match result {
        Ok(_) => {
            info!("程序正常退出");
        }
        Err(e) => {
            error!("程序异常退出: {}", e);
            return Err(e);
        }
    }
    
    Ok(())
}
