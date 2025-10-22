use anyhow::Result;
use std::sync::Arc;
use tokio::signal;
use tracing::{ level_filters::LevelFilter, error};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use takeover::core::{AppCore, AppConfig};
use takeover::core::model::TlsConfig;
use takeover::core::services::{StateManager, StateManagerConfig, ReplayManager, ReplayManagerConfig};
use takeover::adapters::inbound::pf::PfInboundAdapter;
use takeover::adapters::outbound::composite::CompositeOutboundAdapter;
use takeover::adapters::control::api::RestControlAdapter;
use takeover::adapters::api::lua_api::{LuaApiAdapter, LuaApiConfig};
use takeover::core::SimpleEventPort;
use takeover::core::ports::ProxyConfig;
use takeover::config::ProxyType;
use takeover::user_manager::UserManager;
use takeover::core::model::{Rule, RulePattern, RuleAction};

#[tokio::main]
async fn main() -> Result<()> {
    // 1. 初始化日志系统
    
    // 设置特定模块的日志级别（避免过多调试日志）
    let module_log_levels: Vec<&'static str> = vec![
        "takeover::components::process_finder=info",
        "takeover::core::middleware::forwarding=info",
        // 可以在这里继续添加更多模块
        // "takeover::core::middleware::http2_stream=info",
        // "takeover::adapters::outbound::socks5=info",
    ];
    
    let mut env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    
    for directive in module_log_levels {
        env_filter = env_filter.add_directive(directive.parse().unwrap());
    }
    
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_target(true)           // 显示模块路径
                .with_thread_ids(false)      // 不显示线程ID
                .with_span_events(fmt::format::FmtSpan::NONE)  // 不显示span事件
                // 不使用compact()，默认格式会显示span fields (conn, proc)
                // span层级由最外层的handle_connection_lifecycle提供conn和proc
                // 中间件不再使用instrument，避免嵌套层级过深
        )
        .with(env_filter)
        .init();

    
    println!("\n🌐 Takeover 透明代理 v2.1");
    println!("=====================================");

    // 2. 检查用户权限
    let user_manager = UserManager::new();
    if !user_manager.is_admin() {
        println!("⚠️  需要管理员权限 (sudo)");
        println!("   继续运行可能会失败...");
    }

    // 3. 构建应用配置
    let mut config = AppConfig::default();
    config.inbound.intercepted_ports = vec![443, 80];
    config.inbound.bypass_user = Some("_takeover_proxy".to_string());
    config.tls = TlsConfig {
        enabled: true,
        interception_mode: "selective".to_string(),
        intercept_domains: vec![
            "*".to_string(),
            // "*.org".to_string(),  // 拦截所有 .org 域名
            // "*.com".to_string(),  // 拦截所有 .com 域名
            // "*.net".to_string(),  // 拦截所有 .net 域名
            // "*.cn".to_string(),   // 拦截所有 .cn 域名
        ],
        cert_store_path: None,
        cert_cache_duration: 3600,
        max_concurrent_handshakes: 100,
        auto_install_ca: true,
    };
    let config = Arc::new(config);
    println!("✅ 配置: 拦截端口 {:?}, TLS拦截 {}", 
        config.inbound.intercepted_ports,
        config.tls.interception_mode);

    // 4. 构建核心服务
    let state_manager = Arc::new(StateManager::new(StateManagerConfig::default()));
    let api_adapter = Arc::new(LuaApiAdapter::new(LuaApiConfig::default())?);
    let replay_manager = Arc::new(ReplayManager::new(
        api_adapter.clone(), 
        ReplayManagerConfig::default()
    ).await?);

    // 5. 构建端口适配器
    let inbound_port = Arc::new(PfInboundAdapter::default());
    let socks5_proxy = ProxyConfig {
        url: "socks5://127.0.0.1:7897".to_string(),
        proxy_type: ProxyType::Socks5,
        auth: None,
    };
    let outbound_port = Arc::new(CompositeOutboundAdapter::with_socks5_config(socks5_proxy));
    let ctrl = RestControlAdapter::default();
    let control_port = Arc::new(ctrl);
    let event_port = Arc::new(SimpleEventPort::new());
    println!("✅ 适配器: PfInbound + CompositeOutbound (Direct/SOCKS5)");
    

    // 6. 组装 AppCore
    let app_core = Arc::new(AppCore::builder(
        (*config).clone(),
        inbound_port,
        outbound_port,
        control_port,
        event_port,
    )
    .with_api_port(api_adapter)
    .with_default_middlewares()
    .manage(state_manager)
    .manage(replay_manager)
    .build()
    .await);
    
    // 6.5 🎯 设置全局AppCore单例 (类似Tauri的全局AppHandle)
    // 这样所有middleware、adapter都可以通过AppCore::global()访问
    AppCore::set_global(app_core.clone())?;
    println!("✅ 全局AppCore已注册 (任何组件都可通过AppCore::global()访问)");
    
    // 6.6 注册中间件服务（包括 RuleService、TlsService）
    app_core.register_middleware_services().await?;
    
    // 🔍 验证TLS Service是否成功注册
    use takeover::core::services::TlsService;
    let has_tls = app_core.has_service::<TlsService>().await;
    println!("✅ 中间件服务已注册 (RuleService 可用, TlsService: {})", has_tls);
    
    // 7. 配置路由规则
    configure_routing_rules(&app_core).await?;
    println!("✅ AppCore: 中间件管道 + 路由规则 已就绪");

    // 8. 启动主服务
    println!("\n💡 测试: curl https://httpbin.org/get");
    println!("🚀 启动透明代理服务...\n");
    
    let app_core_clone = Arc::clone(&app_core);
    
    // 使用 tokio::select! 来并发运行服务器和等待信号
    let result = tokio::select! {
        run_result = app_core.run() => {
            match run_result {
                Ok(_) => Ok(()),
                Err(e) => {
                    error!("❌ 运行失败: {}", e);
                    println!("❌ 失败: {} (提示: 使用sudo运行)", e);
                    Err(e.into())
                }
            }
        }
        
        _ = signal::ctrl_c() => {
            println!("\n🛑 Ctrl+C 收到，正在关闭...");
            app_core_clone.request_stop().await;
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            println!("✅ 已停止");
            Ok(())
        }
    };

    result
}

/// 配置路由分流规则
async fn configure_routing_rules(app_core: &AppCore) -> Result<()> {
    let rules = vec![
        // 🌐 httpbin.org 域名走 SOCKS5 代理
        Rule::new(
            "httpbin.org",
            "httpbin.org (包括子域名)",
            RulePattern::Or(vec![
                RulePattern::HostMatches("httpbin.org".to_string()),
                RulePattern::HostMatches("*.httpbin.org".to_string()),
            ]),
            RuleAction::ProxyByName("default".to_string()),
            10,
        ),
        // 🌐 Google 全球域名走 SOCKS5 代理（包括国际域名 .com.hk, .co.jp 等）
        Rule::new(
            "google_all_domains_proxy",
            "Google全球域名代理规则",
            RulePattern::HostMatches("*google*".to_string()),
            RuleAction::ProxyByName("default".to_string()),
            11,
        ),
        // // 🔧 curl 程序走 SOCKS5 代理
        // Rule::new(
        //     "curl_proxy",
        //     "curl程序代理规则", 
        //     RulePattern::ProcessMatches("*curl*".to_string()),
        //     RuleAction::ProxyByName("default".to_string()),
        //     20,
        // ),
        // 🔄 默认直连规则（兜底）
        Rule::new(
            "default_direct",
            "默认直连规则",
            RulePattern::HostMatches("*".to_string()),
            RuleAction::Direct,
            100,
        ),
    ];
    
    // 应用规则（RestControlAdapter 会自动同步到 RuleService）
    app_core.control_port().apply_rules(rules).await
        .map_err(|e| anyhow::anyhow!("Failed to apply rules: {}", e))?;
    
    println!("✅ 路由规则配置完成:");
    println!("   1. *.httpbin.org → SOCKS5 代理 (优先级: 10)");
    println!("   2. *google.* → SOCKS5 代理 (优先级: 11)");
    println!("   3. curl 进程 → SOCKS5 代理 (优先级: 20)");
    println!("   4. 其他流量 → 直连 (优先级: 100)");
    println!("\n🔒 pf规则已排除本地和内网地址，防止循环");
    
    Ok(())
}

// ========================================
// 🏗️ 六边形架构设计说明
// ========================================
//
// 本程序完全遵循六边形架构（Hexagonal Architecture）原则：
//
// 1. **核心业务逻辑**（AppCore）
//    - 独立于外部技术栈
//    - 通过端口接口与外界交互
//    - 配置驱动，声明式风格
//
// 2. **端口接口**（Ports）
//    - InboundPort: 接收外部连接
//    - OutboundPort: 建立出站连接
//    - ControlPort: 提供控制接口
//    - EventPort: 事件发布订阅
//    - ScriptPort: 脚本扩展
//
// 3. **适配器实现**（Adapters）
//    - PfInboundAdapter: macOS pfctl 实现
//    - Socks5OutboundAdapter: SOCKS5 代理实现
//    - RestControlAdapter: REST API 实现
//    - LuaScriptAdapter: Lua 脚本实现
//
// 4. **中间件管道**（Middleware Pipeline）
//    - 可插拔的处理链
//    - 每个中间件职责单一
//    - 按优先级顺序执行
//
// 5. **配置驱动**
//    - 所有行为通过 AppConfig 声明
//    - 不直接操作内部组件
//    - 易于测试和扩展
//
// 关键特性：
// - ✅ 业务逻辑与技术实现解耦
// - ✅ 易于单元测试（可替换适配器）
// - ✅ 灵活扩展（新增端口/适配器）
// - ✅ 清晰的依赖方向（向内依赖）
//
// 与 Demo 的对比：
// - Demo: 直接操作 TransparentServer、TlsManager（Raw API）
// - Main: 通过 AppCore + Ports + Adapters（六边形架构）
//
// ========================================
