//! 测试 TransparentProxyManager 功能
//! 
//! 这个示例程序演示如何使用新的分层设计来设置透明代理。
//! 运行前请确保 SOCKS5 代理已经在 localhost:1080 运行。

use anyhow::Result;
use takeover::transparent_proxy_manager::TransparentProxyManager;
use takeover::{TransparentServer, RoutingEngine, ProxyConfig, ProxyType, Config};
use tracing::{info, error};

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志 - 从环境变量 RUST_LOG 读取配置
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("takeover=info"))
        )
        .init();

    info!("=== TransparentProxyManager + TransparentServer 完整测试 ===");
    
    // 配置
    let socks5_proxy_port = 1080;           // 外部SOCKS5代理端口
    let transparent_server_port = 1081;     // 我们的透明代理服务器端口
    let intercepted_ports = vec![80, 443];  // 要拦截的端口
    
    info!("配置信息:");
    info!("  外部SOCKS5代理: localhost:{}", socks5_proxy_port);
    info!("  透明代理服务器: localhost:{}", transparent_server_port);
    info!("  拦截端口: {:?}", intercepted_ports);
    
    // 1. 创建并启动 pfctl 规则管理器
    let mut manager = TransparentProxyManager::new();
    
    info!("🔧 第一步: 设置 pfctl 规则...");
    match manager.start(transparent_server_port, &intercepted_ports, Some("nobody")).await {
        Ok(_) => {
            info!("✅ pfctl 规则设置成功");
        }
        Err(e) => {
            error!("❌ pfctl 规则设置失败: {}", e);
            error!("提示：");
            error!("1. 确保使用 sudo 运行此程序");
            error!("2. 确保 pfctl 可用（macOS）");
            return Err(e);
        }
    }
    
    // 2. 创建并启动透明代理服务器
    info!("🚀 第二步: 启动透明代理服务器...");
    
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
    
    // 启动透明服务器，让它监听我们在pfctl规则中配置的端口
    match transparent_server.start(&intercepted_ports).await {
        Ok(_) => {
            info!("✅ 透明代理服务器启动成功！");
            
            // 显示当前pfctl规则
            match manager.get_current_rules().await {
                Ok(rules) => {
                    info!("当前 pfctl 规则：");
                    for line in rules.lines() {
                        if !line.trim().is_empty() && !line.starts_with('#') {
                            info!("  {}", line);
                        }
                    }
                }
                Err(e) => {
                    error!("获取规则失败: {}", e);
                }
            }
            
            info!("");
            info!("🎉 完整的透明代理系统现在已激活！");
            info!("");
            info!("工作流程:");
            info!("1. HTTP/HTTPS 请求 → pfctl 拦截");
            info!("2. pfctl → 重定向到透明代理服务器 (port {})", transparent_server_port);
            info!("3. 透明代理服务器 → 解析请求");
            info!("4. 透明代理服务器 → 转发给SOCKS5代理 (port {})", socks5_proxy_port);
            info!("5. SOCKS5代理 → 发送到目标服务器");
            info!("");
            info!("测试命令:");
            info!("  curl http://httpbin.org/ip");
            info!("  curl https://httpbin.org/ip");
            info!("");
            info!("按 Ctrl+C 停止透明代理...");
            
            // 等待用户中断
            tokio::signal::ctrl_c().await?;
            
            info!("\n🛑 收到中断信号，正在清理...");
            
            // 3. 停止透明代理服务器
            info!("停止透明代理服务器...");
            transparent_server.stop().await?;
            
            // 4. 停止pfctl规则管理器
            info!("清理 pfctl 规则...");
            manager.stop().await?;
            
            info!("✅ 透明代理系统已完全停止，规则已清理");
        }
        Err(e) => {
            error!("❌ 透明代理服务器启动失败: {}", e);
            
            // 清理pfctl规则
            info!("清理 pfctl 规则...");
            manager.stop().await?;
            
            error!("提示：");
            error!("1. 确保端口 {} 没有被占用", transparent_server_port);
            error!("2. 确保SOCKS5代理在端口 {} 正在运行", socks5_proxy_port);
            return Err(e);
        }
    }
    
    Ok(())
}
