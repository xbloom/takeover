//! HTTP修改中间件
//! 
//! 负责HTTP请求和响应的修改功能，包括：
//! - 请求头部修改
//! - URL重写和修改
//! - 请求体修改
//! - 响应修改（为将来扩展）

use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn, error, debug, instrument};
use async_trait::async_trait;

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{ConnectionContext, ProcessingState, HttpRequest, HttpResponse, RequestModifications, ResponseModifications};
use crate::core::ports::AppError;

/// HTTP修改中间件
/// 
/// 负责应用各种HTTP修改操作
pub struct HttpModificationMiddleware {
    /// 修改配置
    config: HttpModificationConfig,
    /// 全局修改规则
    global_modifications: Arc<tokio::sync::RwLock<Vec<ModificationRule>>>,
    /// 修改统计
    stats: Arc<tokio::sync::RwLock<HttpModificationStats>>,
}

/// HTTP修改配置
#[derive(Debug, Clone)]
pub struct HttpModificationConfig {
    /// 是否启用HTTP修改
    pub enabled: bool,
    /// 是否启用请求修改
    pub enable_request_modification: bool,
    /// 是否启用响应修改
    pub enable_response_modification: bool,
    /// 是否启用URL规范化
    pub enable_url_normalization: bool,
    /// 是否启用头部清理
    pub enable_header_cleanup: bool,
    /// 最大修改次数（防止无限循环）
    pub max_modifications: usize,
}

/// 修改规则
#[derive(Debug, Clone)]
pub struct ModificationRule {
    /// 规则ID
    pub id: String,
    /// 规则名称
    pub name: String,
    /// 匹配条件
    pub condition: ModificationCondition,
    /// 修改动作
    pub action: ModificationAction,
    /// 是否启用
    pub enabled: bool,
    /// 优先级
    pub priority: i32,
}

/// 修改条件
#[derive(Debug, Clone)]
pub enum ModificationCondition {
    /// 总是匹配
    Always,
    /// 主机匹配
    HostMatches(String),
    /// 路径匹配
    PathMatches(String),
    /// 头部存在
    HeaderExists(String),
    /// 头部值匹配
    HeaderValueMatches { header: String, value: String },
    /// 方法匹配
    MethodMatches(String),
    /// 组合条件
    And(Vec<ModificationCondition>),
    Or(Vec<ModificationCondition>),
}

/// 修改动作
#[derive(Debug, Clone)]
pub enum ModificationAction {
    /// 修改请求
    ModifyRequest(RequestModifications),
    /// 修改响应
    ModifyResponse(ResponseModifications),
    /// 添加头部
    AddHeader { name: String, value: String },
    /// 移除头部
    RemoveHeader(String),
    /// 重写URL
    RewriteUrl { pattern: String, replacement: String },
    /// 设置用户代理
    SetUserAgent(String),
    /// 规范化URL
    NormalizeUrl,
    /// 添加认证头部
    AddAuth { username: String, password: String },
}

/// HTTP修改统计
#[derive(Debug, Default, Clone)]
struct HttpModificationStats {
    /// 总处理请求数
    total_requests: u64,
    /// 总修改次数
    total_modifications: u64,
    /// 请求修改次数
    request_modifications: u64,
    /// 响应修改次数
    response_modifications: u64,
    /// 头部添加次数
    headers_added: u64,
    /// 头部移除次数
    headers_removed: u64,
    /// URL重写次数
    url_rewrites: u64,
    /// 失败的修改次数
    failed_modifications: u64,
}

impl HttpModificationMiddleware {
    /// 创建新的HTTP修改中间件
    pub fn new() -> Self {
        Self {
            config: HttpModificationConfig::default(),
            global_modifications: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            stats: Arc::new(tokio::sync::RwLock::new(HttpModificationStats::default())),
        }
    }
    
    /// 使用配置创建HTTP修改中间件
    pub fn with_config(config: HttpModificationConfig) -> Self {
        Self {
            config,
            global_modifications: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            stats: Arc::new(tokio::sync::RwLock::new(HttpModificationStats::default())),
        }
    }
    
    /// 添加全局修改规则
    pub async fn add_modification_rule(&self, rule: ModificationRule) {
        let mut rules = self.global_modifications.write().await;
        rules.push(rule);
        
        // 按优先级排序
        rules.sort_by_key(|r| r.priority);
    }
    
    /// 设置修改规则列表
    pub async fn set_modification_rules(&self, rules: Vec<ModificationRule>) {
        let mut global_rules = self.global_modifications.write().await;
        *global_rules = rules;
        
        // 按优先级排序
        global_rules.sort_by_key(|r| r.priority);
    }
    
    /// 应用请求修改
    #[instrument(skip(self, context))]
    async fn apply_request_modifications(&self, context: &mut ConnectionContext) -> Result<bool, AppError> {
        if !self.config.enable_request_modification {
            return Ok(false);
        }
        
        let mut modifications_applied = false;
        let mut modification_count = 0;
        
        // 应用上下文中已有的修改
        let modifications = context.applied_modifications.clone();
        for modification in &modifications {
            if modification_count >= self.config.max_modifications {
                warn!("Maximum modifications reached for connection {}", context.id);
                break;
            }
            
            if self.apply_request_modification(context, modification).await? {
                modifications_applied = true;
                modification_count += 1;
            }
        }
        
        // 应用全局修改规则
        let rules = self.global_modifications.read().await;
        for rule in rules.iter() {
            if !rule.enabled || modification_count >= self.config.max_modifications {
                continue;
            }
            
            if self.evaluate_modification_condition(&rule.condition, context).await {
                debug!("Applying modification rule '{}' to connection {}", rule.name, context.id);
                
                if self.apply_modification_action(&rule.action, context).await? {
                    modifications_applied = true;
                    modification_count += 1;
                }
            }
        }
        
        if modifications_applied {
            self.update_request_modification_stats().await;
        }
        
        Ok(modifications_applied)
    }
    
    /// 评估修改条件
    fn evaluate_modification_condition<'a>(&'a self, condition: &'a ModificationCondition, context: &'a ConnectionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            match condition {
            ModificationCondition::Always => true,
            ModificationCondition::HostMatches(pattern) => {
                self.wildcard_match(&context.target_host, pattern)
            }
            ModificationCondition::PathMatches(pattern) => {
                if let Some(ref request) = context.request {
                    let path = self.extract_path_from_url(&request.url);
                    self.wildcard_match(&path, pattern)
                } else {
                    false
                }
            }
            ModificationCondition::HeaderExists(header_name) => {
                if let Some(ref request) = context.request {
                    request.headers.contains_key(header_name)
                } else {
                    false
                }
            }
            ModificationCondition::HeaderValueMatches { header, value } => {
                if let Some(ref request) = context.request {
                    request.headers.get(header)
                        .map(|v| self.wildcard_match(v, value))
                        .unwrap_or(false)
                } else {
                    false
                }
            }
            ModificationCondition::MethodMatches(method) => {
                if let Some(ref request) = context.request {
                    request.method.eq_ignore_ascii_case(method)
                } else {
                    false
                }
            }
            ModificationCondition::And(conditions) => {
                for sub_condition in conditions {
                    if !Box::pin(self.evaluate_modification_condition(sub_condition, context)).await {
                        return false;
                    }
                }
                true
            }
            ModificationCondition::Or(conditions) => {
                for sub_condition in conditions {
                    if Box::pin(self.evaluate_modification_condition(sub_condition, context)).await {
                        return true;
                    }
                }
                false
            }
        }
        })
    }
    
    /// 应用修改动作
    async fn apply_modification_action(&self, action: &ModificationAction, context: &mut ConnectionContext) -> Result<bool, AppError> {
        if let Some(ref mut request) = context.request {
            match action {
                ModificationAction::ModifyRequest(modifications) => {
                    self.apply_request_modification(context, modifications).await
                }
                ModificationAction::AddHeader { name, value } => {
                    request.headers.insert(name.clone(), value.clone());
                    self.update_headers_added_stats().await;
                    Ok(true)
                }
                ModificationAction::RemoveHeader(header_name) => {
                    let removed = request.headers.remove(header_name).is_some();
                    if removed {
                        self.update_headers_removed_stats().await;
                    }
                    Ok(removed)
                }
                ModificationAction::RewriteUrl { pattern, replacement } => {
                    let new_url = self.apply_url_rewrite(&request.url, pattern, replacement);
                    if new_url != request.url {
                        request.url = new_url;
                        self.update_url_rewrite_stats().await;
                        Ok(true)
                    } else {
                        Ok(false)
                    }
                }
                ModificationAction::SetUserAgent(user_agent) => {
                    request.headers.insert("User-Agent".to_string(), user_agent.clone());
                    Ok(true)
                }
                ModificationAction::NormalizeUrl => {
                    let normalized_url = self.normalize_url(&request.url);
                    if normalized_url != request.url {
                        request.url = normalized_url;
                        Ok(true)
                    } else {
                        Ok(false)
                    }
                }
                ModificationAction::AddAuth { username, password } => {
                    let auth_value = format!("Basic {}", base64::encode(format!("{}:{}", username, password)));
                    request.headers.insert("Authorization".to_string(), auth_value);
                    Ok(true)
                }
                _ => Ok(false) // 其他动作不在请求阶段处理
            }
        } else {
            Ok(false)
        }
    }
    
    /// 应用请求修改
    async fn apply_request_modification(&self, context: &mut ConnectionContext, modifications: &RequestModifications) -> Result<bool, AppError> {
        let mut modified = false;
        
        if let Some(ref mut request) = context.request {
            // 修改URL
            if let Some(ref new_url) = modifications.url {
                request.url = new_url.clone();
                modified = true;
            }
            
            // 修改方法
            if let Some(ref new_method) = modifications.method {
                request.method = new_method.clone();
                modified = true;
            }
            
            // 修改头部
            if let Some(ref headers) = modifications.headers {
                for (name, value) in headers {
                    request.headers.insert(name.clone(), value.clone());
                }
                modified = true;
            }
            
            // 修改请求体
            if let Some(ref new_body) = modifications.body {
                request.body = new_body.clone();
                // 更新Content-Length
                request.headers.insert("Content-Length".to_string(), new_body.len().to_string());
                modified = true;
            }
            
            // 移除头部
            if let Some(ref remove_headers) = modifications.remove_headers {
                for header in remove_headers {
                    if request.headers.remove(header).is_some() {
                        modified = true;
                    }
                }
            }
            
            // 添加查询参数
            if let Some(ref add_params) = modifications.add_query_params {
                let new_url = self.add_query_params(&request.url, add_params)?;
                if new_url != request.url {
                    request.url = new_url;
                    modified = true;
                }
            }
            
            // 移除查询参数
            if let Some(ref remove_params) = modifications.remove_query_params {
                let new_url = self.remove_query_params(&request.url, remove_params)?;
                if new_url != request.url {
                    request.url = new_url;
                    modified = true;
                }
            }
        }
        
        Ok(modified)
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
                // 简化的多段通配符匹配
                let parts: Vec<&str> = pattern.split('*').collect();
                let mut pos = 0;
                for (i, part) in parts.iter().enumerate() {
                    if part.is_empty() {
                        continue;
                    }
                    if i == 0 {
                        if !text[pos..].starts_with(part) {
                            return false;
                        }
                        pos += part.len();
                    } else if let Some(found) = text[pos..].find(part) {
                        pos += found + part.len();
                    } else {
                        return false;
                    }
                }
                true
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
    
    /// 应用URL重写
    fn apply_url_rewrite(&self, url: &str, pattern: &str, replacement: &str) -> String {
        if self.wildcard_match(url, pattern) {
            // 简化的重写逻辑
            if pattern.contains('*') {
                // 对于包含通配符的模式，进行简单的替换
                replacement.to_string()
            } else {
                // 精确匹配，直接替换
                replacement.to_string()
            }
        } else {
            url.to_string()
        }
    }
    
    /// 规范化URL
    fn normalize_url(&self, url: &str) -> String {
        if let Ok(mut parsed_url) = url::Url::parse(url) {
            // 移除默认端口
            if (parsed_url.scheme() == "http" && parsed_url.port() == Some(80)) ||
               (parsed_url.scheme() == "https" && parsed_url.port() == Some(443)) {
                let _ = parsed_url.set_port(None);
            }
            
            // 规范化路径
            let path = parsed_url.path().to_string();
            if path.ends_with('/') && path.len() > 1 {
                parsed_url.set_path(&path[..path.len()-1]);
            }
            
            parsed_url.to_string()
        } else {
            url.to_string()
        }
    }
    
    /// 添加查询参数
    fn add_query_params(&self, url: &str, params: &HashMap<String, String>) -> Result<String, AppError> {
        if let Ok(mut parsed_url) = url::Url::parse(url) {
            for (key, value) in params {
                parsed_url.query_pairs_mut().append_pair(key, value);
            }
            Ok(parsed_url.to_string())
        } else {
            Err(AppError::Config(format!("Invalid URL: {}", url)))
        }
    }
    
    /// 移除查询参数
    fn remove_query_params(&self, url: &str, params: &[String]) -> Result<String, AppError> {
        if let Ok(mut parsed_url) = url::Url::parse(url) {
            let mut new_pairs = Vec::new();
            
            for (key, value) in parsed_url.query_pairs() {
                if !params.contains(&key.to_string()) {
                    new_pairs.push((key.to_string(), value.to_string()));
                }
            }
            
            parsed_url.query_pairs_mut().clear();
            for (key, value) in new_pairs {
                parsed_url.query_pairs_mut().append_pair(&key, &value);
            }
            
            Ok(parsed_url.to_string())
        } else {
            Err(AppError::Config(format!("Invalid URL: {}", url)))
        }
    }
    
    /// 更新统计信息
    async fn update_request_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.total_requests += 1;
    }
    
    async fn update_request_modification_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.total_modifications += 1;
        stats.request_modifications += 1;
    }
    
    async fn update_headers_added_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.headers_added += 1;
    }
    
    async fn update_headers_removed_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.headers_removed += 1;
    }
    
    async fn update_url_rewrite_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.url_rewrites += 1;
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> HttpModificationStats {
        self.stats.read().await.clone()
    }
}

impl Default for HttpModificationMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for HttpModificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            enable_request_modification: true,
            enable_response_modification: true,
            enable_url_normalization: true,
            enable_header_cleanup: true,
            max_modifications: 10,
        }
    }
}

#[async_trait]
impl Middleware for HttpModificationMiddleware {
    fn name(&self) -> &str {
        "http_modification"
    }
    
    #[instrument(skip(self, context, _core))]
    async fn process(
        &self,
        context: &mut ConnectionContext,
        _core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, crate::error::TakeoverError> {
        debug!("HTTP modification middleware processing connection {}", context.id);
        
        // 更新处理状态
        context.set_processing_state(ProcessingState::HttpModification);
        
        // 更新请求统计
        self.update_request_stats().await;
        
        if !self.config.enabled {
            debug!("HTTP modification disabled, skipping");
            return Ok(MiddlewareResult::Continue);
        }
        
        // 应用请求修改
        match self.apply_request_modifications(context).await {
            Ok(modifications_applied) => {
                if modifications_applied {
                    info!("✅ HTTP modifications applied to connection {}", context.id);
                    
                    // 存储修改信息到上下文
                    context.custom_data.insert(
                        "http_modifications".to_string(),
                        serde_json::json!({
                            "applied": true,
                            "timestamp": chrono::Utc::now().to_rfc3339()
                        })
                    );
                } else {
                    debug!("No HTTP modifications applied to connection {}", context.id);
                }
                
                Ok(MiddlewareResult::Continue)
            }
            Err(e) => {
                error!("❌ HTTP modification failed for connection {}: {}", context.id, e);
                
                // 记录失败但继续处理
                {
                    let mut stats = self.stats.write().await;
                    stats.failed_modifications += 1;
                }
                
                context.custom_data.insert(
                    "http_modifications".to_string(),
                    serde_json::json!({
                        "applied": false,
                        "error": e.to_string(),
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    })
                );
                
                // 不因修改失败而中断连接
                Ok(MiddlewareResult::Continue)
            }
        }
    }
    
    fn priority(&self) -> i32 {
        50 // 在规则匹配之后，转发之前
    }
    
    fn enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Connection, ConnectionMetadata, HttpRequest};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    
    #[tokio::test]
    async fn test_middleware_creation() {
        let middleware = HttpModificationMiddleware::new();
        assert_eq!(middleware.name(), "http_modification");
        assert!(middleware.enabled());
    }
    
    #[tokio::test]
    async fn test_wildcard_matching() {
        let middleware = HttpModificationMiddleware::new();
        
        assert!(middleware.wildcard_match("example.com", "*.com"));
        assert!(middleware.wildcard_match("api.example.com", "api.*"));
        assert!(middleware.wildcard_match("anything", "*"));
        assert!(middleware.wildcard_match("example.com", "example.com"));
        assert!(!middleware.wildcard_match("example.org", "example.com"));
    }
    
    #[tokio::test]
    async fn test_url_normalization() {
        let middleware = HttpModificationMiddleware::new();
        
        assert_eq!(
            middleware.normalize_url("http://example.com:80/path/"),
            "http://example.com/path"
        );
        
        assert_eq!(
            middleware.normalize_url("https://example.com:443/"),
            "https://example.com"
        );
    }
    
    #[tokio::test]
    async fn test_query_param_manipulation() {
        let middleware = HttpModificationMiddleware::new();
        
        let mut params = HashMap::new();
        params.insert("key".to_string(), "value".to_string());
        
        let result = middleware.add_query_params("http://example.com/path", &params).unwrap();
        assert!(result.contains("key=value"));
        
        let remove_params = vec!["key".to_string()];
        let result2 = middleware.remove_query_params(&result, &remove_params).unwrap();
        assert!(!result2.contains("key=value"));
    }
    
    #[tokio::test]
    async fn test_modification_rules() {
        let middleware = HttpModificationMiddleware::new();
        
        let rule = ModificationRule {
            id: "test_rule".to_string(),
            name: "Test Rule".to_string(),
            condition: ModificationCondition::Always,
            action: ModificationAction::AddHeader {
                name: "X-Test".to_string(),
                value: "test-value".to_string(),
            },
            enabled: true,
            priority: 1,
        };
        
        middleware.add_modification_rule(rule).await;
        
        let rules = middleware.global_modifications.read().await;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "test_rule");
    }
    
    #[tokio::test]
    async fn test_condition_evaluation() {
        let middleware = HttpModificationMiddleware::new();
        
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        
        let mut context = ConnectionContext::new(connection, "example.com".to_string(), 80);
        context.request = Some(HttpRequest::new("GET", "http://example.com/"));
        
        // 测试Always条件
        assert!(middleware.evaluate_modification_condition(
            &ModificationCondition::Always,
            &context
        ).await);
        
        // 测试主机匹配
        assert!(middleware.evaluate_modification_condition(
            &ModificationCondition::HostMatches("example.com".to_string()),
            &context
        ).await);
        
        // 测试方法匹配
        assert!(middleware.evaluate_modification_condition(
            &ModificationCondition::MethodMatches("GET".to_string()),
            &context
        ).await);
    }
    
    #[tokio::test]
    async fn test_stats_tracking() {
        let middleware = HttpModificationMiddleware::new();
        
        let initial_stats = middleware.get_stats().await;
        assert_eq!(initial_stats.total_requests, 0);
        
        middleware.update_request_stats().await;
        middleware.update_request_modification_stats().await;
        middleware.update_url_rewrite_stats().await;
        
        let updated_stats = middleware.get_stats().await;
        assert_eq!(updated_stats.total_requests, 1);
        assert_eq!(updated_stats.request_modifications, 1);
        assert_eq!(updated_stats.url_rewrites, 1);
    }
}
