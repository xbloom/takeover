//! 脚本中间件
//! 
//! 将Lua脚本集成到处理管道中，负责：
//! - 脚本触发器管理
//! - 脚本执行协调
//! - 脚本输出处理
//! - 复合请求处理
//! - 脚本错误处理

use std::sync::Arc;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{info, warn, error, debug, instrument};
use async_trait::async_trait;
use crate::core::model;

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{ConnectionContext, ProcessingState, HttpRequest, HttpResponse, CompositeRequestConfig};
use crate::core::ports::{ScriptPort, ApiPort, AppError, ScriptInput, ScriptOutput, ScriptAction, TriggerPoint, TriggerCondition};

/// 脚本中间件
/// 
/// 负责在处理管道中执行Lua脚本
pub struct ScriptMiddleware {
    /// 脚本端口
    script_port: Arc<dyn ScriptPort>,
    /// API端口
    api_port: Arc<dyn ApiPort>,
    /// 启用的脚本和触发器
    enabled_scripts: Arc<tokio::sync::RwLock<HashMap<String, ScriptTrigger>>>,
    /// 中间件配置
    config: ScriptMiddlewareConfig,
    /// 执行统计
    stats: Arc<tokio::sync::RwLock<ScriptStats>>,
}

/// 脚本中间件配置
#[derive(Debug, Clone)]
pub struct ScriptMiddlewareConfig {
    /// 是否启用脚本中间件
    pub enabled: bool,
    /// 最大并发脚本执行数
    pub max_concurrent_scripts: usize,
    /// 脚本执行超时
    pub execution_timeout: Duration,
    /// 是否在脚本失败时继续处理
    pub continue_on_script_failure: bool,
    /// 默认触发点
    pub default_trigger_point: TriggerPoint,
    /// 是否启用复合请求
    pub enable_composite_requests: bool,
    /// 复合请求超时
    pub composite_request_timeout: Duration,
}

/// 脚本触发器
#[derive(Debug, Clone)]
pub struct ScriptTrigger {
    /// 脚本名称
    pub script_name: String,
    /// 触发点
    pub trigger_point: TriggerPoint,
    /// 触发条件
    pub conditions: Vec<TriggerCondition>,
    /// 是否启用
    pub enabled: bool,
    /// 优先级
    pub priority: i32,
}

/// 脚本执行统计
#[derive(Debug, Default, Clone)]
struct ScriptStats {
    /// 总执行次数
    total_executions: u64,
    /// 成功执行次数
    successful_executions: u64,
    /// 失败执行次数
    failed_executions: u64,
    /// 跳过的执行次数
    skipped_executions: u64,
    /// 复合请求次数
    composite_requests: u64,
    /// 平均执行时间
    avg_execution_time: Option<Duration>,
    /// 按动作分类的统计
    action_stats: HashMap<String, u64>,
}

impl ScriptMiddleware {
    /// 创建新的脚本中间件
    pub fn new(
        script_port: Arc<dyn ScriptPort>,
        api_port: Arc<dyn ApiPort>,
    ) -> Self {
        Self {
            script_port,
            api_port,
            enabled_scripts: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            config: ScriptMiddlewareConfig::default(),
            stats: Arc::new(tokio::sync::RwLock::new(ScriptStats::default())),
        }
    }
    
    /// 使用配置创建脚本中间件
    pub fn with_config(
        script_port: Arc<dyn ScriptPort>,
        api_port: Arc<dyn ApiPort>,
        config: ScriptMiddlewareConfig,
    ) -> Self {
        Self {
            script_port,
            api_port,
            enabled_scripts: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            config,
            stats: Arc::new(tokio::sync::RwLock::new(ScriptStats::default())),
        }
    }
    
    /// 添加脚本触发器
    pub async fn add_script_trigger(&self, trigger: ScriptTrigger) {
        let mut scripts = self.enabled_scripts.write().await;
        scripts.insert(trigger.script_name.clone(), trigger);
    }
    
    /// 移除脚本触发器
    pub async fn remove_script_trigger(&self, script_name: &str) {
        let mut scripts = self.enabled_scripts.write().await;
        scripts.remove(script_name);
    }
    
    /// 获取适用于当前触发点的脚本
    async fn get_applicable_scripts(&self, trigger_point: TriggerPoint, context: &ConnectionContext) -> Vec<ScriptTrigger> {
        let scripts = self.enabled_scripts.read().await;
        let mut applicable = Vec::new();
        
        for trigger in scripts.values() {
            if !trigger.enabled {
                continue;
            }
            
            if trigger.trigger_point != trigger_point {
                continue;
            }
            
            if self.evaluate_trigger_conditions(&trigger.conditions, context).await {
                applicable.push(trigger.clone());
            }
        }
        
        // 按优先级排序
        applicable.sort_by_key(|t| t.priority);
        applicable
    }
    
    /// 评估触发条件
    async fn evaluate_trigger_conditions(&self, conditions: &[TriggerCondition], context: &ConnectionContext) -> bool {
        if conditions.is_empty() {
            return true; // 没有条件时总是触发
        }
        
        for condition in conditions {
            match condition {
                TriggerCondition::Always => continue,
                TriggerCondition::Expression(_expr) => {
                    // TODO: 实现表达式评估
                    debug!("Expression trigger condition not yet implemented");
                    return false;
                }
                TriggerCondition::RuleMatch(_rule_name) => {
                    // TODO: 实现规则匹配
                    debug!("Rule match trigger condition not yet implemented");
                    return false;
                }
                TriggerCondition::HostMatches(pattern) => {
                    if !self.wildcard_match(&context.target_host, pattern) {
                        return false;
                    }
                }
                TriggerCondition::PathMatches(pattern) => {
                    if let Some(ref request) = context.request {
                        let path = self.extract_path_from_url(&request.url);
                        if !self.wildcard_match(&path, pattern) {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                TriggerCondition::MethodEquals(method) => {
                    if let Some(ref request) = context.request {
                        if !request.method.eq_ignore_ascii_case(method) {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                TriggerCondition::HeaderExists(header_name) => {
                    if let Some(ref request) = context.request {
                        if !request.headers.contains_key(header_name) {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                TriggerCondition::Custom(_lua_expr) => {
                    // 自定义Lua表达式暂时跳过
                    // 在完整实现中，这里应该执行Lua表达式
                    continue;
                }
            }
        }
        
        true
    }
    
    /// 通配符匹配
    fn wildcard_match(&self, text: &str, pattern: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        
        if pattern.contains('*') {
            if pattern.starts_with('*') && pattern.ends_with('*') {
                let middle = &pattern[1..pattern.len()-1];
                text.contains(middle)
            } else if pattern.starts_with('*') {
                let suffix = &pattern[1..];
                text.ends_with(suffix)
            } else if pattern.ends_with('*') {
                let prefix = &pattern[..pattern.len()-1];
                text.starts_with(prefix)
            } else {
                // 复杂通配符匹配 - 简化实现
                text.contains(&pattern.replace('*', ""))
            }
        } else {
            text == pattern
        }
    }
    
    /// 从URL提取路径
    fn extract_path_from_url(&self, url: &str) -> String {
        if let Ok(parsed_url) = url::Url::parse(url) {
            parsed_url.path().to_string()
        } else {
            "/".to_string()
        }
    }
    
    /// 执行脚本
    #[instrument(skip(self, context, script_name))]
    async fn execute_script(&self, script_name: &str, context: &mut ConnectionContext) -> Result<ScriptOutput, AppError> {
        let start_time = Instant::now();
        
        debug!("Executing script '{}' for connection {}", script_name, context.id);
        
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.total_executions += 1;
        }
        
        // 准备脚本输入
        let script_input = ScriptInput {
            request: context.request.clone(),
            response: context.response.clone(),
            connection_context: context.clone(),
            custom_data: context.custom_data.clone(),
        };
        
        // 执行脚本
        let result = tokio::time::timeout(
            self.config.execution_timeout,
            self.script_port.execute_script(script_name, context, script_input)
        ).await;
        
        let execution_time = start_time.elapsed();
        
        // 更新平均执行时间
        {
            let mut stats = self.stats.write().await;
            stats.avg_execution_time = match stats.avg_execution_time {
                Some(avg) => Some((avg + execution_time) / 2),
                None => Some(execution_time),
            };
        }
        
        match result {
            Ok(Ok(output)) => {
                // 更新成功统计
                {
                    let mut stats = self.stats.write().await;
                    stats.successful_executions += 1;
                    
                    let action_name = format!("{:?}", output.action);
                    *stats.action_stats.entry(action_name).or_insert(0) += 1;
                }
                
                info!("✅ Script '{}' executed successfully in {:?}", script_name, execution_time);
                Ok(output)
            }
            Ok(Err(e)) => {
                // 更新失败统计
                {
                    let mut stats = self.stats.write().await;
                    stats.failed_executions += 1;
                }
                
                error!("❌ Script '{}' execution failed: {}", script_name, e);
                Err(e)
            }
            Err(_) => {
                // 更新失败统计（超时）
                {
                    let mut stats = self.stats.write().await;
                    stats.failed_executions += 1;
                }
                
                error!("❌ Script '{}' execution timed out after {:?}", script_name, self.config.execution_timeout);
                Err(AppError::Config(format!("Script '{}' execution timed out", script_name)))
            }
        }
    }
    
    /// 处理脚本输出
    async fn handle_script_output(
        &self,
        script_name: &str,
        output: ScriptOutput,
        context: &mut ConnectionContext,
    ) -> Result<MiddlewareResult, AppError> {
        debug!("Handling script output for '{}': {:?}", script_name, output.action);
        
        // 应用请求修改
        if let Some(modified_request) = output.modified_request {
            context.request = Some(modified_request);
        }
        
        // 应用响应修改
        if let Some(modified_response) = output.modified_response {
            context.response = Some(modified_response);
        }
        
        // 应用自定义数据
        for (key, value) in output.custom_data {
            context.custom_data.insert(key, value);
        }
        
        // 根据脚本动作决定处理结果
        match output.action {
            ScriptAction::Continue => {
                debug!("Script '{}' returned continue", script_name);
                Ok(MiddlewareResult::Continue)
            }
            ScriptAction::Pause(reason) => {
                info!("⏸️ Script '{}' paused processing: {}", script_name, reason);
                Ok(MiddlewareResult::Pause(reason))
            }
            ScriptAction::Reject(reason) => {
                warn!("❌ Script '{}' rejected request: {}", script_name, reason);
                Ok(MiddlewareResult::Reject(reason))
            }
            ScriptAction::Redirect(url) => {
                info!("🔄 Script '{}' redirected to: {}", script_name, url);
                Ok(MiddlewareResult::Redirect(url))
            }
            ScriptAction::CompositeRequest(config) => {
                if self.config.enable_composite_requests {
                    let model_config = model::CompositeRequestConfig {
                        name: "Generated Composite Request".to_string(),
                        requests: config.requests.into_iter().collect(),
                        merge_strategy: config.merge_strategy.into(),
                        timeout: config.timeout,
                    };
                    self.handle_composite_request(script_name, model_config, context).await
                } else {
                    warn!("Composite request requested by '{}' but disabled in config", script_name);
                    Ok(MiddlewareResult::Continue)
                }
            }
            ScriptAction::Custom(data) => {
                debug!("Script '{}' returned custom action with {} fields", script_name, data.len());
                
                // 存储自定义动作数据到上下文
                context.custom_data.insert(
                    format!("script_{}_custom", script_name),
                    serde_json::json!(data)
                );
                
                Ok(MiddlewareResult::Continue)
            }
        }
    }
    
    /// 处理复合请求
    async fn handle_composite_request(
        &self,
        script_name: &str,
        config: CompositeRequestConfig,
        context: &mut ConnectionContext,
    ) -> Result<MiddlewareResult, AppError> {
        info!("🔀 Handling composite request from '{}' with {} requests", script_name, config.requests.len());
        
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.composite_requests += 1;
        }
        
        let start_time = Instant::now();
        let timeout = config.timeout.unwrap_or(self.config.composite_request_timeout);
        
        // 并发执行所有请求
        let mut request_futures = Vec::new();
        
        let requests = config.requests.clone(); // 克隆请求列表
        for (index, request) in requests.iter().enumerate() {
            let api_port = Arc::clone(&self.api_port);
            let request_clone = request.clone();
            let total_requests = requests.len();
            
            let future = async move {
                debug!("Executing composite request {} of {}", index + 1, total_requests);
                api_port.http_request(request_clone).await
            };
            
            request_futures.push(future);
        }
        
        // 等待所有请求完成或超时
        let results = tokio::time::timeout(
            timeout,
            futures::future::join_all(request_futures)
        ).await;
        
        let execution_time = start_time.elapsed();
        
        match results {
            Ok(responses) => {
                let mut successful_responses = Vec::new();
                let mut failed_count = 0;
                
                for (index, result) in responses.into_iter().enumerate() {
                    match result {
                        Ok(response) => {
                            successful_responses.push(response);
                        }
                        Err(e) => {
                            warn!("Composite request {} failed: {}", index + 1, e);
                            failed_count += 1;
                        }
                    }
                }
                
                info!(
                    "✅ Composite request completed: {} successful, {} failed in {:?}",
                    successful_responses.len(),
                    failed_count,
                    execution_time
                );
                
                // 合并响应
                let merged_response = self.merge_responses(successful_responses.clone(), &config).await?;
                context.response = Some(merged_response);
                
                // 存储复合请求信息到上下文
                context.custom_data.insert(
                    "composite_request_info".to_string(),
                    serde_json::json!({
                        "script": script_name,
                        "successful_requests": successful_responses.len(),
                        "failed_requests": failed_count,
                        "execution_time_ms": execution_time.as_millis()
                    })
                );
                
                Ok(MiddlewareResult::Continue)
            }
            Err(_) => {
                error!("❌ Composite request from '{}' timed out after {:?}", script_name, timeout);
                Err(AppError::Config(format!("Composite request timed out")))
            }
        }
    }
    
    /// 合并多个HTTP响应
    async fn merge_responses(&self, responses: Vec<HttpResponse>, config: &CompositeRequestConfig) -> Result<HttpResponse, AppError> {
        if responses.is_empty() {
            return Err(AppError::Config("No responses to merge".to_string()));
        }
        
        if responses.len() == 1 {
            return Ok(responses.into_iter().next().unwrap());
        }
        
        // 使用第一个响应作为基础
        let mut base_response = responses[0].clone();
        
        match config.merge_strategy {
            crate::core::model::MergeStrategy::ReplaceBody => {
                // 使用最后一个响应的body
                if let Some(last_response) = responses.last() {
                    base_response.body = last_response.body.clone();
                }
            }
            crate::core::model::MergeStrategy::AppendBody => {
                // 合并所有响应的body
                let mut merged_body = Vec::new();
                for response in &responses {
                    merged_body.extend(&response.body);
                }
                base_response.body = merged_body;
            }
            crate::core::model::MergeStrategy::MergeJson => {
                // 尝试合并JSON响应
                self.merge_json_responses(&mut base_response, &responses).await?;
            }
            crate::core::model::MergeStrategy::MergeHeaders => {
                // 合并所有响应的头部
                for response in &responses[1..] {
                    base_response.headers.extend(response.headers.clone());
                }
            }
            crate::core::model::MergeStrategy::CustomScript => {
                // 自定义合并脚本 - 暂时使用简单合并
                warn!("Custom script merge strategy not implemented, using append body");
                let mut merged_body = Vec::new();
                for response in &responses {
                    merged_body.extend(&response.body);
                }
                base_response.body = merged_body;
            }
        }
        
        debug!("✅ Merged {} responses using strategy: {:?}", responses.len(), config.merge_strategy);
        Ok(base_response)
    }
    
    /// 合并JSON响应
    async fn merge_json_responses(&self, base_response: &mut HttpResponse, responses: &[HttpResponse]) -> Result<(), AppError> {
        let mut merged_json = serde_json::Value::Object(serde_json::Map::new());
        
        for response in responses {
            if let Ok(json_value) = serde_json::from_slice::<serde_json::Value>(&response.body) {
                match json_value {
                    serde_json::Value::Object(map) => {
                        if let serde_json::Value::Object(ref mut merged_map) = merged_json {
                            merged_map.extend(map);
                        }
                    }
                    _ => {
                        // 非对象JSON值，添加到数组中
                        merged_json = serde_json::json!([merged_json, json_value]);
                    }
                }
            }
        }
        
        base_response.body = serde_json::to_vec(&merged_json)
            .map_err(|e| AppError::Config(format!("JSON merge failed: {}", e)))?;
        
        // 更新Content-Type头部
        base_response.headers.insert("Content-Type".to_string(), "application/json".to_string());
        base_response.headers.insert("Content-Length".to_string(), base_response.body.len().to_string());
        
        Ok(())
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> ScriptStats {
        self.stats.read().await.clone()
    }
}

impl Default for ScriptMiddlewareConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_concurrent_scripts: 5,
            execution_timeout: Duration::from_secs(30),
            continue_on_script_failure: true,
            default_trigger_point: TriggerPoint::AfterRequest,
            enable_composite_requests: true,
            composite_request_timeout: Duration::from_secs(60),
        }
    }
}

#[async_trait]
impl Middleware for ScriptMiddleware {
    fn name(&self) -> &str {
        "script"
    }
    
    #[instrument(skip(self, context, _core))]
    async fn process(
        &self,
        context: &mut ConnectionContext,
        _core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, crate::error::TakeoverError> {
        debug!("Script middleware processing connection {}", context.id);
        
        if !self.config.enabled {
            debug!("Script middleware disabled, skipping");
            return Ok(MiddlewareResult::Continue);
        }
        
        // 更新处理状态
        context.set_processing_state(ProcessingState::ScriptExecution);
        
        // 确定当前触发点
        let trigger_point = self.config.default_trigger_point.clone();
        
        // 获取适用的脚本
        let applicable_scripts = self.get_applicable_scripts(trigger_point, context).await;
        
        if applicable_scripts.is_empty() {
            debug!("No applicable scripts found for connection {}", context.id);
            
            // 更新跳过统计
            {
                let mut stats = self.stats.write().await;
                stats.skipped_executions += 1;
            }
            
            return Ok(MiddlewareResult::Continue);
        }
        
        debug!("Found {} applicable scripts for connection {}", applicable_scripts.len(), context.id);
        
        // 按优先级执行脚本
        for trigger in applicable_scripts {
            debug!("Executing script '{}' with priority {}", trigger.script_name, trigger.priority);
            
            match self.execute_script(&trigger.script_name, context).await {
                Ok(output) => {
                    // 处理脚本输出
                    match self.handle_script_output(&trigger.script_name, output, context).await {
                        Ok(MiddlewareResult::Continue) => {
                            // 继续执行下一个脚本
                            continue;
                        }
                        Ok(result) => {
                            // 脚本要求特殊处理（暂停、拒绝等）
                            return Ok(result);
                        }
                        Err(e) => {
                            error!("❌ Failed to handle script output from '{}': {}", trigger.script_name, e);
                            
                            if !self.config.continue_on_script_failure {
                                return Err(crate::error::TakeoverError::Config(format!("Script processing failed: {}", e)));
                            }
                            // 否则继续处理下一个脚本
                        }
                    }
                }
                Err(e) => {
                    error!("❌ Script '{}' execution failed: {}", trigger.script_name, e);
                    
                    if !self.config.continue_on_script_failure {
                        return Err(crate::error::TakeoverError::Config(format!("Script execution failed: {}", e)));
                    }
                    // 否则继续处理下一个脚本
                }
            }
        }
        
        info!("✅ All applicable scripts executed successfully for connection {}", context.id);
        Ok(MiddlewareResult::Continue)
    }
    
    fn priority(&self) -> i32 {
        35 // 在规则匹配之后，HTTP修改之前
    }
    
    fn enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Connection, ConnectionMetadata, HttpRequest, TriggerPoint, TriggerCondition};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    
    // Mock implementations for testing
    struct MockScriptPort;
    struct MockApiPort;
    
    #[async_trait]
    impl ScriptPort for MockScriptPort {
        async fn execute_script(&self, _script_name: &str, _context: &mut ConnectionContext, _input: ScriptInput) -> Result<ScriptOutput, AppError> {
            Ok(ScriptOutput {
                action: ScriptAction::Continue,
                modified_request: None,
                modified_response: None,
                additional_requests: Vec::new(),
                custom_data: HashMap::new(),
            })
        }
        
        async fn load_script(&self, _script_name: &str, _script_content: &str) -> Result<(), AppError> { Ok(()) }
        async fn list_scripts(&self) -> Result<Vec<crate::core::ports::ScriptInfo>, AppError> { Ok(vec![]) }
        async fn validate_script(&self, _script_content: &str) -> Result<(), AppError> { Ok(()) }
        async fn add_trigger(&self, _trigger: crate::core::ports::ScriptTrigger) -> Result<(), AppError> { Ok(()) }
        async fn remove_trigger(&self, _script_name: &str) -> Result<(), AppError> { Ok(()) }
    }
    
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
    async fn test_middleware_creation() {
        let script_port = Arc::new(MockScriptPort);
        let api_port = Arc::new(MockApiPort);
        
        let middleware = ScriptMiddleware::new(script_port, api_port);
        assert_eq!(middleware.name(), "script");
        assert!(middleware.enabled());
    }
    
    #[tokio::test]
    async fn test_wildcard_matching() {
        let script_port = Arc::new(MockScriptPort);
        let api_port = Arc::new(MockApiPort);
        let middleware = ScriptMiddleware::new(script_port, api_port);
        
        assert!(middleware.wildcard_match("example.com", "*.com"));
        assert!(middleware.wildcard_match("api.example.com", "api.*"));
        assert!(middleware.wildcard_match("anything", "*"));
        assert!(middleware.wildcard_match("example.com", "example.com"));
        assert!(!middleware.wildcard_match("example.org", "example.com"));
    }
    
    #[tokio::test]
    async fn test_trigger_condition_evaluation() {
        let script_port = Arc::new(MockScriptPort);
        let api_port = Arc::new(MockApiPort);
        let middleware = ScriptMiddleware::new(script_port, api_port);
        
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        
        let mut context = ConnectionContext::new(connection, "example.com".to_string(), 80);
        context.request = Some(HttpRequest::new("GET", "http://example.com/api"));
        
        // 测试主机匹配条件
        let host_condition = vec![TriggerCondition::HostMatches("example.com".to_string())];
        assert!(middleware.evaluate_trigger_conditions(&host_condition, &context).await);
        
        // 测试方法匹配条件
        let method_condition = vec![TriggerCondition::MethodEquals("GET".to_string())];
        assert!(middleware.evaluate_trigger_conditions(&method_condition, &context).await);
        
        // 测试路径匹配条件
        let path_condition = vec![TriggerCondition::PathMatches("/api".to_string())];
        assert!(middleware.evaluate_trigger_conditions(&path_condition, &context).await);
    }
    
    #[tokio::test]
    async fn test_script_trigger_management() {
        let script_port = Arc::new(MockScriptPort);
        let api_port = Arc::new(MockApiPort);
        let middleware = ScriptMiddleware::new(script_port, api_port);
        
        let trigger = ScriptTrigger {
            script_name: "test_script".to_string(),
            trigger_point: TriggerPoint::BeforeRequest,
            conditions: vec![TriggerCondition::HostMatches("example.com".to_string())],
            enabled: true,
            priority: 1,
        };
        
        middleware.add_script_trigger(trigger).await;
        
        let scripts = middleware.enabled_scripts.read().await;
        assert!(scripts.contains_key("test_script"));
        
        drop(scripts);
        middleware.remove_script_trigger("test_script").await;
        
        let scripts = middleware.enabled_scripts.read().await;
        assert!(!scripts.contains_key("test_script"));
    }
    
    #[tokio::test]
    async fn test_stats_tracking() {
        let script_port = Arc::new(MockScriptPort);
        let api_port = Arc::new(MockApiPort);
        let middleware = ScriptMiddleware::new(script_port, api_port);
        
        let initial_stats = middleware.get_stats().await;
        assert_eq!(initial_stats.total_executions, 0);
        
        // 更新统计
        {
            let mut stats = middleware.stats.write().await;
            stats.total_executions += 1;
            stats.successful_executions += 1;
        }
        
        let updated_stats = middleware.get_stats().await;
        assert_eq!(updated_stats.total_executions, 1);
        assert_eq!(updated_stats.successful_executions, 1);
    }
}
