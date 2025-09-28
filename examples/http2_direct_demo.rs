//! HTTP/2 → Direct转发简化Demo
//! 
//! 这个Demo展示了简化的透明代理流程：
//! 1. pfctl透明拦截HTTPS连接
//! 2. TLS拦截和解密 
//! 3. HTTP/2协议解析
//! 4. 转发中间件执行转发
//! 5. Direct适配器直连目标服务器
//! 
//! 使用方法：
//! cargo run --example http2_direct_demo
//! 然后访问 https://httpbin.org/get

use std::sync::Arc;
use anyhow::Result;
use takeover::TlsConfig;
use tracing::{info, level_filters::LevelFilter, debug, warn, error};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use tokio::signal;

// 导入核心组件
use takeover::core::{AppCore, AppConfig, GeneralConfig, InboundConfig, OutboundConfig, ScriptConfig};

// 导入适配器
use takeover::adapters::{PfInboundAdapter, CompositeOutboundAdapter};
use takeover::core::ports::{ControlPort, ProxyConfig};
use takeover::core::model::{Rule, RulePattern, RuleAction};

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    init_logging()?;
    
    info!("🚀 启动HTTP/2 → Direct转发Demo");
    
    // 创建简化配置
    let config = create_demo_config();
    
    // 构建应用程序
    let app = build_demo_app(config).await?;
    
    info!("✅ Demo应用构建完成");
    info!("📋 配置说明:");
    info!("   - 监听端口: 配置为0时自动分配随机端口，非0时使用指定端口");
    info!("   - 拦截端口: [443, 8443, 9443]");
    info!("   - 架构: HTTP/2 Stream → 转发中间件 → Direct适配器");
    info!("   - 用户切换: 启动时切换到_takeover_proxy用户防循环");
    info!("   - 测试命令: curl https://httpbin.org/ip -v (透明拦截)");
    info!("   - 测试命令: curl --connect-to httpbin.org:443:127.0.0.1:8080 https://httpbin.org/ip -v");

    // 启动应用程序
    info!("🎯 启动透明代理服务...");
    info!("按 Ctrl+C 停止透明代理...");
    
    // 使用 tokio::select! 来并发运行服务器和等待信号
    let result = tokio::select! {
        // 运行透明代理服务器（这会一直运行直到出错或被取消）
        app_result = app.run() => {
            match app_result {
                Ok(_) => {
                    info!("🎉 透明代理服务器正常退出");
                    Ok(())
                }
                Err(e) => {
                    error!("❌ 透明代理服务器运行失败: {}", e);
                    Err(e.into())
                }
            }
        }
        
        // 等待 Ctrl+C 信号
        _ = signal::ctrl_c() => {
            info!("🛑 收到 Ctrl+C 信号，正在关闭...");

            // 请求停止服务器
            info!("正在停止透明代理服务器...");
            app.request_stop().await;
            
            // 等待应用程序优雅关闭
            info!("正在等待优雅关闭...");
            if let Err(e) = app.shutdown().await {
                error!("关闭透明代理服务器时出错: {}", e);
                warn!("可能需要手动清理pfctl规则");
                warn!("可以运行: sudo pfctl -d 来禁用pfctl");
                Err(e.into())
            } else {
                info!("✅ 透明代理系统已完全停止，pfctl规则已清理");
                Ok(())
            }
        }
    };
    
    // 处理结果
    match result {
        Ok(_) => {
            info!("🎉 程序正常退出");
        }
        Err(e) => {
            error!("💥 程序异常退出: {}", e);
            return Err(e);
        }
    }
    
    Ok(())
}

/// 初始化日志系统
fn init_logging() -> Result<()> {
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    
    tracing_subscriber::registry()
        .with(fmt::layer().with_target(true).with_thread_ids(false))
        .with(env_filter)
        .init();
    
    Ok(())
}

/// 创建Demo配置
fn create_demo_config() -> AppConfig {
    AppConfig {
        app_name: "HTTP/2 Direct Demo".to_string(),
        version: "1.0.0".to_string(),
        
        // 通用配置
        general: GeneralConfig {
            max_connections: 100,
            connection_timeout: std::time::Duration::from_secs(30),
            log_level: "debug".to_string(),
        },
        
        // 入站配置 - pfctl透明代理
        inbound: InboundConfig {
            listen_address: "127.0.0.1".to_string(),
            listen_port: 0, // 🎯 端口0 = 自动分配随机端口，非0 = 使用指定端口
            max_connections: 100,
            buffer_size: 8192,
            timeout: std::time::Duration::from_secs(60),
            intercepted_ports: vec![80,443], // 拦截HTTPS端口
            bypass_user: Some("_takeover_proxy".to_string()), // 使用正确的用户名
        },
        
        // 出站配置 - 直连  
        outbound: OutboundConfig {
            timeout: std::time::Duration::from_secs(5),
            read_timeout: std::time::Duration::from_secs(15),
            write_timeout: std::time::Duration::from_secs(5),
            retry_count: 1,
            retry_delay: std::time::Duration::from_millis(500),
        },

        tls: TlsConfig {
            enabled: true,
            interception_mode: "selective".to_string(),
            intercept_domains: vec!["*.org".to_string(), "*.com".to_string()],
            cert_store_path: None,
            cert_cache_duration: 86400, // 24小时
            max_concurrent_handshakes: 50,
            auto_install_ca: true,
        },
        
        // 脚本配置 - 禁用
        script: ScriptConfig {
            enabled: false,
            script_dir: "./scripts".into(),
            timeout: std::time::Duration::from_secs(30),
            max_concurrent: 10,
        },
    } 
}

/// 构建Demo应用程序
async fn build_demo_app(config: AppConfig) -> Result<AppCore> {
    info!("🏗️ 构建HTTP/2 Direct Demo应用程序");
    
    let config = Arc::new(config);
    
    // 1. 创建适配器
    let inbound_adapter = create_inbound_adapter(&config).await?;
    let outbound_adapter = create_outbound_adapter(&config).await?;
    let control_adapter = create_control_adapter().await?;
    
    // 2. 创建简单的事件端口
    let event_port = Arc::new(SimpleEventPort::new());
    
    // 3. 组装应用核心
    let mut app_core = AppCore::new(
        (*config).clone(),
        inbound_adapter,
        outbound_adapter,
        control_adapter,
        event_port,
    );
    
    // 4. 配置分流规则
    configure_routing_rules(&mut app_core).await?;
    
    info!("✅ Demo应用程序构建完成");
    Ok(app_core)
}

/// 配置路由分流规则
async fn configure_routing_rules(app_core: &mut AppCore) -> Result<()> {
    info!("🎯 配置路由分流规则");
    
    let rules = vec![
        // Rule 1: Google域名走SOCKS5代理
        Rule::new(
            "google_proxy",
            "Google域名代理规则",
            RulePattern::HostMatches("*.google.com".to_string()),
            RuleAction::ProxyByName("default".to_string()), // 使用代理名称标识
            10, // 高优先级
        ),
        
        // Rule 2: curl程序走SOCKS5代理
        Rule::new(
            "curl_proxy",
            "curl程序代理规则", 
            RulePattern::ProcessMatches("*curl*".to_string()),
            RuleAction::ProxyByName("default".to_string()), // 使用代理名称标识
            20, // 中优先级
        ),
        
        // Rule 3: 其他所有流量直连
        Rule::new(
            "default_direct",
            "默认直连规则",
            RulePattern::HostMatches("*".to_string()),
            RuleAction::Direct,
            100, // 低优先级（兜底规则）
        ),
    ];
    
    // 通过ControlPort应用规则
    app_core.control_port().apply_rules(rules).await
        .map_err(|e| anyhow::anyhow!("Failed to apply rules: {}", e))?;
    
    info!("✅ 规则配置完成:");
    info!("   1. Google域名 → SOCKS5代理 (代理名称: default)");
    info!("   2. curl程序 → SOCKS5代理 (代理名称: default)");
    info!("   3. 其他流量 → 直连");
    info!("   - 使用代理名称标识设计，规则中只存储代理名称");
    info!("   - CompositeOutboundAdapter根据代理名称查找具体配置");
    
    Ok(())
}

/// 创建入站适配器 (pfctl)
async fn create_inbound_adapter(
    _config: &AppConfig
) -> Result<Arc<dyn takeover::core::ports::InboundPort>> {
    info!("📥 创建pfctl入站适配器");
    
    let adapter = PfInboundAdapter::with_anchor_name("takeover_demo");
    
    info!("✅ pfctl入站适配器创建完成");
    Ok(Arc::new(adapter))
}

/// 创建出站适配器 (Composite - Direct + SOCKS5)
async fn create_outbound_adapter(
    _config: &AppConfig
) -> Result<Arc<dyn takeover::core::ports::OutboundPort>> {
    info!("📤 创建复合出站适配器 (Direct + SOCKS5)");
    
    // 配置SOCKS5代理
    let socks5_config = ProxyConfig {
        url: "socks5://127.0.0.1:7897".to_string(),
        proxy_type: takeover::config::ProxyType::Socks5,
        auth: None,
    };
    
    let adapter = CompositeOutboundAdapter::with_socks5_config(socks5_config);
    
    info!("✅ 复合出站适配器创建完成");
    info!("   - Direct适配器：直连目标服务器");  
    info!("   - SOCKS5适配器：127.0.0.1:7897");
    info!("   - 路由规则：根据RuleMatchingMiddleware决策选择");
    Ok(Arc::new(adapter))
}

/// 创建控制适配器
async fn create_control_adapter() -> Result<Arc<dyn takeover::core::ports::ControlPort>> {
    info!("🎛️ 创建Mock控制适配器");
    
    let adapter = MockControlAdapter::new();
    
    info!("✅ Mock控制适配器创建完成");
    Ok(Arc::new(adapter))
}

/// 简单的事件端口实现
struct SimpleEventPort;

impl SimpleEventPort {
    fn new() -> Self {
        Self
    }
}

use async_trait::async_trait;
use std::pin::Pin;
use futures::Stream;

#[async_trait]
impl takeover::core::ports::EventPort for SimpleEventPort {
    async fn publish(&self, event: takeover::core::ports::DomainEvent) -> Result<(), takeover::core::ports::AppError> {
        // 简单的日志输出
        match event {
            takeover::core::ports::DomainEvent::ConnectionNew { id: _, addr: _, .. } => {
                // info!("📡 新连接: {} from {}", id, addr);
            }
            takeover::core::ports::DomainEvent::ConnectionClosed { id: _, duration: _, bytes_transferred: _ } => {
                // info!("📡 连接关闭: {} (持续: {:.2}s, 传输: {}字节)", id, duration.as_secs_f64(), bytes_transferred);
            }
            takeover::core::ports::DomainEvent::RequestProcessed { conn_id, request_id, duration } => {
                debug!("📡 请求处理: 连接{} 请求{} 耗时{:.2}ms", conn_id, request_id, duration.as_millis());
            }
            takeover::core::ports::DomainEvent::ScriptExecuted { .. } => {
                debug!("📡 脚本执行事件");
            }
            takeover::core::ports::DomainEvent::UserIntervention { .. } => {
                debug!("📡 用户干预事件");
            }
            takeover::core::ports::DomainEvent::Error { .. } => {
                info!("📡 错误事件");
            }
        }
        Ok(())
    }
    
    async fn subscribe(&self) -> Result<Pin<Box<dyn Stream<Item = takeover::core::ports::DomainEvent> + Send>>, takeover::core::ports::AppError> {
        // 返回空流
        Ok(Box::pin(futures::stream::empty()))
    }
}

/// Mock控制适配器实现
struct MockControlAdapter;

impl MockControlAdapter {
    fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ControlPort for MockControlAdapter {
    async fn apply_rules(&self, _rules: Vec<takeover::core::ports::Rule>) -> Result<(), takeover::core::ports::AppError> {
        Ok(())
    }
    
    async fn get_active_connections(&self) -> Result<Vec<takeover::core::ports::ConnectionSummary>, takeover::core::ports::AppError> {
        Ok(vec![])
    }
    
    async fn intercept_connection(&self, _conn_id: takeover::core::ports::ConnectionId) -> Result<(), takeover::core::ports::AppError> {
        Ok(())
    }
    
    async fn approve_request(&self, _request_id: takeover::core::ports::RequestId, _modifications: Option<takeover::core::ports::RequestModifications>) -> Result<(), takeover::core::ports::AppError> {
        Ok(())
    }
    
    async fn reject_request(&self, _request_id: takeover::core::ports::RequestId, _reason: String) -> Result<(), takeover::core::ports::AppError> {
        Ok(())
    }
    
    async fn trigger_replay(&self, _request_id: takeover::core::ports::RequestId, _modifications: takeover::core::ports::RequestModifications) -> Result<takeover::core::ports::ReplayResult, takeover::core::ports::AppError> {
        Ok(takeover::core::ports::ReplayResult {
            request_id: _request_id,
            replay_count: 1,
            execution_time: std::time::Duration::from_millis(100),
            success: true,
            error_message: None,
            response: None,
        })
    }
    
    async fn get_active_rules(&self) -> Result<Vec<takeover::core::ports::Rule>, takeover::core::ports::AppError> {
        Ok(vec![])
    }
}
