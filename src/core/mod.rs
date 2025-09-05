//! 核心业务逻辑模块
//! 
//! 这是六边形架构的核心"六边形"，包含：
//! - AppCore: 主要的业务编排器
//! - 核心业务服务的协调逻辑
//! - 与外部世界的交互通过端口接口进行

use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::{RwLock, Semaphore};
use tokio::time::{Duration, Instant};
use futures::StreamExt;
use tracing::{info, warn, error, debug, instrument};
use uuid::Uuid;
use async_trait::async_trait;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;

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
    RuleCondition, RequestModifications, RequestModification, ModificationType,
    DomainEvent, ErrorCategory, RoutingAction, MergeStrategy,
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
    /// 创建新的应用核心实例
    pub fn new(
        config: AppConfig,
        inbound_port: Arc<dyn InboundPort>,
        outbound_port: Arc<dyn OutboundPort>,
        control_port: Arc<dyn ControlPort>,
        event_port: Arc<dyn EventPort>,
    ) -> Self {
        let config = Arc::new(config);
        let max_connections = config.general_config.max_connections;
        
        // 🔧 创建处理引擎并注册默认中间件
        let engine = ProcessingEngine::new_with_defaults();
        
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
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            connection_semaphore: Arc::new(Semaphore::new(max_connections)),
            running: Arc::new(AtomicBool::new(false)),
            stats: Arc::new(RwLock::new(AppStats::default())),
        }
    }
    
    
    /// 创建完整配置的应用核心实例（用于main.rs）
    pub fn new_with_all_ports(
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
        let max_connections = config.general_config.max_connections;
        
        // 创建处理引擎并设置中间件
        let mut engine = ProcessingEngine::new();
        for middleware in middlewares {
            engine.add_middleware(middleware);
        }
        
        // 创建一个简单的事件端口实现
        let event_port = Arc::new(SimpleEventPort::new());
        
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
            active_connections: Arc::new(RwLock::new(HashMap::new())),
            connection_semaphore: Arc::new(Semaphore::new(max_connections)),
            running: Arc::new(AtomicBool::new(false)),
            stats: Arc::new(RwLock::new(AppStats::default())),
        }
    }
    
    /// 设置可选的端口适配器
    pub fn with_script_port(mut self, script_port: Arc<dyn ScriptPort>) -> Self {
        self.script_port = Some(script_port);
        self
    }
    
    pub fn with_api_port(mut self, api_port: Arc<dyn ApiPort>) -> Self {
        self.api_port = Some(api_port);
        self
    }
    
    pub fn with_state_port(mut self, state_port: Arc<dyn StatePort>) -> Self {
        self.state_port = Some(state_port);
        self
    }
    
    pub fn with_ui_port(mut self, ui_port: Arc<dyn UiPort>) -> Self {
        self.ui_port = Some(ui_port);
        self
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
    
    /// 获取处理引擎
    pub fn engine(&self) -> &Arc<ProcessingEngine> {
        &self.engine
    }
    
    /// 主运行循环 - 启动应用核心
    #[instrument(skip(self))]
    pub async fn run(&self) -> Result<(), TakeoverError> {
        info!("🚀 Starting AppCore");
        
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
                listen_address: self.config.inbound_config.listen_address.clone(),
                listen_port: self.config.inbound_config.listen_port,
                max_connections: self.config.inbound_config.max_connections,
                buffer_size: self.config.inbound_config.buffer_size,
                timeout: self.config.inbound_config.timeout,
                intercepted_ports: self.config.inbound_config.intercepted_ports.clone(),
                bypass_user: self.config.inbound_config.bypass_user.clone(),
            })
            .await
            .map_err(|e| TakeoverError::Config(format!("Failed to start inbound listener: {}", e)))?;
        
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
    #[instrument(skip(self, connection))]
    async fn handle_new_connection(&self, connection: Connection) -> Result<(), TakeoverError> {
        let conn_id = connection.id;
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
            connections.insert(conn_id, tracker);
        }
        
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.total_connections += 1;
            stats.active_connections = self.active_connections.read().await.len();
        }
        
        // 发布连接事件
        let _ = self.event_port.publish(ports::DomainEvent::ConnectionNew {
            id: conn_id,
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
            let conn_id = context.id;
            
            // 在异步任务内部获取许可，避免生命周期问题
            let permit = match app_core.connection_semaphore.acquire().await {
                Ok(permit) => permit,
                Err(_) => {
                    error!("Failed to acquire connection permit for {}", conn_id);
                    return;
                }
            };
            
            let result = app_core.handle_connection_lifecycle(context).await;
            
            // 释放连接许可
            drop(permit);
            
            if let Err(e) = result {
                error!("Connection {} failed: {}", conn_id, e);
            }
            
            // 清理连接
            app_core.cleanup_connection(conn_id).await;
        });
    }
    
    /// 处理连接的完整生命周期
    #[instrument(skip(self, context))]
    async fn handle_connection_lifecycle(&self, mut context: ConnectionContext) -> Result<(), TakeoverError> {
        let conn_id = context.id;
        
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
                warn!("Connection {} rejected: {}", conn_id, reason);
                self.update_connection_status(conn_id, ConnectionStatus::Error(reason.clone())).await;
                return Err(TakeoverError::Config(reason));
            }
            Ok(MiddlewareResult::Redirect(url)) => {
                debug!("Connection {} redirected to: {}", conn_id, url);
                // 处理重定向逻辑
                self.handle_redirect(context, url).await?;
            }
            Err(e) => {
                error!("Connection {} processing failed: {}", conn_id, e);
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
                    request.id.unwrap_or_else(|| Uuid::new_v4()),
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
            id: conn_id,
            duration,
            bytes_transferred: 0, // 默认值，后续可以从上下文获取
        }).await;
        
        debug!("Connection {} cleaned up", conn_id);
    }
    
    /// 清理过期连接
    async fn cleanup_expired_connections(&self) {
        let timeout = self.config.general.connection_timeout;
        let now = Instant::now();
        let mut expired_connections = Vec::new();
        
        {
            let connections = self.active_connections.read().await;
            for (conn_id, tracker) in connections.iter() {
                if now.duration_since(tracker.last_activity) > timeout {
                    expired_connections.push(*conn_id);
                }
            }
        }
        
        for conn_id in expired_connections {
            warn!("Connection {} expired, cleaning up", conn_id);
            self.cleanup_connection(conn_id).await;
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
        let shutdown_timeout = Duration::from_secs(30);
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
                id: tracker.context.id,
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
            active_connections: Arc::clone(&self.active_connections),
            connection_semaphore: Arc::clone(&self.connection_semaphore),
            running: Arc::clone(&self.running),
            stats: Arc::clone(&self.stats),
        }
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
    
    fn create_test_app_core() -> AppCore {
        let config = AppConfig::default();
        let inbound = Arc::new(MockInboundPort);
        let outbound = Arc::new(MockOutboundPort);
        let control = Arc::new(MockControlPort);
        let event = Arc::new(MockEventPort);
        
        AppCore::new(config, inbound, outbound, control, event)
    }
    
    #[tokio::test]
    async fn test_app_core_creation() {
        let app_core = create_test_app_core();
        assert!(!app_core.is_running());
        
        let stats = app_core.get_stats().await;
        assert_eq!(stats.total_connections, 0);
        assert_eq!(stats.active_connections, 0);
    }
    
    #[tokio::test]
    async fn test_connection_tracking() {
        let app_core = create_test_app_core();
        
        // 创建测试连接
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        
        let conn_id = connection.id;
        
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
