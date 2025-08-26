use anyhow::Result;
use std::process::Command;
use tokio::time::{sleep, Duration};
use tracing::error;

// 导入我们的模块
use takeover::{ProxyConfig, ProxyType, TransparentProxy, ProxyMode};

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志系统
    tracing_subscriber::fmt()
        .with_env_filter("info,takeover=debug")
        .init();

    println!("🚀 TransparentProxy API 演示");
    println!("===========================");

    // 检查运行环境
    if cfg!(not(target_os = "macos")) {
        println!("❌ 此演示仅支持 macOS 系统");
        return Ok(());
    }

    // 检查权限
    if !check_root_privileges() {
        match takeover::utils::request_privilege_escalation() {
            Ok(true) => {
                println!("✅ 权限验证成功，程序继续运行...");
                // 程序继续执行，需要管理员权限的操作会使用 execute_admin_command
            }
            Ok(false) => {
                error!("❌ 权限验证失败或被拒绝");
                println!("\n💡 或者您可以使用 sudo 运行此程序:");
                println!("💡 sudo cargo run --example transparent_proxy_api_demo");
                return Ok(());
            }
            Err(e) => {
                error!("❌ 权限验证过程出错: {}", e);
                println!("\n💡 请使用 sudo 运行此程序:");
                println!("💡 sudo cargo run --example transparent_proxy_api_demo");
                return Ok(());
            }
        }
    }

    // 配置SOCKS5代理
    let proxy_config = ProxyConfig {
        url: "socks5://127.0.0.1:1080".to_string(),
        proxy_type: ProxyType::Socks5,
        auth: None,
    };

    println!("\n📝 配置信息:");
    println!("   目标代理: {}", proxy_config.url);

    // 运行不同模式的演示
    run_all_modes_demo(proxy_config).await?;

    // 显示总结
    print_summary();

    Ok(())
}

/// 运行所有模式的演示
async fn run_all_modes_demo(proxy_config: ProxyConfig) -> Result<()> {
    println!("\n🎯 开始 TransparentProxy API 演示");
    
    // // 演示1: 全局模式（推荐）
    // demo_global_mode(proxy_config.clone()).await?;
    
    // // 短暂停顿
    // sleep(Duration::from_secs(2)).await;
    
    // // 演示2: 手动指定进程模式
    // demo_manual_mode(proxy_config.clone()).await?;
    
    // // 短暂停顿
    // sleep(Duration::from_secs(2)).await;
    
    // 演示3: 智能监控模式
    demo_smart_monitoring_mode(proxy_config).await?;

    Ok(())
}

/// 演示1: 全局模式 - 拦截指定端口的所有流量
#[allow(dead_code)]
async fn demo_global_mode(proxy_config: ProxyConfig) -> Result<()> {
    println!("\n🌐 演示1: 全局模式");
    println!("================");
    println!("拦截指定端口的所有流量，这是最常用和推荐的模式");

    // 创建全局模式的透明代理
    let mut proxy = TransparentProxy::with_mode(
        proxy_config,
        ProxyMode::Global { 
            ports: vec![80, 443, 8080] // 拦截HTTP/HTTPS流量
        }
    );

    println!("✅ 创建全局模式 TransparentProxy");

    // 启动透明代理
    match proxy.start().await {
        Ok(_) => {
            println!("🎉 全局模式启动成功！");
            
            // 测试透明代理
            test_transparent_proxy("全局模式").await;
            
            // 停止代理
            proxy.stop().await?;
            println!("✅ 全局模式已停止");
        }
        Err(e) => {
            error!("❌ 全局模式启动失败: {}", e);
        }
    }

    Ok(())
}

/// 演示2: 手动指定进程模式
#[allow(dead_code)]
async fn demo_manual_mode(proxy_config: ProxyConfig) -> Result<()> {
    println!("\n🎯 演示2: 手动指定进程模式");
    println!("==========================");
    println!("精确控制特定进程的网络流量");

    // 获取一些示例进程ID（实际使用中应该动态获取）
    let target_pids = get_example_pids();
    
    if target_pids.is_empty() {
        println!("⚠️  没有找到合适的进程进行演示，跳过此模式");
        return Ok(());
    }

    // 创建手动模式的透明代理
    let mut proxy = TransparentProxy::with_mode(
        proxy_config,
        ProxyMode::Manual { 
            target_pids: target_pids.clone()
        }
    );

    println!("✅ 创建手动模式 TransparentProxy (PIDs: {:?})", target_pids);

    // 启动透明代理
    match proxy.start().await {
        Ok(_) => {
            println!("🎉 手动模式启动成功！");
            
            // 测试透明代理
            test_transparent_proxy("手动模式").await;
            
            // 停止代理
            proxy.stop().await?;
            println!("✅ 手动模式已停止");
        }
        Err(e) => {
            error!("❌ 手动模式启动失败: {}", e);
        }
    }

    Ok(())
}

/// 演示3: 智能监控模式
#[allow(dead_code)]
async fn demo_smart_monitoring_mode(proxy_config: ProxyConfig) -> Result<()> {
    println!("\n🔍 演示3: 智能监控模式");
    println!("=====================");
    println!("根据进程名称模式自动检测和拦截");

    // 创建智能监控模式的透明代理
    let mut proxy = TransparentProxy::with_mode(
        proxy_config,
        ProxyMode::SmartMonitoring { 
            target_patterns: vec![
                "curl".to_string(),
                "node".to_string(),
                "python3".to_string(),
                "wget".to_string(),
            ]
        }
    );

    println!("✅ 创建智能监控模式 TransparentProxy");

    // 启动透明代理
    match proxy.start().await {
        Ok(_) => {
            println!("🎉 智能监控模式启动成功！");
            
            // 测试透明代理
            test_transparent_proxy("智能监控模式").await;
            
            // 停止代理
            proxy.stop().await?;
            println!("✅ 智能监控模式已停止");
        }
        Err(e) => {
            error!("❌ 智能监控模式启动失败: {}", e);
        }
    }

    Ok(())
}

/// 测试透明代理功能
async fn test_transparent_proxy(mode_name: &str) {
    println!("\n🧪 测试 {} 透明代理功能...", mode_name);
    
    // 等待代理完全启动
    sleep(Duration::from_secs(2)).await;
    
    // 测试1: HTTP请求
    println!("📡 测试1: HTTP请求到 httpbin.org");
    test_http_request().await;
    
    // 测试2: HTTPS请求
    println!("📡 测试2: HTTPS请求到 httpbin.org");
    test_https_request().await;
    
    // 测试3: 自定义端口请求
    println!("📡 测试3: 自定义端口请求");
    test_custom_port_request().await;
    
    println!("✅ {} 测试完成", mode_name);
}

/// 测试HTTP请求
async fn test_http_request() {
    println!("   → 发送 HTTP 请求...");
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
                println!("   ✅ HTTP 请求成功: {}", response.trim());
            } else {
                let error = String::from_utf8_lossy(&output.stderr);
                println!("   ❌ HTTP 请求失败: {}", error.trim());
            }
        }
        Err(e) => {
            println!("   ❌ 执行curl失败: {}", e);
        }
    }
}

/// 测试HTTPS请求
async fn test_https_request() {
    println!("   → 发送 HTTPS 请求...");
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
                println!("   ✅ HTTPS 请求成功: {}", response.trim());
            } else {
                let error = String::from_utf8_lossy(&output.stderr);
                println!("   ❌ HTTPS 请求失败: {}", error.trim());
            }
        }
        Err(e) => {
            println!("   ❌ 执行curl失败: {}", e);
        }
    }
}

/// 测试自定义端口请求
async fn test_custom_port_request() {
    println!("   → 发送自定义端口请求...");
    match Command::new("curl")
        .args(&[
            "-s", 
            "--max-time", "10",
            "--connect-timeout", "5",
            "http://httpbin.org:8080/ip"
        ])
        .output()
    {
        Ok(output) => {
            if output.status.success() {
                let response = String::from_utf8_lossy(&output.stdout);
                println!("   ✅ 自定义端口请求成功: {}", response.trim());
            } else {
                let error = String::from_utf8_lossy(&output.stderr);
                println!("   ⚠️  自定义端口请求失败 (可能服务不可用): {}", error.trim());
            }
        }
        Err(e) => {
            println!("   ❌ 执行curl失败: {}", e);
        }
    }
}

/// 获取示例进程ID
fn get_example_pids() -> Vec<u32> {
    let mut pids = Vec::new();
    
    // 尝试获取一些常见的系统进程
    let process_names = vec!["Finder", "SystemUIServer", "Dock"];
    
    for process_name in process_names {
        if let Ok(output) = Command::new("pgrep")
            .arg(process_name)
            .output()
        {
            if output.status.success() {
                let pids_str = String::from_utf8_lossy(&output.stdout);
                for line in pids_str.lines() {
                    if let Ok(pid) = line.trim().parse::<u32>() {
                        pids.push(pid);
                        break; // 只取第一个进程ID
                    }
                }
                if !pids.is_empty() {
                    break; // 找到一个就够了
                }
            }
        }
    }
    
    pids
}

/// 检查是否有管理员权限
fn check_root_privileges() -> bool {
    match Command::new("id").arg("-u").output() {
        Ok(output) => {
            let uid = String::from_utf8_lossy(&output.stdout);
            uid.trim() == "0"
        }
        Err(_) => false,
    }
}

/// 显示最终总结
fn print_summary() {
    println!("\n📋 TransparentProxy API 演示总结");
    println!("================================");
    println!();
    println!("🌐 全局模式 (ProxyMode::Global):");
    println!("   - 最常用和推荐的模式");
    println!("   - 拦截指定端口的所有流量");
    println!("   - 适用于系统级透明代理");
    println!();
    println!("🎯 手动模式 (ProxyMode::Manual):");
    println!("   - 精确控制特定进程");
    println!("   - 适用于进程级粒度控制");
    println!("   - 在macOS上会回退到全局模式");
    println!();
    println!("🔍 智能监控模式 (ProxyMode::SmartMonitoring):");
    println!("   - 根据进程名称模式自动检测");
    println!("   - 适用于动态进程管理");
    println!("   - 在macOS上会回退到全局模式");
    println!();
    println!("💡 使用建议:");
    println!("   - 日常使用推荐全局模式");
    println!("   - 可以通过端口列表精确控制拦截范围");
    println!("   - 确保SOCKS5代理服务器正在运行");
    println!("   - 需要管理员权限来修改系统网络规则");
}
