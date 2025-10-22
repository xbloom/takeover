//! 核心业务逻辑模块
//! 
//! 这是六边形架构的核心"六边形"，包含：
//! - AppCore: 主要的业务编排器
//! - 核心业务服务的协调逻辑
//! - 与外部世界的交互通过端口接口进行

use std::sync::Arc;
use std::collections::HashMap;
use std::any::{Any, TypeId};
use tokio::sync::{RwLock, Semaphore};
use tokio::time::{Duration, Instant};
use futures::StreamExt;
use tracing::{info, warn, error, debug, instrument};
use uuid::Uuid;
use async_trait::async_trait;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use once_cell::sync::OnceCell;

// 导出子模块
pub mod ports;
pub mod model;
pub mod engine;
pub mod middleware;
pub mod services;

// 重新导出核心类型，但避免冲突
pub use model::{
    AppConfig, ConnectionId, RequestId, Connection, ConnectionContext, 
    ConnectionMetadata, HttpRequest, HttpResponse, ProcessingState,
    ConnectionStatus, ConnectionSummary, AdapterStatus, Rule, RulePattern, RuleAction,
    RuleCondition, RequestModifications,
    DomainEvent, ErrorCategory, MergeStrategy,
    InboundConfig, OutboundConfig, ScriptConfig, GeneralConfig,
    ResponseModifications, ReplayResult, CompositeRequestConfig,
    TriggerPoint, TriggerCondition, CustomEvent, LogLevel
};
pub use ports::{
    InboundPort, OutboundPort, ControlPort, EventPort, ScriptPort, ApiPort,
    StatePort, UiPort, AppError, NetworkStream
};
pub use engine::{Middleware, MiddlewareResult, ProcessingEngine};
pub use services::{StateManager, ReplayManager};

use crate::error::TakeoverError;
use crate::user_manager::UserManager;

/// 类型安全的服务容器 - 基于 TypeId 的依赖注入
/// 
/// 提供类似 Tauri 风格的服务注册和查询功能：
/// - 编译时类型安全
/// - 运行时动态查询
/// - 支持任何实现 Send + Sync 的类型
#[derive(Debug, Clone)]
pub struct ServiceContainer {
    services: Arc<RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>>,
}

impl ServiceContainer {
    /// 创建新的服务容器
    pub fn new() -> Self {
        Self {
            services: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    /// 注册服务到容器
    /// 
    /// # 示例
    /// ```rust
    /// let container = ServiceContainer::new();
    /// container.register(MyService::new()).await;
    /// ```
    pub async fn register<T: Any + Send + Sync>(&self, service: T) {
        let type_id = TypeId::of::<T>();
        let mut services = self.services.write().await;
        services.insert(type_id, Arc::new(service));
        debug!("📦 Service registered: {:?}", std::any::type_name::<T>());
    }
    
    /// 注册已经包装在 Arc 中的服务
    /// 
    /// # 示例
    /// ```rust
    /// let container = ServiceContainer::new();
    /// let service = Arc::new(MyService::new());
    /// container.register_arc(service).await;
    /// ```
    pub async fn register_arc<T: Any + Send + Sync>(&self, service: Arc<T>) {
        let type_id = TypeId::of::<T>();
        let mut services = self.services.write().await;
        services.insert(type_id, service);
        debug!("📦 Service registered (Arc): {:?}", std::any::type_name::<T>());
    }
    
    /// 从容器中获取服务
    /// 
    /// # 示例
    /// ```rust
    /// if let Some(service) = container.get::<MyService>().await {
    ///     // 使用服务
    /// }
    /// ```
    pub async fn get<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        let type_id = TypeId::of::<T>();
        let services = self.services.read().await;
        services.get(&type_id)
            .and_then(|service| service.clone().downcast::<T>().ok())
    }
    
    /// 检查服务是否已注册
    pub async fn contains<T: Any + Send + Sync>(&self) -> bool {
        let type_id = TypeId::of::<T>();
        let services = self.services.read().await;
        services.contains_key(&type_id)
    }
    
    /// 获取已注册服务的数量
    pub async fn len(&self) -> usize {
        let services = self.services.read().await;
        services.len()
    }
}

/// 全局AppCore单例 - 类似Tauri的全局AppHandle
/// 
/// 使用OnceCell实现线程安全的延迟初始化，确保整个应用程序中只有一个AppCore实例。
/// 
/// # 使用示例
/// ```
/// // 在main.rs中设置全局实例
/// let app_core = Arc::new(AppCore::builder(...).build().await);
/// AppCore::set_global(app_core.clone())?;
/// 
/// // 在任何地方访问
/// let app_core = AppCore::global();
/// let tls_service = app_core.state::<TlsService>().await;
/// ```
static GLOBAL_APP_CORE: OnceCell<Arc<AppCore>> = OnceCell::new();

/// 应用核心 - 六边形架构的中心
/// 
/// AppCore是系统的"大脑"，负责：
/// 1. 协调各个端口适配器
/// 2. 编排业务处理流程
/// 3. 管理连接生命周期
/// 4. 维护系统状态
pub struct AppCore {
    /// 应用配置
    config: Arc<AppConfig>,
    
    /// 端口适配器
    inbound_port: Arc<dyn InboundPort>,
    outbound_port: Arc<dyn OutboundPort>,
    control_port: Arc<dyn ControlPort>,
    event_port: Arc<dyn EventPort>,
    script_port: Option<Arc<dyn ScriptPort>>,
    api_port: Option<Arc<dyn ApiPort>>,
    state_port: Option<Arc<dyn StatePort>>,
    ui_port: Option<Arc<dyn UiPort>>,
    
    /// 处理引擎
    engine: Arc<ProcessingEngine>,
    
    /// 用户管理器（防循环用户切换）
    user_manager: Arc<UserManager>,
    
    /// 服务容器 - TypeId 驱动的依赖注入
    services: ServiceContainer,
    
    /// 连接管理
    active_connections: Arc<RwLock<HashMap<ConnectionId, ConnectionTracker>>>,
    connection_semaphore: Arc<Semaphore>,
    
    /// 运行时状态
    running: Arc<AtomicBool>,
    stats: Arc<RwLock<AppStats>>,
}

/// 连接跟踪器 - 跟踪单个连接的状态
#[derive(Debug)]
struct ConnectionTracker {
    context: ConnectionContext,
    created_at: Instant,
    last_activity: Instant,
    bytes_sent: u64,
    bytes_received: u64,
    status: ConnectionStatus,
}

impl ConnectionTracker {
    fn new(context: ConnectionContext) -> Self {
        let now = Instant::now();
        Self {
            context,
            created_at: now,
            last_activity: now,
            bytes_sent: 0,
            bytes_received: 0,
            status: ConnectionStatus::Connecting,
        }
    }
    
    fn update_activity(&mut self) {
        self.last_activity = Instant::now();
    }
    
    fn add_bytes_sent(&mut self, bytes: u64) {
        self.bytes_sent += bytes;
        self.update_activity();
    }
    
    fn add_bytes_received(&mut self, bytes: u64) {
        self.bytes_received += bytes;
        self.update_activity();
    }
    
    fn set_status(&mut self, status: ConnectionStatus) {
        self.status = status;
        self.update_activity();
    }
}

/// 应用统计信息
#[derive(Debug, Default, Clone)]
pub struct AppStats {
    /// 总连接数
    pub total_connections: u64,
    /// 活跃连接数
    pub active_connections: usize,
    /// 总传输字节数
    pub total_bytes_transferred: u64,
    /// 处理的请求数
    pub requests_processed: u64,
    /// 脚本执行次数
    pub scripts_executed: u64,
    /// 用户干预次数
    pub user_interventions: u64,
    /// 错误计数
    pub error_count: u64,
    /// 启动时间
    pub start_time: Option<Instant>,
}

impl AppCore {
    /// 设置全局AppCore实例 (类似Tauri的全局AppHandle初始化)
    /// 
    /// 这个方法应该在main.rs中AppCore构建后立即调用一次。
    /// 
    /// # 错误
    /// 如果全局实例已经被设置过，会返回错误。
    /// 
    /// # 示例
    /// ```
    /// let app_core = Arc::new(AppCore::builder(...).build().await);
    /// AppCore::set_global(app_core.clone())?;
    /// ```
    pub fn set_global(app_core: Arc<AppCore>) -> Result<(), TakeoverError> {
        GLOBAL_APP_CORE.set(app_core)
            .map_err(|_| TakeoverError::Config("AppCore global instance already set".to_string()))
    }
    
    /// 获取全局AppCore实例 (类似Tauri的AppHandle访问)
    /// 
    /// 这个方法可以在应用程序的任何地方调用，无需显式传递AppCore引用。
    /// 
    /// # Panic
    /// 如果全局实例未设置，会panic。确保在main.rs中先调用`set_global()`。
    /// 
    /// # 示例
    /// ```
    /// // 在任何middleware、adapter中
    /// let app_core = AppCore::global();
    /// let tls_service = app_core.state::<TlsService>().await?;
    /// ```
    pub fn global() -> &'static Arc<AppCore> {
        GLOBAL_APP_CORE.get()
            .expect("AppCore global instance not initialized. Call AppCore::set_global() in main.rs first.")
    }
    
    /// 尝试获取全局AppCore实例 (不会panic的版本)
    /// 
    /// # 返回
    /// 如果全局实例已设置，返回`Some(&Arc<AppCore>)`，否则返回`None`。
    pub fn try_global() -> Option<&'static Arc<AppCore>> {
        GLOBAL_APP_CORE.get()
    }
    
    /// 创建新的应用核心实例 (私有，通过Builder创建)
    fn new(
        config: Arc<AppConfig>,
        inbound_port: Arc<dyn InboundPort>,
        outbound_port: Arc<dyn OutboundPort>,
        control_port: Arc<dyn ControlPort>,
        event_port: Arc<dyn EventPort>,
        engine: ProcessingEngine,
    ) -> Self {
        let max_connections = config.general.max_connections;
        
        // 🔧 创建用户管理器，使用配置中的bypass_user
        let user_manager = if let Some(ref bypass_user) = config.inbound.bypass_user {
            Arc::new(UserManager::with_proxy_user(bypass_user))
        } else {
            Arc::new(UserManager::new())
        };
        
        // 创建服务容器
        let services = ServiceContainer::new();
        
        Self {
            engine: Arc::new(engine),
            config,
            inbound_port,
            outbound_port,
            control_port,
            event_port,
            script_port: None,
            api_port: None,
            state_port: None,
            ui_port: None,
            user_manager,
            services,
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            connection_semaphore: Arc::new(Semaphore::new(max_connections)),
            running: Arc::new(AtomicBool::new(false)),
            stats: Arc::new(RwLock::new(AppStats::default())),
        }
    }
    
    /// 创建简化的应用核心实例（用于Demo和测试，低级API）
    /// 
    /// 这是一个公开的构造方法，供不想使用Builder模式的场景使用。
    /// 
    /// # 参数
    /// - `config`: 应用配置
    /// - `inbound_port`: 入站适配器
    /// - `outbound_port`: 出站适配器
    /// - `control_port`: 控制适配器
    /// - `event_port`: 事件适配器
    /// - `engine`: 处理引擎（需要预先配置好中间件）
    #[deprecated(note = "Use builder instead")]
    pub fn new_simple(
        config: AppConfig,
        inbound_port: Arc<dyn InboundPort>,
        outbound_port: Arc<dyn OutboundPort>,
        control_port: Arc<dyn ControlPort>,
        event_port: Arc<dyn EventPort>,
        engine: ProcessingEngine,
    ) -> Self {
        Self::new(
            Arc::new(config),
            inbound_port,
            outbound_port,
            control_port,
            event_port,
            engine,
        )
    }
    
    
    /// 创建完整配置的应用核心实例（用于main.rs）
    pub async fn new_with_all_ports(
        config: Arc<AppConfig>,
        inbound_port: Arc<dyn InboundPort>,
        outbound_port: Arc<dyn OutboundPort>,
        control_port: Arc<dyn ControlPort>,
        script_port: Arc<dyn ScriptPort>,
        api_port: Arc<dyn ApiPort>,
        state_manager: Arc<StateManager>,
        replay_manager: Arc<ReplayManager>,
        middlewares: Vec<Arc<dyn crate::core::engine::Middleware>>,
    ) -> Self {
        let max_connections = config.general.max_connections;
        
        // 创建处理引擎并设置中间件
        let engine = ProcessingEngine::new();
        for middleware in middlewares {
            engine.add_middleware(middleware);
        }
        
        // 创建一个简单的事件端口实现
        let event_port = Arc::new(SimpleEventPort::new());
        
        // 🔧 创建用户管理器，使用配置中的bypass_user
        let user_manager = if let Some(ref bypass_user) = config.inbound.bypass_user {
            Arc::new(UserManager::with_proxy_user(bypass_user))
        } else {
            Arc::new(UserManager::new())
        };
        
        // 创建服务容器并注册核心服务
        let services = ServiceContainer::new();
        
        // ✅ 注册StateManager到服务容器
        services.register(state_manager).await;
        info!("✅ StateManager registered to AppCore service container");
        
        // ✅ 注册ReplayManager到服务容器
        services.register(replay_manager).await;
        info!("✅ ReplayManager registered to AppCore service container");
        
        Self {
            engine: Arc::new(engine),
            config,
            inbound_port,
            outbound_port,
            control_port,
            event_port,
            script_port: Some(script_port),
            api_port: Some(api_port),
            state_port: None, // 可以在需要时添加
            ui_port: None,    // 可以在需要时添加
            user_manager,
            services,
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            connection_semaphore: Arc::new(Semaphore::new(max_connections)),
            running: Arc::new(AtomicBool::new(false)),
            stats: Arc::new(RwLock::new(AppStats::default())),
        }
    }
    
    /// 返回一个新的 AppCoreBuilder
    pub fn builder(
        config: AppConfig,
        inbound: Arc<dyn InboundPort>,
        outbound: Arc<dyn OutboundPort>,
        control: Arc<dyn ControlPort>,
        event: Arc<dyn EventPort>,
    ) -> AppCoreBuilder {
        AppCoreBuilder::new(config, inbound, outbound, control, event)
    }
    
    
    /// 获取应用配置
    pub fn config(&self) -> &Arc<AppConfig> {
        &self.config
    }
    
    /// 获取各种端口的引用
    pub fn inbound_port(&self) -> &Arc<dyn InboundPort> {
        &self.inbound_port
    }
    
    pub fn outbound_port(&self) -> &Arc<dyn OutboundPort> {
        &self.outbound_port
    }
    
    pub fn control_port(&self) -> &Arc<dyn ControlPort> {
        &self.control_port
    }
    
    pub fn event_port(&self) -> &Arc<dyn EventPort> {
        &self.event_port
    }
    
    pub fn script_port(&self) -> Option<&Arc<dyn ScriptPort>> {
        self.script_port.as_ref()
    }
    
    pub fn api_port(&self) -> Option<&Arc<dyn ApiPort>> {
        self.api_port.as_ref()
    }
    
    pub fn state_port(&self) -> Option<&Arc<dyn StatePort>> {
        self.state_port.as_ref()
    }
    
    pub fn ui_port(&self) -> Option<&Arc<dyn UiPort>> {
        self.ui_port.as_ref()
    }
    
    // ====================================
    // 服务容器管理方法 (Tauri 风格 API)
    // ====================================
    
    /// 注册服务到AppCore - Tauri风格的manage()方法
    /// 
    /// 这个方法提供了类型安全的依赖注入功能：
    /// - 编译时类型检查
    /// - 运行时动态查询
    /// - 支持任何 Send + Sync 的类型
    /// 
    /// # 示例
    /// ```rust
    /// // 注册服务
    /// app_core.manage(MyService::new()).await;
    /// 
    /// // 查询服务
    /// if let Some(service) = app_core.state::<MyService>().await {
    ///     service.do_something().await;
    /// }
    /// ```
    pub async fn manage<T: Any + Send + Sync>(&self, service: T) {
        self.services.register(service).await;
    }
    
    /// 从AppCore获取服务 - Tauri风格的state()方法
    /// 
    /// 通过类型参数获取之前注册的服务实例。
    /// 返回 Arc<T> 以支持多线程访问。
    /// 
    /// # 示例
    /// ```rust
    /// if let Some(rule_service) = app_core.state::<RuleService>().await {
    ///     let rules = rule_service.get_active_rules().await;
    /// }
    /// ```
    pub async fn state<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.services.get::<T>().await
    }
    
    /// 检查服务是否已注册
    pub async fn has_service<T: Any + Send + Sync>(&self) -> bool {
        self.services.contains::<T>().await
    }
    
    /// 获取已注册服务的数量
    pub async fn service_count(&self) -> usize {
        self.services.len().await
    }
    
    /// 获取处理引擎
    pub fn engine(&self) -> &Arc<ProcessingEngine> {
        &self.engine
    }

    /// 添加中间件到处理引擎
    pub async fn add_middleware(&self, middleware: Arc<dyn Middleware>) {
        self.engine.add_middleware(middleware);
    }
    
    /// 🎯 注册所有中间件服务（在 build() 后立即调用，不需要 sudo）
    /// 
    /// 分离注册和初始化两个步骤：
    /// - register_services: 注册服务到 AppCore（不需要 sudo）
    /// - initialize: 初始化中间件，如安装证书（可能需要 sudo）
    pub async fn register_middleware_services(&self) -> Result<(), TakeoverError> {
        info!("🔧 注册所有中间件服务到 AppCore...");
        
        let middlewares = self.engine.get_middlewares().read().unwrap().clone();
        for mw in &middlewares {
            if let Err(e) = mw.register_services(self).await {
                error!("❌ 中间件 '{}' 服务注册失败: {}", mw.name(), e);
                return Err(e);
            }
        }
        
        // 注意：全局AppCore单例已经在main.rs中通过AppCore::set_global()设置
        // 所有组件都可以通过AppCore::global()访问
        
        info!("✅ 所有中间件服务注册完成");
        Ok(())
    }
    
    /// 主运行循环 - 启动应用核心
    pub async fn run(&self) -> Result<(), TakeoverError> {
        info!("🚀 Starting AppCore");
        
        // 🛡️ 步骤1：初始化用户管理，创建代理用户（需要sudo权限）
        info!("🔧 初始化用户管理以防止回环");
        self.user_manager.ensure_proxy_user().await
            .map_err(|e| TakeoverError::Config(format!(
                "❌ 严重错误：创建代理用户失败：{}。\n\
                 💡 代理用户是防止回环的必需组件！\n\
                 🔧 解决方案：使用sudo权限运行以允许创建用户", e)))?;
        
        // 🔐 步骤2：初始化所有中间件（可能需要sudo，如TLS证书安装）
        info!("🔧 初始化所有中间件...");
        let middlewares = self.engine.get_middlewares().read().unwrap().clone();
        for mw in &middlewares {
            if let Err(e) = mw.initialize(self).await {
                error!("❌ 中间件 '{}' 初始化失败: {}", mw.name(), e);
                error!("💡 请确保以sudo权限运行程序（某些中间件如TLS需要安装系统证书）");
                return Err(e);
            }
        }
        info!("✅ 所有中间件初始化完成");
        
        // 设置运行状态
        self.running.store(true, std::sync::atomic::Ordering::SeqCst);
        
        // 更新统计信息
        {
            let mut stats = self.stats.write().await;
            stats.start_time = Some(Instant::now());
        }
        
        // 发布启动事件
        info!("AppCore started successfully");
        
        // 启动连接监听
        let mut connection_stream = self.inbound_port
            .listen(&ports::InboundConfig {
                listen_address: self.config.inbound.listen_address.clone(),
                listen_port: self.config.inbound.listen_port,
                max_connections: self.config.inbound.max_connections,
                buffer_size: self.config.inbound.buffer_size,
                timeout: self.config.inbound.timeout,
                intercepted_ports: self.config.inbound.intercepted_ports.clone(),
                bypass_user: self.config.inbound.bypass_user.clone(),
            })
            .await
            .map_err(|e| TakeoverError::Config(format!("Failed to start inbound listener: {}", e)))?;
        
        
        // 🛡️ 步骤2：永久切换到代理用户身份（避免每次连接都切换）
        info!("🔄 切换到代理用户以防止回环");
        self.user_manager.switch_to_proxy_user_permanently()
            .map_err(|e| TakeoverError::Config(format!(
                "❌ 严重错误：切换到代理用户失败：{}。\n\
                 💡 这会导致无限回环！为安全起见程序已终止。\n\
                 🔧 解决方案：使用sudo运行或确保_takeover_proxy用户存在并具有适当权限", e)))?;
        
        info!("✅ 成功切换到代理用户。出站连接将绕过pfctl拦截");

        info!("✅ AppCore started, listening for connections");
        
        // 主事件循环
        while self.is_running() {
            tokio::select! {
                // 处理新连接
                Some(connection_result) = connection_stream.next() => {
                    match connection_result {
                        Ok(connection) => {
                            if let Err(e) = self.handle_new_connection(connection).await {
                                error!("Failed to handle new connection: {}", e);
                            }
                        }
                        Err(e) => {
                            error!("Connection stream error: {}", e);
                        }
                    }
                }
                
                // 优雅关闭检查
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    // 检查是否需要清理过期连接
                    self.cleanup_expired_connections().await;
                }
            }
        }
        
        info!("🛑 AppCore shutting down");
        self.shutdown().await?;
        Ok(())
    }
    
    /// 处理新连接
    #[instrument(
        skip(self, connection), 
        fields(
            conn = %connection.id, 
            proc = %connection.metadata.process_name.as_deref().unwrap_or("unknown")
        )
    )]
    async fn handle_new_connection(&self, connection: Connection) -> Result<(), TakeoverError> {
        let conn_id = connection.id.clone();
        let source_addr = connection.source_addr;
        
        debug!("Handling new connection: {} from {}", conn_id, source_addr);
        
        // 创建连接上下文
        let target_port = connection.original_dest.port();
        
        // 优先使用协议解析器获取的真实主机名，回退到IP地址
        let target_host = connection.metadata.custom_data
            .get("target_host")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let ip_str = connection.original_dest.ip().to_string();
                debug!("No target_host in metadata, using IP address: {}", ip_str);
                ip_str
            });
        
        debug!("Target host resolved: {} (port: {})", target_host, target_port);
        
        let connection_info = connection.clone_info(); // 为事件和跟踪保存连接信息
        let context = ConnectionContext::new(connection, target_host, target_port); // move connection
        
        // 创建连接跟踪器
        let tracker = ConnectionTracker::new(context.clone());
        
        // 注册连接
        {
            let mut connections = self.active_connections.write().await;
            connections.insert(conn_id.clone(), tracker);
        }
        
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.total_connections += 1;
            stats.active_connections = self.active_connections.read().await.len();
        }
        
        // 发布连接事件
        let _ = self.event_port.publish(ports::DomainEvent::ConnectionNew {
            id: conn_id.clone(),
            addr: source_addr,
            metadata: connection_info.metadata,
        }).await;
        
        // 为连接创建独立的处理任务
        Self::spawn_connection_handler(self.clone(), context);
        
        Ok(())
    }
    
    /// 创建连接处理任务的静态方法
    fn spawn_connection_handler(app_core: AppCore, context: ConnectionContext) {
        tokio::spawn(async move {
            let conn_id = context.id.clone();
            
            // 在异步任务内部获取许可，避免生命周期问题
            let permit = match app_core.connection_semaphore.acquire().await {
                Ok(permit) => permit,
                Err(_) => {
                    error!("Failed to acquire connection permit for {}", conn_id);
                    return;
                }
            };
            
            let _result = app_core.handle_connection_lifecycle(context).await;
            
            // 释放连接许可
            drop(permit);
            // 清理连接
            app_core.cleanup_connection(conn_id).await;
        });
    }
    
    /// 处理连接的完整生命周期
    #[instrument(
        skip(self, context), 
        fields(
            conn = %context.id, 
            proc = %context.connection.metadata.process_name.as_deref().unwrap_or("unknown")
        )
    )]
    async fn handle_connection_lifecycle(&self, mut context: ConnectionContext) -> Result<(), TakeoverError> {
        let conn_id = context.id.clone();
        
        debug!("Starting connection lifecycle for {}", conn_id);
        
        // 🚀 使用预构建的默认pipeline（高效）
        let pipeline = self.engine.get_default_pipeline();
        
        // 执行处理管道
        match pipeline.process(&mut context, self).await {
            Ok(MiddlewareResult::Continue) => {
                debug!("Connection {} processed successfully", conn_id);
                self.update_connection_status(conn_id, ConnectionStatus::Completed).await;
            }
            Ok(MiddlewareResult::Stop) => {
                debug!("Connection {} stopped by middleware (e.g., HTTP/2 manager took over)", conn_id);
                self.update_connection_status(conn_id, ConnectionStatus::Completed).await;
            }
            Ok(MiddlewareResult::Pause(reason)) => {
                warn!("Connection {} paused: {}", conn_id, reason);
                self.update_connection_status(conn_id, ConnectionStatus::Paused).await;
                
                // 等待用户干预或超时
                self.handle_paused_connection(context, reason).await?;
            }
            Ok(MiddlewareResult::Reject(reason)) => {
                debug!("Connection {} rejected: {}", conn_id, reason);
                self.update_connection_status(conn_id, ConnectionStatus::Error(reason.clone())).await;
                return Err(TakeoverError::Config(reason));
            }
            Ok(MiddlewareResult::Redirect(url)) => {
                debug!("Connection {} redirected to: {}", conn_id, url);
                // 处理重定向逻辑
                self.handle_redirect(context, url).await?;
            }
            Err(e) => {
                debug!(" Connection {} processing failed: {}", conn_id, e);
                self.update_connection_status(conn_id, ConnectionStatus::Error(e.to_string())).await;
                return Err(e);
            }
        }
        
        Ok(())
    }
    
    /// 处理暂停的连接（等待用户干预）
    /// [TODO] 实现用户干预逻辑
    async fn handle_paused_connection(&self, context: ConnectionContext, _reason: String) -> Result<(), TakeoverError> {
        if let Some(ui_port) = &self.ui_port {
            // 通知UI有待处理的连接
            if let Some(request) = &context.request {
                let _ = ui_port.notify_pending_approval(
                    request.id.clone().unwrap_or_else(|| Uuid::new_v4().to_string()),
                    request
                ).await;
                
                // 更新统计
                {
                    let mut stats = self.stats.write().await;
                    stats.user_interventions += 1;
                }
            }
        }
        
        // 实际的暂停逻辑可以在这里实现
        // 比如等待用户决策、超时处理等
        
        Ok(())
    }
    
    /// 处理重定向
    async fn handle_redirect(&self, mut context: ConnectionContext, url: String) -> Result<(), TakeoverError> {
        // 修改上下文中的目标URL
        if let Some(ref mut request) = context.request {
            request.url = url;
        }
        
        // 重新进入处理管道
        let pipeline = self.engine.get_default_pipeline();
        pipeline.process(&mut context, self).await?;
        
        Ok(())
    }
    
    /// 更新连接状态
    async fn update_connection_status(&self, conn_id: ConnectionId, status: ConnectionStatus) {
        if let Some(tracker) = self.active_connections.write().await.get_mut(&conn_id) {
            tracker.set_status(status);
        }
    }
    
    /// 清理连接
    async fn cleanup_connection(&self, conn_id: ConnectionId) {
        let duration = {
            let mut connections = self.active_connections.write().await;
            if let Some(tracker) = connections.remove(&conn_id) {
                tracker.created_at.elapsed()
            } else {
                Duration::from_secs(0)
            }
        };
        
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.active_connections = self.active_connections.read().await.len();
        }
        
        // 发布连接关闭事件
        let _ = self.event_port.publish(ports::DomainEvent::ConnectionClosed {
            id: conn_id.clone(),
            duration,
            bytes_transferred: 0, // 默认值，后续可以从上下文获取
        }).await;
        
        debug!("Connection {} cleaned up", &conn_id);
    }
    
    /// 清理过期连接
    async fn cleanup_expired_connections(&self) {
        let timeout = self.config.general.connection_timeout;
        let now = Instant::now();
        let mut expired_connections = Vec::new();
        let mut total = 0;
        let mut expired=0;
        
        {
            let connections = self.active_connections.read().await;
            
            for (conn_id, tracker) in connections.iter() {
                total +=1;
                if now.duration_since(tracker.last_activity) > timeout {
                    expired_connections.push(conn_id.clone());
                }
            }
        }
        
        for conn_id in expired_connections {
            expired +=1;
            debug!("Connection {} expired, cleaning up", conn_id);
            self.cleanup_connection(conn_id).await;
        }
        if expired > 0{
            info!(" 共{}连接，清理:{}", total, expired );
        }

    }
    
    /// 检查是否正在运行
    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::SeqCst)
    }
    
    /// 请求停止
    pub async fn request_stop(&self) {
        info!("Stop requested for AppCore");
        self.running.store(false, std::sync::atomic::Ordering::SeqCst);
    }
    
    /// 优雅关闭
    pub async fn shutdown(&self) -> Result<(), TakeoverError> {
        info!("Shutting down AppCore");
        
        // 停止接受新连接
        if let Err(e) = self.inbound_port.stop().await {
            warn!("Failed to stop inbound port: {}", e);
        }
        
        // 等待活跃连接完成（最多等待30秒）
        let shutdown_timeout = Duration::from_secs(10);
        let start = Instant::now();
        
        while start.elapsed() < shutdown_timeout {
            let active_count = self.active_connections.read().await.len();
            if active_count == 0 {
                break;
            }
            
            info!("Waiting for {} active connections to finish", active_count);
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        
        // 强制清理剩余连接
        {
            let mut connections = self.active_connections.write().await;
            let remaining_count = connections.len();
            if remaining_count > 0 {
                warn!("Forcefully closing {} remaining connections", remaining_count);
                connections.clear();
            }
        }
        
        info!("✅ AppCore shutdown completed");
        Ok(())
    }
    
    /// 获取应用统计信息
    pub async fn get_stats(&self) -> AppStats {
        let stats_guard = self.stats.read().await;
        let mut stats = (*stats_guard).clone();
        stats.active_connections = self.active_connections.read().await.len();
        stats
    }
    
    /// 获取活跃连接摘要
    pub async fn get_active_connections(&self) -> Vec<ConnectionSummary> {
        let connections = self.active_connections.read().await;
        connections
            .values()
            .map(|tracker| ConnectionSummary {
                id: tracker.context.id.clone(),
                source_addr: tracker.context.connection.source_addr,
                target_host: tracker.context.target_host.clone(),
                target_port: tracker.context.target_port,
                protocol: tracker.context.connection.metadata.protocol.clone(),
                status: tracker.status.clone(),
                bytes_sent: tracker.bytes_sent,
                bytes_received: tracker.bytes_received,
                created_at: std::time::SystemTime::now() - tracker.created_at.elapsed(),
                last_activity: Some(std::time::SystemTime::now() - tracker.last_activity.elapsed()),
            })
            .collect()
    }
    
    /// 通过连接ID获取连接上下文
    pub async fn get_connection_context(&self, conn_id: ConnectionId) -> Option<ConnectionContext> {
        self.active_connections
            .read()
            .await
            .get(&conn_id)
            .map(|tracker| tracker.context.clone())
    }
    
    /// 更新连接的传输字节数
    pub async fn update_connection_bytes(&self, conn_id: ConnectionId, sent: u64, received: u64) {
        if let Some(tracker) = self.active_connections.write().await.get_mut(&conn_id) {
            tracker.add_bytes_sent(sent);
            tracker.add_bytes_received(received);
        }
        
        // 更新全局统计
        {
            let mut stats = self.stats.write().await;
            stats.total_bytes_transferred += sent + received;
        }
    }
    
    /// 记录请求处理
    pub async fn record_request_processed(&self) {
        let mut stats = self.stats.write().await;
        stats.requests_processed += 1;
    }
    
    /// 记录脚本执行
    pub async fn record_script_executed(&self) {
        let mut stats = self.stats.write().await;
        stats.scripts_executed += 1;
    }
    
    /// 记录错误
    pub async fn record_error(&self) {
        let mut stats = self.stats.write().await;
        stats.error_count += 1;
    }
}

// 实现Clone trait以支持在异步任务中使用
impl Clone for AppCore {
    fn clone(&self) -> Self {
        Self {
            config: Arc::clone(&self.config),
            inbound_port: Arc::clone(&self.inbound_port),
            outbound_port: Arc::clone(&self.outbound_port),
            control_port: Arc::clone(&self.control_port),
            event_port: Arc::clone(&self.event_port),
            script_port: self.script_port.as_ref().map(Arc::clone),
            api_port: self.api_port.as_ref().map(Arc::clone),
            state_port: self.state_port.as_ref().map(Arc::clone),
            ui_port: self.ui_port.as_ref().map(Arc::clone),
            engine: Arc::clone(&self.engine),
            user_manager: Arc::clone(&self.user_manager),
            services: self.services.clone(),
            active_connections: Arc::clone(&self.active_connections),
            connection_semaphore: Arc::clone(&self.connection_semaphore),
            running: Arc::clone(&self.running),
            stats: Arc::clone(&self.stats),
        }
    }
}

/// AppCore构造器
/// 
/// 职责清晰的架构组装：
/// - Engine 管理 Middlewares（处理管道）
/// - AppCore 管理 Services（公共依赖注入容器）
/// - Builder 负责组装和配置
pub struct AppCoreBuilder {
    config: Arc<AppConfig>,
    inbound_port: Arc<dyn InboundPort>,
    outbound_port: Arc<dyn OutboundPort>,
    control_port: Arc<dyn ControlPort>,
    event_port: Arc<dyn EventPort>,
    api_port: Option<Arc<dyn ApiPort>>,
    engine: ProcessingEngine,  // Engine 自己管理 middlewares
    // 核心服务（将通过 manage() 注册到 AppCore）
    services: Vec<(String, Arc<dyn Any + Send + Sync>)>,
}

impl AppCoreBuilder {
    /// 创建一个新的 AppCoreBuilder
    pub fn new(
        config: AppConfig,
        inbound: Arc<dyn InboundPort>,
        outbound: Arc<dyn OutboundPort>,
        control: Arc<dyn ControlPort>,
        event: Arc<dyn EventPort>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            inbound_port: inbound,
            outbound_port: outbound,
            control_port: control,
            event_port: event,
            api_port: None,
            engine: ProcessingEngine::new(),  // 创建空引擎
            services: Vec::new(),
        }
    }

    /// 设置 API 端口
    pub fn with_api_port(mut self, api_port: Arc<dyn ApiPort>) -> Self {
        self.api_port = Some(api_port);
        self
    }
    
    /// 使用已配置的 Engine（推荐方式）
    /// Engine 自己管理 middlewares
    pub fn with_engine(mut self, engine: ProcessingEngine) -> Self {
        self.engine = engine;
        self
    }
    
    /// 使用默认中间件初始化 Engine（便捷方法）
    /// 
    /// 🏗️ 职责：Builder负责传递配置，Engine负责创建中间件
    /// 
    /// # 设计原则
    /// - Builder传递完整的AppConfig给Engine
    /// - Engine负责创建所有中间件实例
    /// - 每个Middleware从AppConfig中提取自己需要的配置
    pub fn with_default_middlewares(self) -> Self {
        // 🎯 传递完整的AppConfig，不挑选特定配置
        let middlewares = ProcessingEngine::create_default_middlewares_with_config(&self.config);
        
        for middleware in middlewares {
            self.engine.add_middleware(middleware);
        }
        
        info!("✅ 默认中间件已加载 (配置从AppConfig提取)");
        self
    }
    
    /// 注册服务到 AppCore（Tauri 风格）
    /// Service 是公共的，谁都能注册
    pub fn manage<T: Any + Send + Sync>(mut self, service: Arc<T>) -> Self {
        let type_name = std::any::type_name::<T>().to_string();
        self.services.push((type_name, service as Arc<dyn Any + Send + Sync>));
        self
    }

    /// 构建 AppCore 实例
    pub async fn build(self) -> AppCore {
        // 1. 创建 AppCore 实例（Engine 已经配置好了）
        let mut app_core = AppCore::new(
            self.config,
            self.inbound_port,
            self.outbound_port,
            self.control_port,
            self.event_port,
            self.engine,  // Engine 自己管理 middlewares
        );
        
        // 2. 设置 API 端口（如果提供）
        app_core.api_port = self.api_port;

        // 3. 注册所有服务到 AppCore（公共服务容器）
        for (_name, service) in self.services {
            // 使用 downcast 来注册服务
            if let Ok(state_manager) = service.clone().downcast::<services::StateManager>() {
                app_core.services.register_arc(state_manager).await;
                info!("✅ StateManager registered via builder");
            } else if let Ok(replay_manager) = service.clone().downcast::<services::ReplayManager>() {
                app_core.services.register_arc(replay_manager).await;
                info!("✅ ReplayManager registered via builder");
            }
            // 可以继续添加其他服务类型的判断
        }

        // 注意：中间件初始化推迟到 AppCore.run() 时进行
        // 原因：某些中间件的初始化需要sudo权限（如TLS证书安装）
        // 在 build() 时初始化太早，应该在真正启动时才初始化

        app_core
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    
    // Mock implementations for testing
    struct MockInboundPort;
    struct MockOutboundPort;
    struct MockControlPort;
    struct MockEventPort;
    
    #[async_trait::async_trait]
    impl InboundPort for MockInboundPort {
        async fn listen(&self, _config: &InboundConfig) -> Result<Pin<Box<dyn futures::Stream<Item = Result<Connection, AppError>> + Send>>, AppError> {
            // Return empty stream for testing
            Ok(Box::pin(futures::stream::empty()))
        }
        
        async fn stop(&self) -> Result<(), AppError> { Ok(()) }
        async fn status(&self) -> AdapterStatus {
            AdapterStatus {
                name: "mock".to_string(),
                active: true,
                connections: 0,
                last_activity: None,
                errors: 0,
            }
        }
        fn name(&self) -> &str { "mock" }
    }
    
    #[async_trait::async_trait]
    impl OutboundPort for MockOutboundPort {
        async fn connect(&self, _context: &ConnectionContext) -> Result<Box<dyn crate::core::ports::NetworkStream>, AppError> {
            unimplemented!()
        }
        async fn supports_target(&self, _context: &ConnectionContext) -> bool { true }
        fn name(&self) -> &str { "mock" }
    }
    
    #[async_trait::async_trait]
    impl ControlPort for MockControlPort {
        async fn apply_rules(&self, _rules: Vec<Rule>) -> Result<(), AppError> { Ok(()) }
        async fn get_active_connections(&self) -> Result<Vec<ConnectionSummary>, AppError> { Ok(vec![]) }
        async fn intercept_connection(&self, _conn_id: ConnectionId) -> Result<(), AppError> { Ok(()) }
        async fn approve_request(&self, _request_id: RequestId, _modifications: Option<RequestModifications>) -> Result<(), AppError> { Ok(()) }
        async fn reject_request(&self, _request_id: RequestId, _reason: String) -> Result<(), AppError> { Ok(()) }
        async fn trigger_replay(&self, _request_id: RequestId, _modifications: RequestModifications) -> Result<ReplayResult, AppError> {
            unimplemented!()
        }
        async fn get_active_rules(&self) -> Result<Vec<Rule>, AppError> { Ok(vec![]) }
    }
    
    #[async_trait::async_trait]
    impl EventPort for MockEventPort {
        async fn publish(&self, _event: DomainEvent) -> Result<(), AppError> { Ok(()) }
        async fn subscribe(&self) -> Result<Pin<Box<dyn futures::Stream<Item = DomainEvent> + Send>>, AppError> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }
    
    async fn create_test_app_core() -> AppCore {
        let config = AppConfig::default();
        let inbound = Arc::new(MockInboundPort);
        let outbound = Arc::new(MockOutboundPort);
        let control = Arc::new(MockControlPort);
        let event = Arc::new(MockEventPort);
        
        AppCore::builder(config, inbound, outbound, control, event).build().await
    }
    
    #[tokio::test]
    async fn test_app_core_creation() {
        let app_core = create_test_app_core().await;
        assert!(!app_core.is_running());
        
        let stats = app_core.get_stats().await;
        assert_eq!(stats.total_connections, 0);
        assert_eq!(stats.active_connections, 0);
    }
    
    #[tokio::test]
    async fn test_connection_tracking() {
        let app_core = create_test_app_core().await;
        
        // 创建测试连接
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        
        let conn_id = connection.id.clone();
        
        // 模拟处理连接
        // 这里需要实际的连接处理逻辑，目前只测试基本功能
        
        let context = app_core.get_connection_context(conn_id).await;
        assert!(context.is_none()); // 因为连接还没有被注册
    }
}

/// 简单的事件端口实现
pub struct SimpleEventPort {
    // 可以添加事件监听器等
}

impl SimpleEventPort {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl EventPort for SimpleEventPort {
    async fn publish(&self, event: ports::DomainEvent) -> Result<(), AppError> {
        // 简单的日志输出，实际实现可以发送到事件总线
        debug!("Event published: {:?}", event);
        Ok(())
    }
    
    async fn subscribe(&self) -> Result<Pin<Box<dyn futures::Stream<Item = DomainEvent> + Send>>, AppError> {
        // 返回空流，实际实现应该提供真正的事件流
        Ok(Box::pin(futures::stream::empty()))
    }
}
