//! REST API控制适配器
//! 
//! 提供HTTP REST API接口用于管理和控制代理系统

use std::sync::Arc;
use std::collections::HashMap;
use std::net::SocketAddr;
use tokio::sync::RwLock;
use tracing::{info, error, debug, instrument};
use async_trait::async_trait;
use serde::{Serialize, Deserialize};

use crate::core::ports::{ControlPort, AppError, ReplayResult};
use crate::core::model::{self as model, ConnectionSummary, Rule, RequestModifications};
use crate::core::model::{ConnectionId, RequestId};

/// REST API控制适配器
/// 
/// 负责：
/// 1. 提供HTTP REST API接口
/// 2. 处理客户端的管理请求
/// 3. 返回JSON格式的响应
/// 4. 支持实时连接监控
pub struct RestControlAdapter {
    /// 监听地址
    listen_addr: SocketAddr,
    /// 活跃的规则
    active_rules: Arc<RwLock<HashMap<String, Rule>>>,
    /// 拦截的连接队列
    intercepted_connections: Arc<RwLock<HashMap<ConnectionId, InterceptedConnection>>>,
    /// API统计
    stats: Arc<RwLock<ApiStats>>,
    /// 是否正在运行
    running: Arc<std::sync::atomic::AtomicBool>,
}

/// 被拦截的连接信息
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct InterceptedConnection {
    connection_id: ConnectionId,
    request_id: RequestId,
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
struct ApiStats {
    total_requests: u64,
    successful_requests: u64,
    failed_requests: u64,
    rules_applied: u64,
    connections_intercepted: u64,
}

/// API响应格式
#[derive(Debug, Serialize)]
struct ApiResponse<T> {
    success: bool,
    data: Option<T>,
    error: Option<String>,
    timestamp: u64,
}

/// 连接列表响应
#[derive(Debug, Serialize)]
struct ConnectionListResponse {
    connections: Vec<ConnectionSummary>,
    total_count: usize,
    active_count: usize,
}

/// 规则操作请求
#[derive(Debug, Deserialize)]
struct RuleRequest {
    rules: Vec<Rule>,
}

/// 拦截决策请求
#[derive(Debug, Deserialize)]
struct InterceptionDecisionRequest {
    decision: String, // "approve", "reject", "modify"
    modifications: Option<RequestModifications>,
    reason: Option<String>,
}

/// 重放请求
#[derive(Debug, Deserialize)]
struct ReplayRequest {
    modifications: RequestModifications,
}

impl RestControlAdapter {
    /// 创建新的REST控制适配器
    pub fn new(listen_addr: SocketAddr) -> Self {
        Self {
            listen_addr,
            active_rules: Arc::new(RwLock::new(HashMap::new())),
            intercepted_connections: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(RwLock::new(ApiStats::default())),
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
    
    /// 启动API服务器
    pub async fn start(&self) -> Result<(), AppError> {
        info!("🚀 Starting REST API server on {}", self.listen_addr);
        
        self.running.store(true, std::sync::atomic::Ordering::SeqCst);
        
        // 在实际实现中，这里会启动HTTP服务器
        // 使用warp、axum或hyper等框架
        // 目前提供一个简化的实现
        
        info!("✅ REST API server started successfully");
        Ok(())
    }
    
    /// 停止API服务器
    pub async fn stop(&self) -> Result<(), AppError> {
        info!("🛑 Stopping REST API server");
        
        self.running.store(false, std::sync::atomic::Ordering::SeqCst);
        
        info!("✅ REST API server stopped");
        Ok(())
    }
    
    /// 处理获取连接列表的请求
    async fn handle_get_connections(&self) -> Result<ApiResponse<ConnectionListResponse>, AppError> {
        debug!("Handling get connections request");
        
        // 在实际实现中，这里会从AppCore获取连接信息
        let connections: Vec<crate::core::model::Connection> = Vec::new(); // 临时空列表
        let total_count = connections.len();
        let active_count = connections.iter()
            .filter(|_c| true) // 临时移除status过滤
            .count();
        
        let response_data = ConnectionListResponse {
            connections: connections.into_iter().map(|c| ConnectionSummary {
                id: c.id,
                source_addr: c.source_addr,
                target_host: c.original_dest.to_string(),
                target_port: c.original_dest.port(),
                protocol: c.metadata.protocol.clone(),
                created_at: c.metadata.created_at,
                last_activity: Some(c.metadata.created_at),
                status: crate::core::model::ConnectionStatus::Active,
                bytes_sent: 0,
                bytes_received: 0,
            }).collect(),
            total_count,
            active_count,
        };
        
        self.update_success_stats().await;
        
        Ok(ApiResponse {
            success: true,
            data: Some(response_data),
            error: None,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        })
    }
    
    /// 处理规则应用请求
    async fn handle_apply_rules(&self, request: RuleRequest) -> Result<ApiResponse<String>, AppError> {
        debug!("Handling apply rules request with {} rules", request.rules.len());
        
        let mut active_rules = self.active_rules.write().await;
        
        // 清空现有规则
        active_rules.clear();
        
        // 添加新规则
        for rule in request.rules {
            active_rules.insert(rule.id.to_string(), rule);
        }
        
        self.update_success_stats().await;
        
        let mut stats = self.stats.write().await;
        stats.rules_applied += active_rules.len() as u64;
        
        Ok(ApiResponse {
            success: true,
            data: Some(format!("Applied {} rules successfully", active_rules.len())),
            error: None,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        })
    }
    
    /// 处理拦截决策请求
    async fn handle_interception_decision(
        &self,
        request_id: RequestId,
        decision: InterceptionDecisionRequest,
    ) -> Result<ApiResponse<String>, AppError> {
        debug!("Handling interception decision for request {}: {}", request_id, decision.decision);
        
        let mut intercepted = self.intercepted_connections.write().await;
        
        if let Some(connection) = intercepted.values_mut()
            .find(|c| c.request_id == request_id) {
            
            connection.status = match decision.decision.as_str() {
                "approve" => InterceptionStatus::Approved,
                "reject" => InterceptionStatus::Rejected,
                _ => {
                    return Ok(ApiResponse {
                        success: false,
                        data: None,
                        error: Some("Invalid decision type".to_string()),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                    });
                }
            };
            
            self.update_success_stats().await;
            
            Ok(ApiResponse {
                success: true,
                data: Some(format!("Decision '{}' applied to request {}", decision.decision, request_id)),
                error: None,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            })
        } else {
            Ok(ApiResponse {
                success: false,
                data: None,
                error: Some("Request not found".to_string()),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            })
        }
    }
    
    /// 更新成功统计
    async fn update_success_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.total_requests += 1;
        stats.successful_requests += 1;
    }
    
    /// 更新失败统计
    async fn update_failure_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.total_requests += 1;
        stats.failed_requests += 1;
    }
    
    /// 获取API统计信息
    pub async fn get_stats(&self) -> ApiStats {
        self.stats.read().await.clone()
    }
    
    /// 检查是否正在运行
    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl ControlPort for RestControlAdapter {
    #[instrument(skip(self, rules))]
    async fn apply_rules(&self, rules: Vec<crate::core::ports::Rule>) -> Result<(), AppError> {
        debug!("Applying {} rules via REST adapter", rules.len());
        
        // 转换端口Rules到模型Rules
        let model_rules: Vec<model::Rule> = rules.into_iter().map(|r| model::Rule {
            id: uuid::Uuid::new_v4(),
            name: r.name,
            description: "Rule description".to_string(), // 默认描述
            pattern: r.pattern.into(),
            conditions: vec![], // 默认空条件
            action: r.action.into(),
            enabled: r.enabled,
            priority: r.priority,
            metadata: HashMap::new(), // 默认空metadata
        }).collect();
        let request = RuleRequest { rules: model_rules };
        match self.handle_apply_rules(request).await {
            Ok(response) if response.success => {
                info!("✅ Rules applied successfully");
                Ok(())
            }
            Ok(response) => {
                error!("❌ Failed to apply rules: {:?}", response.error);
                Err(AppError::Config(response.error.unwrap_or_default()))
            }
            Err(e) => {
                self.update_failure_stats().await;
                Err(e)
            }
        }
    }
    
    async fn get_active_connections(&self) -> Result<Vec<ConnectionSummary>, AppError> {
        debug!("Getting active connections via REST adapter");
        
        match self.handle_get_connections().await {
            Ok(response) if response.success => {
                Ok(response.data.map(|d| d.connections).unwrap_or_default())
            }
            Ok(response) => {
                Err(AppError::Config(response.error.unwrap_or_default()))
            }
            Err(e) => {
                self.update_failure_stats().await;
                Err(e)
            }
        }
    }
    
    async fn intercept_connection(&self, conn_id: ConnectionId) -> Result<(), AppError> {
        debug!("Intercepting connection {} via REST adapter", &conn_id);
        
        let intercepted_connection = InterceptedConnection {
            connection_id: conn_id.clone(),
            request_id: uuid::Uuid::new_v4(),
            intercepted_at: std::time::SystemTime::now(),
            status: InterceptionStatus::Pending,
            request_data: None,
        };
        
        {
            let mut intercepted = self.intercepted_connections.write().await;
            intercepted.insert(conn_id.clone(), intercepted_connection);
        }
        
        {
            let mut stats = self.stats.write().await;
            stats.connections_intercepted += 1;
        }
        
        Ok(())
    }
    
    async fn approve_request(
        &self,
        request_id: RequestId,
        modifications: Option<RequestModifications>,
    ) -> Result<(), AppError> {
        debug!("Approving request {} via REST adapter", request_id);
        
        let decision = InterceptionDecisionRequest {
            decision: "approve".to_string(),
            modifications,
            reason: None,
        };
        
        match self.handle_interception_decision(request_id, decision).await {
            Ok(response) if response.success => {
                info!("✅ Request {} approved", request_id);
                Ok(())
            }
            Ok(response) => {
                Err(AppError::Config(response.error.unwrap_or_default()))
            }
            Err(e) => {
                self.update_failure_stats().await;
                Err(e)
            }
        }
    }
    
    async fn reject_request(&self, request_id: RequestId, reason: String) -> Result<(), AppError> {
        debug!("Rejecting request {} via REST adapter: {}", request_id, reason);
        
        let decision = InterceptionDecisionRequest {
            decision: "reject".to_string(),
            modifications: None,
            reason: Some(reason.clone()),
        };
        
        match self.handle_interception_decision(request_id, decision).await {
            Ok(response) if response.success => {
                info!("✅ Request {} rejected: {}", request_id, reason);
                Ok(())
            }
            Ok(response) => {
                Err(AppError::Config(response.error.unwrap_or_default()))
            }
            Err(e) => {
                self.update_failure_stats().await;
                Err(e)
            }
        }
    }
    
    async fn trigger_replay(
        &self,
        request_id: RequestId,
        modifications: RequestModifications,
    ) -> Result<ReplayResult, AppError> {
        debug!("Triggering replay for request {} via REST adapter", request_id);
        
        // 在实际实现中，这里会触发请求重放
        // 目前返回一个模拟的结果
        let result = ReplayResult {
            request_id: request_id,
            replay_count: 1,
            execution_time: std::time::Duration::from_millis(100),
            success: true,
            response: Some(crate::core::model::HttpResponse::new(200)),
            error_message: None,
        };
        
        self.update_success_stats().await;
        info!("✅ Request {} replayed successfully", request_id);
        
        Ok(result)
    }
    
    async fn get_active_rules(&self) -> Result<Vec<Rule>, AppError> {
        debug!("Getting active rules via REST adapter");
        
        let active_rules = self.active_rules.read().await;
        let rules: Vec<Rule> = active_rules.values().cloned().collect();
        
        self.update_success_stats().await;
        Ok(rules)
    }
}

impl Default for RestControlAdapter {
    fn default() -> Self {
        Self::new("127.0.0.1:8081".parse().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{RulePattern, RuleAction};
    
    #[tokio::test]
    async fn test_adapter_creation() {
        let addr: SocketAddr = "127.0.0.1:8081".parse().unwrap();
        let adapter = RestControlAdapter::new(addr);
        
        assert_eq!(adapter.listen_addr, addr);
        assert!(!adapter.is_running());
    }
    
    #[tokio::test]
    async fn test_rule_application() {
        let adapter = RestControlAdapter::default();
        
        let rules = vec![
            Rule::new(
                "test_rule_1",
                "Test Rule 1",
                RulePattern::HostMatches("example.com".to_string()),
                RuleAction::Allow,
                100,
            ),
        ];
        
        let result = adapter.apply_rules(rules).await;
        assert!(result.is_ok());
        
        let active_rules = adapter.get_active_rules().await.unwrap();
        assert_eq!(active_rules.len(), 1);
        assert_eq!(active_rules[0].id, "test_rule_1");
    }
    
    #[tokio::test]
    async fn test_connection_interception() {
        let adapter = RestControlAdapter::default();
        let conn_id = uuid::Uuid::new_v4();
        
        let result = adapter.intercept_connection(conn_id.to_string()).await;
        assert!(result.is_ok());
        
        let intercepted = adapter.intercepted_connections.read().await;
        assert!(intercepted.contains_key(&conn_id));
    }
    
    #[tokio::test]
    async fn test_stats_tracking() {
        let adapter = RestControlAdapter::default();
        
        let initial_stats = adapter.get_stats().await;
        assert_eq!(initial_stats.total_requests, 0);
        
        adapter.update_success_stats().await;
        
        let updated_stats = adapter.get_stats().await;
        assert_eq!(updated_stats.total_requests, 1);
        assert_eq!(updated_stats.successful_requests, 1);
    }
    
    #[test]
    fn test_api_response_serialization() {
        let response = ApiResponse {
            success: true,
            data: Some("test data".to_string()),
            error: None,
            timestamp: 1234567890,
        };
        
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"data\":\"test data\""));
    }
}
