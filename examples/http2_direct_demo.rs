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
use tracing::{info, level_filters::LevelFilter, debug};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

// 导入核心组件
use takeover::core::{AppCore, AppConfig, GeneralConfig, InboundConfig, OutboundConfig, ScriptConfig};
use takeover::core::middleware::{Http2StreamMiddleware, ForwardingMiddleware};
use takeover::core::engine::ProcessingEngine;

// 导入适配器
use takeover::adapters::{PfInboundAdapter, DirectOutboundAdapter};
use takeover::core::ports::ControlPort;

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
    info!("   - 监听端口: 8080");
    info!("   - 拦截端口: [443, 8443]");
    info!("   - 架构: HTTP/2 Stream → 转发中间件 → Direct适配器");
    info!("   - 测试命令: curl --connect-to httpbin.org:443:127.0.0.1:8080 https://httpbin.org/ip -v");
    
    // 启动应用程序
    info!("🎯 启动透明代理服务...");
    app.run().await?;
    
    Ok(())
}

/// 初始化日志系统
fn init_logging() -> Result<()> {
    let env_filter = EnvFilter::from_default_env()
        .add_directive(LevelFilter::INFO.into());
    
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
        general_config: GeneralConfig {
            max_connections: 100,
            connection_timeout: std::time::Duration::from_secs(300),
            log_level: "debug".to_string(),
        },
        
        // 入站配置 - pfctl透明代理
        inbound_config: InboundConfig {
            listen_address: "127.0.0.1".to_string(),
            listen_port: 8080,
            max_connections: 100,
            buffer_size: 8192,
            timeout: std::time::Duration::from_secs(60),
            intercepted_ports: vec![], // 拦截HTTPS端口
            bypass_user: Some("nobody".to_string()),
        },
        
        // 出站配置 - 直连
        outbound_config: OutboundConfig {
            timeout: std::time::Duration::from_secs(10),
            read_timeout: std::time::Duration::from_secs(30),
            write_timeout: std::time::Duration::from_secs(10),
            retry_count: 2,
            retry_delay: std::time::Duration::from_millis(500),
        },
        
        // 脚本配置 - 禁用
        script_config: ScriptConfig {
            enabled: false,
            script_dir: "./scripts".into(),
            timeout: std::time::Duration::from_secs(30),
            max_concurrent: 10,
        },
        
        // 补充缺失的字段
        general: GeneralConfig {
            max_connections: 100,
            connection_timeout: std::time::Duration::from_secs(300),
            log_level: "debug".to_string(),
        },
        inbound: InboundConfig {
            listen_address: "127.0.0.1".to_string(),
            listen_port: 8080,
            max_connections: 100,
            buffer_size: 8192,
            timeout: std::time::Duration::from_secs(60),
            intercepted_ports: vec![443, 8443, 9443],
            bypass_user: Some("nobody".to_string()),
        },
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
    
    // 2. 注意：处理引擎会在AppCore内部自动创建和配置
    // let engine = create_processing_engine().await?;
    
    // 3. 创建简单的事件端口
    let event_port = Arc::new(SimpleEventPort::new());
    
    // 4. 组装应用核心
    let app_core = AppCore::new(
        (*config).clone(),
        inbound_adapter,
        outbound_adapter,
        control_adapter,
        event_port,
    );
    
    // 注意: 这个demo版本使用默认的处理引擎
    // 真正的处理引擎设置需要在AppCore内部通过build_pipeline完成
    
    info!("✅ Demo应用程序构建完成");
    Ok(app_core)
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

/// 创建出站适配器 (Direct)
async fn create_outbound_adapter(
    _config: &AppConfig
) -> Result<Arc<dyn takeover::core::ports::OutboundPort>> {
    info!("📤 创建Direct出站适配器");
    
    let adapter = DirectOutboundAdapter::new();
    
    info!("✅ Direct出站适配器创建完成");
    Ok(Arc::new(adapter))
}

/// 创建控制适配器
async fn create_control_adapter() -> Result<Arc<dyn takeover::core::ports::ControlPort>> {
    info!("🎛️ 创建Mock控制适配器");
    
    let adapter = MockControlAdapter::new();
    
    info!("✅ Mock控制适配器创建完成");
    Ok(Arc::new(adapter))
}

/// 创建处理引擎
async fn create_processing_engine() -> Result<Arc<ProcessingEngine>> {
    info!("🔧 创建处理引擎和中间件管道");
    
    let mut engine = ProcessingEngine::new();
    
    // 添加关键中间件（简化版本）
    
    // 1. HTTP/2流中间件 (优先级15)
    let http2_middleware = Arc::new(Http2StreamMiddleware::new());
    engine.add_middleware(http2_middleware);
    info!("   ✅ 添加HTTP/2流中间件 (优先级: 15)");
    
    // 2. 转发中间件 (优先级60)
    let forwarding_middleware = Arc::new(ForwardingMiddleware::new());
    engine.add_middleware(forwarding_middleware);
    info!("   ✅ 添加转发中间件 (优先级: 60)");
    
    info!("✅ 处理引擎创建完成 (包含2个核心中间件)");
    Ok(Arc::new(engine))
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
            takeover::core::ports::DomainEvent::ConnectionNew { id, addr, .. } => {
                // info!("📡 新连接: {} from {}", id, addr);
            }
            takeover::core::ports::DomainEvent::ConnectionClosed { id, duration, bytes_transferred } => {
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
