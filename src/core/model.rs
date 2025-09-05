//! 核心业务模型
//! 
//! 定义系统中的核心数据结构和业务实体

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime};
use std::path::PathBuf;
use serde::{Serialize, Deserialize};
use uuid::Uuid;
use tokio::net::TcpStream;
use tokio_rustls::{server::TlsStream as InboundTlsStream};
use tokio_rustls::{client::TlsStream as OutboundTlsStream};


/// 连接ID类型
pub type ConnectionId = Uuid;

/// 请求ID类型  
pub type RequestId = Uuid;

/// 应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 应用名称
    pub app_name: String,
    /// 版本
    pub version: String,
    /// 入站配置
    pub inbound_config: InboundConfig,
    /// 入站配置(别名)
    pub inbound: InboundConfig,
    /// 出站配置
    pub outbound_config: OutboundConfig,
    /// 脚本配置
    pub script_config: ScriptConfig,
    /// 脚本配置(别名)
    pub script: ScriptConfig,
    /// 通用配置
    pub general_config: GeneralConfig,
    /// 通用配置(别名)
    pub general: GeneralConfig,
}

/// 入站配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundConfig {
    /// 监听地址
    pub listen_address: String,
    /// 监听端口
    pub listen_port: u16,
    /// 最大连接数
    pub max_connections: usize,
    /// 缓冲区大小
    pub buffer_size: usize,
    /// 超时时间
    pub timeout: Duration,
    /// 拦截的端口列表
    pub intercepted_ports: Vec<u16>,
    /// 绕过的用户
    pub bypass_user: Option<String>,
}

/// 出站配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundConfig {
    /// 超时时间
    pub timeout: Duration,
    /// 读取超时
    pub read_timeout: Duration,
    /// 写入超时
    pub write_timeout: Duration,
    /// 重试次数
    pub retry_count: u32,
    /// 重试延迟
    pub retry_delay: Duration,
}

/// 脚本配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptConfig {
    /// 是否启用
    pub enabled: bool,
    /// 脚本目录
    pub script_dir: PathBuf,
    /// 超时时间
    pub timeout: Duration,
    /// 最大并发数
    pub max_concurrent: usize,
}

/// 通用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    /// 最大连接数
    pub max_connections: usize,
    /// 连接超时
    pub connection_timeout: Duration,
    /// 日志级别
    pub log_level: String,
}

/// 连接
#[derive(Serialize, Deserialize)]
pub struct Connection {
    /// 连接ID
    pub id: ConnectionId,
    /// 源地址
    pub source_addr: SocketAddr,
    /// 原始目标地址
    pub original_dest: SocketAddr,
    /// 元数据
    pub metadata: ConnectionMetadata,
    /// TCP流（原始网络连接，序列化时跳过）
    #[serde(skip)]
    pub stream: Option<TcpStream>,
    /// 客户端TLS流（TLS拦截后的解密流，序列化时跳过）
    #[serde(skip)]
    pub inbound_tls_stream: Option<InboundTlsStream<TcpStream>>,
    /// 服务器TLS流（与真实服务器的TLS连接，序列化时跳过）
    #[serde(skip)]
    pub outbound_tls_stream: Option<OutboundTlsStream<TcpStream>>,
}

// 手动实现Debug，跳过TLS流字段
impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("id", &self.id)
            .field("source_addr", &self.source_addr)
            .field("original_dest", &self.original_dest)
            .field("metadata", &self.metadata)
            .field("stream", &self.stream.as_ref().map(|_| "TcpStream(...)"))
            .field("inbound_tls_stream", &self.inbound_tls_stream.as_ref().map(|_| "TlsStream(...)"))
            .field("outbound_tls_stream", &self.outbound_tls_stream.as_ref().map(|_| "TlsStream(...)"))
            .finish()
    }
}



impl Connection {
    pub fn new(source_addr: SocketAddr, original_dest: SocketAddr) -> Self {
        Self {
            id: Uuid::new_v4(),
            source_addr,
            original_dest,
            metadata: ConnectionMetadata::default(),
            stream: None,
            inbound_tls_stream: None,
            outbound_tls_stream: None,
        }
    }
    
    /// 创建带有TCP流的连接
    pub fn new_with_stream(source_addr: SocketAddr, original_dest: SocketAddr, stream: TcpStream) -> Self {
        Self {
            id: Uuid::new_v4(),
            source_addr,
            original_dest,
            metadata: ConnectionMetadata::default(),
            stream: Some(stream),
            inbound_tls_stream: None,
            outbound_tls_stream: None,
        }
    }
    
    /// 创建只包含元数据的连接信息（用于事件发布、跟踪等）
    pub fn clone_info(&self) -> Self {
        Self {
            id: self.id,
            source_addr: self.source_addr,
            original_dest: self.original_dest,
            metadata: self.metadata.clone(),
            stream: None, // 故意不包含流
            inbound_tls_stream: None,
            outbound_tls_stream: None,
        }
    }
    /// 设置客户端TLS流
    pub fn set_client_tls_stream(&mut self, client_tls: InboundTlsStream<TcpStream>) {
        self.inbound_tls_stream = Some(client_tls);
    }

    /// 设置服务器TLS流
    pub fn set_server_tls_stream(&mut self, server_tls: OutboundTlsStream<TcpStream>) {
        self.outbound_tls_stream = Some(server_tls);
    }
    
    /// 检查是否有TLS流
    pub fn has_tls_streams(&self) -> bool {
        self.inbound_tls_stream.is_some() || self.outbound_tls_stream.is_some()
    }
}

// 手动实现Clone，跳过stream和tls_stream字段 
// [TODO] 重新考虑clone实现，stream到底要不要arc？
impl Clone for Connection {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            source_addr: self.source_addr,
            original_dest: self.original_dest,
            metadata: self.metadata.clone(),
            stream: None, // 不克隆TcpStream
            inbound_tls_stream: None, // 不克隆TLS流
            outbound_tls_stream: None, // 不克隆TLS流
        }
    }
}



/// 连接元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionMetadata {
    /// 协议
    pub protocol: String,
    /// 进程名称
    pub process_name: Option<String>,
    /// 用户ID
    pub user_id: Option<u32>,
    /// 创建时间
    pub created_at: SystemTime,
    /// 自定义数据
    pub custom_data: HashMap<String, serde_json::Value>,
}

impl Default for ConnectionMetadata {
    fn default() -> Self {
        Self {
            protocol: "TCP".to_string(), // 默认协议为TCP
            process_name: None,
            user_id: None,
            created_at: SystemTime::now(),
            custom_data: HashMap::new(),
        }
    }
}

/// 连接上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionContext {
    /// 连接ID
    pub id: ConnectionId,
    /// 连接信息
    pub connection: Connection,
    /// 目标主机
    pub target_host: String,
    /// 目标端口
    pub target_port: u16,
    /// HTTP请求
    pub request: Option<HttpRequest>,
    /// HTTP响应
    pub response: Option<HttpResponse>,
    /// 处理状态
    pub processing_state: ProcessingState,
    /// 应用的修改
    pub applied_modifications: Vec<RequestModifications>,
    /// 自定义数据
    pub custom_data: HashMap<String, serde_json::Value>,
}

impl ConnectionContext {
    pub fn new(connection: Connection, target_host: String, target_port: u16) -> Self {
        let id = connection.id;
        Self {
            id,
            connection,
            target_host,
            target_port,
            request: None,
            response: None,
            processing_state: ProcessingState::Initialized,
            applied_modifications: Vec::new(),
            custom_data: HashMap::new(),
        }
    }
    
    pub fn set_processing_state(&mut self, state: ProcessingState) {
        self.processing_state = state;
    }
}

impl ConnectionContext {
    pub fn add_matched_rule(&mut self, rule_id: String) {
        // 添加匹配的规则ID到自定义数据中
        let mut matched_rules = self.custom_data
            .get("matched_rules")
            .and_then(|v| v.as_array())
            .unwrap_or(&vec![])
            .clone();
        matched_rules.push(serde_json::Value::String(rule_id));
        self.custom_data.insert("matched_rules".to_string(), serde_json::Value::Array(matched_rules));
    }
    
    pub fn apply_modification(&mut self, modification: RequestModifications) {
        self.applied_modifications.push(modification);
    }
}

/// 处理状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ProcessingState {
    /// 初始化
    Initialized,
    /// 处理中
    Processing,
    /// 协议检测中
    ProtocolDetection,
    /// TLS处理
    TlsHandshake,
    /// HTTP解析
    HttpParsing,
    /// HTTP修改
    HttpModification,
    /// 规则匹配
    RuleMatching,
    /// 脚本执行
    ScriptExecution,
    /// 用户干预
    UserIntervention,
    /// 转发中
    Forwarding,
    /// 响应处理
    ResponseProcessing,
    /// 响应合并
    ResponseMerging,
    /// 响应写回
    ResponseWriting,
    /// 已完成
    Completed,
    /// 错误
    Error,
}

/// HTTP请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    /// 请求ID
    pub id: Option<RequestId>,
    /// HTTP方法
    pub method: String,
    /// URL
    pub url: String,
    /// HTTP版本
    pub version: String,
    /// 头部
    pub headers: HashMap<String, String>,
    /// 请求体
    pub body: Vec<u8>,
    /// 发送时间
    pub sent_at: Option<SystemTime>,
}

impl HttpRequest {
    pub fn new(method: &str, url: &str) -> Self {
        Self {
            id: Some(Uuid::new_v4()),
            method: method.to_string(),
            url: url.to_string(),
            version: "HTTP/1.1".to_string(),
            headers: HashMap::new(),
            body: Vec::new(),
            sent_at: None,
        }
    }
}

/// HTTP响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    /// 响应ID
    pub id: Option<Uuid>,
    /// 状态码
    pub status_code: u16,
    /// 状态文本
    pub status_text: Option<String>,
    /// 头部
    pub headers: HashMap<String, String>,
    /// 响应体
    pub body: Vec<u8>,
    /// 发送时间
    pub sent_at: Option<SystemTime>,
}

impl HttpResponse {
    pub fn new(status_code: u16) -> Self {
        Self {
            id: Some(Uuid::new_v4()),
            status_code,
            status_text: None,
            headers: HashMap::new(),
            body: Vec::new(),
            sent_at: None,
        }
    }
}

/// 连接状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConnectionStatus {
    /// 连接中
    Connecting,
    /// 已连接/活跃
    Connected,
    /// 活跃状态 (别名)
    Active,
    /// 处理中
    Processing,
    /// 已暂停
    Paused,
    /// 正在关闭
    Closing,
    /// 已关闭
    Closed,
    /// 已完成
    Completed,
    /// 错误
    Error(String),
}

/// 连接摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionSummary {
    /// 连接ID
    pub id: ConnectionId,
    /// 源地址
    pub source_addr: SocketAddr,
    /// 目标主机
    pub target_host: String,
    /// 目标端口
    pub target_port: u16,
    /// 协议
    pub protocol: String,
    /// 状态
    pub status: ConnectionStatus,
    /// 发送字节数
    pub bytes_sent: u64,
    /// 接收字节数
    pub bytes_received: u64,
    /// 创建时间
    pub created_at: SystemTime,
    /// 最后活动时间
    pub last_activity: Option<SystemTime>,
}

/// 适配器状态信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterStatus {
    /// 适配器名称
    pub name: String,
    /// 是否活跃
    pub active: bool,
    /// 活跃连接数
    pub connections: usize,
    /// 最后活动时间
    pub last_activity: Option<SystemTime>,
    /// 错误数量
    pub errors: u64,
}

/// 规则
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// 规则ID
    pub id: Uuid,
    /// 规则名称
    pub name: String,
    /// 规则描述
    pub description: String,
    /// 匹配模式 (主要匹配条件)
    pub pattern: RulePattern,
    /// 匹配条件
    pub conditions: Vec<RuleCondition>,
    /// 动作
    pub action: RuleAction,
    /// 是否启用
    pub enabled: bool,
    /// 优先级
    pub priority: u32,
    /// 元数据
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Rule {
    /// 创建新规则
    pub fn new<T: Into<String>>(
        name: T,
        description: T,
        pattern: RulePattern,
        action: RuleAction,
        priority: u32,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            description: description.into(),
            pattern,
            conditions: Vec::new(),
            action,
            enabled: true,
            priority,
            metadata: HashMap::new(),
        }
    }
    
    /// 添加匹配条件
    pub fn with_condition(mut self, condition: RuleCondition) -> Self {
        self.conditions.push(condition);
        self
    }
    
    /// 设置启用状态
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    
    /// 添加元数据
    pub fn with_metadata(mut self, key: String, value: serde_json::Value) -> Self {
        self.metadata.insert(key, value);
        self
    }
}

/// 规则模式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RulePattern {
    HostMatches(String),
    PathMatches(String),
    HeaderExists(String),
    HeaderValueMatches { header: String, value: String },
    PortMatches(u16),
    ProcessMatches(String),
    And(Vec<RulePattern>),
    Or(Vec<RulePattern>),
    Custom(String), // Lua表达式
}

/// 规则动作
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuleAction {
    Allow,
    Block,
    Direct,
    Proxy(crate::config::ProxyConfig),
    ModifyRequest(RequestModifications),
    ModifyResponse(Vec<RequestModification>),
    InterceptForApproval,
    ExecuteScript(String),
    Log(LogLevel),
}

/// 规则条件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuleCondition {
    /// 域名匹配
    Domain(String),
    /// 端口匹配
    Port(u16),
    /// URL模式匹配
    UrlPattern(String),
    /// 进程路径匹配
    ProcessPath(String),
    /// IP范围匹配
    IpRange(String),
}

/// 路由动作
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoutingAction {
    /// 允许
    Allow,
    /// 拒绝
    Deny,
    /// 重定向
    Redirect(String),
    /// 修改
    Modify(Vec<RequestModification>),
    /// 脚本处理
    Script(String),
}

/// 请求修改配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestModifications {
    pub url: Option<String>,
    pub method: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub body: Option<Vec<u8>>,
    pub remove_headers: Option<Vec<String>>,
    pub add_query_params: Option<HashMap<String, String>>,
    pub remove_query_params: Option<Vec<String>>,
}

/// 请求修改
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestModification {
    /// 修改类型
    pub modification_type: ModificationType,
    /// 目标字段
    pub target: String,
    /// 新值
    pub value: String,
}

/// 修改类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModificationType {
    /// 设置头部
    SetHeader,
    /// 移除头部
    RemoveHeader,
    /// 设置URL参数
    SetUrlParam,
    /// 移除URL参数
    RemoveUrlParam,
    /// 替换URL
    ReplaceUrl,
    /// 替换主机
    ReplaceHost,
}

/// 合并策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MergeStrategy {
    /// 替换响应体
    ReplaceBody,
    /// 追加响应体
    AppendBody,
    /// JSON合并
    MergeJson,
    /// 仅合并头部
    MergeHeaders,
    /// 自定义脚本
    CustomScript,
}

/// 领域事件
#[derive(Debug, Clone)]
pub enum DomainEvent {
    /// 新连接
    ConnectionNew {
        id: ConnectionId,
        addr: SocketAddr,
        metadata: ConnectionMetadata,
    },
    /// 连接关闭
    ConnectionClosed {
        id: ConnectionId,
        duration: Duration,
        bytes_transferred: u64,
    },
    /// 请求处理
    RequestProcessed {
        conn_id: ConnectionId,
        request_id: RequestId,
        duration: Duration,
    },
    /// 脚本执行
    ScriptExecuted {
        conn_id: ConnectionId,
        script_name: String,
        success: bool,
        duration: Duration,
    },
    /// 用户干预
    UserIntervention {
        conn_id: ConnectionId,
        intervention_type: String,
        user_id: Option<String>,
    },
    /// 错误事件
    Error {
        conn_id: Option<ConnectionId>,
        error: String,
        category: ErrorCategory,
    },
}

/// 错误类别
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ErrorCategory {
    /// 网络错误
    Network,
    /// 协议错误
    Protocol,
    /// 配置错误
    Configuration,
    /// 脚本错误
    Script,
    /// 系统错误
    System,
}

// RequestModifications 已在前面定义，删除重复定义

/// 响应修改配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseModifications {
    /// 修改列表
    pub modifications: Vec<RequestModification>, // 重用相同的修改类型
}

/// 重放结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayResult {
    /// 请求ID
    pub request_id: RequestId,
    /// 重放次数
    pub replay_count: u32,
    /// 执行时间
    pub execution_time: Duration,
    /// 是否成功
    pub success: bool,
    /// 响应
    pub response: Option<HttpResponse>,
    /// 错误消息
    pub error_message: Option<String>,
}

/// 复合请求配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeRequestConfig {
    /// 配置名称
    pub name: String,
    /// 请求序列
    pub requests: Vec<HttpRequest>,
    /// 合并策略
    pub merge_strategy: MergeStrategy,
    /// 执行超时
    pub timeout: Option<Duration>,
}

/// 触发点
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TriggerPoint {
    /// 请求前
    BeforeRequest,
    /// 请求后
    AfterRequest,
    /// 响应前
    BeforeResponse,
    /// 响应后
    AfterResponse,
}

/// 触发条件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TriggerCondition {
    /// 总是触发
    Always,
    /// 条件表达式
    Expression(String),
    /// 规则匹配
    RuleMatch(String),
    /// 主机匹配
    HostMatches(String),
    /// 路径匹配
    PathMatches(String),
    /// 方法匹配
    MethodEquals(String),
    /// 头部存在
    HeaderExists(String),
    /// 自定义条件
    Custom(String),
}

/// 自定义事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomEvent {
    /// 事件名称
    pub name: String,
    /// 事件数据
    pub data: HashMap<String, serde_json::Value>,
    /// 时间戳
    pub timestamp: SystemTime,
}

/// 日志级别
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogLevel {
    /// 调试
    Debug,
    /// 信息
    Info,
    /// 警告
    Warn,
    /// 错误
    Error,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            app_name: "takeover".to_string(),
            version: "0.1.0".to_string(),
            inbound_config: InboundConfig {
                listen_address: "127.0.0.1".to_string(),
                listen_port: 8080,
                max_connections: 1000,
                buffer_size: 8192,
                timeout: Duration::from_secs(60),
                intercepted_ports: vec![],
                bypass_user: None,
            },
            outbound_config: OutboundConfig {
                timeout: Duration::from_secs(30),
                read_timeout: Duration::from_secs(30),
                write_timeout: Duration::from_secs(30),
                retry_count: 3,
                retry_delay: Duration::from_secs(1),
            },
            script_config: ScriptConfig {
                enabled: true,
                script_dir: PathBuf::from("./scripts"),
                timeout: Duration::from_secs(30),
                max_concurrent: 10,
            },
            general_config: GeneralConfig {
                max_connections: 1000,
                connection_timeout: Duration::from_secs(3600),
                log_level: "info".to_string(),
            },
            inbound: InboundConfig {
                listen_address: "127.0.0.1".to_string(),
                listen_port: 8080,
                max_connections: 1000,
                buffer_size: 8192,
                timeout: Duration::from_secs(60),
                intercepted_ports: vec![],
                bypass_user: None,
            },
            script: ScriptConfig {
                enabled: true,
                script_dir: PathBuf::from("./scripts"),
                timeout: Duration::from_secs(30),
                max_concurrent: 10,
            },
            general: GeneralConfig {
                max_connections: 1000,
                connection_timeout: Duration::from_secs(3600),
                log_level: "info".to_string(),
            },
        }
    }
}
