//! 响应合并中间件
//! 
//! 提供高级的响应合并和聚合功能：
//! - 多源响应智能合并
//! - 冲突检测和解决
//! - 内容类型感知合并
//! - 缓存集成优化
//! - 性能和延迟优化
//! - 错误处理和降级

use std::sync::Arc;
use std::collections::{HashMap, BTreeMap};
use std::time::{Duration, SystemTime, Instant};
use tokio::sync::RwLock;
use tracing::{info, warn, error, debug, instrument};
use async_trait::async_trait;
use serde::{Serialize, Deserialize};
use uuid::Uuid;

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{ConnectionContext, ProcessingState, HttpRequest, HttpResponse, MergeStrategy};
use crate::core::ports::{ConnectionId, AppError, ApiPort};

/// 响应合并中间件
/// 
/// 负责智能合并多个响应源的数据
pub struct ResponseMergeMiddleware {
    /// 合并配置
    config: ResponseMergeConfig,
    /// API端口（用于发起额外请求）
    api_port: Arc<dyn ApiPort>,
    /// 活跃的合并任务
    active_merges: Arc<RwLock<HashMap<ConnectionId, MergeTask>>>,
    /// 合并缓存
    merge_cache: Arc<RwLock<HashMap<String, CachedMergeResult>>>,
    /// 统计信息
    statistics: Arc<RwLock<MergeStatistics>>,
    /// 合并策略注册表
    merge_strategies: Arc<RwLock<HashMap<String, Box<dyn MergeStrategyHandler + Send + Sync>>>>,
}

/// 响应合并配置
#[derive(Debug, Clone)]
pub struct ResponseMergeConfig {
    /// 是否启用响应合并
    pub enabled: bool,
    /// 默认合并超时
    pub default_timeout: Duration,
    /// 最大并发合并数
    pub max_concurrent_merges: usize,
    /// 缓存配置
    pub cache_config: MergeCacheConfig,
    /// 默认合并策略
    pub default_strategy: MergeStrategy,
    /// 启用的合并策略
    pub enabled_strategies: Vec<String>,
    /// 冲突解决策略
    pub conflict_resolution: ConflictResolutionStrategy,
    /// 是否启用内容压缩
    pub enable_compression: bool,
    /// 性能优化设置
    pub performance_settings: PerformanceSettings,
}

/// 缓存配置
#[derive(Debug, Clone)]
pub struct MergeCacheConfig {
    /// 是否启用缓存
    pub enabled: bool,
    /// 缓存TTL
    pub ttl: Duration,
    /// 最大缓存条目数
    pub max_entries: usize,
    /// 缓存键生成策略
    pub key_strategy: CacheKeyStrategy,
}

/// 性能设置
#[derive(Debug, Clone)]
pub struct PerformanceSettings {
    /// 启用流式合并
    pub enable_streaming: bool,
    /// 批处理大小
    pub batch_size: usize,
    /// 延迟优化
    pub optimize_for_latency: bool,
    /// 内存使用限制
    pub memory_limit_mb: usize,
}

/// 缓存键策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CacheKeyStrategy {
    /// 基于URL
    UrlBased,
    /// 基于内容哈希
    ContentHash,
    /// 基于自定义字段
    CustomFields(Vec<String>),
    /// 复合策略
    Composite(Vec<CacheKeyStrategy>),
}

/// 冲突解决策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConflictResolutionStrategy {
    /// 使用第一个响应
    FirstWins,
    /// 使用最后一个响应
    LastWins,
    /// 使用最快的响应
    FastestWins,
    /// 使用最大的响应
    LargestWins,
    /// 基于优先级
    PriorityBased,
    /// 自定义合并逻辑
    CustomLogic(String),
}

/// 合并任务
#[derive(Debug, Clone)]
pub struct MergeTask {
    /// 任务ID
    pub id: Uuid,
    /// 连接ID
    pub connection_id: ConnectionId,
    /// 主要响应
    pub primary_response: Option<HttpResponse>,
    /// 额外的响应源
    pub additional_responses: Vec<ResponseSource>,
    /// 合并策略
    pub strategy: MergeStrategy,
    /// 开始时间
    pub started_at: Instant,
    /// 超时时间
    pub timeout_at: Instant,
    /// 任务状态
    pub status: MergeTaskStatus,
    /// 合并结果
    pub result: Option<HttpResponse>,
    /// 合并元数据
    pub metadata: MergeMetadata,
}

/// 响应源
#[derive(Debug, Clone)]
pub struct ResponseSource {
    /// 源ID
    pub id: String,
    /// 源URL
    pub url: String,
    /// 响应数据
    pub response: Option<HttpResponse>,
    /// 优先级
    pub priority: i32,
    /// 权重
    pub weight: f64,
    /// 是否必需
    pub required: bool,
    /// 超时时间
    pub timeout: Duration,
    /// 状态
    pub status: SourceStatus,
    /// 获取时间
    pub fetch_time: Option<Duration>,
}

/// 源状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SourceStatus {
    /// 等待中
    Pending,
    /// 获取中
    Fetching,
    /// 成功
    Success,
    /// 失败
    Failed,
    /// 超时
    Timeout,
    /// 跳过
    Skipped,
}

/// 合并任务状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MergeTaskStatus {
    /// 初始化
    Initialized,
    /// 收集响应中
    Collecting,
    /// 合并中
    Merging,
    /// 已完成
    Completed,
    /// 失败
    Failed,
    /// 超时
    Timeout,
}

/// 合并元数据
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MergeMetadata {
    /// 合并开始时间
    pub merge_started_at: Option<SystemTime>,
    /// 合并完成时间
    pub merge_completed_at: Option<SystemTime>,
    /// 总响应数
    pub total_responses: usize,
    /// 成功响应数
    pub successful_responses: usize,
    /// 失败响应数
    pub failed_responses: usize,
    /// 合并策略使用情况
    pub strategy_used: Option<String>,
    /// 冲突数量
    pub conflicts_detected: usize,
    /// 解决的冲突数
    pub conflicts_resolved: usize,
    /// 数据源信息
    pub source_info: HashMap<String, serde_json::Value>,
    /// 性能指标
    pub performance_metrics: PerformanceMetrics,
}

/// 性能指标
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PerformanceMetrics {
    /// 数据获取时间
    pub fetch_time: Option<Duration>,
    /// 合并处理时间
    pub merge_time: Option<Duration>,
    /// 总处理时间
    pub total_time: Option<Duration>,
    /// 内存使用（字节）
    pub memory_used: usize,
    /// 原始数据大小
    pub raw_data_size: usize,
    /// 合并后数据大小
    pub merged_data_size: usize,
    /// 压缩率
    pub compression_ratio: Option<f64>,
}

/// 缓存的合并结果
#[derive(Debug, Clone)]
struct CachedMergeResult {
    /// 缓存键
    key: String,
    /// 合并后的响应
    response: HttpResponse,
    /// 缓存时间
    cached_at: Instant,
    /// 过期时间
    expires_at: Instant,
    /// 命中次数
    hit_count: u64,
    /// 合并元数据
    metadata: MergeMetadata,
}

/// 合并统计
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MergeStatistics {
    /// 总合并任务数
    pub total_merges: u64,
    /// 成功合并数
    pub successful_merges: u64,
    /// 失败合并数
    pub failed_merges: u64,
    /// 超时合并数
    pub timeout_merges: u64,
    /// 缓存命中数
    pub cache_hits: u64,
    /// 缓存未命中数
    pub cache_misses: u64,
    /// 平均合并时间
    pub avg_merge_time: Option<Duration>,
    /// 平均响应数量
    pub avg_response_count: f64,
    /// 按策略统计
    pub by_strategy: HashMap<String, u64>,
    /// 冲突统计
    pub conflicts_detected: u64,
    /// 冲突解决统计
    pub conflicts_resolved: u64,
}

/// 合并策略处理器特征
#[async_trait]
pub trait MergeStrategyHandler {
    /// 策略名称
    fn name(&self) -> &str;
    
    /// 执行合并
    async fn merge(
        &self,
        primary_response: &HttpResponse,
        additional_responses: &[HttpResponse],
        metadata: &mut MergeMetadata,
    ) -> Result<HttpResponse, AppError>;
    
    /// 检测冲突
    async fn detect_conflicts(
        &self,
        responses: &[HttpResponse],
    ) -> Vec<MergeConflict>;
    
    /// 解决冲突
    async fn resolve_conflicts(
        &self,
        conflicts: &[MergeConflict],
        strategy: &ConflictResolutionStrategy,
    ) -> Result<Vec<ConflictResolution>, AppError>;
}

/// 合并冲突
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeConflict {
    /// 冲突ID
    pub id: Uuid,
    /// 冲突类型
    pub conflict_type: ConflictType,
    /// 冲突字段
    pub field_path: String,
    /// 冲突值
    pub conflicting_values: Vec<ConflictValue>,
    /// 严重程度
    pub severity: ConflictSeverity,
    /// 描述
    pub description: String,
}

/// 冲突类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConflictType {
    /// 状态码不同
    StatusCodeMismatch,
    /// 头部值不同
    HeaderValueMismatch,
    /// 内容不同
    ContentMismatch,
    /// 数据类型不兼容
    DataTypeIncompatible,
    /// 结构不匹配
    StructureMismatch,
    /// 自定义冲突
    Custom(String),
}

/// 冲突值
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictValue {
    /// 源ID
    pub source_id: String,
    /// 值
    pub value: serde_json::Value,
    /// 优先级
    pub priority: i32,
    /// 置信度
    pub confidence: f64,
}

/// 冲突严重程度
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConflictSeverity {
    /// 低
    Low = 1,
    /// 中
    Medium = 2,
    /// 高
    High = 3,
    /// 严重
    Critical = 4,
}

/// 冲突解决方案
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictResolution {
    /// 冲突ID
    pub conflict_id: Uuid,
    /// 解决策略
    pub resolution_strategy: String,
    /// 选择的值
    pub resolved_value: serde_json::Value,
    /// 解决原因
    pub reason: String,
    /// 置信度
    pub confidence: f64,
}

/// JSON合并策略处理器
struct JsonMergeStrategy;

/// 头部合并策略处理器
struct HeaderMergeStrategy;

/// 内容追加策略处理器
struct AppendMergeStrategy;

/// 自定义Lua合并策略处理器
struct LuaMergeStrategy {
    script_content: String,
}

impl ResponseMergeMiddleware {
    /// 创建新的响应合并中间件
    pub async fn new(api_port: Arc<dyn ApiPort>, config: ResponseMergeConfig) -> Result<Self, AppError> {
        info!("🔀 Initializing response merge middleware");
        
        let middleware = Self {
            config,
            api_port,
            active_merges: Arc::new(RwLock::new(HashMap::new())),
            merge_cache: Arc::new(RwLock::new(HashMap::new())),
            statistics: Arc::new(RwLock::new(MergeStatistics::default())),
            merge_strategies: Arc::new(RwLock::new(HashMap::new())),
        };
        
        // 注册默认合并策略
        middleware.register_default_strategies().await?;
        
        info!("✅ Response merge middleware initialized");
        Ok(middleware)
    }
    
    /// 注册默认合并策略
    async fn register_default_strategies(&self) -> Result<(), AppError> {
        let mut strategies = self.merge_strategies.write().await;
        
        strategies.insert("json".to_string(), Box::new(JsonMergeStrategy));
        strategies.insert("headers".to_string(), Box::new(HeaderMergeStrategy));
        strategies.insert("append".to_string(), Box::new(AppendMergeStrategy));
        
        debug!("✅ Default merge strategies registered");
        Ok(())
    }
    
    /// 执行响应合并
    #[instrument(skip(self, context))]
    pub async fn execute_merge(
        &self,
        context: &mut ConnectionContext,
        primary_response: HttpResponse,
        additional_sources: Vec<ResponseSource>,
        strategy: MergeStrategy,
    ) -> Result<HttpResponse, AppError> {
        if !self.config.enabled {
            debug!("Response merge disabled, returning primary response");
            return Ok(primary_response);
        }
        
        info!("🔀 Starting response merge for connection {}", context.id);
        
        // 检查缓存
        if let Some(cached_result) = self.check_merge_cache(context, &primary_response, &additional_sources).await {
            info!("💾 Cache hit for merge operation");
            self.update_cache_hit_stats().await;
            return Ok(cached_result);
        }
        
        self.update_cache_miss_stats().await;
        
        // 创建合并任务
        let task_id = Uuid::new_v4();
        let merge_task = MergeTask {
            id: task_id,
            connection_id: context.id.clone(),
            primary_response: Some(primary_response.clone()),
            additional_responses: additional_sources,
            strategy: strategy.clone(),
            started_at: Instant::now(),
            timeout_at: Instant::now() + self.config.default_timeout,
            status: MergeTaskStatus::Initialized,
            result: None,
            metadata: MergeMetadata::default(),
        };
        
        // 检查并发限制
        {
            let active_merges = self.active_merges.read().await;
            if active_merges.len() >= self.config.max_concurrent_merges {
                return Err(AppError::Config("Maximum concurrent merges reached".to_string()));
            }
        }
        
        // 添加到活跃任务
        {
            let mut active_merges = self.active_merges.write().await;
            active_merges.insert(context.id.clone(), merge_task.clone());
        }
        
        // 执行合并
        let result = self.perform_merge(context.id.clone(), merge_task).await;
        
        // 移除活跃任务
        {
            let mut active_merges = self.active_merges.write().await;
            active_merges.remove(&context.id);
        }
        
        match result {
            Ok(merged_response) => {
                // 缓存结果
                if self.config.cache_config.enabled {
                    self.cache_merge_result(context, &primary_response, &merged_response).await;
                }
                
                info!("✅ Response merge completed successfully for connection {}", context.id);
                Ok(merged_response)
            }
            Err(e) => {
                error!("❌ Response merge failed for connection {}: {}", context.id, e);
                
                // 更新失败统计
                {
                    let mut stats = self.statistics.write().await;
                    stats.failed_merges += 1;
                }
                
                Err(e)
            }
        }
    }
    
    /// 执行实际的合并操作
    async fn perform_merge(
        &self,
        connection_id: ConnectionId,
        mut task: MergeTask,
    ) -> Result<HttpResponse, AppError> {
        let start_time = Instant::now();
        
        // 更新任务状态
        task.status = MergeTaskStatus::Collecting;
        task.metadata.merge_started_at = Some(SystemTime::now());
        
        // 收集所有响应
        let responses = self.collect_responses(&mut task).await?;
        
        // 更新任务状态
        task.status = MergeTaskStatus::Merging;
        
        // 检测冲突
        let conflicts = self.detect_all_conflicts(&responses).await;
        task.metadata.conflicts_detected = conflicts.len();
        
        // 解决冲突
        let resolutions = self.resolve_all_conflicts(&conflicts).await?;
        task.metadata.conflicts_resolved = resolutions.len();
        
        // 执行合并
        let merged_response = self.execute_merge_strategy(&responses, &task.strategy, &mut task.metadata).await?;
        
        // 更新任务状态
        task.status = MergeTaskStatus::Completed;
        task.metadata.merge_completed_at = Some(SystemTime::now());
        task.metadata.performance_metrics.total_time = Some(start_time.elapsed());
        task.result = Some(merged_response.clone());
        
        // 更新统计
        {
            let mut stats = self.statistics.write().await;
            stats.total_merges += 1;
            stats.successful_merges += 1;
            
            let strategy_name = format!("{:?}", task.strategy);
            *stats.by_strategy.entry(strategy_name).or_insert(0) += 1;
            
            stats.avg_merge_time = match stats.avg_merge_time {
                Some(avg) => Some((avg + start_time.elapsed()) / 2),
                None => Some(start_time.elapsed()),
            };
            
            stats.avg_response_count = (stats.avg_response_count * (stats.total_merges - 1) as f64 + responses.len() as f64) / stats.total_merges as f64;
            
            stats.conflicts_detected += conflicts.len() as u64;
            stats.conflicts_resolved += resolutions.len() as u64;
        }
        
        info!("🔀 Merge completed for connection {} in {:?}", connection_id, start_time.elapsed());
        Ok(merged_response)
    }
    
    /// 收集所有响应
    async fn collect_responses(&self, task: &mut MergeTask) -> Result<Vec<HttpResponse>, AppError> {
        let mut responses = Vec::new();
        
        // 添加主要响应
        if let Some(ref primary_response) = task.primary_response {
            responses.push(primary_response.clone());
            task.metadata.total_responses += 1;
            task.metadata.successful_responses += 1;
        }
        
        // 收集额外响应
        let mut fetch_tasks = Vec::new();
        
        for (index, source) in task.additional_responses.iter().enumerate() {
            if source.required || responses.len() < 3 { // 最多收集3个额外响应
                let api_port = Arc::clone(&self.api_port);
                let source_clone = source.clone();
                
                let fetch_task = tokio::spawn(async move {
                    let start_time = Instant::now();
                    
                    let request = HttpRequest::new("GET", &source_clone.url);
                    let result = tokio::time::timeout(
                        source_clone.timeout,
                        api_port.http_request(request)
                    ).await;
                    
                    let fetch_time = start_time.elapsed();
                    
                    match result {
                        Ok(Ok(response)) => {
                            Some((index, response, SourceStatus::Success, fetch_time))
                        }
                        Ok(Err(_)) => {
                            Some((index, HttpResponse::new(500), SourceStatus::Failed, fetch_time))
                        }
                        Err(_) => {
                            Some((index, HttpResponse::new(408), SourceStatus::Timeout, fetch_time))
                        }
                    }
                });
                
                fetch_tasks.push(fetch_task);
            }
        }
        
        // 等待所有获取任务完成
        for fetch_task in fetch_tasks {
            if let Ok(Some((index, response, status, fetch_time))) = fetch_task.await {
                if status == SourceStatus::Success {
                    responses.push(response);
                    task.metadata.successful_responses += 1;
                } else {
                    task.metadata.failed_responses += 1;
                }
                
                // 更新源状态
                if let Some(source) = task.additional_responses.get_mut(index) {
                    source.status = status;
                    source.fetch_time = Some(fetch_time);
                }
                
                task.metadata.total_responses += 1;
            }
        }
        
        if responses.is_empty() {
            return Err(AppError::Config("No responses available for merging".to_string()));
        }
        
        Ok(responses)
    }
    
    /// 检测所有冲突
    async fn detect_all_conflicts(&self, responses: &[HttpResponse]) -> Vec<MergeConflict> {
        let mut all_conflicts = Vec::new();
        
        let strategies = self.merge_strategies.read().await;
        
        for strategy in strategies.values() {
            let conflicts = strategy.detect_conflicts(responses).await;
            all_conflicts.extend(conflicts);
        }
        
        all_conflicts
    }
    
    /// 解决所有冲突
    async fn resolve_all_conflicts(&self, conflicts: &[MergeConflict]) -> Result<Vec<ConflictResolution>, AppError> {
        let mut resolutions = Vec::new();
        
        let strategies = self.merge_strategies.read().await;
        
        for strategy in strategies.values() {
            let strategy_resolutions = strategy.resolve_conflicts(conflicts, &self.config.conflict_resolution).await?;
            resolutions.extend(strategy_resolutions);
        }
        
        Ok(resolutions)
    }
    
    /// 执行合并策略
    async fn execute_merge_strategy(
        &self,
        responses: &[HttpResponse],
        strategy: &MergeStrategy,
        metadata: &mut MergeMetadata,
    ) -> Result<HttpResponse, AppError> {
        let merge_start = Instant::now();
        
        if responses.is_empty() {
            return Err(AppError::Config("No responses to merge".to_string()));
        }
        
        if responses.len() == 1 {
            return Ok(responses[0].clone());
        }
        
        let primary_response = &responses[0];
        let additional_responses = &responses[1..];
        
        let result = match strategy {
            MergeStrategy::ReplaceBody => {
                self.merge_replace_body(primary_response, additional_responses).await
            }
            MergeStrategy::AppendBody => {
                self.merge_append_body(primary_response, additional_responses).await
            }
            MergeStrategy::MergeJson => {
                self.merge_json_responses(primary_response, additional_responses, metadata).await
            }
            MergeStrategy::MergeHeaders => {
                self.merge_headers_only(primary_response, additional_responses).await
            }
            MergeStrategy::CustomScript => {
                self.merge_with_custom_script(primary_response, additional_responses, metadata).await
            }
        };
        
        metadata.performance_metrics.merge_time = Some(merge_start.elapsed());
        metadata.strategy_used = Some(format!("{:?}", strategy));
        
        result
    }
    
    /// 替换体合并
    async fn merge_replace_body(&self, primary: &HttpResponse, additional: &[HttpResponse]) -> Result<HttpResponse, AppError> {
        let mut merged = primary.clone();
        
        if let Some(last_response) = additional.last() {
            merged.body = last_response.body.clone();
            merged.headers.insert("Content-Length".to_string(), merged.body.len().to_string());
        }
        
        Ok(merged)
    }
    
    /// 追加体合并
    async fn merge_append_body(&self, primary: &HttpResponse, additional: &[HttpResponse]) -> Result<HttpResponse, AppError> {
        let mut merged = primary.clone();
        
        for response in additional {
            merged.body.extend(&response.body);
        }
        
        merged.headers.insert("Content-Length".to_string(), merged.body.len().to_string());
        Ok(merged)
    }
    
    /// JSON合并
    async fn merge_json_responses(&self, primary: &HttpResponse, additional: &[HttpResponse], metadata: &mut MergeMetadata) -> Result<HttpResponse, AppError> {
        let merge_start = Instant::now();
        
        // 解析主要响应的JSON
        let mut merged_json = serde_json::from_slice::<serde_json::Value>(&primary.body)
            .map_err(|e| AppError::Config(format!("Failed to parse primary response as JSON: {}", e)))?;
        
        let mut total_size = primary.body.len();
        
        // 合并额外响应
        for response in additional {
            if let Ok(additional_json) = serde_json::from_slice::<serde_json::Value>(&response.body) {
                total_size += response.body.len();
                
                merged_json = self.deep_merge_json(merged_json, additional_json);
            } else {
                warn!("Skipping non-JSON response in JSON merge");
            }
        }
        
        // 序列化合并后的JSON
        let merged_body = serde_json::to_vec(&merged_json)
            .map_err(|e| AppError::Config(format!("Failed to serialize merged JSON: {}", e)))?;
        
        let mut merged_response = primary.clone();
        merged_response.body = merged_body;
        merged_response.headers.insert("Content-Type".to_string(), "application/json".to_string());
        merged_response.headers.insert("Content-Length".to_string(), merged_response.body.len().to_string());
        
        // 更新性能指标
        metadata.performance_metrics.raw_data_size = total_size;
        metadata.performance_metrics.merged_data_size = merged_response.body.len();
        metadata.performance_metrics.merge_time = Some(merge_start.elapsed());
        
        // 计算压缩率
        if total_size > 0 {
            metadata.performance_metrics.compression_ratio = Some(
                merged_response.body.len() as f64 / total_size as f64
            );
        }
        
        Ok(merged_response)
    }
    
    /// 深度合并JSON
    fn deep_merge_json(&self, mut base: serde_json::Value, other: serde_json::Value) -> serde_json::Value {
        match (&mut base, other) {
            (serde_json::Value::Object(base_map), serde_json::Value::Object(other_map)) => {
                for (key, value) in other_map {
                    if base_map.contains_key(&key) {
                        let base_value = base_map.get(&key).unwrap().clone();
                        base_map.insert(key, self.deep_merge_json(base_value, value));
                    } else {
                        base_map.insert(key, value);
                    }
                }
                base
            }
            (serde_json::Value::Array(base_arr), serde_json::Value::Array(other_arr)) => {
                base_arr.extend(other_arr);
                base
            }
            (_, other) => other,
        }
    }
    
    /// 仅合并头部
    async fn merge_headers_only(&self, primary: &HttpResponse, additional: &[HttpResponse]) -> Result<HttpResponse, AppError> {
        let mut merged = primary.clone();
        
        for response in additional {
            for (key, value) in &response.headers {
                // 避免覆盖重要的头部
                if !key.to_lowercase().starts_with("content-") && key.to_lowercase() != "server" {
                    merged.headers.insert(key.clone(), value.clone());
                }
            }
        }
        
        Ok(merged)
    }
    
    /// 使用自定义脚本合并
    async fn merge_with_custom_script(
        &self,
        primary: &HttpResponse,
        additional: &[HttpResponse],
        _metadata: &mut MergeMetadata,
    ) -> Result<HttpResponse, AppError> {
        // 这里可以集成Lua脚本进行自定义合并
        // 暂时使用简单的JSON合并作为降级方案
        warn!("Custom script merge not implemented, falling back to JSON merge");
        self.merge_json_responses(primary, additional, _metadata).await
    }
    
    /// 检查合并缓存
    async fn check_merge_cache(
        &self,
        context: &ConnectionContext,
        primary_response: &HttpResponse,
        additional_sources: &[ResponseSource],
    ) -> Option<HttpResponse> {
        if !self.config.cache_config.enabled {
            return None;
        }
        
        let cache_key = self.generate_cache_key(context, primary_response, additional_sources).await;
        let cache = self.merge_cache.read().await;
        
        if let Some(cached_result) = cache.get(&cache_key) {
            if Instant::now() < cached_result.expires_at {
                debug!("💾 Cache hit for merge key: {}", cache_key);
                return Some(cached_result.response.clone());
            } else {
                debug!("💾 Cache entry expired for key: {}", cache_key);
            }
        }
        
        None
    }
    
    /// 生成缓存键
    async fn generate_cache_key(
        &self,
        context: &ConnectionContext,
        primary_response: &HttpResponse,
        additional_sources: &[ResponseSource],
    ) -> String {
        match &self.config.cache_config.key_strategy {
            CacheKeyStrategy::UrlBased => {
                if let Some(ref request) = context.request {
                    format!("url:{}", request.url)
                } else {
                    format!("conn:{}", context.id)
                }
            }
            CacheKeyStrategy::ContentHash => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hasher::write(&mut hasher, &primary_response.body);
                for source in additional_sources {
                    std::hash::Hasher::write(&mut hasher, source.url.as_bytes());
                }
                format!("hash:{:x}", std::hash::Hasher::finish(&hasher))
            }
            CacheKeyStrategy::CustomFields(fields) => {
                let mut key_parts = Vec::new();
                for field in fields {
                    if let Some(ref request) = context.request {
                        if let Some(value) = request.headers.get(field) {
                            key_parts.push(format!("{}:{}", field, value));
                        }
                    }
                }
                format!("custom:{}", key_parts.join("|"))
            }
            CacheKeyStrategy::Composite(strategies) => {
                let mut composite_parts = Vec::new();
                for strategy in strategies {
                    let cache_key = match strategy {
                        CacheKeyStrategy::UrlBased => {
                            if let Some(ref request) = context.request {
                                request.url.clone()
                            } else {
                                context.id.to_string()
                            }
                        }
                        _ => "default".to_string(),
                    };
                    composite_parts.push(cache_key);
                }
                format!("composite:{}", composite_parts.join("|"))
            }
        }
    }
    
    /// 缓存合并结果
    async fn cache_merge_result(
        &self,
        context: &ConnectionContext,
        primary_response: &HttpResponse,
        merged_response: &HttpResponse,
    ) {
        let cache_key = self.generate_cache_key(context, primary_response, &[]).await;
        
        let cached_result = CachedMergeResult {
            key: cache_key.clone(),
            response: merged_response.clone(),
            cached_at: Instant::now(),
            expires_at: Instant::now() + self.config.cache_config.ttl,
            hit_count: 0,
            metadata: MergeMetadata::default(),
        };
        
        let mut cache = self.merge_cache.write().await;
        
        // 检查缓存大小限制
        if cache.len() >= self.config.cache_config.max_entries {
            // 移除最旧的条目
            if let Some(oldest_key) = cache
                .iter()
                .min_by_key(|(_, entry)| entry.cached_at)
                .map(|(key, _)| key.clone()) {
                cache.remove(&oldest_key);
            }
        }
        
        cache.insert(cache_key, cached_result);
        debug!("💾 Cached merge result");
    }
    
    /// 更新缓存命中统计
    async fn update_cache_hit_stats(&self) {
        let mut stats = self.statistics.write().await;
        stats.cache_hits += 1;
    }
    
    /// 更新缓存未命中统计
    async fn update_cache_miss_stats(&self) {
        let mut stats = self.statistics.write().await;
        stats.cache_misses += 1;
    }
    
    /// 获取统计信息
    pub async fn get_statistics(&self) -> MergeStatistics {
        self.statistics.read().await.clone()
    }
    
    /// 获取活跃合并任务
    pub async fn get_active_merges(&self) -> Vec<MergeTask> {
        let active_merges = self.active_merges.read().await;
        active_merges.values().cloned().collect()
    }
    
    /// 清理过期缓存
    pub async fn cleanup_expired_cache(&self) {
        let mut cache = self.merge_cache.write().await;
        let now = Instant::now();
        
        let expired_keys: Vec<String> = cache
            .iter()
            .filter_map(|(key, entry)| {
                if now > entry.expires_at {
                    Some(key.clone())
                } else {
                    None
                }
            })
            .collect();
        
        for key in expired_keys {
            cache.remove(&key);
        }
        
        debug!("🧹 Cleaned up expired cache entries");
    }
}

impl Default for ResponseMergeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_timeout: Duration::from_secs(30),
            max_concurrent_merges: 10,
            cache_config: MergeCacheConfig {
                enabled: true,
                ttl: Duration::from_secs(300),
                max_entries: 1000,
                key_strategy: CacheKeyStrategy::UrlBased,
            },
            default_strategy: MergeStrategy::MergeJson,
            enabled_strategies: vec![
                "json".to_string(),
                "headers".to_string(),
                "append".to_string(),
            ],
            conflict_resolution: ConflictResolutionStrategy::PriorityBased,
            enable_compression: true,
            performance_settings: PerformanceSettings {
                enable_streaming: false,
                batch_size: 100,
                optimize_for_latency: true,
                memory_limit_mb: 256,
            },
        }
    }
}

// 实现默认合并策略

#[async_trait]
impl MergeStrategyHandler for JsonMergeStrategy {
    fn name(&self) -> &str {
        "json"
    }
    
    async fn merge(
        &self,
        primary_response: &HttpResponse,
        additional_responses: &[HttpResponse],
        _metadata: &mut MergeMetadata,
    ) -> Result<HttpResponse, AppError> {
        // 简化的JSON合并实现
        let mut merged = primary_response.clone();
        
        if !additional_responses.is_empty() {
            // 使用最后一个响应的内容（简化实现）
            if let Some(last_response) = additional_responses.last() {
                merged.body = last_response.body.clone();
            }
        }
        
        Ok(merged)
    }
    
    async fn detect_conflicts(&self, responses: &[HttpResponse]) -> Vec<MergeConflict> {
        let mut conflicts = Vec::new();
        
        // 检测状态码冲突
        let mut status_codes: Vec<u16> = responses.iter().map(|r| r.status_code).collect();
        status_codes.sort();
        status_codes.dedup();
        
        if status_codes.len() > 1 {
            let conflict_values: Vec<ConflictValue> = status_codes
                .iter()
                .enumerate()
                .map(|(i, &code)| ConflictValue {
                    source_id: format!("response_{}", i),
                    value: serde_json::Value::Number(code.into()),
                    priority: 1,
                    confidence: 1.0,
                })
                .collect();
            
            conflicts.push(MergeConflict {
                id: Uuid::new_v4(),
                conflict_type: ConflictType::StatusCodeMismatch,
                field_path: "status_code".to_string(),
                conflicting_values: conflict_values,
                severity: ConflictSeverity::Medium,
                description: "Multiple different status codes found".to_string(),
            });
        }
        
        conflicts
    }
    
    async fn resolve_conflicts(
        &self,
        conflicts: &[MergeConflict],
        strategy: &ConflictResolutionStrategy,
    ) -> Result<Vec<ConflictResolution>, AppError> {
        let mut resolutions = Vec::new();
        
        for conflict in conflicts {
            let resolution = match strategy {
                ConflictResolutionStrategy::FirstWins => {
                    if let Some(first_value) = conflict.conflicting_values.first() {
                        ConflictResolution {
                            conflict_id: conflict.id,
                            resolution_strategy: "first_wins".to_string(),
                            resolved_value: first_value.value.clone(),
                            reason: "Using first value".to_string(),
                            confidence: 0.8,
                        }
                    } else {
                        continue;
                    }
                }
                ConflictResolutionStrategy::PriorityBased => {
                    if let Some(highest_priority) = conflict.conflicting_values
                        .iter()
                        .max_by_key(|v| v.priority) {
                        ConflictResolution {
                            conflict_id: conflict.id,
                            resolution_strategy: "priority_based".to_string(),
                            resolved_value: highest_priority.value.clone(),
                            reason: "Using highest priority value".to_string(),
                            confidence: 0.9,
                        }
                    } else {
                        continue;
                    }
                }
                _ => {
                    // 默认使用第一个值
                    if let Some(first_value) = conflict.conflicting_values.first() {
                        ConflictResolution {
                            conflict_id: conflict.id,
                            resolution_strategy: "default".to_string(),
                            resolved_value: first_value.value.clone(),
                            reason: "Default resolution".to_string(),
                            confidence: 0.5,
                        }
                    } else {
                        continue;
                    }
                }
            };
            
            resolutions.push(resolution);
        }
        
        Ok(resolutions)
    }
}

#[async_trait]
impl MergeStrategyHandler for HeaderMergeStrategy {
    fn name(&self) -> &str {
        "headers"
    }
    
    async fn merge(
        &self,
        primary_response: &HttpResponse,
        additional_responses: &[HttpResponse],
        _metadata: &mut MergeMetadata,
    ) -> Result<HttpResponse, AppError> {
        let mut merged = primary_response.clone();
        
        for response in additional_responses {
            for (key, value) in &response.headers {
                merged.headers.insert(key.clone(), value.clone());
            }
        }
        
        Ok(merged)
    }
    
    async fn detect_conflicts(&self, _responses: &[HttpResponse]) -> Vec<MergeConflict> {
        // 简化实现，不检测头部冲突
        Vec::new()
    }
    
    async fn resolve_conflicts(
        &self,
        _conflicts: &[MergeConflict],
        _strategy: &ConflictResolutionStrategy,
    ) -> Result<Vec<ConflictResolution>, AppError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl MergeStrategyHandler for AppendMergeStrategy {
    fn name(&self) -> &str {
        "append"
    }
    
    async fn merge(
        &self,
        primary_response: &HttpResponse,
        additional_responses: &[HttpResponse],
        _metadata: &mut MergeMetadata,
    ) -> Result<HttpResponse, AppError> {
        let mut merged = primary_response.clone();
        
        for response in additional_responses {
            merged.body.extend(&response.body);
        }
        
        merged.headers.insert("Content-Length".to_string(), merged.body.len().to_string());
        Ok(merged)
    }
    
    async fn detect_conflicts(&self, _responses: &[HttpResponse]) -> Vec<MergeConflict> {
        Vec::new()
    }
    
    async fn resolve_conflicts(
        &self,
        _conflicts: &[MergeConflict],
        _strategy: &ConflictResolutionStrategy,
    ) -> Result<Vec<ConflictResolution>, AppError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl Middleware for ResponseMergeMiddleware {
    fn name(&self) -> &str {
        "response_merge"
    }
    
    #[instrument(skip(self, context, _core))]
    async fn process(
        &self,
        context: &mut ConnectionContext,
        _core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, crate::error::TakeoverError> {
        debug!("Response merge middleware processing connection {}", context.id);
        
        if !self.config.enabled {
            return Ok(MiddlewareResult::Continue);
        }
        
        // 检查是否有需要合并的响应
        if context.response.is_some() {
            // 这里可以根据上下文决定是否需要合并额外的响应源
            // 简化实现，直接继续处理
            context.set_processing_state(ProcessingState::ResponseMerging);
            
            debug!("Response available for potential merging");
        }
        
        Ok(MiddlewareResult::Continue)
    }
    
    fn priority(&self) -> i32 {
        50 // 在用户干预之后执行
    }
    
    fn enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Connection, HttpRequest};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    
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
    async fn test_response_merge_middleware_creation() {
        let api_port = Arc::new(MockApiPort);
        let config = ResponseMergeConfig::default();
        
        let middleware = ResponseMergeMiddleware::new(api_port, config).await.unwrap();
        assert_eq!(middleware.name(), "response_merge");
        assert!(middleware.enabled());
    }
    
    #[tokio::test]
    async fn test_json_merge() {
        let api_port = Arc::new(MockApiPort);
        let middleware = ResponseMergeMiddleware::new(api_port, ResponseMergeConfig::default()).await.unwrap();
        
        let primary_response = HttpResponse {
            status_code: 200,
            status_text: Some("OK".to_string()),
            headers: HashMap::new(),
            body: br#"{"name": "John", "age": 30}"#.to_vec(),
            id: None,
            sent_at: None,
        };
        
        let additional_response = HttpResponse {
            status_code: 200,
            status_text: Some("OK".to_string()),
            headers: HashMap::new(),
            body: br#"{"city": "New York", "age": 31}"#.to_vec(),
            id: None,
            sent_at: None,
        };
        
        let mut metadata = MergeMetadata::default();
        let result = middleware.merge_json_responses(
            &primary_response,
            &[additional_response],
            &mut metadata
        ).await.unwrap();
        
        let merged_json: serde_json::Value = serde_json::from_slice(&result.body).unwrap();
        assert_eq!(merged_json["name"], "John");
        assert_eq!(merged_json["city"], "New York");
        assert_eq!(merged_json["age"], 31); // 应该被覆盖
    }
    
    #[tokio::test]
    async fn test_append_merge() {
        let api_port = Arc::new(MockApiPort);
        let middleware = ResponseMergeMiddleware::new(api_port, ResponseMergeConfig::default()).await.unwrap();
        
        let primary_response = HttpResponse {
            status_code: 200,
            status_text: Some("OK".to_string()),
            headers: HashMap::new(),
            body: b"Hello ".to_vec(),
            id: None,
            sent_at: None,
        };
        
        let additional_response = HttpResponse {
            status_code: 200,
            status_text: Some("OK".to_string()),
            headers: HashMap::new(),
            body: b"World!".to_vec(),
            id: None,
            sent_at: None,
        };
        
        let result = middleware.merge_append_body(&primary_response, &[additional_response]).await.unwrap();
        
        assert_eq!(String::from_utf8_lossy(&result.body), "Hello World!");
        assert_eq!(result.headers.get("Content-Length").unwrap(), "12");
    }
    
    #[tokio::test]
    async fn test_cache_key_generation() {
        let api_port = Arc::new(MockApiPort);
        let middleware = ResponseMergeMiddleware::new(api_port, ResponseMergeConfig::default()).await.unwrap();
        
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        
        let mut context = ConnectionContext::new(connection, "example.com".to_string(), 80);
        context.request = Some(HttpRequest::new("GET", "http://example.com/api"));
        
        let response = HttpResponse::new(200);
        let sources = vec![];
        
        let cache_key = middleware.generate_cache_key(&context, &response, &sources).await;
        assert!(cache_key.starts_with("url:"));
        assert!(cache_key.contains("http://example.com/api"));
    }
    
    #[tokio::test]
    async fn test_conflict_detection() {
        let strategy = JsonMergeStrategy;
        
        let response1 = HttpResponse {
            status_code: 200,
            ..HttpResponse::new(200)
        };
        
        let response2 = HttpResponse {
            status_code: 404,
            ..HttpResponse::new(404)
        };
        
        let conflicts = strategy.detect_conflicts(&[response1, response2]).await;
        
        assert_eq!(conflicts.len(), 1);
        assert!(matches!(conflicts[0].conflict_type, ConflictType::StatusCodeMismatch));
        assert_eq!(conflicts[0].conflicting_values.len(), 2);
    }
    
    #[tokio::test]
    async fn test_deep_json_merge() {
        let api_port = Arc::new(MockApiPort);
        let middleware = ResponseMergeMiddleware::new(api_port, ResponseMergeConfig::default()).await.unwrap();
        
        let base = serde_json::json!({
            "user": {
                "name": "John",
                "age": 30
            },
            "items": [1, 2, 3]
        });
        
        let other = serde_json::json!({
            "user": {
                "city": "New York",
                "age": 31
            },
            "items": [4, 5],
            "extra": "data"
        });
        
        let merged = middleware.deep_merge_json(base, other);
        
        assert_eq!(merged["user"]["name"], "John");
        assert_eq!(merged["user"]["city"], "New York");
        assert_eq!(merged["user"]["age"], 31);
        assert_eq!(merged["extra"], "data");
        assert_eq!(merged["items"].as_array().unwrap().len(), 5);
    }
}
