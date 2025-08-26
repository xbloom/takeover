use anyhow::Result;
use std::process::Command;
use tokio::time::{sleep, Duration};
use tracing::error;

// 导入我们的模块
use takeover::config::{Config, ProxyConfig, ProxyType};

#[cfg(target_os = "macos")]
use takeover::{TransparentServer, RoutingEngine};

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志系统
    tracing_subscriber::fmt()
        .with_env_filter("info,takeover=debug")
        .init();

    println!("🍎 macOS 透明代理演示");
    println!("===================");

    // 检查运行环境
    if cfg!(not(target_os = "macos")) {
        println!("❌ 此演示仅支持 macOS 系统");
        return Ok(());
    }

    // 检查当前权限状态
    if check_root_privileges() {
        println!("✅ 已具备管理员权限");
    } else {
        println!("ℹ️  当前以普通用户身份运行");
        println!("💡 系统会在需要时自动申请管理员权限（创建专用用户等操作）");
        println!("🔐 请准备在弹出的对话框中确认权限申请");
    }

    // 检查系统依赖
    check_system_dependencies().await?;

    // 配置SOCKS5代理
    let proxy_config = ProxyConfig {
        url: "socks5://127.0.0.1:1080".to_string(),
        proxy_type: ProxyType::Socks5,
        auth: None,
    };

    println!("\n📝 配置信息:");
    println!("   目标代理: {}", proxy_config.url);
    println!("   认证: 无");

    // 运行透明代理演示
    run_transparent_proxy_demo(proxy_config).await?;

    Ok(())
}

#[cfg(target_os = "macos")]
async fn run_transparent_proxy_demo(proxy_config: ProxyConfig) -> Result<()> {
    // 加载或创建配置
    let config = Config::load_or_create("config.json").unwrap_or_else(|_| Config::default());
    
    // 创建路由引擎
    let routing_engine = RoutingEngine::new(&config);
    
    // 显示路由规则统计
    let stats = routing_engine.get_stats();
    println!("\n📋 路由规则已加载:");
    println!("   - 进程: {}", stats.total_processes);
    println!("   - 进程路径: {}", stats.total_process_paths);
    println!("   - 域名: {}", stats.total_domains);
    println!("   - URL模式: {}", stats.total_url_patterns);
    println!("   - 端口: {}", stats.total_ports);
    println!("   - IP范围: {}", stats.total_ip_ranges);
    println!("   - URL重写: {}", stats.total_url_rewrites);

    // 创建透明代理服务器
    // 用户管理现在是自动的，系统会创建专用的代理用户来避免循环
    let mut transparent_server = TransparentServer::new(proxy_config, routing_engine);
    
    // 定义要拦截的端口
    let intercepted_ports = [80, 443, 8080];

    // 启动透明代理系统
    println!("\n🚀 启动透明代理系统...");
    println!("🔌 拦截端口: {:?}", intercepted_ports);
    
    // 设置信号处理器 - 在单独的任务中处理
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.expect("Failed to listen for Ctrl+C");
        println!("\n🛑 收到Ctrl+C信号，准备停止...");
        let _ = tx.send(());
    });
    
    // 在单独的任务中启动服务器
    let server_handle = tokio::spawn(async move {
        match transparent_server.start(&intercepted_ports).await {
            Ok(()) => {
                println!("✅ 透明代理服务器正常停止");
                Ok(())
            }
            Err(e) => {
                error!("❌ 透明代理服务器错误: {}", e);
                Err(e)
            }
        }
    });
    
    println!("✅ 透明代理系统已启动！");
    println!("\n💡 系统说明:");
    println!("   🌐 全局拦截HTTP/HTTPS流量 (端口 80,443,8080)");
    println!("   🎯 智能路由决策 - 根据域名、URL模式匹配");
    println!("   🔀 匹配的流量通过SOCKS5代理转发");
    println!("   🔗 不匹配的流量直接连接");
    println!("   🛡️ 循环防护 - 自动创建专用用户，使用用户过滤器避免循环");
    println!("   👤 专用用户: 系统会自动创建 '_takeover_proxy' 用户用于socket绑定");
    println!("   🔐 权限管理: 首次创建用户时会弹出权限确认对话框");

    // 提示用户准备测试
    println!("\n🧪 准备进行透明代理测试...");
    println!("⚠️  注意: HTTP/HTTPS流量现在会根据规则智能路由");
    
    // 自动运行测试
    println!("\n开始自动测试...");
    run_transparent_tests().await?;

    // 等待用户决定何时停止或收到信号
    println!("\n🏁 测试完成！");
    println!("透明代理仍在运行，您可以:");
    println!("  • 在其他终端窗口测试任何网络命令");
    println!("  • curl https://httpbin.org/ip");
    println!("  • curl https://api.openai.com/v1/models");
    println!("  • node -e \"require('https').get('https://github.com/', res => console.log('GitHub:', res.statusCode))\"");
    println!("  • 按 Enter 停止透明代理系统，或使用 Ctrl+C");
    
    // 等待用户输入或信号
    tokio::select! {
        _ = async {
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).unwrap();
        } => {
            println!("收到用户停止信号...");
        }
        _ = &mut rx => {
            println!("收到Ctrl+C信号...");
        }
    }

    // 停止服务器
    println!("\n🛑 停止透明代理系统...");
    server_handle.abort();
    println!("✅ 透明代理系统已停止");

    Ok(())
}

#[cfg(not(target_os = "macos"))]
async fn run_transparent_proxy_demo(_proxy_config: ProxyConfig) -> Result<()> {
    println!("❌ 透明代理功能仅在 macOS 上可用");
    Ok(())
}

/// 检查是否有root权限
fn check_root_privileges() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout).trim() == "0"
        })
        .unwrap_or(false)
}

/// 检查系统依赖
async fn check_system_dependencies() -> Result<()> {
    println!("🔍 检查系统依赖...");

    // 检查pfctl
    match Command::new("pfctl").arg("-h").output() {
        Ok(_) => println!("  ✅ pfctl 可用"),
        Err(_) => {
            error!("  ❌ pfctl 不可用");
            return Err(anyhow::anyhow!("pfctl is required but not available"));
        }
    }

    // 检查curl
    match Command::new("curl").arg("--version").output() {
        Ok(_) => println!("  ✅ curl 可用"),
        Err(_) => println!("  ⚠️  curl 不可用，将跳过curl测试"),
    }

    // 检查node
    match Command::new("node").arg("--version").output() {
        Ok(_) => println!("  ✅ Node.js 可用"),
        Err(_) => println!("  ⚠️  Node.js 不可用，将跳过Node.js测试"),
    }

    Ok(())
}

/// 运行透明代理测试
async fn run_transparent_tests() -> Result<()> {
    println!("🧪 开始透明代理测试...");

    // 等待一会儿确保系统稳定
    sleep(Duration::from_secs(2)).await;

    // 测试1: curl HTTP
    println!("\n📝 测试1: curl HTTP请求");
    test_curl_http().await;

    // 测试2: curl HTTPS  
    println!("\n📝 测试2: curl HTTPS请求");
    test_curl_https().await;

    // 测试3: Node.js HTTP请求
    println!("\n📝 测试3: Node.js HTTP请求");
    test_nodejs_http().await;

    Ok(())
}

async fn test_curl_http() {
    match Command::new("curl")
        .args(&[
            "-s",
            "--max-time", "10",
            "--connect-timeout", "5",
            "http://httpbin.org/ip"
        ])
        .output()
    {
        Ok(output) => {
            if output.status.success() {
                let response = String::from_utf8_lossy(&output.stdout);
                println!("  ✅ 成功！响应: {}", response.trim());
            } else {
                let error = String::from_utf8_lossy(&output.stderr);
                println!("  ⚠️  失败: {}", error);
            }
        }
        Err(e) => {
            println!("  ❌ 执行失败: {}", e);
        }
    }
}

async fn test_curl_https() {
    match Command::new("curl")
        .args(&[
            "-s",
            "--max-time", "10", 
            "--connect-timeout", "5",
            "https://httpbin.org/ip"
        ])
        .output()
    {
        Ok(output) => {
            if output.status.success() {
                let response = String::from_utf8_lossy(&output.stdout);
                println!("  ✅ 成功！响应: {}", response.trim());
            } else {
                let error = String::from_utf8_lossy(&output.stderr);
                println!("  ⚠️  失败: {}", error);
            }
        }
        Err(e) => {
            println!("  ❌ 执行失败: {}", e);
        }
    }
}

async fn test_nodejs_http() {
    let node_code = r#"
const http = require('http');
const req = http.get('http://httpbin.org/ip', (res) => {
    let data = '';
    res.on('data', chunk => data += chunk);
    res.on('end', () => {
        try {
            const result = JSON.parse(data);
            console.log('Node.js HTTP result:', result);
        } catch (e) {
            console.log('Node.js parsing error:', e.message);
        }
    });
});

req.on('error', (err) => {
    console.log('Node.js HTTP error:', err.message);
});

req.setTimeout(10000, () => {
    console.log('Node.js HTTP timeout');
    req.destroy();
});
"#;

    match Command::new("node")
        .args(&["-e", node_code])
        .output()
    {
        Ok(output) => {
            if output.status.success() {
                let response = String::from_utf8_lossy(&output.stdout);
                println!("  ✅ 成功！{}", response.trim());
            } else {
                let error = String::from_utf8_lossy(&output.stderr);
                println!("  ⚠️  失败: {}", error);
            }
        }
        Err(e) => {
            println!("  ❌ Node.js不可用: {}", e);
        }
    }
}
