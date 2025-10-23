//! 控制适配器 - Rust API 层
//! 
//! # ControlService
//! - 实现 `ControlPort` trait
//! - 注册到 `AppCore`，供所有组件访问
//! - 管理规则、连接、拦截决策等核心功能

use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::instrument;
use async_trait::async_trait;

use crate::core::ports::{ControlPort, AppError, ReplayResult};
use crate::core::model::{self as model, ConnectionSummary, Rule, RequestModifications};
use crate::core::model::{ConnectionId, RequestId};

/// 控制服务（Rust API）
/// 
/// 实现 `ControlPort` trait，注册到 `AppCore` 供所有组件访问。
/// 
/// # 使用方式
/// ```rust
/// let app_core = AppCore::global();
/// let control = app_core.state::<ControlService>().await.unwrap();
/// control.apply_rules(rules).await?;
/// ```
pub struct ControlService {
    active_rules: Arc<RwLock<HashMap<String, Rule>>>,
    intercepted_connections: Arc<RwLock<HashMap<ConnectionId, InterceptedConnection>>>,
    stats: Arc<RwLock<ApiStats>>,
}

/// 被拦截的连接信息
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct InterceptedConnection {
    connection_id: ConnectionId,
    request_id: String,
    intercepted_at: std::time::SystemTime,
    status: InterceptionStatus,
    request_data: Option<Vec<u8>>,
}

/// 拦截状态
#[allow(dead_code)]
#[derive(Debug, Clone)]
enum InterceptionStatus {
    Pending,
    Approved,
    Rejected,
    Timeout,
}

/// API统计信息
#[derive(Debug, Default, Clone)]
pub struct ApiStats {
    #[allow(dead_code)]
    total_requests: u64,
    #[allow(dead_code)]
    successful_requests: u64,
    #[allow(dead_code)]
    failed_requests: u64,
    rules_applied: u64,
    connections_intercepted: u64,
}

impl ControlService {
    pub fn new() -> Self {
        Self {
            active_rules: Arc::new(RwLock::new(HashMap::new())),
            intercepted_connections: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(RwLock::new(ApiStats::default())),
        }
    }
    
    pub async fn get_stats(&self) -> ApiStats {
        self.stats.read().await.clone()
    }
}

#[async_trait]
impl ControlPort for ControlService {
    #[instrument(skip(self, rules))]
    async fn apply_rules(&self, rules: Vec<crate::core::ports::Rule>) -> Result<(), AppError> {
        let mut active_rules = self.active_rules.write().await;
        
        // Upsert语义：保留现有规则，只更新/添加传入的规则
        for rule in &rules {
            let model_rule = model::Rule {
                id: rule.id.clone(),
                name: rule.name.clone(),
                description: rule.description.clone(),
                pattern: rule.pattern.clone().into(),
                conditions: rule.conditions.iter().map(|c| c.clone().into()).collect(),
                action: rule.action.clone().into(),
                enabled: rule.enabled,
                priority: rule.priority,
                metadata: rule.metadata.clone(),
            };
            active_rules.insert(rule.id.clone(), model_rule);
        }
        
        // 同步到 RuleService
        use crate::core::middleware::rule_matching::RuleService;
        let app_core = crate::core::AppCore::global();
        if let Some(rule_service) = app_core.state::<RuleService>().await {
            let model_rules: Vec<model::Rule> = rules.into_iter().map(|r| model::Rule {
                id: r.id, name: r.name, description: r.description,
                pattern: r.pattern.into(), conditions: r.conditions.into_iter().map(|c| c.into()).collect(),
                action: r.action.into(), enabled: r.enabled, priority: r.priority, metadata: r.metadata,
            }).collect();
            rule_service.apply_rules(model_rules).await
                .map_err(|e| AppError::Config(format!("Failed to sync rules: {}", e)))?;
        }
        
        let mut stats = self.stats.write().await;
        stats.rules_applied += active_rules.len() as u64;
        Ok(())
    }
    
    async fn get_active_connections(&self) -> Result<Vec<ConnectionSummary>, AppError> {
        Ok(Vec::new())
    }
    
    async fn intercept_connection(&self, conn_id: ConnectionId) -> Result<(), AppError> {
        let mut intercepted = self.intercepted_connections.write().await;
        intercepted.insert(conn_id.clone(), InterceptedConnection {
            connection_id: conn_id,
            request_id: uuid::Uuid::new_v4().to_string(),
            intercepted_at: std::time::SystemTime::now(),
            status: InterceptionStatus::Pending,
            request_data: None,
        });
        
        let mut stats = self.stats.write().await;
        stats.connections_intercepted += 1;
        Ok(())
    }
    
    async fn approve_request(&self, request_id: RequestId, _modifications: Option<RequestModifications>) -> Result<(), AppError> {
        let mut intercepted = self.intercepted_connections.write().await;
        if let Some(conn) = intercepted.values_mut().find(|c| c.request_id == request_id) {
            conn.status = InterceptionStatus::Approved;
            Ok(())
        } else {
            Err(AppError::Config("Request not found".to_string()))
        }
    }
    
    async fn reject_request(&self, request_id: RequestId, _reason: String) -> Result<(), AppError> {
        let mut intercepted = self.intercepted_connections.write().await;
        if let Some(conn) = intercepted.values_mut().find(|c| c.request_id == request_id) {
            conn.status = InterceptionStatus::Rejected;
            Ok(())
        } else {
            Err(AppError::Config("Request not found".to_string()))
        }
    }
    
    async fn trigger_replay(&self, request_id: RequestId, _modifications: RequestModifications) -> Result<ReplayResult, AppError> {
        Ok(ReplayResult {
            request_id: request_id.to_string(),
            replay_count: 1,
            execution_time: std::time::Duration::from_millis(100),
            success: true,
            response: Some(crate::core::model::HttpResponse::new(200)),
            error_message: None,
        })
    }
    
    async fn get_active_rules(&self) -> Result<Vec<Rule>, AppError> {
        let active_rules = self.active_rules.read().await;
        Ok(active_rules.values().cloned().collect())
    }
    
    async fn get_rules_by_name(&self, name_pattern: &str) -> Result<Vec<Rule>, AppError> {
        let active_rules = self.active_rules.read().await;
        let pattern_lower = name_pattern.to_lowercase();
        
        // 模糊匹配：规则名称包含查询模式（不区分大小写）
        let matched_rules: Vec<Rule> = active_rules
            .values()
            .filter(|r| r.name.to_lowercase().contains(&pattern_lower))
            .cloned()
            .collect();
        
        Ok(matched_rules)
    }
    
    async fn get_rule_by_id(&self, id: &str) -> Result<Option<Rule>, AppError> {
        let active_rules = self.active_rules.read().await;
        Ok(active_rules.get(id).cloned())
    }
    
    #[instrument(skip(self, rule))]
    async fn upsert_rule(&self, rule: Rule) -> Result<Rule, AppError> {
        let mut active_rules = self.active_rules.write().await;
        let rule_id = rule.id.clone();
        
        active_rules.insert(rule_id.clone(), rule.clone());
        
        // 同步到 RuleService
        use crate::core::middleware::rule_matching::RuleService;
        let app_core = crate::core::AppCore::global();
        if let Some(rule_service) = app_core.state::<RuleService>().await {
            rule_service.apply_rules(vec![rule.clone()]).await
                .map_err(|e| AppError::Config(format!("Failed to sync rule: {}", e)))?;
        }
        
        Ok(rule)
    }
    
    #[instrument(skip(self))]
    async fn delete_rule(&self, id: &str) -> Result<(), AppError> {
        let mut active_rules = self.active_rules.write().await;
        
        if active_rules.remove(id).is_none() {
            return Err(AppError::Config(format!("Rule {} not found", id)));
        }
        
        // 同步到 RuleService
        use crate::core::middleware::rule_matching::RuleService;
        let app_core = crate::core::AppCore::global();
        if let Some(rule_service) = app_core.state::<RuleService>().await {
            // 直接调用RuleService的delete_rule
            rule_service.delete_rule(id).await
                .map_err(|e| AppError::Config(format!("Failed to delete rule from RuleService: {}", e)))?;
        }
        
        Ok(())
    }
}

impl Default for ControlService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ports;
    use std::collections::HashMap;
    
    #[tokio::test]
    async fn test_control_service_creation() {
        let service = ControlService::new();
        let stats = service.get_stats().await;
        assert_eq!(stats.total_requests, 0);
    }
    
    #[tokio::test]
    async fn test_rule_application() {
        let service = ControlService::default();
        
        let rules = vec![
            ports::Rule {
                id: "test-rule-1".to_string(),
                name: "test_rule_1".to_string(),
                description: "Test Rule 1".to_string(),
                pattern: ports::RulePattern::HostMatches("example.com".to_string()),
                conditions: vec![],
                action: ports::RuleAction::Allow,
                enabled: true,
                priority: 100,
                metadata: HashMap::new(),
            },
        ];
        
        let result = service.apply_rules(rules).await;
        assert!(result.is_ok());
        
        let active_rules = service.get_active_rules().await.unwrap();
        assert_eq!(active_rules.len(), 1);
        assert_eq!(active_rules[0].name, "test_rule_1");
    }
    
    #[tokio::test]
    async fn test_connection_interception() {
        let service = ControlService::default();
        let conn_id = uuid::Uuid::new_v4().to_string();
        
        let result = service.intercept_connection(conn_id.clone()).await;
        assert!(result.is_ok());
        
        let intercepted = service.intercepted_connections.read().await;
        assert!(intercepted.contains_key(&conn_id));
    }
    

}
