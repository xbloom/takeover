//! 核心端口接口定义
//! 
//! 这个模块定义了六边形架构中的所有端口接口。
//! 端口定义了核心业务逻辑与外部世界交互的契约。
//! 
//! 注意：这个模块只定义接口（traits），所有数据结构在 model.rs 中定义

use async_trait::async_trait;
use futures::stream::Stream;
use std::pin::Pin;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use std::net::SocketAddr;
use serde::{Serialize, Deserialize};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::TakeoverError;

// 重新导出现有的一些类型（在重构完成前）
pub use crate::config::{ProxyConfig, ProxyType};

// 导入并重新导出 model 中的所有数据结构，使其可以通过 ports 模块访问
pub use crate::core::model::{
    // 基础类型
    ConnectionId, RequestId,
    // 连接相关
    Connection, ConnectionMetadata, ConnectionContext, ConnectionSummary, ConnectionStatus, AdapterStatus,
    // HTTP相关  
    HttpRequest, HttpResponse, RequestModifications,
    // 配置相关
    AppConfig, InboundConfig, OutboundConfig, ScriptConfig, GeneralConfig,
    // 规则相关
    Rule, RulePattern, RuleAction, RuleCondition,
    // 处理相关
    ProcessingState, DomainEvent, ErrorCategory,
    // 脚本相关
    TriggerPoint, TriggerCondition, CompositeRequestConfig, MergeStrategy, CustomEvent, LogLevel,
    // 重放相关
    ReplayResult,
};

/// 应用错误类型别名
pub type AppError = TakeoverError;

/// 合并会话标识符 (接口专用)
pub type MergeSessionId = uuid::Uuid;

/// 网络流抽象 (接口专用)
pub trait NetworkStream: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static {}



// ================================
// 端口接口定义
// ================================

/// 入站端口：负责监听并产生新的连接流
#[async_trait]
pub trait InboundPort: Send + Sync {
    /// 开始监听连接
    async fn listen(
        &self,
        config: &InboundConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Connection, AppError>> + Send>>, AppError>;
    
    /// 停止监听
    async fn stop(&self) -> Result<(), AppError>;
    
    /// 获取适配器状态
    async fn status(&self) -> AdapterStatus;
    
    /// 适配器名称
    fn name(&self) -> &str;
}

/// 出站端口：负责根据上下文建立到目标服务器的连接
/// 
/// # 全局AppCore访问
/// 适配器可以通过`AppCore::global()`访问全局AppCore实例来获取服务：
/// ```
/// let app_core = AppCore::global();
/// let tls_service = app_core.state::<TlsService>().await?;
/// ```
#[async_trait]
pub trait OutboundPort: Send + Sync {
    /// 建立出站连接
    async fn connect(&self, context: &ConnectionContext) -> Result<Box<dyn NetworkStream>, AppError>;
    
    /// 检查是否支持目标地址
    async fn supports_target(&self, context: &ConnectionContext) -> bool;
    
    /// 适配器名称
    fn name(&self) -> &str;
}

/// 控制端口：负责接收来自管理界面(UI)的命令
/// 
/// # 全局AppCore访问
/// 适配器可以通过`AppCore::global()`访问全局AppCore实例来获取服务：
/// ```
/// let app_core = AppCore::global();
/// let rule_service = app_core.state::<RuleService>().await?;
/// ```
#[async_trait]
pub trait ControlPort: Send + Sync {
    /// 应用规则配置
    async fn apply_rules(&self, rules: Vec<Rule>) -> Result<(), AppError>;
    
    /// 获取活跃连接列表
    async fn get_active_connections(&self) -> Result<Vec<ConnectionSummary>, AppError>;
    
    /// 拦截指定连接
    async fn intercept_connection(&self, conn_id: ConnectionId) -> Result<(), AppError>;
    
    /// 批准请求（可选修改）
    async fn approve_request(
        &self, 
        request_id: RequestId, 
        modifications: Option<RequestModifications>
    ) -> Result<(), AppError>;
    
    /// 拒绝请求
    async fn reject_request(&self, request_id: RequestId, reason: String) -> Result<(), AppError>;
    
    /// 触发重放
    async fn trigger_replay(
        &self, 
        request_id: RequestId, 
        modifications: RequestModifications
    ) -> Result<ReplayResult, AppError>;
    
    /// 获取当前规则
    async fn get_active_rules(&self) -> Result<Vec<Rule>, AppError>;
}

/// 事件端口：负责将核心产生的领域事件发布出去
#[async_trait]
pub trait EventPort: Send + Sync {
    /// 发布领域事件
    async fn publish(&self, event: DomainEvent) -> Result<(), AppError>;
    
    /// 订阅事件（返回事件流）
    async fn subscribe(&self) -> Result<Pin<Box<dyn Stream<Item = DomainEvent> + Send>>, AppError>;
}

/// 脚本端口：负责执行Lua脚本
#[async_trait]
pub trait ScriptPort: Send + Sync {
    /// 执行脚本并返回结果
    async fn execute_script(
        &self, 
        script_name: &str, 
        context: &mut ConnectionContext,
        input: ScriptInput
    ) -> Result<ScriptOutput, AppError>;
    
    /// 加载或重新加载脚本
    async fn load_script(&self, script_name: &str, script_content: &str) -> Result<(), AppError>;
    
    /// 获取可用的脚本列表
    async fn list_scripts(&self) -> Result<Vec<ScriptInfo>, AppError>;
    
    /// 验证脚本语法
    async fn validate_script(&self, script_content: &str) -> Result<(), AppError>;
    
    /// 添加脚本触发器
    async fn add_trigger(&self, trigger: ScriptTrigger) -> Result<(), AppError>;
    
    /// 移除脚本触发器
    async fn remove_trigger(&self, script_name: &str) -> Result<(), AppError>;
}

/// API端口：为Lua脚本提供系统功能访问
#[async_trait]
pub trait ApiPort: Send + Sync {
    /// 发起HTTP请求
    async fn http_request(&self, request: HttpRequest) -> Result<HttpResponse, AppError>;
    
    /// 获取连接信息
    async fn get_connection_info(&self, conn_id: ConnectionId) -> Result<Option<ConnectionInfo>, AppError>;
    
    /// 记录日志
    async fn log(&self, level: LogLevel, message: &str) -> Result<(), AppError>;
    
    /// 获取配置值
    async fn get_config(&self, key: &str) -> Result<Option<String>, AppError>;
    
    /// 存储临时数据
    async fn store_data(&self, key: &str, value: serde_json::Value) -> Result<(), AppError>;
    
    /// 获取临时数据
    async fn get_data(&self, key: &str) -> Result<Option<serde_json::Value>, AppError>;
    
    /// 触发自定义事件
    async fn emit_event(&self, event: CustomEvent) -> Result<(), AppError>;
}

/// 状态端口：负责请求响应的缓存、重放和合并
#[async_trait]
pub trait StatePort: Send + Sync {
    /// 缓存请求以供重放
    async fn buffer_request(
        &self, 
        request: HttpRequest, 
        context: &ConnectionContext
    ) -> Result<RequestId, AppError>;
    
    /// 获取缓存的请求
    async fn get_buffered_request(&self, request_id: RequestId) -> Result<Option<BufferedRequest>, AppError>;
    
    /// 执行请求重放
    async fn replay_request(
        &self, 
        request_id: RequestId, 
        modifications: RequestModifications
    ) -> Result<HttpResponse, AppError>;
    
    /// 开始响应合并会话
    async fn start_response_merge(&self, primary_response: HttpResponse) -> Result<MergeSessionId, AppError>;
    
    /// 添加额外响应到合并会话
    async fn add_additional_response(
        &self, 
        session_id: MergeSessionId, 
        response: HttpResponse
    ) -> Result<(), AppError>;
    
    /// 完成响应合并
    async fn finalize_merge(
        &self, 
        session_id: MergeSessionId, 
        rules: MergeRules
    ) -> Result<HttpResponse, AppError>;
    
    /// 跟踪连接状态
    async fn track_connection(&self, context: &ConnectionContext) -> Result<(), AppError>;
    
    /// 获取连接状态
    async fn get_connection_state(&self, conn_id: ConnectionId) -> Result<Option<ConnectionState>, AppError>;
}

/// UI端口：负责与用户界面的实时交互
#[async_trait]
pub trait UiPort: Send + Sync {
    /// 推送连接事件到UI
    async fn push_connection_event(&self, event: ConnectionEvent) -> Result<(), AppError>;
    
    /// 推送请求详情到UI
    async fn push_request_details(&self, request: RequestDetails) -> Result<(), AppError>;
    
    /// 推送响应详情到UI
    async fn push_response_details(&self, response: ResponseDetails) -> Result<(), AppError>;
    
    /// 请求用户决策
    async fn request_user_decision(&self, decision_request: UserDecisionRequest) -> Result<UserDecision, AppError>;
    
    /// 通知待批准的请求
    async fn notify_pending_approval(&self, request_id: RequestId, request: &HttpRequest) -> Result<(), AppError>;
    
    /// 获取UI连接状态
    async fn is_connected(&self) -> bool;
}

// ================================
// 支持数据结构
// ================================



/// 脚本输入数据 (接口专用)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptInput {
    pub request: Option<HttpRequest>,
    pub response: Option<HttpResponse>,
    pub connection_context: ConnectionContext,
    pub custom_data: HashMap<String, serde_json::Value>,
}

/// 脚本输出数据 (接口专用)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptOutput {
    pub action: ScriptAction,
    pub modified_request: Option<HttpRequest>,
    pub modified_response: Option<HttpResponse>,
    pub additional_requests: Vec<HttpRequest>,
    pub custom_data: HashMap<String, serde_json::Value>,
}

/// 脚本执行动作 (接口专用)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScriptAction {
    Continue,
    Pause(String),
    Reject(String),
    Redirect(String),
    CompositeRequest(CompositeRequestConfig),
    Custom(HashMap<String, serde_json::Value>),
}

/// 脚本信息 (接口专用)
#[derive(Debug, Clone)]
pub struct ScriptInfo {
    pub name: String,
    pub description: String,
    pub version: String,
    pub size: usize,
    pub enabled: bool,
    pub loaded_at: Option<SystemTime>,
    pub last_executed: Option<SystemTime>,
    pub execution_count: usize,
}

/// 脚本触发器 (接口专用)
#[derive(Debug, Clone)]
pub struct ScriptTrigger {
    pub script_name: String,
    pub trigger_point: TriggerPoint,
    pub conditions: Vec<TriggerCondition>,
    pub enabled: bool,
}

/// 连接信息 (接口专用)
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub id: ConnectionId,
    pub source_addr: SocketAddr,
    pub target_addr: SocketAddr,
    pub protocol: String,
    pub tls_info: Option<TlsInfo>,
    pub process_info: Option<ProcessInfo>,
}

/// TLS信息 (接口专用)
#[derive(Debug, Clone)]
pub struct TlsInfo {
    pub version: String,
    pub cipher_suite: String,
    pub server_name: Option<String>,
}

/// 进程信息 (接口专用)
#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub path: String,
    pub user: String,
}

/// 缓存的请求 (接口专用)
#[derive(Debug, Clone)]
pub struct BufferedRequest {
    pub id: RequestId,
    pub original: HttpRequest,
    pub context: ConnectionContext,
    pub timestamp: SystemTime,
    pub status: RequestStatus,
    pub replay_count: usize,
}

/// 请求状态 (接口专用)
#[derive(Debug, Clone)]
pub enum RequestStatus {
    Pending,
    PendingApproval,
    Approved,
    Rejected,
    Replayed(usize),
}

/// 合并规则 (接口专用)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeRules {
    pub strategy: MergeStrategy,
    pub header_rules: HeaderMergeRules,
    pub body_rules: BodyMergeRules,
    pub custom_script: Option<String>,
}

/// 头部合并规则 (接口专用)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderMergeRules {
    pub keep_original: bool,
    pub override_headers: Vec<String>,
    pub append_headers: Vec<String>,
}

/// 响应体合并规则 (接口专用)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BodyMergeRules {
    pub json_merge_strategy: JsonMergeStrategy,
    pub text_separator: Option<String>,
}

/// JSON合并策略 (接口专用)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JsonMergeStrategy {
    DeepMerge,
    Append,
    ReplaceFields(Vec<String>),
}

/// 连接事件 (接口专用)
#[derive(Debug, Clone)]
pub struct ConnectionEvent {
    pub conn_id: ConnectionId,
    pub event_type: ConnectionEventType,
    pub timestamp: SystemTime,
    pub data: Option<serde_json::Value>,
}

/// 连接事件类型 (接口专用)
#[derive(Debug, Clone)]
pub enum ConnectionEventType {
    Connected,
    Disconnected,
    DataSent(usize),
    DataReceived(usize),
    Error(String),
}

/// 请求详情 (接口专用)
#[derive(Debug, Clone)]
pub struct RequestDetails {
    pub request_id: RequestId,
    pub conn_id: ConnectionId,
    pub request: HttpRequest,
    pub timestamp: SystemTime,
    pub processing_time: Option<Duration>,
}

/// 响应详情 (接口专用)
#[derive(Debug, Clone)]
pub struct ResponseDetails {
    pub request_id: RequestId,
    pub conn_id: ConnectionId,
    pub response: HttpResponse,
    pub timestamp: SystemTime,
    pub processing_time: Duration,
}

/// 用户决策请求 (接口专用)
#[derive(Debug, Clone)]
pub struct UserDecisionRequest {
    pub request_id: RequestId,
    pub request: HttpRequest,
    pub suggested_modifications: Option<RequestModifications>,
    pub timeout: Duration,
    pub context: String,
}

/// 用户决策 (接口专用)
#[derive(Debug, Clone)]
pub enum UserDecision {
    Approve(Option<RequestModifications>),
    Reject(String),
    Modify(RequestModifications),
    Timeout,
}

/// 连接状态详情 (接口专用)
#[derive(Debug, Clone)]
pub struct ConnectionState {
    pub id: ConnectionId,
    pub status: ConnectionStatus,
    pub request_count: usize,
    pub bytes_transferred: u64,
    pub last_activity: SystemTime,
    pub metadata: HashMap<String, serde_json::Value>,
}
