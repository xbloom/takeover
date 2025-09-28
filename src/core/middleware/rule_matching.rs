//! 规则匹配中间件
//! 
//! 将现有的路由引擎功能重构为中间件架构，负责：
//! - 路由规则匹配和决策
//! - URL重写处理
//! - 出站策略选择
//! - 代理vs直连决策

use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn, error, debug, instrument};
use async_trait::async_trait;

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{ConnectionContext, ProcessingState, Rule, RequestModifications};
use crate::core::model::{RulePattern, RuleAction};
use crate::core::ports::AppError;
use crate::routing_engine::{RoutingEngine, RoutingDecision, TlsInterceptionMode};

/// 规则匹配中间件
/// 
/// 负责应用路由规则和做出代理决策
pub struct RuleMatchingMiddleware {
    /// 路由引擎
    routing_engine: Option<Arc<RoutingEngine>>,
    /// 规则集合
    rules: Arc<tokio::sync::RwLock<Vec<Rule>>>,
    /// 中间件配置
    config: RuleMatchingConfig,
    /// 匹配统计
    stats: Arc<tokio::sync::RwLock<RuleMatchingStats>>,
}

/// 规则匹配配置
#[derive(Debug, Clone)]
pub struct RuleMatchingConfig {
    /// 是否启用规则匹配
    pub enabled: bool,
    /// 默认动作（当没有规则匹配时）
    pub default_action: DefaultAction,
    /// 是否启用URL重写
    pub enable_url_rewrite: bool,
    /// 是否启用进程匹配
    pub enable_process_matching: bool,
    /// 规则优先级模式
    pub priority_mode: PriorityMode,
}

/// 默认动作
#[derive(Debug, Clone)]
pub enum DefaultAction {
    /// 允许通过
    Allow,
    /// 直连
    Direct,
    /// 使用默认代理
    Proxy,
    /// 拒绝连接
    Deny,
}

/// 优先级模式
#[derive(Debug, Clone)]
pub enum PriorityMode {
    /// 按规则定义顺序
    Sequential,
    /// 按优先级数字
    Priority,
    /// 最佳匹配
    BestMatch,
}

/// 规则匹配统计
#[derive(Debug, Default, Clone)]
pub struct RuleMatchingStats {
    /// 总处理连接数
    total_connections: u64,
    /// 规则匹配数
    rule_matches: u64,
    /// 使用默认动作数
    default_actions: u64,
    /// 代理决策数
    proxy_decisions: u64,
    /// 直连决策数
    direct_decisions: u64,
    /// 拒绝决策数
    denied_decisions: u64,
    /// URL重写次数
    #[allow(dead_code)]
    url_rewrites: u64,
    /// 规则匹配详情
    rule_match_details: HashMap<String, u64>,
}

/// 路由决策结果
#[derive(Debug, Clone)]
pub struct RoutingResult {
    /// 决策类型
    pub decision: RoutingDecisionType,
    /// 匹配的规则ID
    pub matched_rule_id: Option<String>,
    /// 重写后的URL
    pub rewritten_url: Option<String>,
    /// 应用的修改
    pub applied_modifications: Option<RequestModifications>,
    /// TLS拦截模式
    pub tls_interception_mode: TlsInterceptionMode,
}

/// 路由决策类型
#[derive(Debug, Clone)]
pub enum RoutingDecisionType {
    /// 通过代理
    Proxy { proxy_config: Option<crate::config::ProxyConfig> },
    /// 通过代理名称标识的代理（推荐使用）
    ProxyByName { proxy_name: String },
    /// 直接连接
    Direct,
    /// 拒绝连接
    Deny { reason: String },
    /// 暂停等待用户决策
    Pause { reason: String },
}

impl RuleMatchingMiddleware {
    /// 创建新的规则匹配中间件
    pub fn new() -> Self {
        Self {
            routing_engine: None,
            rules: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            config: RuleMatchingConfig::default(),
            stats: Arc::new(tokio::sync::RwLock::new(RuleMatchingStats::default())),
        }
    }
    
    /// 使用配置创建规则匹配中间件
    pub fn with_config(config: RuleMatchingConfig) -> Self {
        Self {
            routing_engine: None,
            rules: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            config,
            stats: Arc::new(tokio::sync::RwLock::new(RuleMatchingStats::default())),
        }
    }
    
    /// 设置路由引擎
    pub fn with_routing_engine(mut self, engine: Arc<RoutingEngine>) -> Self {
        self.routing_engine = Some(engine);
        self
    }
    
    /// 添加规则
    pub async fn add_rule(&self, rule: Rule) {
        let mut rules = self.rules.write().await;
        rules.push(rule);
        
        // 根据优先级模式排序
        match self.config.priority_mode {
            PriorityMode::Priority => {
                rules.sort_by_key(|r| r.priority);
            }
            PriorityMode::BestMatch => {
                // 按复杂度排序，复杂的规则优先
                rules.sort_by(|a, b| self.rule_complexity(b).cmp(&self.rule_complexity(a)));
            }
            PriorityMode::Sequential => {
                // 保持添加顺序
            }
        }
    }
    
    /// 设置规则集合
    pub async fn set_rules(&self, new_rules: Vec<Rule>) {
        let mut rules = self.rules.write().await;
        *rules = new_rules;
        
        // 应用排序
        match self.config.priority_mode {
            PriorityMode::Priority => {
                rules.sort_by_key(|r| r.priority);
            }
            PriorityMode::BestMatch => {
                rules.sort_by(|a, b| self.rule_complexity(b).cmp(&self.rule_complexity(a)));
            }
            PriorityMode::Sequential => {}
        }
    }
    
    /// 计算规则复杂度（用于最佳匹配排序）
    fn rule_complexity(&self, rule: &Rule) -> usize {
        match &rule.pattern {
            RulePattern::HostMatches(pattern) => {
                if pattern.contains('*') { 1 } else { 3 }
            }
            RulePattern::PathMatches(_) => 2,
            RulePattern::HeaderExists(_) => 2,
            RulePattern::HeaderValueMatches { .. } => 3,
            RulePattern::PortMatches(_) => 1,
            RulePattern::ProcessMatches(_) => 2,
            RulePattern::And(patterns) => patterns.len() * 2,
            RulePattern::Or(patterns) => patterns.len(),
            RulePattern::Custom(_) => 4,
        }
    }
    
    /// 执行路由决策
    #[instrument(skip(self, context))]
    async fn make_routing_decision(&self, context: &ConnectionContext) -> Result<RoutingResult, AppError> {
        debug!("Making routing decision for {}:{}", context.target_host, context.target_port);
        
        // 首先尝试使用现有的路由引擎（兼容性）
        if let Some(ref routing_engine) = self.routing_engine {
            let request_url = context.request.as_ref().map(|r| r.url.as_str());
            let process_path = context.connection.metadata.process_name.as_deref();
            
            let legacy_decision = routing_engine.should_proxy(
                &context.target_host,
                context.target_port,
                request_url,
                process_path,
                &context.connection.source_addr,
            );
            
            return Ok(self.convert_legacy_decision(legacy_decision));
        }
        
        // 使用新的规则匹配系统
        let rules = self.rules.read().await;
        
        for rule in rules.iter() {
            if !rule.enabled {
                continue;
            }
            
            if self.evaluate_rule_pattern(&rule.pattern, context).await {
                debug!("Rule '{}' matched for {}", rule.name, context.target_host);
                
                // 更新匹配统计
                self.update_rule_match_stats(&rule.id.to_string()).await;
                
                let result = self.apply_rule_action(&rule.action, context, Some(rule.id.to_string())).await?;
                return Ok(result);
            }
        }
        
        // 没有规则匹配，使用默认动作
        debug!("No rules matched, using default action: {:?}", self.config.default_action);
        self.update_default_action_stats().await;
        
        Ok(self.apply_default_action(context).await)
    }
    
    /// 评估规则模式
    fn evaluate_rule_pattern<'a>(&'a self, pattern: &'a RulePattern, context: &'a ConnectionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            match pattern {
                RulePattern::HostMatches(host_pattern) => {
                    self.wildcard_match(&context.target_host, host_pattern)
                }
                RulePattern::PathMatches(path_pattern) => {
                    if let Some(ref request) = context.request {
                        let path = self.extract_path_from_url(&request.url);
                        self.wildcard_match(&path, path_pattern)
                    } else {
                        false
                    }
                }
                RulePattern::HeaderExists(header_name) => {
                    if let Some(ref request) = context.request {
                        request.headers.contains_key(header_name)
                    } else {
                        false
                    }
                }
                RulePattern::HeaderValueMatches { header, value } => {
                    if let Some(ref request) = context.request {
                        request.headers.get(header)
                            .map(|v| self.wildcard_match(v, value))
                            .unwrap_or(false)
                    } else {
                        false
                    }
                }
                RulePattern::PortMatches(port) => {
                    context.target_port == *port
                }
                RulePattern::ProcessMatches(process_pattern) => {
                    if let Some(ref process_name) = context.connection.metadata.process_name {
                        self.wildcard_match(process_name, process_pattern)
                    } else {
                        false
                    }
                }
                RulePattern::And(patterns) => {
                    for sub_pattern in patterns {
                        if !self.evaluate_rule_pattern(sub_pattern, context).await {
                            return false;
                        }
                    }
                    true
                }
                RulePattern::Or(patterns) => {
                    for sub_pattern in patterns {
                        if self.evaluate_rule_pattern(sub_pattern, context).await {
                            return true;
                        }
                    }
                    false
                }
                RulePattern::Custom(_lua_expr) => {
                    // 自定义Lua表达式暂时返回false
                    // 在Lua集成完成后实现
                    false
                }
            }
        })
    }
    
    /// 通配符匹配
    fn wildcard_match(&self, text: &str, pattern: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        
        if pattern.contains('*') {
            // 简化的通配符匹配
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
                // 复杂的通配符匹配
                self.complex_wildcard_match(text, pattern)
            }
        } else {
            text == pattern
        }
    }
    
    /// 复杂通配符匹配
    fn complex_wildcard_match(&self, text: &str, pattern: &str) -> bool {
        // 简化实现，可以扩展为完整的glob匹配
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 1 {
            return text == pattern;
        }
        
        let mut text_pos = 0;
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            
            if i == 0 {
                // 第一部分必须在开头
                if !text[text_pos..].starts_with(part) {
                    return false;
                }
                text_pos += part.len();
            } else if i == parts.len() - 1 {
                // 最后一部分必须在结尾
                return text[text_pos..].ends_with(part);
            } else {
                // 中间部分必须存在
                if let Some(pos) = text[text_pos..].find(part) {
                    text_pos += pos + part.len();
                } else {
                    return false;
                }
            }
        }
        
        true
    }
    
    /// 从URL提取路径
    fn extract_path_from_url(&self, url: &str) -> String {
        if let Ok(parsed_url) = url::Url::parse(url) {
            parsed_url.path().to_string()
        } else {
            "/".to_string()
        }
    }
    
    /// 应用规则动作
    async fn apply_rule_action(
        &self,
        action: &RuleAction,
        context: &ConnectionContext,
        rule_id: Option<String>,
    ) -> Result<RoutingResult, AppError> {
        match action {
            RuleAction::Allow => {
                self.update_direct_decision_stats().await;
                Ok(RoutingResult {
                    decision: RoutingDecisionType::Direct,
                    matched_rule_id: rule_id,
                    rewritten_url: None,
                    applied_modifications: None,
                    tls_interception_mode: TlsInterceptionMode::None,
                })
            }
            RuleAction::Block => {
                self.update_denied_decision_stats().await;
                Ok(RoutingResult {
                    decision: RoutingDecisionType::Deny {
                        reason: "Blocked by rule".to_string()
                    },
                    matched_rule_id: rule_id,
                    rewritten_url: None,
                    applied_modifications: None,
                    tls_interception_mode: TlsInterceptionMode::None,
                })
            }
            RuleAction::Proxy(proxy_config) => {
                self.update_proxy_decision_stats().await;
                Ok(RoutingResult {
                    decision: RoutingDecisionType::Proxy {
                        proxy_config: Some(proxy_config.clone())
                    },
                    matched_rule_id: rule_id,
                    rewritten_url: None,
                    applied_modifications: None,
                    tls_interception_mode: TlsInterceptionMode::Selective,
                })
            }
            RuleAction::ProxyByName(proxy_name) => {
                self.update_proxy_decision_stats().await;
                Ok(RoutingResult {
                    decision: RoutingDecisionType::ProxyByName {
                        proxy_name: proxy_name.clone()
                    },
                    matched_rule_id: rule_id,
                    rewritten_url: None,
                    applied_modifications: None,
                    tls_interception_mode: TlsInterceptionMode::Selective,
                })
            }
            RuleAction::Direct => {
                self.update_direct_decision_stats().await;
                Ok(RoutingResult {
                    decision: RoutingDecisionType::Direct,
                    matched_rule_id: rule_id,
                    rewritten_url: None,
                    applied_modifications: None,
                    tls_interception_mode: TlsInterceptionMode::None,
                })
            }
            RuleAction::ModifyRequest(modifications) => {
                self.update_direct_decision_stats().await;
                Ok(RoutingResult {
                    decision: RoutingDecisionType::Direct,
                    matched_rule_id: rule_id,
                    rewritten_url: modifications.url.clone(),
                    applied_modifications: Some(modifications.clone()),
                    tls_interception_mode: TlsInterceptionMode::None,
                })
            }
            RuleAction::ModifyResponse(_modifications) => {
                // 响应修改在响应阶段处理
                self.update_direct_decision_stats().await;
                Ok(RoutingResult {
                    decision: RoutingDecisionType::Direct,
                    matched_rule_id: rule_id,
                    rewritten_url: None,
                    applied_modifications: None,
                    tls_interception_mode: TlsInterceptionMode::None,
                })
            }
            RuleAction::InterceptForApproval => {
                Ok(RoutingResult {
                    decision: RoutingDecisionType::Pause {
                        reason: "Intercepted for user approval".to_string()
                    },
                    matched_rule_id: rule_id,
                    rewritten_url: None,
                    applied_modifications: None,
                    tls_interception_mode: TlsInterceptionMode::Full,
                })
            }
            RuleAction::ExecuteScript(_script_name) => {
                // 脚本执行在脚本中间件中处理
                self.update_direct_decision_stats().await;
                Ok(RoutingResult {
                    decision: RoutingDecisionType::Direct,
                    matched_rule_id: rule_id,
                    rewritten_url: None,
                    applied_modifications: None,
                    tls_interception_mode: TlsInterceptionMode::None,
                })
            }
            RuleAction::Log(_level) => {
                // 日志记录，然后继续默认处理
                self.apply_default_action(context).await;
                Ok(RoutingResult {
                    decision: RoutingDecisionType::Direct,
                    matched_rule_id: rule_id,
                    rewritten_url: None,
                    applied_modifications: None,
                    tls_interception_mode: TlsInterceptionMode::None,
                })
            }
        }
    }
    
    /// 应用默认动作
    async fn apply_default_action(&self, _context: &ConnectionContext) -> RoutingResult {
        match self.config.default_action {
            DefaultAction::Allow | DefaultAction::Direct => {
                RoutingResult {
                    decision: RoutingDecisionType::Direct,
                    matched_rule_id: None,
                    rewritten_url: None,
                    applied_modifications: None,
                    tls_interception_mode: TlsInterceptionMode::None,
                }
            }
            DefaultAction::Proxy => {
                RoutingResult {
                    decision: RoutingDecisionType::Proxy { proxy_config: None },
                    matched_rule_id: None,
                    rewritten_url: None,
                    applied_modifications: None,
                    tls_interception_mode: TlsInterceptionMode::Selective,
                }
            }
            DefaultAction::Deny => {
                RoutingResult {
                    decision: RoutingDecisionType::Deny {
                        reason: "Denied by default policy".to_string()
                    },
                    matched_rule_id: None,
                    rewritten_url: None,
                    applied_modifications: None,
                    tls_interception_mode: TlsInterceptionMode::None,
                }
            }
        }
    }
    
    /// 转换旧的路由决策
    fn convert_legacy_decision(&self, legacy_decision: RoutingDecision) -> RoutingResult {
        match legacy_decision {
            RoutingDecision::Proxy { rewritten_url, tls_interception } => {
                RoutingResult {
                    decision: RoutingDecisionType::Proxy { proxy_config: None },
                    matched_rule_id: None,
                    rewritten_url,
                    applied_modifications: None,
                    tls_interception_mode: tls_interception,
                }
            }
            RoutingDecision::Direct { rewritten_url, tls_interception } => {
                RoutingResult {
                    decision: RoutingDecisionType::Direct,
                    matched_rule_id: None,
                    rewritten_url,
                    applied_modifications: None,
                    tls_interception_mode: tls_interception,
                }
            }
        }
    }
    
    /// 更新统计信息
    async fn update_connection_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.total_connections += 1;
    }
    
    async fn update_rule_match_stats(&self, rule_id: &str) {
        let mut stats = self.stats.write().await;
        stats.rule_matches += 1;
        *stats.rule_match_details.entry(rule_id.to_string()).or_insert(0) += 1;
    }
    
    async fn update_default_action_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.default_actions += 1;
    }
    
    async fn update_proxy_decision_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.proxy_decisions += 1;
    }
    
    async fn update_direct_decision_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.direct_decisions += 1;
    }
    
    async fn update_denied_decision_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.denied_decisions += 1;
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> RuleMatchingStats {
        self.stats.read().await.clone()
    }
}

impl Default for RuleMatchingMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for RuleMatchingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_action: DefaultAction::Direct,
            enable_url_rewrite: true,
            enable_process_matching: true,
            priority_mode: PriorityMode::Priority,
        }
    }
}

#[async_trait]
impl Middleware for RuleMatchingMiddleware {
    fn name(&self) -> &str {
        "rule_matching"
    }
    
    #[instrument(skip(self, context, _core))]
    async fn process(
        &self,
        context: &mut ConnectionContext,
        _core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, crate::error::TakeoverError> {
        debug!("Rule matching middleware processing connection {}", context.id);
        
        // 更新处理状态
        context.set_processing_state(ProcessingState::RuleMatching);
        
        // 更新连接统计
        self.update_connection_stats().await;
        
        // 执行路由决策
        match self.make_routing_decision(context).await {
            Ok(routing_result) => {
                debug!("Routing decision made: {:?}", routing_result.decision);
                
                // 记录匹配的规则
                if let Some(rule_id) = &routing_result.matched_rule_id {
                    context.add_matched_rule(rule_id.clone());
                }
                
                // 应用URL重写
                if let Some(rewritten_url) = &routing_result.rewritten_url {
                    if let Some(ref mut request) = context.request {
                        request.url = rewritten_url.clone();
                    }
                }
                
                // 应用请求修改
                if let Some(modifications) = routing_result.applied_modifications {
                    context.apply_modification(modifications);
                }
                
                // 存储路由决策结果
                context.custom_data.insert(
                    "routing_decision".to_string(),
                    serde_json::json!({
                        "decision": match &routing_result.decision {
                            RoutingDecisionType::Proxy { .. } => "proxy",
                            RoutingDecisionType::ProxyByName { .. } => "proxy_by_name",
                            RoutingDecisionType::Direct => "direct",
                            RoutingDecisionType::Deny { .. } => "deny",
                            RoutingDecisionType::Pause { .. } => "pause"
                        },
                        "proxy_name": match &routing_result.decision {
                            RoutingDecisionType::ProxyByName { proxy_name } => Some(proxy_name.clone()),
                            _ => None
                        },
                        "matched_rule_id": routing_result.matched_rule_id,
                        "rewritten_url": routing_result.rewritten_url,
                        "tls_interception_mode": format!("{:?}", routing_result.tls_interception_mode)
                    })
                );
                
                // 根据决策类型返回结果
                match routing_result.decision {
                    RoutingDecisionType::Proxy { .. } => {
                        debug!("Connection {} routed via proxy", context.id);
                        Ok(MiddlewareResult::Continue)
                    }
                    RoutingDecisionType::ProxyByName { ref proxy_name } => {
                        debug!("Connection {} routed via proxy: {}", context.id, proxy_name);
                        Ok(MiddlewareResult::Continue)
                    }
                    RoutingDecisionType::Direct => {
                        debug!("Connection {} routed directly", context.id);
                        Ok(MiddlewareResult::Continue)
                    }
                    RoutingDecisionType::Deny { reason } => {
                        warn!("❌ Connection {} denied: {}", context.id, reason);
                        Ok(MiddlewareResult::Reject(reason))
                    }
                    RoutingDecisionType::Pause { reason } => {
                        info!("⏸️ Connection {} paused: {}", context.id, reason);
                        Ok(MiddlewareResult::Pause(reason))
                    }
                }
            }
            Err(e) => {
                error!("❌ Routing decision failed for {}: {}", context.id, e);
                Err(crate::error::TakeoverError::Config(format!("Rule matching failed: {}", e)))
            }
        }
    }
    
    fn priority(&self) -> i32 {
        30 // 在HTTP解析之后，HTTP修改之前
    }
    
    fn enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Connection, RulePattern, RuleAction};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    
    #[tokio::test]
    async fn test_middleware_creation() {
        let middleware = RuleMatchingMiddleware::new();
        assert_eq!(middleware.name(), "rule_matching");
        assert!(middleware.enabled());
    }
    
    #[tokio::test]
    async fn test_wildcard_matching() {
        let middleware = RuleMatchingMiddleware::new();
        
        // 测试简单通配符
        assert!(middleware.wildcard_match("example.com", "*.com"));
        assert!(middleware.wildcard_match("api.example.com", "api.*"));
        assert!(middleware.wildcard_match("test", "*"));
        
        // 测试精确匹配
        assert!(middleware.wildcard_match("example.com", "example.com"));
        assert!(!middleware.wildcard_match("example.org", "example.com"));
    }
    
    #[tokio::test]
    async fn test_rule_priority() {
        let config = RuleMatchingConfig {
            priority_mode: PriorityMode::Priority,
            ..Default::default()
        };
        let middleware = RuleMatchingMiddleware::with_config(config);
        
        let rule1 = Rule::new(
            "rule1",
            "Low Priority",
            RulePattern::HostMatches("*.com".to_string()),
            RuleAction::Direct,
            10,
        );
        
        let rule2 = Rule::new(
            "rule2", 
            "High Priority",
            RulePattern::HostMatches("example.com".to_string()),
            RuleAction::Block,
            1,
        );
        
        let rule1_id = rule1.id;
        let rule2_id = rule2.id;
        
        middleware.add_rule(rule1).await;
        middleware.add_rule(rule2).await;
        
        let rules = middleware.rules.read().await;
        assert_eq!(rules[0].id, rule2_id); // 高优先级在前
        assert_eq!(rules[1].id, rule1_id);
    }
    
    #[tokio::test]
    async fn test_pattern_evaluation() {
        let middleware = RuleMatchingMiddleware::new();
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        
        let context = ConnectionContext::new(connection, "example.com".to_string(), 80);
        
        // 测试主机匹配
        assert!(middleware.evaluate_rule_pattern(
            &RulePattern::HostMatches("example.com".to_string()),
            &context
        ).await);
        
        assert!(middleware.evaluate_rule_pattern(
            &RulePattern::HostMatches("*.com".to_string()),
            &context
        ).await);
        
        // 测试端口匹配
        assert!(middleware.evaluate_rule_pattern(
            &RulePattern::PortMatches(80),
            &context
        ).await);
        
        assert!(!middleware.evaluate_rule_pattern(
            &RulePattern::PortMatches(443),
            &context
        ).await);
    }
    
    #[tokio::test]
    async fn test_stats_tracking() {
        let middleware = RuleMatchingMiddleware::new();
        
        let initial_stats = middleware.get_stats().await;
        assert_eq!(initial_stats.total_connections, 0);
        
        middleware.update_connection_stats().await;
        middleware.update_proxy_decision_stats().await;
        middleware.update_rule_match_stats("test_rule").await;
        
        let updated_stats = middleware.get_stats().await;
        assert_eq!(updated_stats.total_connections, 1);
        assert_eq!(updated_stats.proxy_decisions, 1);
        assert_eq!(updated_stats.rule_matches, 1);
        assert_eq!(updated_stats.rule_match_details.get("test_rule"), Some(&1));
    }
}
