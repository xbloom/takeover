//! 请求重放管理器
//! 
//! 负责HTTP请求的录制、存储、重放和分析：
//! - 请求录制和回放
//! - 重放会话管理
//! - 批量重放操作
//! - 重放结果比较和分析
//! - 请求修改和重发
//! - 重放历史和统计

use std::sync::Arc;
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};
use std::path::{Path, PathBuf};
use tokio::sync::RwLock;
use tokio::fs;
use tracing::{info, warn, debug, instrument};
use serde::{Serialize, Deserialize};
use uuid::Uuid;

use crate::core::ports::{ConnectionId, ApiPort, AppError};
use crate::core::model::{HttpRequest, HttpResponse, ConnectionContext};

/// 重放管理器
/// 
/// 管理HTTP请求的录制和重放功能
pub struct ReplayManager {
    /// 录制的请求存储
    recorded_requests: Arc<RwLock<HashMap<String, RecordedSession>>>,
    /// 重放会话存储
    replay_sessions: Arc<RwLock<HashMap<String, ReplaySession>>>,
    /// API端口引用（用于发送请求）
    api_port: Arc<dyn ApiPort>,
    /// 配置
    config: ReplayManagerConfig,
    /// 统计信息
    statistics: Arc<RwLock<ReplayStatistics>>,
    /// 事件监听器
    event_listeners: Arc<RwLock<HashMap<String, Vec<Box<dyn Fn(&ReplayEvent) + Send + Sync>>>>>,
}

/// 重放管理器配置
#[derive(Debug, Clone)]
pub struct ReplayManagerConfig {
    /// 最大录制会话数
    pub max_recorded_sessions: usize,
    /// 最大重放会话数
    pub max_replay_sessions: usize,
    /// 单个会话最大请求数
    pub max_requests_per_session: usize,
    /// 请求录制保留期
    pub recording_retention: Duration,
    /// 重放会话保留期
    pub replay_session_retention: Duration,
    /// 重放请求超时
    pub replay_timeout: Duration,
    /// 并发重放限制
    pub max_concurrent_replays: usize,
    /// 存储目录
    pub storage_dir: PathBuf,
    /// 是否启用自动录制
    pub auto_recording: bool,
    /// 录制过滤器
    pub recording_filters: Vec<RecordingFilter>,
    /// 是否启用结果比较
    pub enable_result_comparison: bool,
    /// 比较容差
    pub comparison_tolerance: f64,
}

/// 录制的会话
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedSession {
    /// 会话ID
    pub session_id: String,
    /// 会话名称
    pub name: String,
    /// 创建时间
    pub created_at: SystemTime,
    /// 录制的请求
    pub requests: Vec<RecordedRequest>,
    /// 会话标签
    pub tags: Vec<String>,
    /// 描述信息
    pub description: Option<String>,
    /// 录制统计
    pub statistics: RecordingStatistics,
}

/// 录制的请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedRequest {
    /// 请求ID
    pub id: Uuid,
    /// 原始连接ID
    pub connection_id: ConnectionId,
    /// HTTP请求
    pub request: HttpRequest,
    /// HTTP响应
    pub response: Option<HttpResponse>,
    /// 录制时间
    pub recorded_at: SystemTime,
    /// 请求耗时
    pub duration: Option<Duration>,
    /// 请求状态
    pub status: RequestStatus,
    /// 自定义数据
    pub metadata: HashMap<String, serde_json::Value>,
}

/// 重放会话
#[derive(Debug, Clone)]
pub struct ReplaySession {
    /// 会话ID
    pub session_id: String,
    /// 基于的录制会话ID
    pub based_on_recording: String,
    /// 会话名称
    pub name: String,
    /// 创建时间
    pub created_at: SystemTime,
    /// 重放配置
    pub config: ReplayConfig,
    /// 重放结果
    pub results: Vec<ReplayResult>,
    /// 会话状态
    pub status: ReplaySessionStatus,
    /// 开始时间
    pub started_at: Option<SystemTime>,
    /// 完成时间
    pub completed_at: Option<SystemTime>,
    /// 重放统计
    pub statistics: ReplayStatistics,
}

/// 重放配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayConfig {
    /// 目标主机（如果需要重定向）
    pub target_host: Option<String>,
    /// 目标端口
    pub target_port: Option<u16>,
    /// 请求修改规则
    pub request_modifiers: Vec<RequestModifier>,
    /// 并发级别
    pub concurrency: usize,
    /// 重放延迟
    pub delay_between_requests: Option<Duration>,
    /// 重试次数
    pub retry_count: usize,
    /// 是否忽略SSL错误
    pub ignore_ssl_errors: bool,
    /// 是否跟随重定向
    pub follow_redirects: bool,
    /// 自定义头部
    pub custom_headers: HashMap<String, String>,
    /// 是否比较响应
    pub compare_responses: bool,
}

/// 请求修改器
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestModifier {
    /// 修改类型
    pub modifier_type: ModifierType,
    /// 目标字段
    pub target: String,
    /// 新值
    pub value: String,
    /// 是否启用
    pub enabled: bool,
}

/// 修改器类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModifierType {
    /// 替换头部
    ReplaceHeader,
    /// 添加头部
    AddHeader,
    /// 移除头部
    RemoveHeader,
    /// 替换URL参数
    ReplaceUrlParam,
    /// 添加URL参数
    AddUrlParam,
    /// 替换主机
    ReplaceHost,
    /// 替换路径
    ReplacePath,
    /// 替换请求体
    ReplaceBody,
    /// 自定义Lua脚本
    LuaScript,
}

/// 重放结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayResult {
    /// 结果ID
    pub id: Uuid,
    /// 原始请求ID
    pub original_request_id: Uuid,
    /// 重放的请求
    pub replayed_request: HttpRequest,
    /// 重放的响应
    pub replayed_response: Option<HttpResponse>,
    /// 重放时间
    pub replayed_at: SystemTime,
    /// 重放耗时
    pub duration: Option<Duration>,
    /// 重放状态
    pub status: ReplayStatus,
    /// 错误信息
    pub error: Option<String>,
    /// 比较结果
    pub comparison_result: Option<ComparisonResult>,
}

/// 录制过滤器
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingFilter {
    /// 过滤器名称
    pub name: String,
    /// 过滤条件
    pub condition: FilterCondition,
    /// 是否启用
    pub enabled: bool,
}

/// 过滤条件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FilterCondition {
    /// 主机匹配
    HostMatches(String),
    /// 路径匹配
    PathMatches(String),
    /// 方法匹配
    MethodEquals(String),
    /// 状态码匹配
    StatusCodeEquals(u16),
    /// 内容类型匹配
    ContentTypeMatches(String),
    /// 复合条件
    And(Vec<FilterCondition>),
    /// 或条件
    Or(Vec<FilterCondition>),
    /// 非条件
    Not(Box<FilterCondition>),
}

/// 比较结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonResult {
    /// 是否匹配
    pub matches: bool,
    /// 相似度分数 (0.0 - 1.0)
    pub similarity_score: f64,
    /// 差异详情
    pub differences: Vec<Difference>,
    /// 比较时间
    pub compared_at: SystemTime,
}

/// 差异信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Difference {
    /// 差异类型
    pub difference_type: DifferenceType,
    /// 字段路径
    pub field_path: String,
    /// 原始值
    pub original_value: Option<String>,
    /// 重放值
    pub replayed_value: Option<String>,
    /// 差异描述
    pub description: String,
}

/// 差异类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DifferenceType {
    /// 状态码不同
    StatusCode,
    /// 头部不同
    Header,
    /// 内容长度不同
    ContentLength,
    /// 内容不同
    Content,
    /// 响应时间差异过大
    ResponseTime,
    /// 缺失字段
    MissingField,
    /// 额外字段
    ExtraField,
}

/// 请求状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RequestStatus {
    /// 已录制
    Recorded,
    /// 录制失败
    RecordingFailed,
    /// 已过滤
    Filtered,
}

/// 重放状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReplayStatus {
    /// 等待中
    Pending,
    /// 进行中
    InProgress,
    /// 成功
    Success,
    /// 失败
    Failed,
    /// 超时
    Timeout,
    /// 已跳过
    Skipped,
}

/// 重放会话状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReplaySessionStatus {
    /// 创建
    Created,
    /// 运行中
    Running,
    /// 已完成
    Completed,
    /// 已失败
    Failed,
    /// 已暂停
    Paused,
    /// 已取消
    Cancelled,
}

/// 录制统计
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RecordingStatistics {
    /// 总请求数
    pub total_requests: u64,
    /// 成功录制数
    pub successful_recordings: u64,
    /// 失败录制数
    pub failed_recordings: u64,
    /// 过滤的请求数
    pub filtered_requests: u64,
    /// 录制时间范围
    pub time_range: Option<(SystemTime, SystemTime)>,
    /// 涉及的主机
    pub hosts: Vec<String>,
    /// 请求方法分布
    pub method_distribution: HashMap<String, u64>,
}

/// 重放统计
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReplayStatistics {
    /// 总重放数
    pub total_replays: u64,
    /// 成功重放数
    pub successful_replays: u64,
    /// 失败重放数
    pub failed_replays: u64,
    /// 超时重放数
    pub timeout_replays: u64,
    /// 平均响应时间
    pub avg_response_time: Option<Duration>,
    /// 匹配的响应数
    pub matching_responses: u64,
    /// 不匹配的响应数
    pub non_matching_responses: u64,
    /// 平均相似度
    pub avg_similarity_score: f64,
}

/// 重放事件
#[derive(Debug, Clone)]
pub struct ReplayEvent {
    /// 事件类型
    pub event_type: ReplayEventType,
    /// 会话ID
    pub session_id: String,
    /// 请求ID（如果适用）
    pub request_id: Option<Uuid>,
    /// 事件数据
    pub data: HashMap<String, serde_json::Value>,
    /// 时间戳
    pub timestamp: SystemTime,
}

/// 重放事件类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayEventType {
    /// 录制开始
    RecordingStarted,
    /// 录制结束
    RecordingCompleted,
    /// 请求已录制
    RequestRecorded,
    /// 重放会话开始
    ReplaySessionStarted,
    /// 重放会话完成
    ReplaySessionCompleted,
    /// 请求重放完成
    RequestReplayed,
    /// 比较完成
    ComparisonCompleted,
    /// 错误发生
    ErrorOccurred,
}

impl ReplayManager {
    /// 创建新的重放管理器
    pub async fn new(api_port: Arc<dyn ApiPort>, config: ReplayManagerConfig) -> Result<Self, AppError> {
        info!("🎬 Initializing replay manager");
        
        // 确保存储目录存在
        if !config.storage_dir.exists() {
            fs::create_dir_all(&config.storage_dir)
                .await
                .map_err(|e| AppError::Config(format!("Failed to create storage directory: {}", e)))?;
        }
        
        let manager = Self {
            recorded_requests: Arc::new(RwLock::new(HashMap::new())),
            replay_sessions: Arc::new(RwLock::new(HashMap::new())),
            api_port,
            config,
            statistics: Arc::new(RwLock::new(ReplayStatistics::default())),
            event_listeners: Arc::new(RwLock::new(HashMap::new())),
        };
        
        // 加载已保存的录制会话
        manager.load_saved_sessions().await?;
        
        info!("✅ Replay manager initialized successfully");
        Ok(manager)
    }
    
    /// 开始录制新会话
    #[instrument(skip(self))]
    pub async fn start_recording(&self, session_name: String, description: Option<String>) -> Result<String, AppError> {
        let session_id = Uuid::new_v4().to_string();
        
        info!("🔴 Starting recording session: {} ({})", session_name, session_id);
        
        let session = RecordedSession {
            session_id: session_id.clone(),
            name: session_name,
            created_at: SystemTime::now(),
            requests: Vec::new(),
            tags: Vec::new(),
            description,
            statistics: RecordingStatistics::default(),
        };
        
        let mut recorded_requests = self.recorded_requests.write().await;
        
        // 检查录制会话数限制
        if recorded_requests.len() >= self.config.max_recorded_sessions {
            // 移除最旧的会话
            if let Some(oldest_id) = recorded_requests
                .iter()
                .min_by_key(|(_, session)| session.created_at)
                .map(|(id, _)| id.clone()) {
                recorded_requests.remove(&oldest_id);
                warn!("Removed oldest recording session due to limit");
            }
        }
        
        recorded_requests.insert(session_id.clone(), session);
        drop(recorded_requests);
        
        // 发送事件
        self.emit_event(ReplayEvent {
            event_type: ReplayEventType::RecordingStarted,
            session_id: session_id.clone(),
            request_id: None,
            data: HashMap::new(),
            timestamp: SystemTime::now(),
        }).await;
        
        Ok(session_id)
    }
    
    /// 录制请求
    #[instrument(skip(self, context, request, response))]
    pub async fn record_request(
        &self,
        session_id: &str,
        context: &ConnectionContext,
        request: &HttpRequest,
        response: Option<&HttpResponse>,
    ) -> Result<(), AppError> {
        debug!("Recording request for session: {}", session_id);
        
        // 应用录制过滤器
        if !self.should_record_request(request, response).await {
            debug!("Request filtered out, not recording");
            return Ok(());
        }
        
        let recorded_request = RecordedRequest {
            id: Uuid::new_v4(),
            connection_id: context.id.clone(),
            request: request.clone(),
            response: response.cloned(),
            recorded_at: SystemTime::now(),
            duration: None, // 可以从上下文中计算
            status: RequestStatus::Recorded,
            metadata: context.custom_data.clone(),
        };
        
        let mut recorded_requests = self.recorded_requests.write().await;
        
        if let Some(session) = recorded_requests.get_mut(session_id) {
            // 检查单个会话的请求数限制
            if session.requests.len() >= self.config.max_requests_per_session {
                warn!("Recording session {} reached request limit", session_id);
                return Err(AppError::Config("Recording session request limit reached".to_string()));
            }
            
            session.requests.push(recorded_request.clone());
            
            // 更新统计
            session.statistics.total_requests += 1;
            session.statistics.successful_recordings += 1;
            
            // 更新主机列表
            if let Some(host) = Self::extract_host_from_request(request) {
                if !session.statistics.hosts.contains(&host) {
                    session.statistics.hosts.push(host);
                }
            }
            
            // 更新方法分布
            let method = request.method.clone();
            *session.statistics.method_distribution.entry(method).or_insert(0) += 1;
            
            // 发送事件
            self.emit_event(ReplayEvent {
                event_type: ReplayEventType::RequestRecorded,
                session_id: session_id.to_string(),
                request_id: Some(recorded_request.id),
                data: {
                    let mut data = HashMap::new();
                    data.insert("method".to_string(), serde_json::Value::String(request.method.clone()));
                    data.insert("url".to_string(), serde_json::Value::String(request.url.clone()));
                    data
                },
                timestamp: SystemTime::now(),
            }).await;
            
            debug!("✅ Request recorded successfully in session {}", session_id);
            Ok(())
        } else {
            Err(AppError::Config(format!("Recording session {} not found", session_id)))
        }
    }
    
    /// 停止录制会话
    #[instrument(skip(self))]
    pub async fn stop_recording(&self, session_id: &str) -> Result<(), AppError> {
        info!("⏹️ Stopping recording session: {}", session_id);
        
        let recorded_requests = self.recorded_requests.read().await;
        
        if let Some(session) = recorded_requests.get(session_id) {
            // 保存到磁盘
            self.save_session_to_disk(session).await?;
            
            // 发送事件
            self.emit_event(ReplayEvent {
                event_type: ReplayEventType::RecordingCompleted,
                session_id: session_id.to_string(),
                request_id: None,
                data: {
                    let mut data = HashMap::new();
                    data.insert("total_requests".to_string(), 
                               serde_json::Value::Number(session.statistics.total_requests.into()));
                    data
                },
                timestamp: SystemTime::now(),
            }).await;
            
            info!("✅ Recording session {} stopped with {} requests", 
                  session_id, session.requests.len());
            Ok(())
        } else {
            Err(AppError::Config(format!("Recording session {} not found", session_id)))
        }
    }
    
    /// 创建重放会话
    #[instrument(skip(self))]
    pub async fn create_replay_session(
        &self,
        recording_session_id: &str,
        name: String,
        config: ReplayConfig,
    ) -> Result<String, AppError> {
        let session_id = Uuid::new_v4().to_string();
        
        info!("🎮 Creating replay session: {} ({})", name, session_id);
        
        // 验证录制会话存在
        let recorded_requests = self.recorded_requests.read().await;
        if !recorded_requests.contains_key(recording_session_id) {
            return Err(AppError::Config(format!("Recording session {} not found", recording_session_id)));
        }
        drop(recorded_requests);
        
        let replay_session = ReplaySession {
            session_id: session_id.clone(),
            based_on_recording: recording_session_id.to_string(),
            name,
            created_at: SystemTime::now(),
            config,
            results: Vec::new(),
            status: ReplaySessionStatus::Created,
            started_at: None,
            completed_at: None,
            statistics: ReplayStatistics::default(),
        };
        
        let mut replay_sessions = self.replay_sessions.write().await;
        
        // 检查重放会话数限制
        if replay_sessions.len() >= self.config.max_replay_sessions {
            // 移除最旧的会话
            if let Some(oldest_id) = replay_sessions
                .iter()
                .min_by_key(|(_, session)| session.created_at)
                .map(|(id, _)| id.clone()) {
                replay_sessions.remove(&oldest_id);
                warn!("Removed oldest replay session due to limit");
            }
        }
        
        replay_sessions.insert(session_id.clone(), replay_session);
        drop(replay_sessions);
        
        info!("✅ Replay session created: {}", session_id);
        Ok(session_id)
    }
    
    /// 执行重放会话
    #[instrument(skip(self))]
    pub async fn execute_replay_session(&self, session_id: &str) -> Result<(), AppError> {
        info!("▶️ Executing replay session: {}", session_id);
        
        // 获取重放会话和录制数据
        let (replay_session, recorded_requests) = {
            let replay_sessions = self.replay_sessions.read().await;
            let recorded_requests = self.recorded_requests.read().await;
            
            let replay_session = replay_sessions.get(session_id)
                .ok_or_else(|| AppError::Config(format!("Replay session {} not found", session_id)))?
                .clone();
            
            let recorded_session = recorded_requests.get(&replay_session.based_on_recording)
                .ok_or_else(|| AppError::Config(format!("Recording session {} not found", replay_session.based_on_recording)))?
                .clone();
            
            (replay_session, recorded_session.requests)
        };
        
        // 更新会话状态
        self.update_replay_session_status(session_id, ReplaySessionStatus::Running).await?;
        
        // 发送开始事件
        self.emit_event(ReplayEvent {
            event_type: ReplayEventType::ReplaySessionStarted,
            session_id: session_id.to_string(),
            request_id: None,
            data: {
                let mut data = HashMap::new();
                data.insert("total_requests".to_string(), 
                           serde_json::Value::Number(recorded_requests.len().into()));
                data
            },
            timestamp: SystemTime::now(),
        }).await;
        
        // 执行重放
        let results = self.replay_requests(session_id, &recorded_requests, &replay_session.config).await?;
        
        // 更新重放会话结果
        {
            let mut replay_sessions = self.replay_sessions.write().await;
            if let Some(session) = replay_sessions.get_mut(session_id) {
                session.results = results;
                session.status = ReplaySessionStatus::Completed;
                session.completed_at = Some(SystemTime::now());
                
                // 更新统计
                self.update_replay_statistics(session).await;
            }
        }
        
        // 发送完成事件
        self.emit_event(ReplayEvent {
            event_type: ReplayEventType::ReplaySessionCompleted,
            session_id: session_id.to_string(),
            request_id: None,
            data: HashMap::new(),
            timestamp: SystemTime::now(),
        }).await;
        
        info!("✅ Replay session {} completed", session_id);
        Ok(())
    }
    
    /// 重放单个请求
    async fn replay_single_request(
        &self,
        recorded_request: &RecordedRequest,
        config: &ReplayConfig,
    ) -> ReplayResult {
        let start_time = Instant::now();
        
        // 应用请求修改
        let mut modified_request = recorded_request.request.clone();
        self.apply_request_modifiers(&mut modified_request, config).await;
        
        // 应用自定义头部
        for (key, value) in &config.custom_headers {
            if let (Ok(name), Ok(val)) = (key.parse::<hyper::header::HeaderName>(), value.parse::<hyper::header::HeaderValue>()) {
                modified_request.headers.insert(name, val);
            }
        }
        
        debug!("Replaying request: {} {}", modified_request.method, modified_request.url);
        
        // 发送请求
        let response_result = tokio::time::timeout(
            self.config.replay_timeout,
            self.api_port.http_request(modified_request.clone())
        ).await;
        
        let duration = start_time.elapsed();
        
        let (status, response, error, comparison_result) = match response_result {
            Ok(Ok(response)) => {
                let comparison = if config.compare_responses {
                    if let Some(ref original_response) = recorded_request.response {
                        Some(self.compare_responses(original_response, &response).await)
                    } else {
                        None
                    }
                } else {
                    None
                };
                
                (ReplayStatus::Success, Some(response), None, comparison)
            }
            Ok(Err(e)) => {
                (ReplayStatus::Failed, None, Some(format!("Request failed: {}", e)), None)
            }
            Err(_) => {
                (ReplayStatus::Timeout, None, Some("Request timed out".to_string()), None)
            }
        };
        
        ReplayResult {
            id: Uuid::new_v4(),
            original_request_id: recorded_request.id,
            replayed_request: modified_request,
            replayed_response: response,
            replayed_at: SystemTime::now(),
            duration: Some(duration),
            status,
            error,
            comparison_result,
        }
    }
    
    /// 批量重放请求
    async fn replay_requests(
        &self,
        session_id: &str,
        requests: &[RecordedRequest],
        config: &ReplayConfig,
    ) -> Result<Vec<ReplayResult>, AppError> {
        let mut results = Vec::new();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(config.concurrency));
        let mut tasks = Vec::new();
        
        for (index, request) in requests.iter().enumerate() {
            let permit = semaphore.clone().acquire_owned().await
                .map_err(|_| AppError::Config("Failed to acquire semaphore permit".to_string()))?;
            
            let request_clone = request.clone();
            let config_clone = config.clone();
            let session_id = session_id.to_string();
            let self_clone = self.clone_for_task();
            
            let task = tokio::spawn(async move {
                let _permit = permit; // 保持许可证直到任务完成
                
                // 应用延迟
                if let Some(delay) = config_clone.delay_between_requests {
                    if index > 0 {
                        tokio::time::sleep(delay).await;
                    }
                }
                
                let mut result = self_clone.replay_single_request(&request_clone, &config_clone).await;
                
                // 重试逻辑
                let mut retry_count = 0;
                while result.status == ReplayStatus::Failed && retry_count < config_clone.retry_count {
                    retry_count += 1;
                    debug!("Retrying request {} (attempt {})", request_clone.id, retry_count + 1);
                    
                    tokio::time::sleep(Duration::from_millis((1000 * retry_count) as u64)).await;
                    result = self_clone.replay_single_request(&request_clone, &config_clone).await;
                }
                
                // 发送重放完成事件
                self_clone.emit_event(ReplayEvent {
                    event_type: ReplayEventType::RequestReplayed,
                    session_id,
                    request_id: Some(request_clone.id),
                    data: {
                        let mut data = HashMap::new();
                        data.insert("status".to_string(), 
                                   serde_json::Value::String(format!("{:?}", result.status)));
                        if let Some(duration) = result.duration {
                            data.insert("duration_ms".to_string(), 
                                       serde_json::Value::Number((duration.as_millis() as u64).into()));
                        }
                        data
                    },
                    timestamp: SystemTime::now(),
                }).await;
                
                result
            });
            
            tasks.push(task);
        }
        
        // 等待所有任务完成
        for task in tasks {
            let result = task.await
                .map_err(|e| AppError::Config(format!("Task execution failed: {}", e)))?;
            results.push(result);
        }
        
        Ok(results)
    }
    
    /// 应用请求修改器
    async fn apply_request_modifiers(&self, request: &mut HttpRequest, config: &ReplayConfig) {
        for modifier in &config.request_modifiers {
            if !modifier.enabled {
                continue;
            }
            
            match modifier.modifier_type {
                ModifierType::ReplaceHeader => {
                    if let (Ok(name), Ok(val)) = (modifier.target.parse::<hyper::header::HeaderName>(), modifier.value.parse::<hyper::header::HeaderValue>()) {
                        request.headers.insert(name, val);
                    }
                }
                ModifierType::AddHeader => {
                    if let Ok(name) = modifier.target.parse::<hyper::header::HeaderName>() {
                        if !request.headers.contains_key(&name) {
                            if let Ok(val) = modifier.value.parse() {
                                request.headers.insert(name, val);
                            }
                        }
                    }
                }
                ModifierType::RemoveHeader => {
                    if let Ok(name) = modifier.target.parse::<hyper::header::HeaderName>() {
                        request.headers.remove(name);
                    }
                }
                ModifierType::ReplaceHost => {
                    request.url = self.replace_url_host(&request.url, &modifier.value);
                }
                ModifierType::ReplacePath => {
                    request.url = self.replace_url_path(&request.url, &modifier.value);
                }
                ModifierType::ReplaceUrlParam => {
                    request.url = self.replace_url_param(&request.url, &modifier.target, &modifier.value);
                }
                ModifierType::AddUrlParam => {
                    request.url = self.add_url_param(&request.url, &modifier.target, &modifier.value);
                }
                ModifierType::ReplaceBody => {
                    request.body = modifier.value.as_bytes().to_vec();
                }
                ModifierType::LuaScript => {
                    // 这里可以集成Lua脚本来修改请求
                    // 暂时跳过实现
                    debug!("Lua script modifier not implemented: {}", modifier.value);
                }
            }
        }
        
        // 应用目标主机和端口重定向
        if let Some(ref target_host) = config.target_host {
            request.url = self.replace_url_host(&request.url, target_host);
        }
        
        if let Some(target_port) = config.target_port {
            request.url = self.replace_url_port(&request.url, target_port);
        }
    }
    
    /// 比较响应
    async fn compare_responses(&self, original: &HttpResponse, replayed: &HttpResponse) -> ComparisonResult {
        let mut differences = Vec::new();
        let mut score = 1.0;
        
        // 比较状态码
        if original.status_code != replayed.status_code {
            differences.push(Difference {
                difference_type: DifferenceType::StatusCode,
                field_path: "status_code".to_string(),
                original_value: Some(original.status_code.to_string()),
                replayed_value: Some(replayed.status_code.to_string()),
                description: "Status codes differ".to_string(),
            });
            score -= 0.2;
        }
        
        // 比较头部
        for (key, original_value) in &original.headers {
            if let Some(replayed_value) = replayed.headers.get(key) {
                if original_value != replayed_value {
                    differences.push(Difference {
                        difference_type: DifferenceType::Header,
                        field_path: format!("headers.{}", key),
                        original_value: original_value.to_str().ok().map(|s| s.to_string()),
                        replayed_value: replayed_value.to_str().ok().map(|s| s.to_string()),
                        description: format!("Header '{}' values differ", key),
                    });
                    score -= 0.05;
                }
            } else {
                differences.push(Difference {
                    difference_type: DifferenceType::MissingField,
                    field_path: format!("headers.{}", key),
                    original_value: original_value.to_str().ok().map(|s| s.to_string()),
                    replayed_value: None,
                    description: format!("Header '{}' missing in replayed response", key),
                });
                score -= 0.05;
            }
        }
        
        // 检查额外的头部
        for key in replayed.headers.keys() {
            if !original.headers.contains_key(key) {
                differences.push(Difference {
                    difference_type: DifferenceType::ExtraField,
                    field_path: format!("headers.{}", key),
                    original_value: None,
                    replayed_value: replayed.headers.get(key).and_then(|v| v.to_str().ok()).map(|s| s.to_string()),
                    description: format!("Extra header '{}' in replayed response", key),
                });
                score -= 0.02;
            }
        }
        
        // 比较内容长度
        if original.body.len() != replayed.body.len() {
            differences.push(Difference {
                difference_type: DifferenceType::ContentLength,
                field_path: "body.length".to_string(),
                original_value: Some(original.body.len().to_string()),
                replayed_value: Some(replayed.body.len().to_string()),
                description: "Content lengths differ".to_string(),
            });
            score -= 0.1;
        }
        
        // 比较内容（如果是文本）
        if Self::is_text_content(&original.headers) && Self::is_text_content(&replayed.headers) {
            let original_text = String::from_utf8_lossy(&original.body);
            let replayed_text = String::from_utf8_lossy(&replayed.body);
            
            if original_text != replayed_text {
                differences.push(Difference {
                    difference_type: DifferenceType::Content,
                    field_path: "body.content".to_string(),
                    original_value: Some(Self::truncate_string(&original_text, 100)),
                    replayed_value: Some(Self::truncate_string(&replayed_text, 100)),
                    description: "Content differs".to_string(),
                });
                
                // 计算文本相似度
                let text_similarity = Self::calculate_text_similarity(&original_text, &replayed_text);
                score = (score + text_similarity) / 2.0;
            }
        }
        
        // 确保分数在合理范围内
        score = score.max(0.0).min(1.0);
        
        let matches = score >= self.config.comparison_tolerance;
        
        ComparisonResult {
            matches,
            similarity_score: score,
            differences,
            compared_at: SystemTime::now(),
        }
    }
    
    /// 检查录制过滤器
    async fn should_record_request(&self, request: &HttpRequest, _response: Option<&HttpResponse>) -> bool {
        if self.config.recording_filters.is_empty() {
            return true;
        }
        
        for filter in &self.config.recording_filters {
            if !filter.enabled {
                continue;
            }
            
            if self.evaluate_filter_condition(&filter.condition, request).await {
                return true;
            }
        }
        
        false
    }
    
    /// 评估过滤条件
    fn evaluate_filter_condition<'a>(&'a self, condition: &'a FilterCondition, request: &'a HttpRequest) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            match condition {
            FilterCondition::HostMatches(pattern) => {
                if let Some(host) = Self::extract_host_from_request(request) {
                    self.wildcard_match(&host, pattern)
                } else {
                    false
                }
            }
            FilterCondition::PathMatches(pattern) => {
                let path = Self::extract_path_from_request(request);
                self.wildcard_match(&path, pattern)
            }
            FilterCondition::MethodEquals(method) => {
                request.method.eq_ignore_ascii_case(method)
            }
            FilterCondition::StatusCodeEquals(_status_code) => {
                // 需要响应信息，这里暂时返回true
                true
            }
            FilterCondition::ContentTypeMatches(pattern) => {
                if let Some(content_type) = request.headers.get("content-type") {
                    if let Ok(ct_str) = content_type.to_str() {
                        return self.wildcard_match(ct_str, pattern);
                    }
                }
                false
            }
            FilterCondition::And(conditions) => {
                for condition in conditions {
                    if !Box::pin(self.evaluate_filter_condition(condition, request)).await {
                        return false;
                    }
                }
                true
            }
            FilterCondition::Or(conditions) => {
                for condition in conditions {
                    if Box::pin(self.evaluate_filter_condition(condition, request)).await {
                        return true;
                    }
                }
                false
            }
            FilterCondition::Not(condition) => {
                !Box::pin(self.evaluate_filter_condition(condition, request)).await
            }
        }
        })
    }
    
    /// 通配符匹配
    fn wildcard_match(&self, text: &str, pattern: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        
        // 简化的通配符实现
        if pattern.contains('*') {
            let parts: Vec<&str> = pattern.split('*').collect();
            if parts.len() == 2 && parts[0].is_empty() {
                text.ends_with(parts[1])
            } else if parts.len() == 2 && parts[1].is_empty() {
                text.starts_with(parts[0])
            } else {
                text.contains(&pattern.replace('*', ""))
            }
        } else {
            text == pattern
        }
    }
    
    /// 从请求中提取主机名
    fn extract_host_from_request(request: &HttpRequest) -> Option<String> {
        if let Ok(url) = url::Url::parse(&request.url) {
            url.host_str().map(|s| s.to_string())
        } else {
            None
        }
    }
    
    /// 从请求中提取路径
    fn extract_path_from_request(request: &HttpRequest) -> String {
        if let Ok(url) = url::Url::parse(&request.url) {
            url.path().to_string()
        } else {
            "/".to_string()
        }
    }
    
    /// URL操作辅助方法
    fn replace_url_host(&self, url: &str, new_host: &str) -> String {
        if let Ok(mut parsed_url) = url::Url::parse(url) {
            if parsed_url.set_host(Some(new_host)).is_ok() {
                return parsed_url.to_string();
            }
        }
        url.to_string()
    }
    
    fn replace_url_port(&self, url: &str, new_port: u16) -> String {
        if let Ok(mut parsed_url) = url::Url::parse(url) {
            if parsed_url.set_port(Some(new_port)).is_ok() {
                return parsed_url.to_string();
            }
        }
        url.to_string()
    }
    
    fn replace_url_path(&self, url: &str, new_path: &str) -> String {
        if let Ok(mut parsed_url) = url::Url::parse(url) {
            parsed_url.set_path(new_path);
            return parsed_url.to_string();
        }
        url.to_string()
    }
    
    fn replace_url_param(&self, url: &str, param_name: &str, param_value: &str) -> String {
        if let Ok(mut parsed_url) = url::Url::parse(url) {
            let mut query_params: Vec<(String, String)> = parsed_url.query_pairs()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            
            // 替换或添加参数
            let mut found = false;
            for (key, value) in &mut query_params {
                if key == param_name {
                    *value = param_value.to_string();
                    found = true;
                    break;
                }
            }
            
            if !found {
                query_params.push((param_name.to_string(), param_value.to_string()));
            }
            
            // 重新构建查询字符串
            let query_string = query_params.iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join("&");
            
            parsed_url.set_query(if query_string.is_empty() { None } else { Some(&query_string) });
            return parsed_url.to_string();
        }
        url.to_string()
    }
    
    fn add_url_param(&self, url: &str, param_name: &str, param_value: &str) -> String {
        self.replace_url_param(url, param_name, param_value)
    }
    
    /// 检查是否是文本内容
    fn is_text_content(headers: &hyper::HeaderMap) -> bool {
        if let Some(content_type) = headers.get("content-type") {
            if let Ok(ct_str) = content_type.to_str() {
                return ct_str.starts_with("text/") || 
                    ct_str.starts_with("application/json") ||
                    ct_str.starts_with("application/xml");
            }
        }
        false
    }
    
    /// 截断字符串
    fn truncate_string(s: &str, max_len: usize) -> String {
        if s.len() <= max_len {
            s.to_string()
        } else {
            format!("{}...", &s[..max_len])
        }
    }
    
    /// 计算文本相似度
    fn calculate_text_similarity(text1: &str, text2: &str) -> f64 {
        // 简化的相似度计算 - 在实际实现中可以使用更高级的算法
        let len1 = text1.len();
        let len2 = text2.len();
        
        if len1 == 0 && len2 == 0 {
            return 1.0;
        }
        
        if len1 == 0 || len2 == 0 {
            return 0.0;
        }
        
        let max_len = len1.max(len2);
        let common_chars = text1.chars()
            .zip(text2.chars())
            .take_while(|(c1, c2)| c1 == c2)
            .count();
        
        common_chars as f64 / max_len as f64
    }
    
    /// 工具方法：更新重放会话状态
    async fn update_replay_session_status(&self, session_id: &str, status: ReplaySessionStatus) -> Result<(), AppError> {
        let mut replay_sessions = self.replay_sessions.write().await;
        
        if let Some(session) = replay_sessions.get_mut(session_id) {
            session.status = status;
            match session.status {
                ReplaySessionStatus::Running => {
                    session.started_at = Some(SystemTime::now());
                }
                ReplaySessionStatus::Completed | ReplaySessionStatus::Failed | ReplaySessionStatus::Cancelled => {
                    session.completed_at = Some(SystemTime::now());
                }
                _ => {}
            }
            Ok(())
        } else {
            Err(AppError::Config(format!("Replay session {} not found", session_id)))
        }
    }
    
    /// 更新重放统计
    async fn update_replay_statistics(&self, session: &mut ReplaySession) {
        let total_replays = session.results.len() as u64;
        let successful_replays = session.results.iter()
            .filter(|r| r.status == ReplayStatus::Success)
            .count() as u64;
        let failed_replays = session.results.iter()
            .filter(|r| r.status == ReplayStatus::Failed)
            .count() as u64;
        let timeout_replays = session.results.iter()
            .filter(|r| r.status == ReplayStatus::Timeout)
            .count() as u64;
        
        let response_times: Vec<Duration> = session.results.iter()
            .filter_map(|r| r.duration)
            .collect();
        
        let avg_response_time = if !response_times.is_empty() {
            let total_time = response_times.iter().sum::<Duration>();
            Some(total_time / response_times.len() as u32)
        } else {
            None
        };
        
        let comparisons: Vec<&ComparisonResult> = session.results.iter()
            .filter_map(|r| r.comparison_result.as_ref())
            .collect();
        
        let matching_responses = comparisons.iter()
            .filter(|c| c.matches)
            .count() as u64;
        
        let non_matching_responses = comparisons.iter()
            .filter(|c| !c.matches)
            .count() as u64;
        
        let avg_similarity_score = if !comparisons.is_empty() {
            comparisons.iter()
                .map(|c| c.similarity_score)
                .sum::<f64>() / comparisons.len() as f64
        } else {
            0.0
        };
        
        session.statistics = ReplayStatistics {
            total_replays,
            successful_replays,
            failed_replays,
            timeout_replays,
            avg_response_time,
            matching_responses,
            non_matching_responses,
            avg_similarity_score,
        };
    }
    
    /// 保存会话到磁盘
    async fn save_session_to_disk(&self, session: &RecordedSession) -> Result<(), AppError> {
        let file_path = self.config.storage_dir.join(format!("{}.json", session.session_id));
        
        let json_data = serde_json::to_string_pretty(session)
            .map_err(|e| AppError::Config(format!("Failed to serialize session: {}", e)))?;
        
        fs::write(file_path, json_data)
            .await
            .map_err(|e| AppError::Config(format!("Failed to save session to disk: {}", e)))?;
        
        debug!("Session {} saved to disk", session.session_id);
        Ok(())
    }
    
    /// 加载已保存的会话
    async fn load_saved_sessions(&self) -> Result<(), AppError> {
        let mut dir = match fs::read_dir(&self.config.storage_dir).await {
            Ok(dir) => dir,
            Err(_) => return Ok(()), // 目录不存在或无法读取，跳过
        };
        
        let mut loaded_count = 0;
        
        while let Some(entry) = dir.next_entry().await
            .map_err(|e| AppError::Config(format!("Failed to read directory entry: {}", e)))? {
            
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                match self.load_session_from_file(&path).await {
                    Ok(session) => {
                        let mut recorded_requests = self.recorded_requests.write().await;
                        recorded_requests.insert(session.session_id.clone(), session);
                        loaded_count += 1;
                    }
                    Err(e) => {
                        warn!("Failed to load session from {}: {}", path.display(), e);
                    }
                }
            }
        }
        
        if loaded_count > 0 {
            info!("✅ Loaded {} recorded sessions from disk", loaded_count);
        }
        
        Ok(())
    }
    
    /// 从文件加载会话
    async fn load_session_from_file(&self, file_path: &Path) -> Result<RecordedSession, AppError> {
        let content = fs::read_to_string(file_path)
            .await
            .map_err(|e| AppError::Config(format!("Failed to read session file: {}", e)))?;
        
        let session: RecordedSession = serde_json::from_str(&content)
            .map_err(|e| AppError::Config(format!("Failed to parse session file: {}", e)))?;
        
        Ok(session)
    }
    
    /// 发送事件
    async fn emit_event(&self, event: ReplayEvent) {
        let event_type_str = format!("{:?}", event.event_type);
        let listeners = self.event_listeners.read().await;
        
        if let Some(event_listeners) = listeners.get(&event_type_str) {
            for listener in event_listeners {
                listener(&event);
            }
        }
        
        // 也发送到通用监听器
        if let Some(general_listeners) = listeners.get("*") {
            for listener in general_listeners {
                listener(&event);
            }
        }
    }
    
    /// 为异步任务创建克隆
    fn clone_for_task(&self) -> Self {
        Self {
            recorded_requests: Arc::clone(&self.recorded_requests),
            replay_sessions: Arc::clone(&self.replay_sessions),
            api_port: Arc::clone(&self.api_port),
            config: self.config.clone(),
            statistics: Arc::clone(&self.statistics),
            event_listeners: Arc::clone(&self.event_listeners),
        }
    }
    
    /// 获取录制会话列表
    pub async fn list_recorded_sessions(&self) -> Vec<String> {
        let recorded_requests = self.recorded_requests.read().await;
        recorded_requests.keys().cloned().collect()
    }
    
    /// 获取重放会话列表
    pub async fn list_replay_sessions(&self) -> Vec<String> {
        let replay_sessions = self.replay_sessions.read().await;
        replay_sessions.keys().cloned().collect()
    }
    
    /// 获取录制会话详情
    pub async fn get_recorded_session(&self, session_id: &str) -> Option<RecordedSession> {
        let recorded_requests = self.recorded_requests.read().await;
        recorded_requests.get(session_id).cloned()
    }
    
    /// 获取重放会话详情
    pub async fn get_replay_session(&self, session_id: &str) -> Option<ReplaySession> {
        let replay_sessions = self.replay_sessions.read().await;
        replay_sessions.get(session_id).cloned()
    }
}

impl Default for ReplayManagerConfig {
    fn default() -> Self {
        Self {
            max_recorded_sessions: 1000,
            max_replay_sessions: 500,
            max_requests_per_session: 10000,
            recording_retention: Duration::from_secs(7 * 24 * 3600), // 7天
            replay_session_retention: Duration::from_secs(3 * 24 * 3600), // 3天
            replay_timeout: Duration::from_secs(30),
            max_concurrent_replays: 10,
            storage_dir: PathBuf::from("./replay_data"),
            auto_recording: false,
            recording_filters: vec![
                RecordingFilter {
                    name: "default".to_string(),
                    condition: FilterCondition::MethodEquals("GET".to_string()),
                    enabled: true,
                }
            ],
            enable_result_comparison: true,
            comparison_tolerance: 0.8,
        }
    }
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            target_host: None,
            target_port: None,
            request_modifiers: Vec::new(),
            concurrency: 5,
            delay_between_requests: None,
            retry_count: 0,
            ignore_ssl_errors: false,
            follow_redirects: true,
            custom_headers: HashMap::new(),
            compare_responses: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Connection, HttpRequest};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use async_trait::async_trait;
    
    // Mock API port for testing
    struct MockApiPort;
    
    #[async_trait]
    impl ApiPort for MockApiPort {
        async fn http_request(&self, _request: HttpRequest) -> Result<HttpResponse, AppError> {
            Ok(HttpResponse::new(200))
        }
        
        async fn get_connection_info(&self, _conn_id: crate::core::ports::ConnectionId) -> Result<Option<crate::core::ports::ConnectionInfo>, AppError> { Ok(None) }
        async fn log(&self, _level: crate::core::ports::LogLevel, _message: &str) -> Result<(), AppError> { Ok(()) }
        async fn get_config(&self, _key: &str) -> Result<Option<String>, AppError> { Ok(None) }
        async fn store_data(&self, _key: &str, _value: serde_json::Value) -> Result<(), AppError> { Ok(()) }
        async fn get_data(&self, _key: &str) -> Result<Option<serde_json::Value>, AppError> { Ok(None) }
        async fn emit_event(&self, _event: crate::core::ports::CustomEvent) -> Result<(), AppError> { Ok(()) }
    }
    
    #[tokio::test]
    async fn test_replay_manager_creation() {
        let api_port = Arc::new(MockApiPort);
        let config = ReplayManagerConfig::default();
        
        let manager = ReplayManager::new(api_port, config).await.unwrap();
        assert!(manager.recorded_requests.read().await.is_empty());
    }
    
    #[tokio::test]
    async fn test_recording_session_lifecycle() {
        let api_port = Arc::new(MockApiPort);
        let config = ReplayManagerConfig::default();
        let manager = ReplayManager::new(api_port, config).await.unwrap();
        
        // 开始录制
        let session_id = manager.start_recording(
            "test_session".to_string(),
            Some("Test recording session".to_string())
        ).await.unwrap();
        
        // 创建模拟连接上下文
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        let context = ConnectionContext::new(connection, "example.com".to_string(), 80);
        
        // 录制请求
        let request = HttpRequest::new("GET", "http://example.com/api");
        let response = HttpResponse::new(200);
        
        manager.record_request(&session_id, &context, &request, Some(&response)).await.unwrap();
        
        // 验证录制
        let session = manager.get_recorded_session(&session_id).await.unwrap();
        assert_eq!(session.requests.len(), 1);
        assert_eq!(session.requests[0].request.url, "http://example.com/api");
        
        // 停止录制
        manager.stop_recording(&session_id).await.unwrap();
    }
    
    #[tokio::test]
    async fn test_replay_session_creation() {
        let api_port = Arc::new(MockApiPort);
        let config = ReplayManagerConfig::default();
        let manager = ReplayManager::new(api_port, config).await.unwrap();
        
        // 先创建录制会话
        let recording_session_id = manager.start_recording(
            "recording_session".to_string(),
            None
        ).await.unwrap();
        
        // 创建重放会话
        let replay_config = ReplayConfig::default();
        let replay_session_id = manager.create_replay_session(
            &recording_session_id,
            "replay_session".to_string(),
            replay_config
        ).await.unwrap();
        
        // 验证重放会话
        let replay_session = manager.get_replay_session(&replay_session_id).await.unwrap();
        assert_eq!(replay_session.based_on_recording, recording_session_id);
        assert_eq!(replay_session.status, ReplaySessionStatus::Created);
    }
    
    #[tokio::test]
    async fn test_request_modifiers() {
        let api_port = Arc::new(MockApiPort);
        let config = ReplayManagerConfig::default();
        let manager = ReplayManager::new(api_port, config).await.unwrap();
        
        let mut request = HttpRequest::new("GET", "http://example.com/api");
        request.headers.insert("Authorization".parse::<hyper::header::HeaderName>().unwrap(), "Bearer old_token".parse::<hyper::header::HeaderValue>().unwrap());
        
        let replay_config = ReplayConfig {
            request_modifiers: vec![
                RequestModifier {
                    modifier_type: ModifierType::ReplaceHeader,
                    target: "Authorization".to_string(),
                    value: "Bearer new_token".to_string(),
                    enabled: true,
                }
            ],
            ..Default::default()
        };
        
        manager.apply_request_modifiers(&mut request, &replay_config).await;
        
        assert_eq!(request.headers.get("Authorization").unwrap(), "Bearer new_token");
    }
    
    #[tokio::test]
    async fn test_url_manipulation() {
        let api_port = Arc::new(MockApiPort);
        let config = ReplayManagerConfig::default();
        let manager = ReplayManager::new(api_port, config).await.unwrap();
        
        let original_url = "http://example.com/path?param1=value1";
        
        // 测试主机替换
        let new_url = manager.replace_url_host(original_url, "newhost.com");
        assert_eq!(new_url, "http://newhost.com/path?param1=value1");
        
        // 测试路径替换
        let new_url = manager.replace_url_path(original_url, "/newpath");
        assert_eq!(new_url, "http://example.com/newpath?param1=value1");
        
        // 测试参数替换
        let new_url = manager.replace_url_param(original_url, "param1", "newvalue");
        assert_eq!(new_url, "http://example.com/path?param1=newvalue");
        
        // 测试参数添加
        let new_url = manager.add_url_param(original_url, "param2", "value2");
        assert!(new_url.contains("param2=value2"));
    }
    
    #[tokio::test]
    async fn test_response_comparison() {
        let api_port = Arc::new(MockApiPort);
        let config = ReplayManagerConfig::default();
        let manager = ReplayManager::new(api_port, config).await.unwrap();
        
        let original_response = HttpResponse {
            status_code: 200,
            status_text: Some("OK".to_string()),
            headers: {
                let mut h = hyper::HeaderMap::new();
                h.insert("Content-Type".parse::<hyper::header::HeaderName>().unwrap(), "application/json".parse::<hyper::header::HeaderValue>().unwrap());
                h
            },
            body: b"{\"message\": \"hello\"}".to_vec(),
            id: None,
            sent_at: None,
        };
        
        let replayed_response = HttpResponse {
            status_code: 200,
            status_text: Some("OK".to_string()),
            headers: {
                let mut h = hyper::HeaderMap::new();
                h.insert("Content-Type".parse::<hyper::header::HeaderName>().unwrap(), "application/json".parse::<hyper::header::HeaderValue>().unwrap());
                h
            },
            body: b"{\"message\": \"hello\"}".to_vec(),
            id: None,
            sent_at: None,
        };
        
        let comparison = manager.compare_responses(&original_response, &replayed_response).await;
        
        assert!(comparison.matches);
        assert!(comparison.similarity_score > 0.9);
        assert!(comparison.differences.is_empty());
    }
    
    #[tokio::test]
    async fn test_filter_conditions() {
        let api_port = Arc::new(MockApiPort);
        let config = ReplayManagerConfig::default();
        let manager = ReplayManager::new(api_port, config).await.unwrap();
        
        let request = HttpRequest::new("GET", "http://example.com/api/users");
        
        // 测试主机匹配
        let condition = FilterCondition::HostMatches("example.com".to_string());
        assert!(manager.evaluate_filter_condition(&condition, &request).await);
        
        // 测试路径匹配
        let condition = FilterCondition::PathMatches("/api/*".to_string());
        assert!(manager.evaluate_filter_condition(&condition, &request).await);
        
        // 测试方法匹配
        let condition = FilterCondition::MethodEquals("GET".to_string());
        assert!(manager.evaluate_filter_condition(&condition, &request).await);
        
        // 测试复合条件 (AND)
        let condition = FilterCondition::And(vec![
            FilterCondition::HostMatches("example.com".to_string()),
            FilterCondition::MethodEquals("GET".to_string()),
        ]);
        assert!(manager.evaluate_filter_condition(&condition, &request).await);
    }
}
