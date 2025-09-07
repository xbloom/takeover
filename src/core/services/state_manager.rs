//! 状态管理服务
//! 
//! 负责管理系统的各种状态：
//! - 连接状态和生命周期
//! - 用户会话管理
//! - 缓存管理
//! - 临时数据存储
//! - 统计信息聚合
//! - 系统配置状态

use std::sync::Arc;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant, SystemTime};
use std::net::SocketAddr;
use tokio::sync::{RwLock, Mutex};
use tracing::{info, warn, debug, instrument};
use serde::{Serialize, Deserialize};
use uuid::Uuid;

use crate::core::ports::{ConnectionId, AppError};
use crate::core::model::{HttpRequest, HttpResponse, ProcessingState};

/// 状态管理器
/// 
/// 系统的核心状态管理服务，提供统一的状态存储和访问接口
pub struct StateManager {
    /// 连接状态存储
    connections: Arc<RwLock<HashMap<ConnectionId, ConnectionState>>>,
    /// 用户会话存储
    sessions: Arc<RwLock<HashMap<String, SessionState>>>,
    /// 全局缓存
    cache: Arc<RwLock<HashMap<String, CacheEntry>>>,
    /// 临时数据存储
    temp_storage: Arc<RwLock<HashMap<String, TempDataEntry>>>,
    /// 系统统计
    statistics: Arc<RwLock<SystemStatistics>>,
    /// 配置状态
    config: StateManagerConfig,
    /// 清理任务状态
    cleanup_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// 事件监听器
    event_listeners: Arc<RwLock<HashMap<String, Vec<Box<dyn Fn(&StateEvent) + Send + Sync>>>>>,
}

/// 状态管理器配置
#[derive(Debug, Clone)]
pub struct StateManagerConfig {
    /// 连接状态最大存储数量
    pub max_connections: usize,
    /// 会话最大存储数量
    pub max_sessions: usize,
    /// 缓存最大存储数量
    pub max_cache_entries: usize,
    /// 临时数据最大存储数量
    pub max_temp_data: usize,
    /// 连接状态过期时间
    pub connection_timeout: Duration,
    /// 会话过期时间
    pub session_timeout: Duration,
    /// 缓存过期时间
    pub cache_timeout: Duration,
    /// 临时数据过期时间
    pub temp_data_timeout: Duration,
    /// 清理任务执行间隔
    pub cleanup_interval: Duration,
    /// 是否启用统计收集
    pub enable_statistics: bool,
    /// 统计数据保留期
    pub statistics_retention: Duration,
}

/// 连接状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionState {
    /// 连接ID
    pub id: ConnectionId,
    /// 源地址
    pub source_addr: SocketAddr,
    /// 目标地址
    pub target_addr: SocketAddr,
    /// 目标主机名
    pub target_host: String,
    /// 目标端口
    pub target_port: u16,
    /// 当前处理状态
    pub processing_state: ProcessingState,
    /// 创建时间
    pub created_at: SystemTime,
    /// 最后活动时间
    pub last_activity: SystemTime,
    /// 请求历史
    pub request_history: VecDeque<RequestRecord>,
    /// 响应历史
    pub response_history: VecDeque<ResponseRecord>,
    /// 自定义数据
    pub custom_data: HashMap<String, serde_json::Value>,
    /// 连接标签
    pub tags: Vec<String>,
    /// 错误信息
    pub errors: Vec<ErrorRecord>,
    /// 性能指标
    pub performance: PerformanceMetrics,
    /// 是否已完成
    pub completed: bool,
}

/// 用户会话状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    /// 会话ID
    pub session_id: String,
    /// 用户ID
    pub user_id: Option<String>,
    /// 创建时间
    pub created_at: SystemTime,
    /// 最后访问时间
    pub last_access: SystemTime,
    /// 会话数据
    pub data: HashMap<String, serde_json::Value>,
    /// 认证状态
    pub authenticated: bool,
    /// 权限列表
    pub permissions: Vec<String>,
    /// 关联的连接ID列表
    pub connections: Vec<ConnectionId>,
    /// 会话标签
    pub tags: Vec<String>,
}

/// 缓存条目
#[derive(Debug, Clone)]
struct CacheEntry {
    /// 键
    #[allow(dead_code)]
    key: String,
    /// 值
    value: serde_json::Value,
    /// 创建时间
    #[allow(dead_code)]
    created_at: Instant,
    /// 过期时间
    expires_at: Option<Instant>,
    /// 访问次数
    access_count: u64,
    /// 最后访问时间
    last_access: Instant,
    /// 标签
    #[allow(dead_code)]
    tags: Vec<String>,
}

/// 临时数据条目
#[derive(Debug, Clone)]
struct TempDataEntry {
    /// 键
    #[allow(dead_code)]
    key: String,
    /// 值
    value: serde_json::Value,
    /// 创建时间
    created_at: Instant,
    /// 过期时间
    expires_at: Instant,
    /// 所有者
    #[allow(dead_code)]
    owner: Option<String>,
}

/// 请求记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestRecord {
    /// 请求ID
    pub id: Uuid,
    /// 请求对象
    pub request: HttpRequest,
    /// 时间戳
    pub timestamp: SystemTime,
    /// 处理时长
    pub processing_time: Option<Duration>,
    /// 状态
    pub status: String,
}

/// 响应记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseRecord {
    /// 响应ID
    pub id: Uuid,
    /// 关联的请求ID
    pub request_id: Uuid,
    /// 响应对象
    pub response: HttpResponse,
    /// 时间戳
    pub timestamp: SystemTime,
    /// 状态
    pub status: String,
}

/// 错误记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorRecord {
    /// 错误ID
    pub id: Uuid,
    /// 错误消息
    pub message: String,
    /// 错误类型
    pub error_type: String,
    /// 时间戳
    pub timestamp: SystemTime,
    /// 堆栈跟踪
    pub stack_trace: Option<String>,
    /// 上下文信息
    pub context: HashMap<String, serde_json::Value>,
}

/// 性能指标
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PerformanceMetrics {
    /// 总请求数
    pub total_requests: u64,
    /// 成功请求数
    pub successful_requests: u64,
    /// 失败请求数
    pub failed_requests: u64,
    /// 平均响应时间
    pub avg_response_time: Option<Duration>,
    /// 最大响应时间
    pub max_response_time: Option<Duration>,
    /// 最小响应时间
    pub min_response_time: Option<Duration>,
    /// 数据传输量（字节）
    pub bytes_transferred: u64,
    /// 连接持续时间
    pub connection_duration: Option<Duration>,
}

/// 系统统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatistics {
    /// 统计开始时间
    #[serde(default = "SystemTime::now")]
    pub start_time: SystemTime,
    /// 活跃连接数
    pub active_connections: u64,
    /// 总连接数
    pub total_connections: u64,
    /// 活跃会话数
    pub active_sessions: u64,
    /// 总会话数
    pub total_sessions: u64,
    /// 缓存命中率
    pub cache_hit_ratio: f64,
    /// 缓存条目数
    pub cache_entries: u64,
    /// 临时数据条目数
    pub temp_data_entries: u64,
    /// 系统内存使用（估算）
    pub memory_usage_bytes: u64,
    /// 各状态的计数
    pub state_counts: HashMap<String, u64>,
    /// 错误统计
    pub error_counts: HashMap<String, u64>,
    /// 最后更新时间
    #[serde(default = "SystemTime::now")]
    pub last_updated: SystemTime,
}

/// 状态事件
#[derive(Debug, Clone)]
pub struct StateEvent {
    /// 事件类型
    pub event_type: StateEventType,
    /// 连接ID（如果适用）
    pub connection_id: Option<ConnectionId>,
    /// 会话ID（如果适用）
    pub session_id: Option<String>,
    /// 事件数据
    pub data: HashMap<String, serde_json::Value>,
    /// 时间戳
    pub timestamp: SystemTime,
}

/// 状态事件类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateEventType {
    /// 连接创建
    ConnectionCreated,
    /// 连接更新
    ConnectionUpdated,
    /// 连接关闭
    ConnectionClosed,
    /// 会话创建
    SessionCreated,
    /// 会话更新
    SessionUpdated,
    /// 会话过期
    SessionExpired,
    /// 缓存命中
    CacheHit,
    /// 缓存未命中
    CacheMiss,
    /// 缓存过期
    CacheExpired,
    /// 统计更新
    StatisticsUpdated,
    /// 错误发生
    ErrorOccurred,
    /// 自定义事件
    Custom(String),
}

impl StateManager {
    /// 创建新的状态管理器
    pub fn new(config: StateManagerConfig) -> Self {
        let manager = Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            cache: Arc::new(RwLock::new(HashMap::new())),
            temp_storage: Arc::new(RwLock::new(HashMap::new())),
            statistics: Arc::new(RwLock::new(SystemStatistics {
                start_time: SystemTime::now(),
                last_updated: SystemTime::now(),
                active_connections: 0,
                total_connections: 0,
                active_sessions: 0,
                total_sessions: 0,
                cache_hit_ratio: 0.0,
                cache_entries: 0,
                temp_data_entries: 0,
                memory_usage_bytes: 0,
                state_counts: HashMap::new(),
                error_counts: HashMap::new(),
            })),
            config,
            cleanup_task: Arc::new(Mutex::new(None)),
            event_listeners: Arc::new(RwLock::new(HashMap::new())),
        };
        
        manager
    }
    
    /// 启动状态管理器
    pub async fn start(&self) -> Result<(), AppError> {
        info!("🚀 Starting state manager");
        
        // 启动清理任务
        self.start_cleanup_task().await?;
        
        info!("✅ State manager started successfully");
        Ok(())
    }
    
    /// 停止状态管理器
    pub async fn stop(&self) -> Result<(), AppError> {
        info!("🛑 Stopping state manager");
        
        // 停止清理任务
        let mut cleanup_task = self.cleanup_task.lock().await;
        if let Some(task) = cleanup_task.take() {
            task.abort();
        }
        
        info!("✅ State manager stopped");
        Ok(())
    }
    
    /// 启动清理任务
    async fn start_cleanup_task(&self) -> Result<(), AppError> {
        let connections = Arc::clone(&self.connections);
        let sessions = Arc::clone(&self.sessions);
        let cache = Arc::clone(&self.cache);
        let temp_storage = Arc::clone(&self.temp_storage);
        let config = self.config.clone();
        
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.cleanup_interval);
            
            loop {
                interval.tick().await;
                
                debug!("Running periodic cleanup");
                
                // 清理过期连接
                let now = SystemTime::now();
                let mut connections = connections.write().await;
                let expired_connections: Vec<ConnectionId> = connections
                    .iter()
                    .filter_map(|(id, state)| {
                        if let Ok(duration) = now.duration_since(state.last_activity) {
                            if duration > config.connection_timeout {
                                Some(id.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .collect();
                
                for conn_id in expired_connections {
                    connections.remove(&conn_id);
                    debug!("Cleaned up expired connection: {}", conn_id);
                }
                drop(connections);
                
                // 清理过期会话
                let mut sessions = sessions.write().await;
                let expired_sessions: Vec<String> = sessions
                    .iter()
                    .filter_map(|(id, state)| {
                        if let Ok(duration) = now.duration_since(state.last_access) {
                            if duration > config.session_timeout {
                                Some(id.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .collect();
                
                for session_id in expired_sessions {
                    sessions.remove(&session_id);
                    debug!("Cleaned up expired session: {}", session_id);
                }
                drop(sessions);
                
                // 清理过期缓存
                let instant_now = Instant::now();
                let mut cache = cache.write().await;
                let expired_cache: Vec<String> = cache
                    .iter()
                    .filter_map(|(key, entry)| {
                        if let Some(expires_at) = entry.expires_at {
                            if instant_now > expires_at {
                                Some(key.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .collect();
                
                for key in expired_cache {
                    cache.remove(&key);
                    debug!("Cleaned up expired cache entry: {}", key);
                }
                drop(cache);
                
                // 清理过期临时数据
                let mut temp_storage = temp_storage.write().await;
                let expired_temp: Vec<String> = temp_storage
                    .iter()
                    .filter_map(|(key, entry)| {
                        if instant_now > entry.expires_at {
                            Some(key.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                
                for key in expired_temp {
                    temp_storage.remove(&key);
                    debug!("Cleaned up expired temp data: {}", key);
                }
                
                debug!("Cleanup completed");
            }
        });
        
        *self.cleanup_task.lock().await = Some(task);
        Ok(())
    }
    
    /// 创建新连接状态
    #[instrument(skip(self))]
    pub async fn create_connection_state(
        &self,
        connection_id: ConnectionId,
        source_addr: SocketAddr,
        target_addr: SocketAddr,
        target_host: String,
        target_port: u16,
    ) -> Result<(), AppError> {
        debug!("Creating connection state for {}", connection_id);
        
        let state = ConnectionState {
            id: connection_id.clone(),
            source_addr,
            target_addr,
            target_host,
            target_port,
            processing_state: ProcessingState::Initialized,
            created_at: SystemTime::now(),
            last_activity: SystemTime::now(),
            request_history: VecDeque::new(),
            response_history: VecDeque::new(),
            custom_data: HashMap::new(),
            tags: Vec::new(),
            errors: Vec::new(),
            performance: PerformanceMetrics::default(),
            completed: false,
        };
        
        let mut connections = self.connections.write().await;
        
        // 检查连接数限制
        if connections.len() >= self.config.max_connections {
            // 移除最旧的连接
            if let Some(oldest_id) = connections
                .iter()
                .min_by_key(|(_, state)| state.last_activity)
                .map(|(id, _)| id.clone()) {
                connections.remove(&oldest_id);
                warn!("Removed oldest connection {} due to limit", oldest_id);
            }
        }
        
        connections.insert(connection_id.clone(), state);
        drop(connections);
        
        // 更新统计
        if self.config.enable_statistics {
            let mut stats = self.statistics.write().await;
            stats.active_connections += 1;
            stats.total_connections += 1;
            stats.last_updated = SystemTime::now();
        }
        
        // 发送事件
        self.emit_event(StateEvent {
            event_type: StateEventType::ConnectionCreated,
            connection_id: Some(connection_id.clone()),
            session_id: None,
            data: HashMap::new(),
            timestamp: SystemTime::now(),
        }).await;
        
        info!("✅ Connection state created for {}", &connection_id);
        Ok(())
    }
    
    /// 更新连接状态
    #[instrument(skip(self, updater))]
    pub async fn update_connection_state<F>(&self, connection_id: ConnectionId, updater: F) -> Result<(), AppError>
    where
        F: FnOnce(&mut ConnectionState),
    {
        let mut connections = self.connections.write().await;
        
        if let Some(state) = connections.get_mut(&connection_id) {
            updater(state);
            state.last_activity = SystemTime::now();
            
            // 发送更新事件
            self.emit_event(StateEvent {
                event_type: StateEventType::ConnectionUpdated,
                connection_id: Some(connection_id.clone()),
                session_id: None,
                data: HashMap::new(),
                timestamp: SystemTime::now(),
            }).await;
            
            debug!("Updated connection state for {}", &connection_id);
            Ok(())
        } else {
            Err(AppError::Config(format!("Connection {} not found", connection_id)))
        }
    }
    
    /// 获取连接状态
    pub async fn get_connection_state(&self, connection_id: ConnectionId) -> Option<ConnectionState> {
        let connections = self.connections.read().await;
        connections.get(&connection_id).cloned()
    }
    
    /// 关闭连接状态
    #[instrument(skip(self))]
    pub async fn close_connection_state(&self, connection_id: ConnectionId) -> Result<(), AppError> {
        let mut connections = self.connections.write().await;
        
        if let Some(mut state) = connections.remove(&connection_id) {
            state.completed = true;
            
            // 更新统计
            if self.config.enable_statistics {
                let mut stats = self.statistics.write().await;
                if stats.active_connections > 0 {
                    stats.active_connections -= 1;
                }
                stats.last_updated = SystemTime::now();
            }
            
            // 发送关闭事件
            self.emit_event(StateEvent {
                event_type: StateEventType::ConnectionClosed,
                connection_id: Some(connection_id.clone()),
                session_id: None,
                data: HashMap::new(),
                timestamp: SystemTime::now(),
            }).await;
            
            info!("✅ Connection state closed for {}", &connection_id);
            Ok(())
        } else {
            Err(AppError::Config(format!("Connection {} not found", connection_id)))
        }
    }
    
    /// 创建会话
    #[instrument(skip(self))]
    pub async fn create_session(&self, session_id: String, user_id: Option<String>) -> Result<(), AppError> {
        debug!("Creating session: {}", session_id);
        
        let session = SessionState {
            session_id: session_id.clone(),
            user_id,
            created_at: SystemTime::now(),
            last_access: SystemTime::now(),
            data: HashMap::new(),
            authenticated: false,
            permissions: Vec::new(),
            connections: Vec::new(),
            tags: Vec::new(),
        };
        
        let mut sessions = self.sessions.write().await;
        
        // 检查会话数限制
        if sessions.len() >= self.config.max_sessions {
            // 移除最旧的会话
            if let Some(oldest_id) = sessions
                .iter()
                .min_by_key(|(_, session)| session.last_access)
                .map(|(id, _)| id.clone()) {
                sessions.remove(&oldest_id);
                warn!("Removed oldest session {} due to limit", oldest_id);
            }
        }
        
        sessions.insert(session_id.clone(), session);
        drop(sessions);
        
        // 更新统计
        if self.config.enable_statistics {
            let mut stats = self.statistics.write().await;
            stats.active_sessions += 1;
            stats.total_sessions += 1;
            stats.last_updated = SystemTime::now();
        }
        
        // 发送事件
        self.emit_event(StateEvent {
            event_type: StateEventType::SessionCreated,
            connection_id: None,
            session_id: Some(session_id.clone()),
            data: HashMap::new(),
            timestamp: SystemTime::now(),
        }).await;
        
        info!("✅ Session created: {}", session_id);
        Ok(())
    }
    
    /// 更新会话
    #[instrument(skip(self, updater))]
    pub async fn update_session<F>(&self, session_id: &str, updater: F) -> Result<(), AppError>
    where
        F: FnOnce(&mut SessionState),
    {
        let mut sessions = self.sessions.write().await;
        
        if let Some(session) = sessions.get_mut(session_id) {
            updater(session);
            session.last_access = SystemTime::now();
            
            // 发送更新事件
            self.emit_event(StateEvent {
                event_type: StateEventType::SessionUpdated,
                connection_id: None,
                session_id: Some(session_id.to_string()),
                data: HashMap::new(),
                timestamp: SystemTime::now(),
            }).await;
            
            debug!("Updated session: {}", session_id);
            Ok(())
        } else {
            Err(AppError::Config(format!("Session {} not found", session_id)))
        }
    }
    
    /// 获取会话
    pub async fn get_session(&self, session_id: &str) -> Option<SessionState> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).cloned()
    }
    
    /// 设置缓存
    #[instrument(skip(self, value))]
    pub async fn set_cache(&self, key: String, value: serde_json::Value, ttl: Option<Duration>) -> Result<(), AppError> {
        let entry = CacheEntry {
            key: key.clone(),
            value,
            created_at: Instant::now(),
            expires_at: ttl.map(|t| Instant::now() + t),
            access_count: 0,
            last_access: Instant::now(),
            tags: Vec::new(),
        };
        
        let mut cache = self.cache.write().await;
        
        // 检查缓存数限制
        if cache.len() >= self.config.max_cache_entries && !cache.contains_key(&key) {
            // 移除最少使用的缓存项
            if let Some(lru_key) = cache
                .iter()
                .min_by_key(|(_, entry)| (entry.access_count, entry.last_access))
                .map(|(key, _)| key.clone()) {
                cache.remove(&lru_key);
                debug!("Removed LRU cache entry: {}", lru_key);
            }
        }
        
        cache.insert(key.clone(), entry);
        
        debug!("Cache set: {}", key);
        Ok(())
    }
    
    /// 获取缓存
    #[instrument(skip(self))]
    pub async fn get_cache(&self, key: &str) -> Option<serde_json::Value> {
        let mut cache = self.cache.write().await;
        
        if let Some(entry) = cache.get_mut(key) {
            // 检查是否过期
            if let Some(expires_at) = entry.expires_at {
                if Instant::now() > expires_at {
                    cache.remove(key);
                    self.emit_event(StateEvent {
                        event_type: StateEventType::CacheExpired,
                        connection_id: None,
                        session_id: None,
                        data: {
                            let mut data = HashMap::new();
                            data.insert("key".to_string(), serde_json::Value::String(key.to_string()));
                            data
                        },
                        timestamp: SystemTime::now(),
                    }).await;
                    return None;
                }
            }
            
            // 更新访问统计
            entry.access_count += 1;
            entry.last_access = Instant::now();
            
            // 发送缓存命中事件
            self.emit_event(StateEvent {
                event_type: StateEventType::CacheHit,
                connection_id: None,
                session_id: None,
                data: {
                    let mut data = HashMap::new();
                    data.insert("key".to_string(), serde_json::Value::String(key.to_string()));
                    data
                },
                timestamp: SystemTime::now(),
            }).await;
            
            debug!("Cache hit: {}", key);
            Some(entry.value.clone())
        } else {
            // 发送缓存未命中事件
            self.emit_event(StateEvent {
                event_type: StateEventType::CacheMiss,
                connection_id: None,
                session_id: None,
                data: {
                    let mut data = HashMap::new();
                    data.insert("key".to_string(), serde_json::Value::String(key.to_string()));
                    data
                },
                timestamp: SystemTime::now(),
            }).await;
            
            debug!("Cache miss: {}", key);
            None
        }
    }
    
    /// 设置临时数据
    #[instrument(skip(self, value))]
    pub async fn set_temp_data(&self, key: String, value: serde_json::Value, ttl: Duration, owner: Option<String>) -> Result<(), AppError> {
        let entry = TempDataEntry {
            key: key.clone(),
            value,
            created_at: Instant::now(),
            expires_at: Instant::now() + ttl,
            owner,
        };
        
        let mut temp_storage = self.temp_storage.write().await;
        
        // 检查临时数据数限制
        if temp_storage.len() >= self.config.max_temp_data && !temp_storage.contains_key(&key) {
            // 移除最旧的临时数据
            if let Some(oldest_key) = temp_storage
                .iter()
                .min_by_key(|(_, entry)| entry.created_at)
                .map(|(key, _)| key.clone()) {
                temp_storage.remove(&oldest_key);
                debug!("Removed oldest temp data: {}", oldest_key);
            }
        }
        
        temp_storage.insert(key.clone(), entry);
        
        debug!("Temp data set: {}", key);
        Ok(())
    }
    
    /// 获取临时数据
    #[instrument(skip(self))]
    pub async fn get_temp_data(&self, key: &str) -> Option<serde_json::Value> {
        let mut temp_storage = self.temp_storage.write().await;
        
        if let Some(entry) = temp_storage.get(key) {
            // 检查是否过期
            if Instant::now() > entry.expires_at {
                temp_storage.remove(key);
                debug!("Temp data expired: {}", key);
                return None;
            }
            
            debug!("Temp data retrieved: {}", key);
            Some(entry.value.clone())
        } else {
            debug!("Temp data not found: {}", key);
            None
        }
    }
    
    /// 获取系统统计
    pub async fn get_statistics(&self) -> SystemStatistics {
        self.statistics.read().await.clone()
    }
    
    /// 添加事件监听器
    pub async fn add_event_listener<F>(&self, event_type: String, listener: F)
    where
        F: Fn(&StateEvent) + Send + Sync + 'static,
    {
        let mut listeners = self.event_listeners.write().await;
        listeners.entry(event_type).or_insert_with(Vec::new).push(Box::new(listener));
    }
    
    /// 发送事件
    async fn emit_event(&self, event: StateEvent) {
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
    
    /// 获取连接列表
    pub async fn list_connections(&self) -> Vec<ConnectionId> {
        let connections = self.connections.read().await;
        connections.keys().cloned().collect()
    }
    
    /// 获取会话列表
    pub async fn list_sessions(&self) -> Vec<String> {
        let sessions = self.sessions.read().await;
        sessions.keys().cloned().collect()
    }
    
    /// 清理所有数据
    pub async fn clear_all(&self) -> Result<(), AppError> {
        info!("🧹 Clearing all state data");
        
        self.connections.write().await.clear();
        self.sessions.write().await.clear();
        self.cache.write().await.clear();
        self.temp_storage.write().await.clear();
        
        // 重置统计
        if self.config.enable_statistics {
            let mut stats = self.statistics.write().await;
            *stats = SystemStatistics {
                start_time: SystemTime::now(),
                last_updated: SystemTime::now(),
                active_connections: 0,
                total_connections: 0,
                active_sessions: 0,
                total_sessions: 0,
                cache_hit_ratio: 0.0,
                cache_entries: 0,
                temp_data_entries: 0,
                memory_usage_bytes: 0,
                state_counts: HashMap::new(),
                error_counts: HashMap::new(),
            };
        }
        
        info!("✅ All state data cleared");
        Ok(())
    }
}

impl Default for StateManagerConfig {
    fn default() -> Self {
        Self {
            max_connections: 10000,
            max_sessions: 5000,
            max_cache_entries: 50000,
            max_temp_data: 10000,
            connection_timeout: Duration::from_secs(3600), // 1小时
            session_timeout: Duration::from_secs(7200),     // 2小时
            cache_timeout: Duration::from_secs(1800),       // 30分钟
            temp_data_timeout: Duration::from_secs(300),    // 5分钟
            cleanup_interval: Duration::from_secs(60),      // 1分钟
            enable_statistics: true,
            statistics_retention: Duration::from_secs(86400), // 24小时
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    
    #[tokio::test]
    async fn test_state_manager_creation() {
        let config = StateManagerConfig::default();
        let manager = StateManager::new(config);
        
        assert!(manager.start().await.is_ok());
        assert!(manager.stop().await.is_ok());
    }
    
    #[tokio::test]
    async fn test_connection_state_lifecycle() {
        let manager = StateManager::new(StateManagerConfig::default());
        manager.start().await.unwrap();
        
        let conn_id = crate::utils::generate_short_connection_id()
        ;
        let source_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345);
        let target_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80);
        
        // 创建连接状态
        manager.create_connection_state(
            conn_id.clone(),
            source_addr,
            target_addr,
            "example.com".to_string(),
            80
        ).await.unwrap();
        
        // 获取连接状态
        let state = manager.get_connection_state(conn_id.clone()).await;
        assert!(state.is_some());
        assert_eq!(state.unwrap().target_host, "example.com");
        
        // 更新连接状态
        manager.update_connection_state(conn_id.clone(), |state| {
            state.processing_state = ProcessingState::Processing;
        }).await.unwrap();
        
        // 关闭连接状态
        manager.close_connection_state(conn_id.clone()).await.unwrap();
        
        // 连接应该被移除
        let state = manager.get_connection_state(conn_id.clone()).await;
        assert!(state.is_none());
        
        manager.stop().await.unwrap();
    }
    
    #[tokio::test]
    async fn test_session_management() {
        let manager = StateManager::new(StateManagerConfig::default());
        manager.start().await.unwrap();
        
        let session_id = "test_session";
        let user_id = Some("user123".to_string());
        
        // 创建会话
        manager.create_session(session_id.to_string(), user_id.clone()).await.unwrap();
        
        // 获取会话
        let session = manager.get_session(session_id).await;
        assert!(session.is_some());
        assert_eq!(session.unwrap().user_id, user_id);
        
        // 更新会话
        manager.update_session(session_id, |session| {
            session.authenticated = true;
            session.permissions.push("read".to_string());
        }).await.unwrap();
        
        let updated_session = manager.get_session(session_id).await.unwrap();
        assert!(updated_session.authenticated);
        assert_eq!(updated_session.permissions.len(), 1);
        
        manager.stop().await.unwrap();
    }
    
    #[tokio::test]
    async fn test_cache_operations() {
        let manager = StateManager::new(StateManagerConfig::default());
        manager.start().await.unwrap();
        
        let key = "test_key";
        let value = serde_json::json!({"message": "hello", "count": 42});
        
        // 设置缓存
        manager.set_cache(key.to_string(), value.clone(), Some(Duration::from_secs(10))).await.unwrap();
        
        // 获取缓存
        let cached_value = manager.get_cache(key).await;
        assert!(cached_value.is_some());
        assert_eq!(cached_value.unwrap(), value);
        
        // 测试缓存未命中
        let non_existent = manager.get_cache("non_existent").await;
        assert!(non_existent.is_none());
        
        manager.stop().await.unwrap();
    }
    
    #[tokio::test]
    async fn test_temp_data_operations() {
        let manager = StateManager::new(StateManagerConfig::default());
        manager.start().await.unwrap();
        
        let key = "temp_key";
        let value = serde_json::json!("temporary_data");
        
        // 设置临时数据
        manager.set_temp_data(key.to_string(), value.clone(), Duration::from_secs(1), None).await.unwrap();
        
        // 立即获取应该成功
        let temp_value = manager.get_temp_data(key).await;
        assert!(temp_value.is_some());
        assert_eq!(temp_value.unwrap(), value);
        
        // 等待过期
        tokio::time::sleep(Duration::from_secs(2)).await;
        
        // 现在应该获取不到
        let expired_value = manager.get_temp_data(key).await;
        assert!(expired_value.is_none());
        
        manager.stop().await.unwrap();
    }
    
    #[tokio::test]
    async fn test_statistics() {
        let manager = StateManager::new(StateManagerConfig::default());
        manager.start().await.unwrap();
        
        let stats = manager.get_statistics().await;
        assert_eq!(stats.active_connections, 0);
        assert_eq!(stats.total_connections, 0);
        
        // 创建一个连接
        let conn_id = crate::utils::generate_short_connection_id();
        let source_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345);
        let target_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80);
        
        manager.create_connection_state(
            conn_id,
            source_addr,
            target_addr,
            "example.com".to_string(),
            80
        ).await.unwrap();
        
        let updated_stats = manager.get_statistics().await;
        assert_eq!(updated_stats.active_connections, 1);
        assert_eq!(updated_stats.total_connections, 1);
        
        manager.stop().await.unwrap();
    }
}
