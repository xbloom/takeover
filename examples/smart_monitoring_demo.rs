use anyhow::Result;
use std::process::Command;
use tokio::time::Duration;
use tokio::signal;
use tracing::{info, error};

// 导入我们的模块
use takeover::{ProxyConfig, ProxyType, TransparentProxy, ProxyMode};

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志系统
    tracing_subscriber::fmt()
        .with_env_filter("info,takeover=debug")
        .init();

    println!("🔍 Smart Monitoring 透明代理演示");
    println!("================================");
    println!("此demo将持续运行，监控指定进程模式的网络流量");
    println!("按 Ctrl+C 退出");

    // 检查运行环境
    if cfg!(not(target_os = "macos")) {
        println!("❌ 此演示仅支持 macOS 系统");
        return Ok(());
    }

    // 检查权限
    if !check_root_privileges() {
        error!("❌ 此程序需要管理员权限来修改pfctl规则");
        println!("\n💡 请使用 sudo 运行此程序:");
        println!("💡 sudo cargo run --example smart_monitoring_demo");
        return Ok(());
    }

    // 配置SOCKS5代理
    let proxy_config = ProxyConfig {
        url: "socks5://127.0.0.1:1080".to_string(),
        proxy_type: ProxyType::Socks5,
        auth: None,
    };

    println!("\n📝 配置信息:");
    println!("   目标代理: {}", proxy_config.url);
    println!("   模式: Smart Monitoring");

    // 定义要监控的进程模式
    let target_patterns = vec![
        "curl".to_string(),
        "node".to_string(),
        "python3".to_string(),
        "python".to_string(),
        "wget".to_string(),
        "npm".to_string(),
        "git".to_string(),
        "brew".to_string(),
        "pip".to_string(),
        "npm".to_string(),
        "yarn".to_string(),
        "go".to_string(),
        "rust".to_string(),
        "cargo".to_string(),
    ];

    println!("\n🎯 监控的进程模式:");
    for pattern in &target_patterns {
        println!("   - {}", pattern);
    }

    // 创建Smart Monitoring模式的透明代理
    let mut proxy = TransparentProxy::with_mode(
        proxy_config,
        ProxyMode::SmartMonitoring { 
            target_patterns: target_patterns.clone()
        }
    );

    println!("\n🚀 启动 Smart Monitoring 透明代理...");

    // 启动透明代理
    match proxy.start().await {
        Ok(_) => {
            info!("✅ Smart Monitoring 透明代理启动成功！");
            
            // 显示使用说明
            print_usage_instructions(&target_patterns);
            
            // 持续运行直到收到退出信号
            let start_time = std::time::Instant::now();
            run_monitoring_loop(&target_patterns).await?;
            let uptime = start_time.elapsed();
            
            // 优雅停止
            println!("\n🛑 正在停止透明代理...");
            proxy.stop().await?;
            println!("✅ 透明代理已停止");
            
            // 显示最终统计
            print_final_stats(uptime);
        }
        Err(e) => {
            error!("❌ Smart Monitoring 启动失败: {}", e);
            return Err(e);
        }
    }

    Ok(())
}

/// 显示使用说明
fn print_usage_instructions(target_patterns: &[String]) {
    println!("\n📋 使用说明");
    println!("==========");
    println!("Smart Monitoring 透明代理现在正在运行...");
    println!();
    println!("🔍 监控模式:");
    println!("   - 自动检测匹配进程名称模式的新进程");
    println!("   - 透明代理这些进程的网络流量");
    println!("   - 实时监控和日志记录");
    println!();
    println!("🎯 当前监控的进程模式:");
    for pattern in target_patterns {
        println!("   ✓ {}", pattern);
    }
    println!();
    println!("🧪 测试命令示例:");
    println!("   curl https://httpbin.org/ip");
    println!("   node -e \"require('https').get('https://httpbin.org/ip', res => res.pipe(process.stdout))\"");
    println!("   python3 -c \"import urllib.request; print(urllib.request.urlopen('https://httpbin.org/ip').read())\"");
    println!("   wget -qO- https://httpbin.org/ip");
    println!("   git clone https://github.com/user/repo.git");
    println!("   npm install package-name");
    println!();
    println!("📊 监控状态:");
    println!("   - 透明代理会自动拦截匹配进程的网络请求");
    println!("   - 查看日志了解详细的连接信息");
    println!("   - 按 Ctrl+C 优雅退出");
    println!();
}

/// 主监控循环
async fn run_monitoring_loop(target_patterns: &[String]) -> Result<()> {
    println!("🔄 进入监控循环...");
    
    // 设置Ctrl+C信号处理
    let ctrl_c = signal::ctrl_c();
    tokio::pin!(ctrl_c);
    
    // 定期状态报告的计时器
    let mut status_interval = tokio::time::interval(Duration::from_secs(30));
    let mut activity_check_interval = tokio::time::interval(Duration::from_secs(10));
    
    let start_time = std::time::Instant::now();
    let mut last_activity_report = std::time::Instant::now();
    
    loop {
        tokio::select! {
            // 处理Ctrl+C信号
            _ = &mut ctrl_c => {
                println!("\n📡 收到退出信号 (Ctrl+C)");
                break;
            }
            
            // 定期状态报告
            _ = status_interval.tick() => {
                let uptime = start_time.elapsed();
                info!("📊 监控状态: 运行时间 {:?}", uptime);
                info!("🎯 持续监控进程模式: {:?}", target_patterns);
                
                // 检查活跃进程
                check_active_processes(target_patterns).await;
            }
            
            // 活跃度检查
            _ = activity_check_interval.tick() => {
                if last_activity_report.elapsed() > Duration::from_secs(60) {
                    info!("💡 提示: 尝试运行一些网络命令来测试透明代理功能");
                    info!("   例如: curl https://httpbin.org/ip");
                    last_activity_report = std::time::Instant::now();
                }
            }
        }
    }
    
    Ok(())
}

/// 检查当前活跃的目标进程
async fn check_active_processes(target_patterns: &[String]) {
    info!("🔍 检查活跃的目标进程...");
    
    let mut found_processes = Vec::new();
    
    for pattern in target_patterns {
        if let Ok(output) = Command::new("pgrep")
            .arg(pattern)
            .output()
        {
            if output.status.success() {
                let pids_str = String::from_utf8_lossy(&output.stdout);
                let pids: Vec<&str> = pids_str.lines().filter(|line| !line.trim().is_empty()).collect();
                
                if !pids.is_empty() {
                    found_processes.push((pattern.clone(), pids.len()));
                    info!("   ✓ 发现 {} 个 '{}' 进程", pids.len(), pattern);
                }
            }
        }
    }
    
    if found_processes.is_empty() {
        info!("   📭 当前没有活跃的目标进程");
    } else {
        info!("   📊 总计发现 {} 种类型的目标进程", found_processes.len());
    }
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

/// 显示最终统计信息
fn print_final_stats(uptime: Duration) {
    println!("\n📈 最终统计");
    println!("============");
    println!("🕒 总运行时间: {:?}", uptime);
    println!("🔍 监控模式: Smart Monitoring");
    println!("💡 谢谢使用 Takeover 透明代理！");
}
