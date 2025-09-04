// //! Takeover 透明代理应用程序入口点
// //! 
// //! 使用六边形架构和依赖注入来组装整个应用程序

// use std::sync::Arc;
// use std::time::Duration;
// use std::path::PathBuf;

// use clap::{Parser, Subcommand};
use anyhow::Result;
// use tracing::{info, warn, level_filters::LevelFilter};
// use tracing_subscriber::{EnvFilter, fmt, prelude::*};

// // 导入新架构组件
// use takeover::core::{AppCore, AppConfig};
// use takeover::core::services::{StateManager, StateManagerConfig, ReplayManager, ReplayManagerConfig};
// use takeover::core::middleware::{
//     TlsMiddleware, HttpParsingMiddleware, RuleMatchingMiddleware, 
//     HttpModificationMiddleware, ScriptMiddleware, UserInterventionMiddleware, 
//     ResponseMergeMiddleware
// };

// // 导入适配器
// use takeover::adapters::{
//     PfInboundAdapter, PfInboundConfig,
//     DirectOutboundAdapter, Socks5OutboundAdapter,
//     ApiControlAdapter, LuaScriptAdapter, LuaApiAdapter
// };

// // 导入配置和错误类型
// use takeover::{Config, TakeoverError};

// /// Takeover 透明代理命令行接口
// #[derive(Parser)]
// #[command(name = "takeover")]
// #[command(about = "A powerful transparent proxy with advanced features")]
// #[command(version = env!("CARGO_PKG_VERSION"))]
// struct Cli {
//     /// 配置文件路径
//     #[arg(short, long, default_value = "takeover.toml")]
//     config: PathBuf,
    
//     /// 日志级别
//     #[arg(short, long, default_value = "info")]
//     log_level: String,
    
//     /// 是否启用详细输出
//     #[arg(short, long)]
//     verbose: bool,
    
//     /// 命令
//     #[command(subcommand)]
//     command: Option<Commands>,
// }

// /// 可用的命令
// #[derive(Subcommand)]
// enum Commands {
//     /// 启动透明代理服务
//     Start {
//         /// 监听端口
//         #[arg(short, long, default_value = "8080")]
//         port: u16,
        
//         /// 是否以守护进程模式运行
//         #[arg(short, long)]
//         daemon: bool,
//     },
    
//     /// 停止透明代理服务
//     Stop,
    
//     /// 重启透明代理服务
//     Restart,
    
//     /// 显示服务状态
//     Status,
    
//     /// 验证配置文件
//     Validate,
    
//     /// 测试连接
//     Test {
//         /// 目标URL
//         url: String,
//     },
    
//     /// 管理重放会话
//     Replay {
//         #[command(subcommand)]
//         action: ReplayCommands,
//     },
    
//     /// 管理Lua脚本
//     Script {
//         #[command(subcommand)]
//         action: ScriptCommands,
//     },
// }

// /// 重放相关命令
// #[derive(Subcommand)]
// enum ReplayCommands {
//     /// 列出录制会话
//     List,
//     /// 开始录制
//     Record {
//         /// 会话名称
//         name: String,
//     },
//     /// 停止录制
//     Stop {
//         /// 会话ID
//         session_id: String,
//     },
//     /// 重放会话
//     Play {
//         /// 录制会话ID
//         session_id: String,
//         /// 目标主机（可选）
//         #[arg(long)]
//         target_host: Option<String>,
//     },
// }

// /// 脚本相关命令
// #[derive(Subcommand)]
// enum ScriptCommands {
//     /// 列出脚本
//     List,
//     /// 加载脚本
//     Load {
//         /// 脚本文件路径
//         file: PathBuf,
//     },
//     /// 验证脚本
//     Validate {
//         /// 脚本文件路径
//         file: PathBuf,
//     },
// }

// /// 应用程序构建器
// /// 
// /// 使用构建器模式和依赖注入来组装应用程序
// pub struct AppBuilder {
//     config: Config,
//     app_config: AppConfig,
// }

// impl AppBuilder {
//     /// 创建新的应用程序构建器
//     pub fn new(config: Config) -> Self {
//         let app_config = AppConfig {
//             app_name: "takeover".to_string(),
//             version: env!("CARGO_PKG_VERSION").to_string(),
//             proxy_config: config.proxy_config.clone(),
//             routing_rules: config.routing_engine.routing_rules.clone(),
//             tls_config: config.tls_interception.clone(),
//             user_config: config.user_manager.clone(),
//         };
        
//         Self { config, app_config }
//     }
    
//     /// 构建完整的应用程序
//     pub async fn build(self) -> Result<AppCore, TakeoverError> {
//         info!("🏗️ Building Takeover application with new architecture");
        
//         // 1. 创建核心服务
//         let state_manager = self.build_state_manager().await?;
//         let replay_manager = self.build_replay_manager().await?;
        
//         // 2. 创建适配器
//         let inbound_adapter = self.build_inbound_adapter().await?;
//         let outbound_adapter = self.build_outbound_adapter().await?;
//         let control_adapter = self.build_control_adapter().await?;
//         let script_adapter = self.build_script_adapter().await?;
//         let api_adapter = self.build_api_adapter().await?;
        
//         // 3. 创建中间件管道
//         let middlewares = self.build_middleware_pipeline(
//             Arc::clone(&script_adapter),
//             Arc::clone(&api_adapter),
//         ).await?;
        
//         // 4. 组装核心应用
//         let app_core = AppCore::new_with_all_ports(
//             Arc::new(self.app_config),
//             inbound_adapter,
//             outbound_adapter,
//             control_adapter,
//             script_adapter,
//             api_adapter,
//             state_manager,
//             replay_manager,
//             middlewares,
//         );
        
//         info!("✅ Application built successfully");
//         Ok(app_core)
//     }
    
//     /// 构建状态管理器
//     async fn build_state_manager(&self) -> Result<Arc<StateManager>, TakeoverError> {
//         info!("🗄️ Building state manager");
        
//         let config = StateManagerConfig {
//             max_connections: 10000,
//             max_sessions: 5000,
//             connection_timeout: Duration::from_secs(3600),
//             session_timeout: Duration::from_secs(7200),
//             cleanup_interval: Duration::from_secs(60),
//             enable_statistics: true,
//             ..Default::default()
//         };
        
//         let state_manager = StateManager::new(config);
//         Ok(Arc::new(state_manager))
//     }
    
//     /// 构建重放管理器
//     async fn build_replay_manager(&self) -> Result<Arc<ReplayManager>, TakeoverError> {
//         info!("🎬 Building replay manager");
        
//         // 这里需要一个API端口的引用，我们暂时使用Arc::new()的方式
//         // 在实际的依赖注入中，这应该是从容器中获取的
//         let api_adapter = Arc::new(LuaApiAdapter::new(Default::default()).unwrap());
        
//         let config = ReplayManagerConfig {
//             storage_dir: PathBuf::from("./replay_data"),
//             auto_recording: false,
//             enable_result_comparison: true,
//             ..Default::default()
//         };
        
//         let replay_manager = ReplayManager::new(api_adapter, config).await
//             .map_err(|e| TakeoverError::Config(format!("Failed to create replay manager: {}", e)))?;
        
//         Ok(Arc::new(replay_manager))
//     }
    
//     /// 构建入站适配器
//     async fn build_inbound_adapter(&self) -> Result<Arc<dyn takeover::core::ports::InboundPort>, TakeoverError> {
//         info!("📥 Building inbound adapter");
        
//         let config = PfInboundConfig {
//             listen_port: self.config.transparent_proxy.listen_port,
//             bind_address: self.config.transparent_proxy.bind_ip.clone(),
//             enable_ipv6: false,
//             buffer_size: 8192,
//             connection_timeout: Duration::from_secs(60),
//             max_connections: 1000,
//         };
        
//         let adapter = PfInboundAdapter::new(config).await
//             .map_err(|e| TakeoverError::Config(format!("Failed to create inbound adapter: {}", e)))?;
        
//         Ok(Arc::new(adapter))
//     }
    
//     /// 构建出站适配器
//     async fn build_outbound_adapter(&self) -> Result<Arc<dyn takeover::core::ports::OutboundPort>, TakeoverError> {
//         info!("📤 Building outbound adapter");
        
//         // 根据配置选择出站适配器
//         match &self.config.proxy_config.proxy_type {
//             takeover::ProxyType::Direct => {
//                 let adapter = DirectOutboundAdapter::new();
//                 Ok(Arc::new(adapter))
//             }
//             takeover::ProxyType::Socks5 => {
//                 let adapter = Socks5OutboundAdapter::new(self.config.proxy_config.clone());
//                 Ok(Arc::new(adapter))
//             }
//         }
//     }
    
//     /// 构建控制适配器
//     async fn build_control_adapter(&self) -> Result<Arc<dyn takeover::core::ports::ControlPort>, TakeoverError> {
//         info!("🎛️ Building control adapter");
        
//         let adapter = ApiControlAdapter::new().await;
//         Ok(Arc::new(adapter))
//     }
    
//     /// 构建脚本适配器
//     async fn build_script_adapter(&self) -> Result<Arc<dyn takeover::core::ports::ScriptPort>, TakeoverError> {
//         info!("📜 Building script adapter");
        
//         // 这里同样需要API适配器的引用
//         let api_adapter = Arc::new(LuaApiAdapter::new(Default::default()).unwrap());
        
//         let config = takeover::adapters::script::lua_adapter::LuaConfig {
//             script_dir: PathBuf::from("./scripts"),
//             max_concurrent_executions: 10,
//             execution_timeout: Duration::from_secs(30),
//             sandbox_enabled: true,
//             auto_reload: false,
//             ..Default::default()
//         };
        
//         let adapter = LuaScriptAdapter::new(api_adapter, config).await
//             .map_err(|e| TakeoverError::Config(format!("Failed to create script adapter: {}", e)))?;
        
//         Ok(Arc::new(adapter))
//     }
    
//     /// 构建API适配器
//     async fn build_api_adapter(&self) -> Result<Arc<dyn takeover::core::ports::ApiPort>, TakeoverError> {
//         info!("🔌 Building API adapter");
        
//         let config = takeover::adapters::api::lua_api::LuaApiConfig {
//             allowed_domains: vec![], // 允许所有域名
//             enable_request_logging: true,
//             ..Default::default()
//         };
        
//         let adapter = LuaApiAdapter::new(config)
//             .map_err(|e| TakeoverError::Config(format!("Failed to create API adapter: {}", e)))?;
        
//         Ok(Arc::new(adapter))
//     }
    
//     /// 构建中间件管道
//     async fn build_middleware_pipeline(
//         &self,
//         script_adapter: Arc<dyn takeover::core::ports::ScriptPort>,
//         api_adapter: Arc<dyn takeover::core::ports::ApiPort>,
//     ) -> Result<Vec<Arc<dyn takeover::core::engine::Middleware>>, TakeoverError> {
//         info!("🔧 Building middleware pipeline");
        
//         let mut middlewares: Vec<Arc<dyn takeover::core::engine::Middleware>> = Vec::new();
        
//         // 1. TLS中间件 (优先级: 10)
//         if self.config.tls_interception.enabled {
//             let tls_middleware = TlsMiddleware::new(self.config.tls_interception.clone()).await
//                 .map_err(|e| TakeoverError::Config(format!("Failed to create TLS middleware: {}", e)))?;
//             middlewares.push(Arc::new(tls_middleware));
//         }
        
//         // 2. HTTP解析中间件 (优先级: 20)
//         let http_parsing_middleware = HttpParsingMiddleware::new();
//         middlewares.push(Arc::new(http_parsing_middleware));
        
//         // 3. 规则匹配中间件 (优先级: 30)
//         let rule_matching_middleware = RuleMatchingMiddleware::new(
//             self.config.routing_engine.routing_rules.clone()
//         );
//         middlewares.push(Arc::new(rule_matching_middleware));
        
//         // 4. 脚本中间件 (优先级: 35)
//         let script_middleware = ScriptMiddleware::new(script_adapter, Arc::clone(&api_adapter));
//         middlewares.push(Arc::new(script_middleware));
        
//         // 5. HTTP修改中间件 (优先级: 40)
//         let http_modification_middleware = HttpModificationMiddleware::new();
//         middlewares.push(Arc::new(http_modification_middleware));
        
//         // 6. 用户干预中间件 (优先级: 45)
//         let user_intervention_config = takeover::core::middleware::user_intervention::UserInterventionConfig {
//             enabled: true,
//             default_timeout: Duration::from_secs(300),
//             require_authentication: false, // 简化配置
//             ..Default::default()
//         };
//         let user_intervention_middleware = UserInterventionMiddleware::new(user_intervention_config);
//         middlewares.push(Arc::new(user_intervention_middleware));
        
//         // 7. 响应合并中间件 (优先级: 50)
//         let response_merge_config = takeover::core::middleware::response_merge::ResponseMergeConfig {
//             enabled: true,
//             ..Default::default()
//         };
//         let response_merge_middleware = ResponseMergeMiddleware::new(
//             Arc::clone(&api_adapter), 
//             response_merge_config
//         ).await.map_err(|e| TakeoverError::Config(format!("Failed to create response merge middleware: {}", e)))?;
//         middlewares.push(Arc::new(response_merge_middleware));
        
//         // 按优先级排序
//         middlewares.sort_by_key(|m| m.priority());
        
//         info!("✅ Built middleware pipeline with {} middlewares", middlewares.len());
//         for middleware in &middlewares {
//             info!("  - {} (priority: {})", middleware.name(), middleware.priority());
//         }
        
//         Ok(middlewares)
//     }
// }

// /// 初始化日志系统
// fn init_logging(log_level: &str, verbose: bool) -> Result<()> {
//     let level = match log_level.to_lowercase().as_str() {
//         "trace" => LevelFilter::TRACE,
//         "debug" => LevelFilter::DEBUG,
//         "info" => LevelFilter::INFO,
//         "warn" => LevelFilter::WARN,
//         "error" => LevelFilter::ERROR,
//         _ => LevelFilter::INFO,
//     };
    
//     let env_filter = if verbose {
//         EnvFilter::from_default_env()
//             .add_directive(level.into())
//             .add_directive("takeover=trace".parse()?)
//     } else {
//         EnvFilter::from_default_env()
//             .add_directive(level.into())
//     };
    
//     tracing_subscriber::registry()
//         .with(fmt::layer().with_target(verbose).with_thread_ids(verbose))
//         .with(env_filter)
//         .init();
    
//     Ok(())
// }

// /// 加载配置文件
// fn load_config(config_path: &PathBuf) -> Result<Config> {
//     if config_path.exists() {
//         info!("📖 Loading configuration from: {}", config_path.display());
//         Config::from_file(config_path)
//             .map_err(|e| anyhow::anyhow!("Failed to load config: {}", e))
//     } else {
//         warn!("⚠️ Configuration file not found, using defaults");
//         Ok(Config::default())
//     }
// }

/// 主函数
#[tokio::main]
async fn main() -> Result<()> {
//     let cli = Cli::parse();
    
//     // 初始化日志
//     init_logging(&cli.log_level, cli.verbose)?;
    
//     info!("🚀 Starting Takeover Transparent Proxy v{}", env!("CARGO_PKG_VERSION"));
    
//     // 加载配置
//     let config = load_config(&cli.config)?;
    
//     match cli.command {
//         Some(Commands::Start { port, daemon }) => {
//             start_service(config, port, daemon).await
//         }
//         Some(Commands::Stop) => {
//             stop_service().await
//         }
//         Some(Commands::Restart) => {
//             restart_service().await
//         }
//         Some(Commands::Status) => {
//             show_status().await
//         }
//         Some(Commands::Validate) => {
//             validate_config(config).await
//         }
//         Some(Commands::Test { url }) => {
//             test_connection(url).await
//         }
//         Some(Commands::Replay { action }) => {
//             handle_replay_commands(action, config).await
//         }
//         Some(Commands::Script { action }) => {
//             handle_script_commands(action, config).await
//         }
//         None => {
//             // 默认启动服务
//             start_service(config, 8080, false).await
//         }
//     }
    Ok(())
}

// /// 启动服务
// async fn start_service(config: Config, port: u16, daemon: bool) -> Result<()> {
//     info!("🌟 Starting Takeover proxy service on port {}", port);
    
//     if daemon {
//         info!("👤 Running as daemon");
//         // TODO: 实现守护进程模式
//     }
    
//     // 构建应用程序
//     let app_builder = AppBuilder::new(config);
//     let app_core = app_builder.build().await?;
    
//     // 启动应用程序
//     info!("🎯 Starting application core");
//     app_core.run().await
//         .map_err(|e| anyhow::anyhow!("Application failed: {}", e))?;
    
//     Ok(())
// }

// /// 停止服务
// async fn stop_service() -> Result<()> {
//     info!("🛑 Stopping Takeover proxy service");
//     // TODO: 实现服务停止逻辑
//     Ok(())
// }

// /// 重启服务
// async fn restart_service() -> Result<()> {
//     info!("🔄 Restarting Takeover proxy service");
//     stop_service().await?;
//     // TODO: 重新启动逻辑
//     Ok(())
// }

// /// 显示服务状态
// async fn show_status() -> Result<()> {
//     info!("📊 Checking Takeover proxy service status");
//     // TODO: 实现状态检查逻辑
//     println!("Service status: Running");
//     Ok(())
// }

// /// 验证配置
// async fn validate_config(config: Config) -> Result<()> {
//     info!("✅ Validating configuration");
    
//     // 验证配置的有效性
//     if config.transparent_proxy.listen_port == 0 {
//         return Err(anyhow::anyhow!("Invalid listen port: 0"));
//     }
    
//     if config.routing_engine.routing_rules.domains.is_empty() 
//         && config.routing_engine.routing_rules.ports.is_empty() {
//         warn!("⚠️ No routing rules configured, all traffic will be proxied");
//     }
    
//     println!("✅ Configuration is valid");
//     Ok(())
// }

// /// 测试连接
// async fn test_connection(url: String) -> Result<()> {
//     info!("🧪 Testing connection to: {}", url);
    
//     // 简单的连接测试
//     let client = reqwest::Client::new();
//     match client.get(&url).send().await {
//         Ok(response) => {
//             println!("✅ Connection successful: {} {}", response.status(), url);
//         }
//         Err(e) => {
//             println!("❌ Connection failed: {}", e);
//         }
//     }
    
//     Ok(())
// }

// /// 处理重放相关命令
// async fn handle_replay_commands(action: ReplayCommands, config: Config) -> Result<()> {
//     match action {
//         ReplayCommands::List => {
//             info!("📋 Listing recorded sessions");
//             // TODO: 实现会话列表功能
//             println!("No recorded sessions found");
//         }
//         ReplayCommands::Record { name } => {
//             info!("🔴 Starting recording session: {}", name);
//             // TODO: 实现录制开始功能
//         }
//         ReplayCommands::Stop { session_id } => {
//             info!("⏹️ Stopping recording session: {}", session_id);
//             // TODO: 实现录制停止功能
//         }
//         ReplayCommands::Play { session_id, target_host } => {
//             info!("▶️ Playing back session: {}", session_id);
//             if let Some(host) = target_host {
//                 info!("🎯 Target host: {}", host);
//             }
//             // TODO: 实现重放功能
//         }
//     }
//     Ok(())
// }

// /// 处理脚本相关命令
// async fn handle_script_commands(action: ScriptCommands, config: Config) -> Result<()> {
//     match action {
//         ScriptCommands::List => {
//             info!("📋 Listing available scripts");
//             // TODO: 实现脚本列表功能
//             let scripts_dir = PathBuf::from("./scripts");
//             if scripts_dir.exists() {
//                 if let Ok(entries) = std::fs::read_dir(scripts_dir) {
//                     for entry in entries {
//                         if let Ok(entry) = entry {
//                             if entry.path().extension().and_then(|s| s.to_str()) == Some("lua") {
//                                 println!("📜 {}", entry.file_name().to_string_lossy());
//                             }
//                         }
//                     }
//                 }
//             } else {
//                 println!("No scripts directory found");
//             }
//         }
//         ScriptCommands::Load { file } => {
//             info!("📂 Loading script: {}", file.display());
//             // TODO: 实现脚本加载功能
//         }
//         ScriptCommands::Validate { file } => {
//             info!("✅ Validating script: {}", file.display());
//             // TODO: 实现脚本验证功能
//         }
//     }
//     Ok(())
// }
