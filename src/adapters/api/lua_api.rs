//! Lua API适配器
//! 
//! 实现ApiPort接口，为Lua脚本提供系统功能访问：
//! - HTTP请求功能
//! - 日志记录
//! - 配置访问
//! - 数据存储
//! - 连接信息查询
//! - 事件发射

use std::sync::Arc;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use tokio::sync::RwLock;
use tracing::{info, warn, error, debug, instrument};
use async_trait::async_trait;
use serde_json::Value as JsonValue;

use crate::core::ports::{ApiPort, AppError, ConnectionId, ConnectionInfo, LogLevel, CustomEvent};
use crate::core::model::{HttpRequest, HttpResponse};

/// Lua API适配器
/// 
/// 为Lua脚本提供对系统功能的安全访问
pub struct LuaApiAdapter {
    /// HTTP客户端
    http_client: Arc<HttpClient>,
    /// 配置存储
    config_store: Arc<RwLock<HashMap<String, String>>>,
    /// 临时数据存储
    data_store: Arc<RwLock<HashMap<String, JsonValue>>>,
    /// 连接信息缓存
    connection_cache: Arc<RwLock<HashMap<ConnectionId, ConnectionInfo>>>,
    /// 事件发射器
    event_emitter: Arc<EventEmitter>,
    /// API配置
    config: LuaApiConfig,
    /// API统计
    stats: Arc<RwLock<ApiStats>>,
}

/// Lua API配置
#[derive(Debug, Clone)]
pub struct LuaApiConfig {
    /// HTTP请求超时
    pub http_timeout: Duration,
    /// 最大并发HTTP请求数
    pub max_concurrent_requests: usize,
    /// 数据存储最大大小
    pub max_data_store_size: usize,
    /// 连接缓存最大大小
    pub max_connection_cache_size: usize,
    /// 是否启用请求日志
    pub enable_request_logging: bool,
    /// 允许的域名（HTTP请求白名单）
    pub allowed_domains: Vec<String>,
    /// 用户代理字符串
    pub user_agent: String,
}

/// HTTP客户端包装器
struct HttpClient {
    client: reqwest::Client,
    config: LuaApiConfig,
    stats: Arc<RwLock<HttpStats>>,
}

/// HTTP统计
#[derive(Debug, Default, Clone)]
struct HttpStats {
    total_requests: u64,
    successful_requests: u64,
    failed_requests: u64,
    timeout_requests: u64,
    blocked_requests: u64,
    avg_response_time: Option<Duration>,
}

/// 事件发射器
struct EventEmitter {
    event_listeners: Arc<RwLock<HashMap<String, Vec<EventListener>>>>,
}

/// 事件监听器
type EventListener = Box<dyn Fn(&CustomEvent) + Send + Sync>;

/// API统计
#[derive(Debug, Default, Clone)]
struct ApiStats {
    http_requests: u64,
    log_messages: u64,
    config_accesses: u64,
    data_operations: u64,
    connection_queries: u64,
    events_emitted: u64,
}

impl LuaApiAdapter {
    /// 创建新的Lua API适配器
    pub fn new(config: LuaApiConfig) -> Result<Self, AppError> {
        let http_client = Arc::new(HttpClient::new(config.clone())?);
        
        Ok(Self {
            http_client,
            config_store: Arc::new(RwLock::new(HashMap::new())),
            data_store: Arc::new(RwLock::new(HashMap::new())),
            connection_cache: Arc::new(RwLock::new(HashMap::new())),
            event_emitter: Arc::new(EventEmitter::new()),
            config,
            stats: Arc::new(RwLock::new(ApiStats::default())),
        })
    }
    
    /// 预填充配置存储
    pub async fn populate_config(&self, config_map: HashMap<String, String>) {
        let mut store = self.config_store.write().await;
        store.extend(config_map);
    }
    
    /// 添加连接信息到缓存
    pub async fn cache_connection_info(&self, conn_id: ConnectionId, info: ConnectionInfo) {
        let mut cache = self.connection_cache.write().await;
        
        // 检查缓存大小限制
        if cache.len() >= self.config.max_connection_cache_size {
            // 移除最旧的条目（简化实现，可以用LRU）
            if let Some(key) = cache.keys().next().cloned() {
                cache.remove(&key);
            }
        }
        
        cache.insert(conn_id, info);
    }
    
    /// 检查域名是否被允许
    fn is_domain_allowed(&self, url: &str) -> bool {
        if self.config.allowed_domains.is_empty() {
            return true; // 没有限制时允许所有域名
        }
        
        if let Ok(parsed_url) = url::Url::parse(url) {
            if let Some(domain) = parsed_url.domain() {
                return self.config.allowed_domains.iter().any(|allowed| {
                    if allowed == "*" {
                        true
                    } else if allowed.starts_with("*.") {
                        let suffix = &allowed[2..];
                        domain.ends_with(suffix)
                    } else {
                        domain == allowed
                    }
                });
            }
        }
        
        false
    }
    
    /// 获取API统计信息
    pub async fn get_stats(&self) -> ApiStats {
        self.stats.read().await.clone()
    }
    
    /// 获取HTTP统计信息
    pub async fn get_http_stats(&self) -> HttpStats {
        self.http_client.get_stats().await
    }
}

impl HttpClient {
    fn new(config: LuaApiConfig) -> Result<Self, AppError> {
        let client = reqwest::Client::builder()
            .timeout(config.http_timeout)
            .user_agent(config.user_agent.clone())
            .build()
            .map_err(|e| AppError::Config(format!("Failed to create HTTP client: {}", e)))?;
        
        Ok(Self {
            client,
            config,
            stats: Arc::new(RwLock::new(HttpStats::default())),
        })
    }
    
    async fn make_request(&self, request: HttpRequest) -> Result<HttpResponse, AppError> {
        let start_time = std::time::Instant::now();
        
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.total_requests += 1;
        }
        
        // 构建请求
        let mut req_builder = match request.method.to_uppercase().as_str() {
            "GET" => self.client.get(&request.url),
            "POST" => self.client.post(&request.url),
            "PUT" => self.client.put(&request.url),
            "DELETE" => self.client.delete(&request.url),
            "HEAD" => self.client.head(&request.url),
            "PATCH" => self.client.patch(&request.url),
            _ => return Err(AppError::Config(format!("Unsupported HTTP method: {}", request.method))),
        };
        
        // 添加头部
        for (name, value) in &request.headers {
            req_builder = req_builder.header(name, value);
        }
        
        // 添加请求体
        if !request.body.is_empty() {
            req_builder = req_builder.body(request.body);
        }
        
        // 发送请求
        match req_builder.send().await {
            Ok(response) => {
                let status_code = response.status().as_u16();
                let headers: HashMap<String, String> = response
                    .headers()
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
                    .collect();
                
                let body = response.bytes().await
                    .map_err(|e| AppError::Config(format!("Failed to read response body: {}", e)))?
                    .to_vec();
                
                let response_time = start_time.elapsed();
                
                // 更新成功统计
                {
                    let mut stats = self.stats.write().await;
                    stats.successful_requests += 1;
                    stats.avg_response_time = match stats.avg_response_time {
                        Some(avg) => Some((avg + response_time) / 2),
                        None => Some(response_time),
                    };
                }
                
                Ok(HttpResponse {
                    status_code,
                    status_text: None,
                    headers,
                    body,
                    id: None,
                    sent_at: Some(SystemTime::now()),
                })
            }
            Err(e) => {
                // 更新失败统计
                {
                    let mut stats = self.stats.write().await;
                    if e.is_timeout() {
                        stats.timeout_requests += 1;
                    } else {
                        stats.failed_requests += 1;
                    }
                }
                
                Err(AppError::Config(format!("HTTP request failed: {}", e)))
            }
        }
    }
    
    async fn get_stats(&self) -> HttpStats {
        self.stats.read().await.clone()
    }
}

impl EventEmitter {
    fn new() -> Self {
        Self {
            event_listeners: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    async fn emit_event(&self, event: CustomEvent) -> Result<(), AppError> {
        let listeners = self.event_listeners.read().await;
        
        if let Some(event_listeners) = listeners.get(&event.name) {
            for listener in event_listeners {
                listener(&event);
            }
        }
        
        // 也可以在这里记录事件到日志或持久化存储
        debug!("Event emitted: {} with {} data fields", event.name, event.data.len());
        
        Ok(())
    }
    
    #[allow(dead_code)]
    async fn add_listener(&self, event_name: String, listener: EventListener) {
        let mut listeners = self.event_listeners.write().await;
        listeners.entry(event_name).or_insert_with(Vec::new).push(listener);
    }
}

impl Default for LuaApiConfig {
    fn default() -> Self {
        Self {
            http_timeout: Duration::from_secs(30),
            max_concurrent_requests: 10,
            max_data_store_size: 1000,
            max_connection_cache_size: 500,
            enable_request_logging: true,
            allowed_domains: vec![], // 空列表表示允许所有域名
            user_agent: "Takeover-Lua-Script/1.0".to_string(),
        }
    }
}

#[async_trait]
impl ApiPort for LuaApiAdapter {
    #[instrument(skip(self, request))]
    async fn http_request(&self, request: HttpRequest) -> Result<HttpResponse, AppError> {
        debug!("Lua API HTTP request: {} {}", request.method, request.url);
        
        // 检查域名白名单
        if !self.is_domain_allowed(&request.url) {
            warn!("HTTP request to {} blocked by domain whitelist", request.url);
            
            // 更新统计
            {
                let mut stats = self.http_client.stats.write().await;
                stats.blocked_requests += 1;
            }
            
            return Err(AppError::Config(format!("Domain not allowed: {}", request.url)));
        }
        
        // 更新API统计
        {
            let mut stats = self.stats.write().await;
            stats.http_requests += 1;
        }
        
        // 发送请求
        let result = self.http_client.make_request(request).await;
        
        if self.config.enable_request_logging {
            match &result {
                Ok(response) => {
                    info!("✅ HTTP request completed with status: {}", response.status_code);
                }
                Err(e) => {
                    error!("❌ HTTP request failed: {}", e);
                }
            }
        }
        
        result
    }
    
    async fn get_connection_info(&self, conn_id: ConnectionId) -> Result<Option<ConnectionInfo>, AppError> {
        debug!("Lua API get connection info: {}", conn_id);
        
        // 更新API统计
        {
            let mut stats = self.stats.write().await;
            stats.connection_queries += 1;
        }
        
        let cache = self.connection_cache.read().await;
        Ok(cache.get(&conn_id).cloned())
    }
    
    async fn log(&self, level: LogLevel, message: &str) -> Result<(), AppError> {
        // 更新API统计
        {
            let mut stats = self.stats.write().await;
            stats.log_messages += 1;
        }
        
        // 使用tracing记录日志
        match level {
            LogLevel::Debug => debug!("[Lua Script] {}", message),
            LogLevel::Info => info!("[Lua Script] {}", message),
            LogLevel::Warn => warn!("[Lua Script] {}", message),
            LogLevel::Error => error!("[Lua Script] {}", message),
        }
        
        Ok(())
    }
    
    async fn get_config(&self, key: &str) -> Result<Option<String>, AppError> {
        debug!("Lua API get config: {}", key);
        
        // 更新API统计
        {
            let mut stats = self.stats.write().await;
            stats.config_accesses += 1;
        }
        
        let store = self.config_store.read().await;
        Ok(store.get(key).cloned())
    }
    
    async fn store_data(&self, key: &str, value: JsonValue) -> Result<(), AppError> {
        debug!("Lua API store data: {}", key);
        
        // 更新API统计
        {
            let mut stats = self.stats.write().await;
            stats.data_operations += 1;
        }
        
        let mut store = self.data_store.write().await;
        
        // 检查存储大小限制
        if store.len() >= self.config.max_data_store_size && !store.contains_key(key) {
            return Err(AppError::Config(format!(
                "Data store size limit reached: {} items",
                self.config.max_data_store_size
            )));
        }
        
        store.insert(key.to_string(), value);
        Ok(())
    }
    
    async fn get_data(&self, key: &str) -> Result<Option<JsonValue>, AppError> {
        debug!("Lua API get data: {}", key);
        
        // 更新API统计
        {
            let mut stats = self.stats.write().await;
            stats.data_operations += 1;
        }
        
        let store = self.data_store.read().await;
        Ok(store.get(key).cloned())
    }
    
    async fn emit_event(&self, event: CustomEvent) -> Result<(), AppError> {
        debug!("Lua API emit event: {}", event.name);
        
        // 更新API统计
        {
            let mut stats = self.stats.write().await;
            stats.events_emitted += 1;
        }
        
        self.event_emitter.emit_event(event).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::HttpRequest;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    
    #[tokio::test]
    async fn test_api_adapter_creation() {
        let config = LuaApiConfig::default();
        let adapter = LuaApiAdapter::new(config).unwrap();
        
        let stats = adapter.get_stats().await;
        assert_eq!(stats.http_requests, 0);
    }
    
    #[tokio::test]
    async fn test_domain_whitelist() {
        let mut config = LuaApiConfig::default();
        config.allowed_domains = vec!["example.com".to_string(), "*.google.com".to_string()];
        
        let adapter = LuaApiAdapter::new(config).unwrap();
        
        assert!(adapter.is_domain_allowed("https://example.com/api"));
        assert!(adapter.is_domain_allowed("https://api.google.com/search"));
        assert!(!adapter.is_domain_allowed("https://malicious.com/api"));
    }
    
    #[tokio::test]
    async fn test_config_operations() {
        let adapter = LuaApiAdapter::new(LuaApiConfig::default()).unwrap();
        
        // 预填充配置
        let mut config_map = HashMap::new();
        config_map.insert("test_key".to_string(), "test_value".to_string());
        adapter.populate_config(config_map).await;
        
        // 测试获取配置
        let value = adapter.get_config("test_key").await.unwrap();
        assert_eq!(value, Some("test_value".to_string()));
        
        let non_existent = adapter.get_config("non_existent").await.unwrap();
        assert_eq!(non_existent, None);
    }
    
    #[tokio::test]
    async fn test_data_store_operations() {
        let adapter = LuaApiAdapter::new(LuaApiConfig::default()).unwrap();
        
        let test_data = serde_json::json!({"message": "hello", "count": 42});
        
        // 存储数据
        adapter.store_data("test_key", test_data.clone()).await.unwrap();
        
        // 获取数据
        let retrieved = adapter.get_data("test_key").await.unwrap();
        assert_eq!(retrieved, Some(test_data));
        
        // 获取不存在的数据
        let non_existent = adapter.get_data("non_existent").await.unwrap();
        assert_eq!(non_existent, None);
    }
    
    #[tokio::test]
    async fn test_data_store_size_limit() {
        let mut config = LuaApiConfig::default();
        config.max_data_store_size = 2;
        
        let adapter = LuaApiAdapter::new(config).unwrap();
        
        // 添加到限制
        adapter.store_data("key1", serde_json::json!("value1")).await.unwrap();
        adapter.store_data("key2", serde_json::json!("value2")).await.unwrap();
        
        // 超过限制应该失败
        let result = adapter.store_data("key3", serde_json::json!("value3")).await;
        assert!(result.is_err());
    }
    
    #[tokio::test]
    async fn test_logging() {
        let adapter = LuaApiAdapter::new(LuaApiConfig::default()).unwrap();
        
        // 测试不同级别的日志
        adapter.log(LogLevel::Info, "Test info message").await.unwrap();
        adapter.log(LogLevel::Warn, "Test warning message").await.unwrap();
        adapter.log(LogLevel::Error, "Test error message").await.unwrap();
        
        let stats = adapter.get_stats().await;
        assert_eq!(stats.log_messages, 3);
    }
    
    #[tokio::test]
    async fn test_connection_cache() {
        let adapter = LuaApiAdapter::new(LuaApiConfig::default()).unwrap();
        
        let conn_id = uuid::Uuid::new_v4();
        let conn_info = ConnectionInfo {
            id: conn_id,
            source_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            target_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80),
            protocol: "HTTP".to_string(),
            tls_info: None,
            process_info: None,
        };
        
        // 缓存连接信息
        adapter.cache_connection_info(conn_id, conn_info.clone()).await;
        
        // 查询连接信息
        let retrieved = adapter.get_connection_info(conn_id).await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().id, conn_id);
    }
    
    #[tokio::test]
    async fn test_event_emission() {
        let adapter = LuaApiAdapter::new(LuaApiConfig::default()).unwrap();
        
        let event = CustomEvent {
            name: "test_event".to_string(),
            data: {
                let mut data = HashMap::new();
                data.insert("key".to_string(), serde_json::json!("value"));
                data
            },
            timestamp: SystemTime::now(),
        };
        
        adapter.emit_event(event).await.unwrap();
        
        let stats = adapter.get_stats().await;
        assert_eq!(stats.events_emitted, 1);
    }
}
