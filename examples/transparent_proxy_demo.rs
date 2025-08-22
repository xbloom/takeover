use anyhow::Result;
use std::process::Command;
use tokio::time::{sleep, Duration};
use tracing::error;
use clap::{Arg, Command as ClapCommand, ArgMatches};

// 导入我们的模块
use takeover::config::{ProxyConfig, ProxyType};

#[cfg(target_os = "macos")]
use takeover::transparent_proxy::{TransparentProxy, ProxyMode};

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

    // 检查权限
    if !check_root_privileges() {
        error!("❌ 此程序需要管理员权限来修改pfctl规则");
        println!("\n💡 请使用 sudo 运行此程序:");
        println!("💡 sudo cargo run --example transparent_proxy_demo");
        return Ok(());
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
    // 创建透明代理实例
    let mut transparent_proxy = TransparentProxy::new(proxy_config);

    // 启动透明代理系统
    println!("\n🚀 启动透明代理系统...");
    match transparent_proxy.start().await {
        Ok(()) => {
            if let Some(port) = transparent_proxy.get_local_port() {
                println!("✅ 透明代理系统已启动！本地代理端口: {}", port);
            } else {
                println!("✅ 透明代理系统已启动！");
            }
        }
        Err(e) => {
            error!("❌ 启动透明代理系统失败: {}", e);
            println!("\n💡 可能的解决方案:");
            println!("   1. 确保有一个SOCKS5代理运行在127.0.0.1:1080");
            println!("   2. 尝试启动SSH隧道: ssh -D 1080 user@remote-server");
            println!("   3. 或使用其他SOCKS5代理工具");
            return Ok(());
        }
    }

    // 提示用户准备测试
    println!("\n🧪 准备进行透明代理测试...");
    println!("⚠️  注意: 所有HTTP/HTTPS流量现在都将通过代理");
    
    // 自动运行测试
    println!("\n开始自动测试...");
    run_transparent_tests().await?;

    // 等待用户决定何时停止
    println!("\n🏁 测试完成！");
    println!("透明代理仍在运行，您可以:");
    println!("  • 在其他终端窗口测试任何网络命令");
    println!("  • curl https://httpbin.org/ip");
    println!("  • node -e \"console.log('test')\"");
    println!("  • 按 Enter 停止透明代理系统");
    
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    // 停止透明代理系统
    println!("\n🛑 停止透明代理系统...");
    transparent_proxy.stop().await?;
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
