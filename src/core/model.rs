//! 核心业务模型
//! 
//! 定义系统中的核心数据结构和业务实体

use crate::utils::generate_short_connection_id;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream as OutboundTlsStream, server::TlsStream as InboundTlsStream};
use uuid::Uuid;
use hyper::HeaderMap;

// HeaderMap 序列化辅助模块
// 📝 注意：序列化主要用于日志/调试，运行时处理使用 HeaderMap.append() 保留所有多值头部
mod headermap_serde {
    use super::*;
    use serde::{Deserializer, Serializer};
    use serde::ser::SerializeMap;
    use std::collections::HashMap;
    
    pub fn serialize<S>(headers: &HeaderMap, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // 简单序列化：遍历所有头部（包括重复的）
        let mut map = serializer.serialize_map(None)?;
        for (key, value) in headers.iter() {
            if let Ok(v) = value.to_str() {
                map.serialize_entry(key.as_str(), v)?;
            }
        }
        map.end()
    }
    
    pub fn deserialize<'de, D>(deserializer: D) -> Result<HeaderMap, D::Error>
    where
        D: Deserializer<'de>,
    {
        let map: HashMap<String, serde_json::Value> = HashMap::deserialize(deserializer)?;
        let mut headers = HeaderMap::new();
        
        for (key, value) in map {
            if let Ok(name) = key.parse::<hyper::header::HeaderName>() {
                match value {
                    serde_json::Value::String(s) => {
                        if let Ok(val) = s.parse::<hyper::header::HeaderValue>() {
                            headers.insert(name, val);
                        }
                    }
                    serde_json::Value::Array(arr) => {
                        // 支持数组格式（多值头部）
                        for item in arr {
                            if let serde_json::Value::String(s) = item {
                                if let Ok(val) = s.parse::<hyper::header::HeaderValue>() {
                                    headers.append(name.clone(), val);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(headers)
    }
}

/// 连接ID类型 - 使用Sqids生成的短ID
pub type ConnectionId = String;

/// 请求ID类型
pub type RequestId = String;

/// 应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 应用名称
    pub app_name: String,
    /// 版本
    pub version: String,
    /// 入站配置
    pub inbound: InboundConfig,
    /// 出站配置
    pub outbound: OutboundConfig,
    /// 脚本配置
    pub script: ScriptConfig,
    /// TLS配置
    pub tls: TlsConfig,
    /// 通用配置
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

/// TLS配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    /// 是否启用TLS拦截
    pub enabled: bool,
    /// 拦截模式
    pub interception_mode: String, // "none", "selective", "full"
    /// 需要拦截的域名模式
    pub intercept_domains: Vec<String>,
    /// 证书存储路径
    pub cert_store_path: Option<String>,
    /// 证书缓存时间（秒）
    pub cert_cache_duration: u64,
    /// 最大并发TLS握手数
    pub max_concurrent_handshakes: usize,
    /// 是否自动安装根CA到系统
    pub auto_install_ca: bool,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interception_mode: "selective".to_string(),
            intercept_domains: vec!["*".to_string()],
            cert_store_path: None,
            cert_cache_duration: 24 * 60 * 60, // 24 hours
            max_concurrent_handshakes: 100,
            auto_install_ca: true,
        }
    }
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
            id: crate::utils::generate_short_connection_id(),
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
            id: crate::utils::generate_short_connection_id(),
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
            id: self.id.clone(),
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
            id: self.id.clone(),
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
    /// 用户名
    pub user_name: Option<String>,
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
            user_name: None,
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
        let id = connection.id.clone();
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
    /// 进程信息查询
    ProcessInfoQuery,
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
    /// 头部 (支持多值头部，如 Set-Cookie)
    #[serde(with = "headermap_serde")]
    pub headers: HeaderMap,
    /// 请求体
    pub body: Vec<u8>,
    /// 发送时间
    pub sent_at: Option<SystemTime>,
}

impl HttpRequest {
    pub fn new(method: &str, url: &str) -> Self {
        Self {
            id: Some(Uuid::new_v4().to_string()),
            method: method.to_string(),
            url: url.to_string(),
            version: "HTTP/1.1".to_string(),
            headers: HeaderMap::new(),
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
    /// 头部 (支持多值头部，如 Set-Cookie)
    #[serde(with = "headermap_serde")]
    pub headers: HeaderMap,
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
            headers: HeaderMap::new(),
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

// =======================================================================
// Phase 2: 完整版规则数据模型 (V3 Design)
// =======================================================================

/// 规则定义 - 完整版本
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    pub id: String,  // 使用 utils::generate_short_connection_id()
    pub name: String,
    pub description: String,
    pub pattern: RulePattern,
    pub conditions: Vec<RuleCondition>,
    pub action: RuleAction,
    pub enabled: bool,
    pub priority: i32,
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Rule {
    pub fn new(name: &str, description: &str, pattern: RulePattern, action: RuleAction, priority: i32) -> Self {
        Self {
            id: generate_short_connection_id(),  // 使用现有工具方法
            name: name.to_string(),
            description: description.to_string(),
            pattern,
            conditions: Vec::new(),
            action,
            enabled: true,
            priority,
            metadata: HashMap::new(),
        }
    }
    
    /// 便捷构造器 - 兼容现有代码
    pub fn simple(name: &str, pattern: RulePattern, action: RuleAction, priority: i32) -> Self {
        Self::new(name, name, pattern, action, priority)
    }
}

/// 规则匹配模式 - 完整版本
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RulePattern {
    // 基础匹配
    HostMatches(String),              // 主机匹配：*.google.com
    PathMatches(String),              // 路径匹配：/api/*
    HeaderExists(String),             // 请求头存在：Authorization
    HeaderValueMatches {
        header: String,
        value: String
    },                                // 请求头值匹配：User-Agent: *Chrome*
    PortMatches(u16),                 // 端口匹配：443
    ProcessMatches(String),           // 进程匹配：*curl*
    UserMatches(String),              // 用户匹配：john
    
    // 时间匹配
    TimeRange {
        start: String,                // HH:MM 格式：09:00
        end: String                   // HH:MM 格式：17:00
    },
    WeekdayMatches(Vec<u8>),         // 星期几：[1,2,3,4,5] (Mon-Fri)
    
    // 复合匹配
    And(Vec<RulePattern>),           // 逻辑与
    Or(Vec<RulePattern>),            // 逻辑或
    Not(Box<RulePattern>),           // 逻辑非
    
    // 高级匹配
    Custom(String),                  // 自定义Lua脚本匹配
}

/// 规则条件 - 补充匹配条件
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuleCondition {
    pub name: String,
    pub condition_type: RuleConditionType,
    pub value: serde_json::Value,
    pub negated: bool,  // 是否取反
}

/// 规则条件类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RuleConditionType {
    // 网络条件
    SourceIPInRange,     // 源IP范围
    TargetIPInRange,     // 目标IP范围
    ConnectionCount,     // 连接数量限制
    BandwidthUsage,      // 带宽使用情况
    
    // 请求条件
    RequestSize,         // 请求大小
    RequestMethod,       // HTTP方法
    ContentType,         // Content-Type
    
    // 应用条件
    ProcessPID,          // 进程PID
    ProcessParent,       // 父进程
    ProcessCPUUsage,     // CPU使用率
    ProcessMemoryUsage,  // 内存使用率
    
    // 自定义条件
    CustomScript,        // 自定义脚本
}

/// 规则动作 - 完整版本
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RuleAction {
    // 基础动作
    Allow,                           // 允许通过 (无修改)
    Direct,                          // 直连
    Block,                           // 拒绝连接
    ProxyByName(String),            // 通过指定代理
    
    // 修改动作
    ModifyRequest(RequestModifications),      // 修改请求
    ModifyResponse(ResponseModifications),    // 修改响应
    RedirectTo(String),                       // 重定向到指定URL
    
    // 交互动作
    InterceptForApproval,            // 拦截等待人工批准
    PauseForInspection(u64),        // 暂停指定秒数供检查
    
    // 脚本动作
    ExecuteScript(String),           // 执行指定脚本
    CompositeRequest(CompositeRequestConfig), // 复合请求处理
    
    // 日志动作
    Log(LogLevel),                   // 记录日志
    Alert(AlertConfig),             // 发送告警
    
    // 自定义动作
    Custom(HashMap<String, serde_json::Value>),
}

/// 请求修改配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RequestModifications {
    pub add_headers: Option<HashMap<String, String>>,
    pub remove_headers: Option<Vec<String>>,
    pub modify_headers: Option<HashMap<String, String>>,
    pub change_method: Option<String>,
    pub change_path: Option<String>,
    pub change_body: Option<String>,
}

/// 响应修改配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ResponseModifications {
    pub change_status: Option<u16>,
    pub add_headers: Option<HashMap<String, String>>,
    pub remove_headers: Option<Vec<String>>,
    pub modify_headers: Option<HashMap<String, String>>,
    pub change_body: Option<String>,
}

/// 复合请求配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompositeRequestConfig {
    pub additional_requests: Vec<HttpRequestConfig>,
    pub merge_strategy: MergeStrategy,
    pub timeout: u64, // 秒
}

/// HTTP请求配置 (用于复合请求)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HttpRequestConfig {
    pub url: String,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
}

impl From<HttpRequestConfig> for HttpRequest {
    fn from(config: HttpRequestConfig) -> Self {
        let mut headers = HeaderMap::new();
        for (k, v) in config.headers {
            if let (Ok(name), Ok(val)) = (k.parse::<hyper::header::HeaderName>(), v.parse::<hyper::header::HeaderValue>()) {
                headers.insert(name, val);
            }
        }
        
        Self {
            id: Some(Uuid::new_v4().to_string()),
            method: config.method,
            url: config.url,
            version: "HTTP/1.1".to_string(), // Default version
            headers,
            body: config.body.unwrap_or_default().into_bytes(),
            sent_at: None,
        }
    }
}

/// 合并策略
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MergeStrategy {
    Replace,           // 使用新响应替换
    Merge,            // 合并响应内容
    Append,           // 追加到原响应
    Custom(String),   // 自定义合并脚本
}

/// 日志级别
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// 告警配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertConfig {
    pub alert_type: AlertType,
    pub message: String,
    pub severity: AlertSeverity,
    pub channels: Vec<String>, // 告警渠道：email, slack, webhook等
}

/// 告警类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AlertType {
    SecurityThreat,
    PerformanceIssue,
    PolicyViolation,
    SystemAnomaly,
    Custom(String),
}

/// 告警严重级别
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AlertSeverity {
    Low,
    Medium,
    High,
    Critical,
}

/// 匹配结果 - 完整版本
#[derive(Debug, Clone)]
pub struct MatchResult {
    pub rule_id: String,
    pub rule_name: String,
    pub rule_description: String,
    pub action: RuleAction,
    pub match_duration: Duration,
    pub matched_patterns: Vec<String>, // 哪些模式匹配了
    pub matched_conditions: Vec<String>, // 哪些条件匹配了
}

/// 规则编译缓存 - 性能优化
#[derive(Debug)]
pub struct CompiledRule {
    pub rule: Rule,
    pub compiled_patterns: Vec<CompiledPattern>,
    pub compiled_at: SystemTime,
}

/// 预编译的匹配模式 - 性能优化
#[derive(Debug)]
pub struct CompiledPattern {
    pub original_pattern: RulePattern,
    pub regex: Option<regex::Regex>,
    pub glob: Option<globset::Glob>,
    pub exact_match: Option<String>,
    pub pattern_type: PatternType,
}

/// 模式类型分级 - 用于性能优化
#[derive(Debug, PartialEq, Ord, PartialOrd, Eq)]
pub enum PatternType {
    Exact,      // 精确匹配 - 最快
    Prefix,     // 前缀匹配 - 快
    Suffix,     // 后缀匹配 - 快
    Wildcard,   // 通配符匹配 - 中等
    Regex,      // 正则表达式 - 慢
    Composite,  // 复合模式 (And/Or/Not) - 最慢
    Custom,     // 自定义脚本 - 很慢
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


/// 重放结果
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            app_name: "takeover".to_string(),
            version: "0.1.0".to_string(),
            inbound: InboundConfig {
                listen_address: "127.0.0.1".to_string(),
                listen_port: 0, // 🎯 默认使用随机端口，更安全且避免冲突
                max_connections: 1000,
                buffer_size: 8192,
                timeout: Duration::from_secs(60),
                intercepted_ports: vec![],
                bypass_user: None,
            },
            outbound: OutboundConfig {
                timeout: Duration::from_secs(30),
                read_timeout: Duration::from_secs(30),
                write_timeout: Duration::from_secs(30),
                retry_count: 3,
                retry_delay: Duration::from_secs(1),
            },
            script: ScriptConfig {
                enabled: true,
                script_dir: PathBuf::from("./scripts"),
                timeout: Duration::from_secs(30),
                max_concurrent: 10,
            },
            tls: TlsConfig::default(),
            general: GeneralConfig {
                max_connections: 1000,
                connection_timeout: Duration::from_secs(3600),
                log_level: "info".to_string(),
            }
        }
    }
}
