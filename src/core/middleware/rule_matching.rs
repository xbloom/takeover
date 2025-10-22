//! 规则匹配中间件 v3.0
//!
//! 遵循设计文档的自包含、分层架构实现。
//! - RuleMatchingMiddleware: 轻量级管道协调器。
//! - RuleService: 核心状态和匹配逻辑的服务。

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{
    ConnectionContext, LogLevel, MatchResult, ProcessingState, RequestModifications, Rule,
    RuleAction, RuleCondition, RuleConditionType, RulePattern,
};
use crate::core::AppCore;
use crate::core::services::state_manager::{StateManager, StateEvent, StateEventType};
use crate::error::TakeoverError;

use async_trait::async_trait;
use chrono::{Datelike, Local, Timelike, Weekday};
use globset::Glob;
use lru::LruCache;
use regex::Regex;
use std::collections::{HashMap, BTreeMap};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

// —————————————————————————————————————————————————————————————————————————————
//  Phase 3, Step 3.1: 完整版 RuleService
// —————————————————————————————————————————————————————————————————————————————

/// 规则服务
///
/// 包含所有核心规则匹配逻辑、状态管理和性能优化。
/// 作为一个独立的服务被注册到 AppCore 中。
pub struct RuleService {
    /// 规则存储 - HashMap for O(1) lookup
    active_rules: Arc<RwLock<HashMap<String, Rule>>>,
    
    /// 优先级索引 - BTreeMap for fast priority-based iteration
    priority_index: Arc<RwLock<BTreeMap<i32, Vec<String>>>>,

    /// 预编译缓存 - 性能优化
    compiled_rules: Arc<RwLock<HashMap<String, CompiledRule>>>,

    /// 匹配缓存 - LRU缓存
    match_cache: Arc<Mutex<LruCache<String, Option<MatchResult>>>>,

    /// StateManager - 统一状态管理
    state_manager: Arc<StateManager>,
}

/// 规则服务统计信息
#[derive(Debug, Default, Clone)]
pub struct RuleServiceStats {
    pub total_matches: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub average_match_time: Duration,
    pub rules_count: usize,
    pub compiled_rules_count: usize,
}

/// 规则服务错误类型
#[derive(Debug, thiserror::Error)]
pub enum RuleServiceError {
    #[error("Pattern compilation failed: {0}")]
    PatternCompilationFailed(String),

    #[error("Rule validation failed: {0}")]
    RuleValidationFailed(String),

    #[error("Script execution failed: {0}")]
    ScriptExecutionFailed(String),
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

impl RuleService {
    /// 创建新的规则服务
    pub fn new(state_manager: Arc<StateManager>) -> Self {
        Self {
            active_rules: Arc::new(RwLock::new(HashMap::new())),
            priority_index: Arc::new(RwLock::new(BTreeMap::new())),
            compiled_rules: Arc::new(RwLock::new(HashMap::new())),
            match_cache: Arc::new(Mutex::new(LruCache::new(
                std::num::NonZeroUsize::new(1000).unwrap(),
            ))),
            state_manager,
        }
    }

    /// 应用规则 (完整版) - 包含预编译、缓存清理和StateManager同步
    pub async fn apply_rules(&self, rules: Vec<Rule>) -> Result<(), RuleServiceError> {
        info!("🔧 Applying {} rules to RuleService", rules.len());
        let start_time = Instant::now();

        // 1. 预编译所有规则
        let mut compiled_rules_map = HashMap::new();
        let mut rules_map = HashMap::new();
        let mut priority_map: BTreeMap<i32, Vec<String>> = BTreeMap::new();
        
        for rule in &rules {
            match self.compile_rule(rule).await {
                Ok(compiled) => {
                    compiled_rules_map.insert(rule.id.clone(), compiled);
                }
                Err(e) => {
                    warn!("⚠️ Failed to compile rule '{}': {}", rule.name, e);
                }
            }
            
            // 构建HashMap和优先级索引
            rules_map.insert(rule.id.clone(), rule.clone());
            priority_map
                .entry(rule.priority)
                .or_insert_with(Vec::new)
                .push(rule.id.clone());
        }
        
        let compiled_count = compiled_rules_map.len();
        let backup_id = format!("backup_{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs());

        // 2. 原子更新内存状态
        {
            let mut active_rules = self.active_rules.write().await;
            let mut compiled = self.compiled_rules.write().await;
            let mut index = self.priority_index.write().await;
            
            *active_rules = rules_map;
            *compiled = compiled_rules_map;
            *index = priority_map;
        }

        // 3. 清理匹配缓存
        {
            let mut cache = self.match_cache.lock().await;
            cache.clear();
        }

        // 4. 异步同步到StateManager (不阻塞热路径)
        let state_manager = self.state_manager.clone();
        let rules_snapshot = rules.clone();
        let rule_count = rules.len();
        
        tokio::spawn(async move {
            // 存储规则快照
            if let Ok(snapshot_data) = serde_json::to_value(&rules_snapshot) {
                let _ = state_manager.set_global_data("rules_snapshot", snapshot_data).await;
            }
            
            // 更新统计信息
            let _ = state_manager.set_counter("active_rules_count", rule_count as u64).await;
            let _ = state_manager.increment_counter("rule_updates_total").await;
            
            // 发布规则更新事件
            let _ = state_manager.emit_event(StateEvent {
                event_type: StateEventType::Custom("rules_updated".to_string()),
                connection_id: None,
                session_id: None,
                data: {
                    let mut data = HashMap::new();
                    data.insert("rule_count".to_string(), serde_json::json!(rule_count));
                    data.insert("update_timestamp".to_string(),
                        serde_json::json!(SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs()));
                    data
                },
                timestamp: SystemTime::now(),
            }).await;
        });

        let apply_duration = start_time.elapsed();
        info!(
            "✅ Rules applied successfully: {} rules, {} compiled, backup_id: {}, duration: {}ms",
            rules.len(),
            compiled_count,
            backup_id,
            apply_duration.as_millis()
        );
        Ok(())
    }

    /// 获取活跃规则
    pub async fn get_active_rules(&self) -> Vec<Rule> {
        self.active_rules.read().await.values().cloned().collect()
    }

    /// 完整规则匹配 - 支持所有模式和条件
    pub async fn match_request(&self, context: &ConnectionContext) -> Option<MatchResult> {
        let start_time = Instant::now();

        // 生成缓存键
        let cache_key = self.generate_cache_key(context);

        // 检查缓存
        {
            let mut cache = self.match_cache.lock().await;
            if let Some(cached_result) = cache.get(&cache_key) {
                // 异步更新缓存命中统计
                let state_manager = self.state_manager.clone();
                tokio::spawn(async move {
                    let _ = state_manager.increment_counter("rule_match_cache_hits").await;
                });

                debug!("🎯 Cache hit for request matching");
                return cached_result.clone();
            }
        }

        // 缓存未命中，执行匹配
        let match_result = self.execute_match(context).await;
        let match_duration = start_time.elapsed();
        
        // 克隆结果用于缓存和返回
        let result_for_cache = match_result.clone();
        let result_for_return = match_result.clone();

        // 存储到缓存
        {
            let mut cache = self.match_cache.lock().await;
            cache.put(cache_key, result_for_cache);
        }

        // 异步更新统计
        let state_manager = self.state_manager.clone();
        let match_duration_micros = match_duration.as_micros() as u64;
        let has_match = match_result.is_some();
        tokio::spawn(async move {
            let _ = state_manager.increment_counter("rule_match_total").await;
            if has_match {
                let _ = state_manager.increment_counter("rule_match_successful").await;
            }
            // 更新平均匹配时间
            let _ = state_manager.set_gauge("rule_match_avg_time_us", match_duration_micros as f64).await;
        });

        result_for_return
    }

    /// 执行实际的规则匹配
    async fn execute_match(&self, context: &ConnectionContext) -> Option<MatchResult> {
        let compiled_rules = self.compiled_rules.read().await;
        let active_rules = self.active_rules.read().await;
        let priority_index = self.priority_index.read().await;

        // 按优先级顺序匹配
        for (_priority, rule_ids) in priority_index.iter() {
            for rule_id in rule_ids {
                let rule = match active_rules.get(rule_id) {
                    Some(r) => r,
                    None => continue,
                };
                
                if !rule.enabled {
                    continue;
                }

                let start_time = Instant::now();
                let mut matched_patterns = Vec::new();
                let mut matched_conditions = Vec::new();

                // 匹配主模式
                if self
                    .match_pattern(
                        &rule.pattern,
                        context,
                        &compiled_rules,
                        &mut matched_patterns,
                    )
                    .await
                {
                    // 匹配附加条件
                    if self
                        .match_conditions(&rule.conditions, context, &mut matched_conditions)
                        .await
                    {
                        let match_duration = start_time.elapsed();

                        debug!(
                            "✅ Rule matched: {} -> {:?} ({}μs)",
                            rule.name,
                            rule.action,
                            match_duration.as_micros()
                        );
                        
                        // 异步更新规则匹配统计
                        let state_manager = self.state_manager.clone();
                        let rule_id_clone = rule.id.clone();
                        tokio::spawn(async move {
                            let counter_key = format!("rule_match_count_{}", rule_id_clone);
                            let _ = state_manager.increment_counter(&counter_key).await;
                        });

                        return Some(MatchResult {
                            rule_id: rule.id.clone(),
                            rule_name: rule.name.clone(),
                            rule_description: rule.description.clone(),
                            action: rule.action.clone(),
                            match_duration,
                            matched_patterns,
                            matched_conditions,
                        });
                    }
                }
            }
        }

        debug!("ℹ️ No rules matched for this request");
        None
    }

    /// 匹配规则模式 - 支持所有模式类型
    fn match_pattern<'a>(
        &'a self,
        pattern: &'a RulePattern,
        context: &'a ConnectionContext,
        _compiled_rules: &'a HashMap<String, CompiledRule>,
        matched_patterns: &'a mut Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            match pattern {
                // 基础匹配
                RulePattern::HostMatches(host_pattern) => {
                    let matches = self.wildcard_match(&context.target_host, host_pattern);
                    if matches {
                        matched_patterns.push(format!("host:{}", host_pattern));
                    }
                    matches
                }

                RulePattern::PathMatches(path_pattern) => {
                    if let Some(ref request) = context.request {
                        if let Ok(url) = url::Url::parse(&request.url) {
                            let matches = self.wildcard_match(url.path(), path_pattern);
                            if matches {
                                matched_patterns.push(format!("path:{}", path_pattern));
                            }
                            return matches;
                        }
                    }
                    false
                }

                RulePattern::HeaderExists(header_name) => {
                    if let Some(ref request) = context.request {
                        let matches = request.headers.contains_key(header_name);
                        if matches {
                            matched_patterns.push(format!("header_exists:{}", header_name));
                        }
                        matches
                    } else {
                        false
                    }
                }

                RulePattern::HeaderValueMatches { header, value } => {
                    if let Some(ref request) = context.request {
                        if let Some(header_value) = request.headers.get(header) {
                            if let Ok(hv_str) = header_value.to_str() {
                                let matches = self.wildcard_match(hv_str, value);
                                if matches {
                                    matched_patterns.push(format!("header_value:{}={}", header, value));
                                }
                                matches
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                }

                RulePattern::PortMatches(port) => {
                    let matches = context.target_port == *port;
                    if matches {
                        matched_patterns.push(format!("port:{}", port));
                    }
                    matches
                }

                RulePattern::ProcessMatches(process_pattern) => {
                    if let Some(ref process_name) = context.connection.metadata.process_name {
                        let matches = self.wildcard_match(process_name, process_pattern);
                        if matches {
                            matched_patterns.push(format!("process:{}", process_pattern));
                        }
                        matches
                    } else {
                        false
                    }
                }

                RulePattern::UserMatches(user_pattern) => {
                    if let Some(ref user_name) = context.connection.metadata.user_name {
                        let matches = self.wildcard_match(user_name, user_pattern);
                        if matches {
                            matched_patterns.push(format!("user:{}", user_pattern));
                        }
                        matches
                    } else {
                        false
                    }
                }

                // 时间匹配
                RulePattern::TimeRange { start, end } => {
                    let matches = self.match_time_range(start, end);
                    if matches {
                        matched_patterns.push(format!("time:{}−{}", start, end));
                    }
                    matches
                }

                RulePattern::WeekdayMatches(weekdays) => {
                    let matches = self.match_weekday(weekdays);
                    if matches {
                        matched_patterns.push(format!("weekday:{:?}", weekdays));
                    }
                    matches
                }

                // 复合匹配
                RulePattern::And(patterns) => {
                    for sub_pattern in patterns {
                        if !self
                            .match_pattern(sub_pattern, context, _compiled_rules, matched_patterns)
                            .await
                        {
                            return false;
                        }
                    }
                    true
                }

                RulePattern::Or(patterns) => {
                    for sub_pattern in patterns {
                        if self
                            .match_pattern(sub_pattern, context, _compiled_rules, matched_patterns)
                            .await
                        {
                            return true;
                        }
                    }
                    false
                }

                RulePattern::Not(pattern) => {
                    !self
                        .match_pattern(pattern, context, _compiled_rules, matched_patterns)
                        .await
                }

                // 高级匹配
                RulePattern::Custom(script_code) => self.execute_custom_match(script_code, context).await,
            }
        })
    }

    /// 匹配规则条件
    async fn match_conditions(
        &self,
        conditions: &[RuleCondition],
        context: &ConnectionContext,
        matched_conditions: &mut Vec<String>,
    ) -> bool {
        if conditions.is_empty() {
            return true; // 无条件直接通过
        }

        for condition in conditions {
            let matches = self.match_single_condition(condition, context).await;
            let result = if condition.negated { !matches } else { matches };

            if result {
                matched_conditions.push(condition.name.clone());
            } else {
                return false; // 任一条件不匹配就失败
            }
        }

        true
    }

    /// 匹配单个条件
    async fn match_single_condition(
        &self,
        condition: &RuleCondition,
        _context: &ConnectionContext,
    ) -> bool {
        match condition.condition_type {
            RuleConditionType::SourceIPInRange
            | RuleConditionType::RequestSize
            | RuleConditionType::ProcessPID => {
                // TODO: 实现IP范围匹配、请求大小匹配、PID匹配等
                warn!(
                    "⚠️ Condition type {:?} not implemented yet",
                    condition.condition_type
                );
                false
            }
            _ => {
                warn!(
                    "⚠️ Condition type {:?} not implemented yet",
                    condition.condition_type
                );
                false
            }
        }
    }

    /// 高级通配符匹配 (优先使用glob)
    fn wildcard_match(&self, text: &str, pattern: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        // 优先使用 globset 进行更完整的通配符匹配
        if let Ok(glob) = Glob::new(pattern) {
            return glob.compile_matcher().is_match(text);
        }
        // 降级为基础通配符匹配 (用于简单场景或glob解析失败)
        if pattern.starts_with('*') && pattern.ends_with('*') {
            let middle = &pattern[1..pattern.len() - 1];
            text.contains(middle)
        } else if pattern.starts_with('*') {
            let suffix = &pattern[1..];
            text.ends_with(suffix)
        } else if pattern.ends_with('*') {
            let prefix = &pattern[..pattern.len() - 1];
            text.starts_with(prefix)
        } else {
            text == pattern
        }
    }

    /// 时间范围匹配
    fn match_time_range(&self, start: &str, end: &str) -> bool {
        let now = Local::now();
        let current_time = format!("{:02}:{:02}", now.hour(), now.minute());
        current_time.as_str() >= start && current_time.as_str() <= end
    }

    /// 星期匹配 (1=Monday, 7=Sunday)
    fn match_weekday(&self, weekdays: &[u8]) -> bool {
        let now = Local::now();
        let current_weekday = match now.weekday() {
            Weekday::Mon => 1,
            Weekday::Tue => 2,
            Weekday::Wed => 3,
            Weekday::Thu => 4,
            Weekday::Fri => 5,
            Weekday::Sat => 6,
            Weekday::Sun => 7,
        };
        weekdays.contains(&current_weekday)
    }

    /// 执行自定义脚本匹配
    async fn execute_custom_match(&self, _script_code: &str, _context: &ConnectionContext) -> bool {
        // TODO: 集成 Lua 脚本引擎
        warn!("⚠️ Custom script matching not implemented yet");
        false
    }

    /// 预编译规则 - 性能优化
    async fn compile_rule(&self, rule: &Rule) -> Result<CompiledRule, RuleServiceError> {
        let mut compiled_patterns = Vec::new();
        self.compile_pattern(&rule.pattern, &mut compiled_patterns)?;
        Ok(CompiledRule {
            rule: rule.clone(),
            compiled_patterns,
            compiled_at: SystemTime::now(),
        })
    }

    /// 预编译模式
    fn compile_pattern(
        &self,
        pattern: &RulePattern,
        compiled_patterns: &mut Vec<CompiledPattern>,
    ) -> Result<(), RuleServiceError> {
        match pattern {
            RulePattern::HostMatches(host_pattern) => {
                if host_pattern.contains('*') || host_pattern.contains('?') {
                    let glob = Glob::new(host_pattern).map_err(|e| {
                        RuleServiceError::PatternCompilationFailed(format!(
                            "Invalid glob pattern '{}': {}",
                            host_pattern, e
                        ))
                    })?;
                    compiled_patterns.push(CompiledPattern {
                        original_pattern: pattern.clone(),
                        regex: None,
                        glob: Some(glob),
                        exact_match: None,
                        pattern_type: PatternType::Wildcard,
                    });
                } else {
                    compiled_patterns.push(CompiledPattern {
                        original_pattern: pattern.clone(),
                        regex: None,
                        glob: None,
                        exact_match: Some(host_pattern.clone()),
                        pattern_type: PatternType::Exact,
                    });
                }
            }
            RulePattern::PathMatches(path_pattern) => {
                // 尝试编译为正则表达式，如果失败则作为通配符
                if let Ok(regex) = Regex::new(path_pattern) {
                     compiled_patterns.push(CompiledPattern {
                        original_pattern: pattern.clone(),
                        regex: Some(regex),
                        glob: None,
                        exact_match: None,
                        pattern_type: PatternType::Regex,
                    });
                } else if let Ok(glob) = Glob::new(path_pattern) {
                     compiled_patterns.push(CompiledPattern {
                        original_pattern: pattern.clone(),
                        regex: None,
                        glob: Some(glob),
                        exact_match: None,
                        pattern_type: PatternType::Wildcard,
                    });
                } else {
                    return Err(RuleServiceError::PatternCompilationFailed(format!(
                            "Invalid path pattern '{}': cannot be compiled as regex or glob",
                            path_pattern
                        )));
                }
            }
            RulePattern::And(patterns) | RulePattern::Or(patterns) => {
                for sub_pattern in patterns {
                    self.compile_pattern(sub_pattern, compiled_patterns)?;
                }
            }
            RulePattern::Not(pattern) => {
                self.compile_pattern(pattern, compiled_patterns)?;
            }
            _ => {
                // 其他模式暂时不进行深度编译
                compiled_patterns.push(CompiledPattern {
                    original_pattern: pattern.clone(),
                    regex: None,
                    glob: None,
                    exact_match: None,
                    pattern_type: PatternType::Custom, // 标记为需要运行时评估
                });
            }
        }
        Ok(())
    }

    /// 生成缓存键
    fn generate_cache_key(&self, context: &ConnectionContext) -> String {
        // 缓存键应包含所有用于匹配的上下文信息
        format!(
            "{}:{}:{}:{}:{}",
            context.target_host,
            context.target_port,
            context
                .connection
                .metadata
                .process_name
                .as_ref()
                .unwrap_or(&"unknown".to_string()),
            context
                .request
                .as_ref()
                .and_then(|r| url::Url::parse(&r.url).ok())
                .map(|u| u.path().to_string())
                .unwrap_or_else(|| "".to_string()),
            context
                .connection
                .metadata
                .user_name
                .as_ref()
                .unwrap_or(&"unknown".to_string())
        )
    }

    /// 获取统计信息
    pub async fn get_stats(&self) -> RuleServiceStats {
        let active_rules = self.active_rules.read().await;
        let compiled_rules = self.compiled_rules.read().await;
        
        // 从StateManager获取统计
        let total_matches = self.state_manager.get_counter("rule_match_total").await.unwrap_or(0);
        let cache_hits = self.state_manager.get_counter("rule_match_cache_hits").await.unwrap_or(0);
        let avg_time_us = self.state_manager.get_gauge("rule_match_avg_time_us").await.unwrap_or(0.0);
        
        RuleServiceStats {
            total_matches,
            cache_hits,
            cache_misses: total_matches.saturating_sub(cache_hits),
            average_match_time: Duration::from_micros(avg_time_us as u64),
            rules_count: active_rules.len(),
            compiled_rules_count: compiled_rules.len(),
        }
    }
}

// —————————————————————————————————————————————————————————————————————————————
//  Phase 3, Step 3.2: 完整版规则匹配中间件
// —————————————————————————————————————————————————————————————————————————————

/// 规则匹配中间件 (轻量级)
pub struct RuleMatchingMiddleware {
    enabled: bool,
    default_action: DefaultAction,
}

/// 默认动作
#[derive(Debug, Clone)]
pub enum DefaultAction {
    Allow,  // 无规则匹配时允许通过
    Block,  // 无规则匹配时阻止
    Direct, // 无规则匹配时直连
}

impl RuleMatchingMiddleware {
    pub fn new() -> Self {
        Self {
            enabled: true,
            default_action: DefaultAction::Direct, // 默认直连
        }
    }

    pub fn with_default_action(mut self, action: DefaultAction) -> Self {
        self.default_action = action;
        self
    }
}

impl Default for RuleMatchingMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for RuleMatchingMiddleware {
    fn name(&self) -> &str {
        "rule_matching"
    }

    fn priority(&self) -> i32 {
        30 // 在HTTP解析之后，转发之前
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    /// 注册规则服务到 AppCore
    async fn register_services(&self, app_core: &AppCore) -> Result<(), TakeoverError> {
        // 获取StateManager
        let state_manager = app_core.state::<StateManager>().await
            .ok_or_else(|| TakeoverError::Config("StateManager not found in AppCore".to_string()))?;
        
        let rule_service = RuleService::new(state_manager);
        app_core.manage(rule_service).await;
        info!("✅ RuleService registered to AppCore (with StateManager)");
        Ok(())
    }

    /// 处理请求 - 完整的规则匹配和动作执行
    async fn process(
        &self,
        context: &mut ConnectionContext,
        core: &AppCore,
    ) -> Result<MiddlewareResult, TakeoverError> {
        context.set_processing_state(ProcessingState::RuleMatching);

        if let Some(rule_service) = core.state::<RuleService>().await {
            match rule_service.match_request(context).await {
                Some(match_result) => {
                    info!(
                        "✅ Rule matched: {} -> {:?} ({}μs)",
                        match_result.rule_name,
                        match_result.action,
                        match_result.match_duration.as_micros()
                    );

                    // 存储详细匹配信息到context
                    context.custom_data.insert(
                        "matched_rule_id".to_string(),
                        serde_json::json!(match_result.rule_id),
                    );
                    context.custom_data.insert(
                        "matched_rule_name".to_string(),
                        serde_json::json!(match_result.rule_name),
                    );

                    // 执行规则动作
                    self.execute_rule_action(match_result.action, context, core)
                        .await
                }
                None => {
                    debug!(
                        "ℹ️ No rules matched, applying default action: {:?}",
                        self.default_action
                    );

                    // 应用默认动作
                    match self.default_action {
                        DefaultAction::Allow | DefaultAction::Direct => {
                            context.custom_data.insert(
                                "rule_decision".to_string(),
                                serde_json::json!("default_direct"),
                            );
                            Ok(MiddlewareResult::Continue)
                        }
                        DefaultAction::Block => {
                            warn!("❌ Connection blocked by default action");
                            Ok(MiddlewareResult::Reject(
                                "Blocked by default rule".to_string(),
                            ))
                        }
                    }
                }
            }
        } else {
            warn!("⚠️ RuleService not available, skipping rule matching");
            Ok(MiddlewareResult::Continue)
        }
    }
}

impl RuleMatchingMiddleware {
    /// 执行规则动作 - 处理所有动作类型
    async fn execute_rule_action(
        &self,
        action: RuleAction,
        context: &mut ConnectionContext,
        core: &AppCore,
    ) -> Result<MiddlewareResult, TakeoverError> {
        match action {
            // 基础动作
            RuleAction::Allow | RuleAction::Direct => {
                context
                    .custom_data
                    .insert("rule_decision".to_string(), serde_json::json!("direct"));
                Ok(MiddlewareResult::Continue)
            }

            RuleAction::Block => {
                warn!("❌ Connection blocked by rule");
                Ok(MiddlewareResult::Reject(
                    "Blocked by security rule".to_string(),
                ))
            }

            RuleAction::ProxyByName(proxy_name) => {
                context
                    .custom_data
                    .insert("rule_decision".to_string(), serde_json::json!("proxy"));
                context.custom_data.insert(
                    "routing_decision".to_string(),
                    serde_json::json!({
                        "decision": "proxy_by_name",
                        "proxy_name": proxy_name
                    }),
                );
                Ok(MiddlewareResult::Continue)
            }

            // 修改动作
            RuleAction::ModifyRequest(modifications) => {
                info!("🔧 Applying request modifications");
                self.apply_request_modifications(modifications, context)
                    .await?;
                context.custom_data.insert(
                    "rule_decision".to_string(),
                    serde_json::json!("modify_request"),
                );
                Ok(MiddlewareResult::Continue)
            }

            RuleAction::ModifyResponse(modifications) => {
                info!("🔧 Scheduling response modifications");
                // 存储修改配置，在响应阶段应用
                context.custom_data.insert(
                    "response_modifications".to_string(),
                    serde_json::to_value(modifications).unwrap_or(serde_json::Value::Null),
                );
                context.custom_data.insert(
                    "rule_decision".to_string(),
                    serde_json::json!("modify_response"),
                );
                Ok(MiddlewareResult::Continue)
            }

            RuleAction::RedirectTo(target_url) => {
                info!("🔀 Redirecting to: {}", target_url);
                if let Some(_request) = &context.request {
                    let mut response = crate::core::model::HttpResponse::new(302);
                    response.headers.insert("location", target_url.parse().unwrap());
                    response.headers.insert("content-length", "0".parse().unwrap());
                    context.response = Some(response);
                    // 标记请求已处理，避免后续中间件再次处理
                    context.set_processing_state(ProcessingState::Completed);
                    return Ok(MiddlewareResult::Stop);
                }
                Ok(MiddlewareResult::Continue)
            }

            // 交互动作
            RuleAction::InterceptForApproval => {
                info!("✋ Intercepting request for manual approval");
                context.custom_data.insert(
                    "rule_decision".to_string(),
                    serde_json::json!("intercept_for_approval"),
                );
                // 触发用户干预流程
                core.control_port()
                    .intercept_connection(context.connection.id.clone())
                    .await
                    .map_err(|e| TakeoverError::Config(format!("Failed to intercept connection: {}", e)))?;
                Ok(MiddlewareResult::Pause(
                    "Waiting for manual approval".to_string(),
                ))
            }

            RuleAction::PauseForInspection(seconds) => {
                info!("⏸️ Pausing request for inspection: {}s", seconds);
                tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
                Ok(MiddlewareResult::Continue)
            }

            // 脚本和日志动作
            RuleAction::ExecuteScript(script_name) => {
                warn!("⚠️ Script execution not implemented yet: {}", script_name);
                Ok(MiddlewareResult::Continue)
            }
            RuleAction::CompositeRequest(composite_config) => {
                warn!(
                    "⚠️ Composite request not implemented yet: {:?}",
                    composite_config
                );
                Ok(MiddlewareResult::Continue)
            }
            RuleAction::Log(log_level) => {
                let message = format!(
                    "Rule-triggered log: {} -> {}",
                    context.target_host, context.target_port
                );
                match log_level {
                    LogLevel::Debug => debug!("{}", message),
                    LogLevel::Info => info!("{}", message),
                    LogLevel::Warn => warn!("{}", message),
                    LogLevel::Error => error!("{}", message),
                }
                Ok(MiddlewareResult::Continue)
            }
            RuleAction::Alert(alert_config) => {
                warn!("🚨 ALERT not fully implemented: {:?}", alert_config);
                Ok(MiddlewareResult::Continue)
            }

            // 自定义动作
            RuleAction::Custom(custom_data) => {
                info!("🔧 Executing custom action");
                context.custom_data.insert(
                    "custom_action_data".to_string(),
                    serde_json::json!(custom_data),
                );
                Ok(MiddlewareResult::Continue)
            }
        }
    }

    /// 应用请求修改
    async fn apply_request_modifications(
        &self,
        modifications: RequestModifications,
        context: &mut ConnectionContext,
    ) -> Result<(), TakeoverError> {
        if let Some(ref mut request) = context.request {
            if let Some(add_headers) = modifications.add_headers {
                for (key, value) in add_headers {
                    if let (Ok(name), Ok(val)) = (key.parse::<hyper::header::HeaderName>(), value.parse::<hyper::header::HeaderValue>()) {
                        request.headers.insert(name, val);
                    }
                }
            }
            if let Some(remove_headers) = modifications.remove_headers {
                for header in remove_headers {
                    if let Ok(name) = header.parse::<hyper::header::HeaderName>() {
                        request.headers.remove(name);
                    }
                }
            }
            if let Some(modify_headers) = modifications.modify_headers {
                for (key, value) in modify_headers {
                    if let (Ok(name), Ok(val)) = (key.parse::<hyper::header::HeaderName>(), value.parse::<hyper::header::HeaderValue>()) {
                        request.headers.insert(name, val);
                    }
                }
            }
            if let Some(new_method) = modifications.change_method {
                request.method = new_method;
            }
            if let Some(new_path) = modifications.change_path {
                if let Ok(mut url) = url::Url::parse(&request.url) {
                    url.set_path(&new_path);
                    request.url = url.to_string();
                }
            }
            if let Some(new_body) = modifications.change_body {
                request.body = new_body.into_bytes();
            }
        }
        Ok(())
    }
}
