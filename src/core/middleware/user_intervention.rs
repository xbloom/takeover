//! 用户干预中间件
//! 
//! 提供交互式的用户控制功能：
//! - 暂停请求处理等待用户决策
//! - 手动修改请求和响应内容
//! - 实时调试和检查
//! - 审批流程和权限控制
//! - 交互式请求重放
//! - 条件性暂停和断点

use std::sync::Arc;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, SystemTime, Instant};
use tokio::sync::{RwLock, oneshot, Notify};
use tracing::{info, warn, error, debug, instrument};
use async_trait::async_trait;
use serde::{Serialize, Deserialize};
use uuid::Uuid;

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{ConnectionContext, ProcessingState, HttpRequest, HttpResponse};
use crate::core::ports::{ConnectionId, AppError};

/// 用户干预中间件
/// 
/// 允许用户手动控制请求处理流程
pub struct UserInterventionMiddleware {
    /// 干预配置
    config: UserInterventionConfig,
    /// 待处理的干预请求
    pending_interventions: Arc<RwLock<HashMap<InterventionId, PendingIntervention>>>,
    /// 活跃的用户会话
    user_sessions: Arc<RwLock<HashMap<String, UserSession>>>,
    /// 干预历史
    intervention_history: Arc<RwLock<VecDeque<InterventionRecord>>>,
    /// 事件通知器
    event_notifier: Arc<Notify>,
    /// 统计信息
    statistics: Arc<RwLock<InterventionStatistics>>,
    /// 断点管理
    breakpoints: Arc<RwLock<Vec<Breakpoint>>>,
    /// 自动批准规则
    auto_approval_rules: Arc<RwLock<Vec<AutoApprovalRule>>>,
}

/// 用户干预配置
#[derive(Debug, Clone)]
pub struct UserInterventionConfig {
    /// 是否启用用户干预
    pub enabled: bool,
    /// 默认超时时间
    pub default_timeout: Duration,
    /// 最大并发干预数
    pub max_concurrent_interventions: usize,
    /// 干预历史保留数量
    pub history_retention_count: usize,
    /// 是否需要用户认证
    pub require_authentication: bool,
    /// 允许的用户角色
    pub allowed_roles: Vec<String>,
    /// 自动暂停条件
    pub auto_pause_conditions: Vec<PauseCondition>,
    /// 是否启用断点功能
    pub enable_breakpoints: bool,
    /// 是否记录详细历史
    pub detailed_history: bool,
}

/// 干预ID类型
type InterventionId = Uuid;

/// 待处理的干预
#[derive(Debug)]
pub struct PendingIntervention {
    /// 干预ID
    pub id: InterventionId,
    /// 连接ID
    pub connection_id: ConnectionId,
    /// 干预类型
    pub intervention_type: InterventionType,
    /// 原始请求
    pub original_request: Option<HttpRequest>,
    /// 原始响应
    pub original_response: Option<HttpResponse>,
    /// 修改后的请求
    pub modified_request: Option<HttpRequest>,
    /// 修改后的响应
    pub modified_response: Option<HttpResponse>,
    /// 创建时间
    pub created_at: SystemTime,
    /// 超时时间
    pub timeout_at: SystemTime,
    /// 用户消息
    pub message: String,
    /// 可选的操作
    pub available_actions: Vec<InterventionAction>,
    /// 上下文数据
    pub context_data: HashMap<String, serde_json::Value>,
    /// 响应通道
    pub response_channel: Option<oneshot::Sender<InterventionDecision>>,
    /// 优先级
    pub priority: InterventionPriority,
    /// 自动处理建议
    pub auto_suggestion: Option<InterventionDecision>,
}

/// 用户会话
#[derive(Debug, Clone)]
pub struct UserSession {
    /// 会话ID
    pub session_id: String,
    /// 用户ID
    pub user_id: String,
    /// 用户角色
    pub roles: Vec<String>,
    /// 认证状态
    pub authenticated: bool,
    /// 创建时间
    pub created_at: SystemTime,
    /// 最后活动时间
    pub last_activity: SystemTime,
    /// 活跃的干预
    pub active_interventions: Vec<InterventionId>,
    /// 会话权限
    pub permissions: Vec<String>,
    /// 用户偏好
    pub preferences: UserPreferences,
}

/// 用户偏好
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserPreferences {
    /// 自动批准某些类型的请求
    pub auto_approve_get_requests: bool,
    /// 自动拒绝某些域名
    pub auto_reject_domains: Vec<String>,
    /// 默认超时时间
    pub default_timeout_seconds: Option<u64>,
    /// 是否显示详细信息
    pub show_detailed_info: bool,
    /// 是否启用声音通知
    pub enable_sound_notifications: bool,
    /// 自定义断点
    pub custom_breakpoints: Vec<String>,
}

/// 干预类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum InterventionType {
    /// 请求暂停
    RequestPause,
    /// 响应暂停
    ResponsePause,
    /// 请求修改
    RequestModification,
    /// 响应修改
    ResponseModification,
    /// 安全审查
    SecurityReview,
    /// 规则确认
    RuleConfirmation,
    /// 断点触发
    BreakpointTriggered,
    /// 错误处理
    ErrorHandling,
    /// 自定义干预
    Custom(String),
}

/// 干预动作
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterventionAction {
    /// 动作ID
    pub id: String,
    /// 动作名称
    pub name: String,
    /// 动作描述
    pub description: String,
    /// 是否为默认动作
    pub is_default: bool,
    /// 动作类型
    pub action_type: ActionType,
    /// 是否需要额外数据
    pub requires_data: bool,
}

/// 动作类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionType {
    /// 继续处理
    Continue,
    /// 暂停处理
    Pause,
    /// 拒绝请求
    Reject,
    /// 重定向
    Redirect,
    /// 修改请求
    ModifyRequest,
    /// 修改响应
    ModifyResponse,
    /// 重放请求
    ReplayRequest,
    /// 自定义处理
    Custom(String),
}

/// 干预决策
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterventionDecision {
    /// 决策ID
    pub decision_id: Uuid,
    /// 选择的动作
    pub action: ActionType,
    /// 修改后的请求（如果适用）
    pub modified_request: Option<HttpRequest>,
    /// 修改后的响应（如果适用）
    pub modified_response: Option<HttpResponse>,
    /// 决策原因
    pub reason: Option<String>,
    /// 附加数据
    pub additional_data: HashMap<String, serde_json::Value>,
    /// 决策者
    pub decided_by: Option<String>,
    /// 决策时间
    pub decided_at: SystemTime,
    /// 是否永久应用此决策
    pub make_permanent: bool,
}

/// 干预优先级
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum InterventionPriority {
    /// 低优先级
    Low = 1,
    /// 普通优先级
    Normal = 2,
    /// 高优先级
    High = 3,
    /// 紧急优先级
    Critical = 4,
}

/// 暂停条件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PauseCondition {
    /// 条件名称
    pub name: String,
    /// 条件表达式
    pub condition: ConditionExpression,
    /// 是否启用
    pub enabled: bool,
    /// 暂停消息
    pub message: String,
    /// 优先级
    pub priority: InterventionPriority,
}

/// 条件表达式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConditionExpression {
    /// 主机匹配
    HostMatches(String),
    /// 路径匹配
    PathMatches(String),
    /// 方法匹配
    MethodEquals(String),
    /// 头部存在
    HeaderExists(String),
    /// 头部值匹配
    HeaderValueMatches(String, String),
    /// 状态码匹配
    StatusCodeEquals(u16),
    /// 内容包含
    ContentContains(String),
    /// 复合条件
    And(Vec<ConditionExpression>),
    /// 或条件
    Or(Vec<ConditionExpression>),
    /// 非条件
    Not(Box<ConditionExpression>),
    /// 自定义Lua表达式
    LuaExpression(String),
}

/// 断点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Breakpoint {
    /// 断点ID
    pub id: Uuid,
    /// 断点名称
    pub name: String,
    /// 断点条件
    pub condition: ConditionExpression,
    /// 是否启用
    pub enabled: bool,
    /// 创建时间
    pub created_at: SystemTime,
    /// 创建者
    pub created_by: String,
    /// 触发次数
    pub hit_count: u64,
    /// 描述
    pub description: Option<String>,
}

/// 自动批准规则
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoApprovalRule {
    /// 规则ID
    pub id: Uuid,
    /// 规则名称
    pub name: String,
    /// 匹配条件
    pub condition: ConditionExpression,
    /// 自动动作
    pub action: ActionType,
    /// 是否启用
    pub enabled: bool,
    /// 创建时间
    pub created_at: SystemTime,
    /// 使用次数
    pub usage_count: u64,
}

/// 干预记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterventionRecord {
    /// 记录ID
    pub id: Uuid,
    /// 干预ID
    pub intervention_id: InterventionId,
    /// 连接ID
    pub connection_id: ConnectionId,
    /// 干预类型
    pub intervention_type: InterventionType,
    /// 开始时间
    pub started_at: SystemTime,
    /// 完成时间
    pub completed_at: Option<SystemTime>,
    /// 处理时长
    pub duration: Option<Duration>,
    /// 决策
    pub decision: Option<InterventionDecision>,
    /// 是否超时
    pub timed_out: bool,
    /// 处理者
    pub handled_by: Option<String>,
    /// 自动处理
    pub auto_handled: bool,
}

/// 干预统计
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InterventionStatistics {
    /// 总干预数
    pub total_interventions: u64,
    /// 成功处理数
    pub successful_interventions: u64,
    /// 超时数
    pub timeout_interventions: u64,
    /// 自动处理数
    pub auto_handled_interventions: u64,
    /// 手动处理数
    pub manual_handled_interventions: u64,
    /// 平均处理时间
    pub avg_handling_time: Option<Duration>,
    /// 按类型统计
    pub by_type: HashMap<String, u64>,
    /// 按用户统计
    pub by_user: HashMap<String, u64>,
    /// 断点触发统计
    pub breakpoint_hits: HashMap<String, u64>,
}

impl UserInterventionMiddleware {
    /// 创建新的用户干预中间件
    pub fn new(config: UserInterventionConfig) -> Self {
        Self {
            config,
            pending_interventions: Arc::new(RwLock::new(HashMap::new())),
            user_sessions: Arc::new(RwLock::new(HashMap::new())),
            intervention_history: Arc::new(RwLock::new(VecDeque::new())),
            event_notifier: Arc::new(Notify::new()),
            statistics: Arc::new(RwLock::new(InterventionStatistics::default())),
            breakpoints: Arc::new(RwLock::new(Vec::new())),
            auto_approval_rules: Arc::new(RwLock::new(Vec::new())),
        }
    }
    
    /// 启动用户会话
    #[instrument(skip(self))]
    pub async fn start_user_session(
        &self,
        user_id: String,
        roles: Vec<String>,
        permissions: Vec<String>,
    ) -> Result<String, AppError> {
        let session_id = Uuid::new_v4().to_string();
        
        info!("🔐 Starting user session for user: {}", user_id);
        
        let session = UserSession {
            session_id: session_id.clone(),
            user_id: user_id.clone(),
            roles,
            authenticated: true,
            created_at: SystemTime::now(),
            last_activity: SystemTime::now(),
            active_interventions: Vec::new(),
            permissions,
            preferences: UserPreferences::default(),
        };
        
        let mut user_sessions = self.user_sessions.write().await;
        user_sessions.insert(session_id.clone(), session);
        
        info!("✅ User session started: {}", session_id);
        Ok(session_id)
    }
    
    /// 创建干预请求
    #[instrument(skip(self, context))]
    pub async fn create_intervention(
        &self,
        intervention_type: InterventionType,
        context: &ConnectionContext,
        message: String,
        priority: InterventionPriority,
        timeout: Option<Duration>,
    ) -> Result<InterventionId, AppError> {
        if !self.config.enabled {
            return Err(AppError::Config("User intervention is disabled".to_string()));
        }
        
        let intervention_id = Uuid::new_v4();
        let timeout_duration = timeout.unwrap_or(self.config.default_timeout);
        
        debug!("Creating intervention: {:?} for connection {}", intervention_type, context.id);
        
        // 检查自动批准规则
        if let Some(auto_decision) = self.check_auto_approval_rules(context, &intervention_type).await {
            debug!("Auto-approval rule matched, applying decision automatically");
            return self.apply_auto_decision(intervention_id, auto_decision).await;
        }
        
        // 创建可用的操作
        let available_actions = self.create_available_actions(&intervention_type);
        
        // 创建响应通道
        let (tx, rx) = oneshot::channel();
        
        let intervention = PendingIntervention {
            id: intervention_id,
            connection_id: context.id.clone(),
            intervention_type: intervention_type.clone(),
            original_request: context.request.clone(),
            original_response: context.response.clone(),
            modified_request: None,
            modified_response: None,
            created_at: SystemTime::now(),
            timeout_at: SystemTime::now() + timeout_duration,
            message,
            available_actions,
            context_data: context.custom_data.clone(),
            response_channel: Some(tx),
            priority,
            auto_suggestion: self.generate_auto_suggestion(context, &intervention_type).await,
        };
        
        // 检查并发限制
        {
            let pending = self.pending_interventions.read().await;
            if pending.len() >= self.config.max_concurrent_interventions {
                return Err(AppError::Config("Maximum concurrent interventions reached".to_string()));
            }
        }
        
        // 添加到待处理列表
        {
            let mut pending = self.pending_interventions.write().await;
            pending.insert(intervention_id, intervention);
        }
        
        // 更新统计
        {
            let mut stats = self.statistics.write().await;
            stats.total_interventions += 1;
            let type_key = format!("{:?}", intervention_type);
            *stats.by_type.entry(type_key).or_insert(0) += 1;
        }
        
        // 通知用户界面
        self.event_notifier.notify_waiters();
        
        info!("🛑 User intervention created: {} (type: {:?})", intervention_id, intervention_type);
        
        // 等待用户决策或超时
        let start_time = Instant::now();
        let decision = tokio::time::timeout(timeout_duration, rx).await;
        let duration = start_time.elapsed();
        
        // 移除干预
        {
            let mut pending = self.pending_interventions.write().await;
            pending.remove(&intervention_id);
        }
        
        match decision {
            Ok(Ok(decision)) => {
                self.record_intervention_completion(
                    intervention_id,
                    context.id.clone(),
                    intervention_type,
                    Some(decision.clone()),
                    duration,
                    false,
                    decision.decided_by.clone(),
                ).await;
                
                // 应用决策
                self.apply_intervention_decision(context, decision).await?;
                
                info!("✅ User intervention completed: {}", intervention_id);
                Ok(intervention_id)
            }
            Ok(Err(_)) => {
                // 响应通道被关闭
                error!("❌ User intervention channel closed: {}", intervention_id);
                Err(AppError::Config("Intervention channel closed".to_string()))
            }
            Err(_) => {
                // 超时
                warn!("⏰ User intervention timed out: {}", intervention_id);
                
                self.record_intervention_completion(
                    intervention_id,
                    context.id.clone(),
                    intervention_type,
                    None,
                    duration,
                    true,
                    None,
                ).await;
                
                // 更新超时统计
                {
                    let mut stats = self.statistics.write().await;
                    stats.timeout_interventions += 1;
                }
                
                Err(AppError::Config("User intervention timed out".to_string()))
            }
        }
    }
    
    /// 处理用户决策
    #[instrument(skip(self, decision))]
    pub async fn handle_user_decision(
        &self,
        intervention_id: InterventionId,
        decision: InterventionDecision,
        user_session_id: Option<String>,
    ) -> Result<(), AppError> {
        debug!("Handling user decision for intervention: {}", intervention_id);
        
        // 验证用户权限
        if let Some(session_id) = &user_session_id {
            self.validate_user_permissions(session_id, &decision).await?;
        }
        
        // 获取干预信息
        let mut pending = self.pending_interventions.write().await;
        if let Some(mut intervention) = pending.remove(&intervention_id) {
            // 发送决策
            if let Some(tx) = intervention.response_channel.take() {
                tx.send(decision.clone()).ok();
            }
            
            // 更新用户会话活动
            if let Some(session_id) = user_session_id {
                self.update_user_session_activity(&session_id).await;
                
                // 更新用户统计
                {
                    let mut stats = self.statistics.write().await;
                    *stats.by_user.entry(session_id).or_insert(0) += 1;
                    stats.manual_handled_interventions += 1;
                }
            }
            
            // 如果是永久决策，创建自动批准规则
            if decision.make_permanent {
                self.create_auto_approval_rule_from_decision(&intervention, &decision).await?;
            }
            
            info!("✅ User decision applied for intervention: {}", intervention_id);
            Ok(())
        } else {
            Err(AppError::Config(format!("Intervention {} not found or already handled", intervention_id)))
        }
    }
    
    /// 检查是否需要暂停
    async fn should_pause(&self, context: &ConnectionContext) -> Option<(InterventionType, String, InterventionPriority)> {
        // 检查断点
        if self.config.enable_breakpoints {
            if let Some(breakpoint) = self.check_breakpoints(context).await {
                return Some((
                    InterventionType::BreakpointTriggered,
                    format!("Breakpoint '{}' triggered", breakpoint.name),
                    InterventionPriority::High,
                ));
            }
        }
        
        // 检查自动暂停条件
        for condition in &self.config.auto_pause_conditions {
            if condition.enabled && self.evaluate_condition(&condition.condition, context).await {
                return Some((
                    InterventionType::RequestPause,
                    condition.message.clone(),
                    condition.priority.clone(),
                ));
            }
        }
        
        None
    }
    
    /// 检查断点
    async fn check_breakpoints(&self, context: &ConnectionContext) -> Option<Breakpoint> {
        let mut breakpoints = self.breakpoints.write().await;
        
        for breakpoint in breakpoints.iter_mut() {
            if breakpoint.enabled && self.evaluate_condition(&breakpoint.condition, context).await {
                breakpoint.hit_count += 1;
                
                // 更新统计
                {
                    let mut stats = self.statistics.write().await;
                    *stats.breakpoint_hits.entry(breakpoint.name.clone()).or_insert(0) += 1;
                }
                
                return Some(breakpoint.clone());
            }
        }
        
        None
    }
    
    /// 评估条件表达式
    fn evaluate_condition<'a>(&'a self, condition: &'a ConditionExpression, context: &'a ConnectionContext) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            match condition {
            ConditionExpression::HostMatches(pattern) => {
                self.wildcard_match(&context.target_host, pattern)
            }
            ConditionExpression::PathMatches(pattern) => {
                if let Some(ref request) = context.request {
                    let path = self.extract_path_from_url(&request.url);
                    self.wildcard_match(&path, pattern)
                } else {
                    false
                }
            }
            ConditionExpression::MethodEquals(method) => {
                if let Some(ref request) = context.request {
                    request.method.eq_ignore_ascii_case(method)
                } else {
                    false
                }
            }
            ConditionExpression::HeaderExists(header) => {
                if let Some(ref request) = context.request {
                    request.headers.contains_key(header)
                } else {
                    false
                }
            }
            ConditionExpression::HeaderValueMatches(header, pattern) => {
                if let Some(ref request) = context.request {
                    if let Some(value) = request.headers.get(header) {
                        if let Ok(v_str) = value.to_str() {
                            return self.wildcard_match(v_str, pattern);
                        }
                    }
                }
                false
            }
            ConditionExpression::StatusCodeEquals(status) => {
                if let Some(ref response) = context.response {
                    response.status_code == *status
                } else {
                    false
                }
            }
            ConditionExpression::ContentContains(pattern) => {
                if let Some(ref request) = context.request {
                    let body_str = String::from_utf8_lossy(&request.body);
                    body_str.contains(pattern)
                } else if let Some(ref response) = context.response {
                    let body_str = String::from_utf8_lossy(&response.body);
                    body_str.contains(pattern)
                } else {
                    false
                }
            }
            ConditionExpression::And(conditions) => {
                for condition in conditions {
                    if !Box::pin(self.evaluate_condition(condition, context)).await {
                        return false;
                    }
                }
                true
            }
            ConditionExpression::Or(conditions) => {
                for condition in conditions {
                    if Box::pin(self.evaluate_condition(condition, context)).await {
                        return true;
                    }
                }
                false
            }
            ConditionExpression::Not(condition) => {
                !Box::pin(self.evaluate_condition(condition, context)).await
            }
            ConditionExpression::LuaExpression(_script) => {
                // 这里可以集成Lua脚本评估
                // 暂时返回false
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
    
    /// 检查自动批准规则
    async fn check_auto_approval_rules(
        &self,
        context: &ConnectionContext,
        _intervention_typeeee: &InterventionType,
    ) -> Option<InterventionDecision> {
        let mut rules = self.auto_approval_rules.write().await;
        
        for rule in rules.iter_mut() {
            if rule.enabled && self.evaluate_condition(&rule.condition, context).await {
                rule.usage_count += 1;
                
                return Some(InterventionDecision {
                    decision_id: Uuid::new_v4(),
                    action: rule.action.clone(),
                    modified_request: None,
                    modified_response: None,
                    reason: Some(format!("Auto-approved by rule: {}", rule.name)),
                    additional_data: HashMap::new(),
                    decided_by: Some("auto-approval-rule".to_string()),
                    decided_at: SystemTime::now(),
                    make_permanent: false,
                });
            }
        }
        
        None
    }
    
    /// 应用自动决策
    #[allow(unused_variables)]
    async fn apply_auto_decision(
        &self,
        intervention_id: InterventionId,
        _decisionn: InterventionDecision,
    ) -> Result<InterventionId, AppError> {
        debug!("Applying auto decision for intervention: {}", intervention_id);
        
        // 更新统计
        {
            let mut stats = self.statistics.write().await;
            stats.auto_handled_interventions += 1;
        }
        
        info!("🤖 Auto-decision applied for intervention: {}", intervention_id);
        Ok(intervention_id)
    }
    
    /// 生成自动建议
    async fn generate_auto_suggestion(
        &self,
        context: &ConnectionContext,
        intervention_type: &InterventionType,
    ) -> Option<InterventionDecision> {
        // 简单的启发式建议
        match intervention_type {
            InterventionType::RequestPause => {
                if let Some(ref request) = context.request {
                    if request.method == "GET" {
                        return Some(InterventionDecision {
                            decision_id: Uuid::new_v4(),
                            action: ActionType::Continue,
                            modified_request: None,
                            modified_response: None,
                            reason: Some("GET requests are generally safe to continue".to_string()),
                            additional_data: HashMap::new(),
                            decided_by: Some("auto-suggestion".to_string()),
                            decided_at: SystemTime::now(),
                            make_permanent: false,
                        });
                    }
                }
            }
            InterventionType::SecurityReview => {
                return Some(InterventionDecision {
                    decision_id: Uuid::new_v4(),
                    action: ActionType::Pause,
                    modified_request: None,
                    modified_response: None,
                    reason: Some("Security reviews should be carefully examined".to_string()),
                    additional_data: HashMap::new(),
                    decided_by: Some("auto-suggestion".to_string()),
                    decided_at: SystemTime::now(),
                    make_permanent: false,
                });
            }
            _ => {}
        }
        
        None
    }
    
    /// 创建可用操作
    fn create_available_actions(&self, intervention_type: &InterventionType) -> Vec<InterventionAction> {
        let mut actions = Vec::new();
        
        // 通用操作
        actions.push(InterventionAction {
            id: "continue".to_string(),
            name: "Continue".to_string(),
            description: "Continue processing without changes".to_string(),
            is_default: true,
            action_type: ActionType::Continue,
            requires_data: false,
        });
        
        actions.push(InterventionAction {
            id: "reject".to_string(),
            name: "Reject".to_string(),
            description: "Reject the request".to_string(),
            is_default: false,
            action_type: ActionType::Reject,
            requires_data: false,
        });
        
        // 基于干预类型的特定操作
        match intervention_type {
            InterventionType::RequestPause | InterventionType::RequestModification => {
                actions.push(InterventionAction {
                    id: "modify_request".to_string(),
                    name: "Modify Request".to_string(),
                    description: "Modify the request before proceeding".to_string(),
                    is_default: false,
                    action_type: ActionType::ModifyRequest,
                    requires_data: true,
                });
                
                actions.push(InterventionAction {
                    id: "redirect".to_string(),
                    name: "Redirect".to_string(),
                    description: "Redirect to a different URL".to_string(),
                    is_default: false,
                    action_type: ActionType::Redirect,
                    requires_data: true,
                });
            }
            InterventionType::ResponsePause | InterventionType::ResponseModification => {
                actions.push(InterventionAction {
                    id: "modify_response".to_string(),
                    name: "Modify Response".to_string(),
                    description: "Modify the response before returning".to_string(),
                    is_default: false,
                    action_type: ActionType::ModifyResponse,
                    requires_data: true,
                });
            }
            InterventionType::BreakpointTriggered => {
                actions.push(InterventionAction {
                    id: "replay_request".to_string(),
                    name: "Replay Request".to_string(),
                    description: "Replay the request with modifications".to_string(),
                    is_default: false,
                    action_type: ActionType::ReplayRequest,
                    requires_data: true,
                });
            }
            _ => {}
        }
        
        actions
    }
    
    /// 应用干预决策
    async fn apply_intervention_decision(
        &self,
        context: &ConnectionContext,
        decision: InterventionDecision,
    ) -> Result<(), AppError> {
        match decision.action {
            ActionType::Continue => {
                debug!("Continuing processing for connection {}", context.id);
                Ok(())
            }
            ActionType::Pause => {
                debug!("Pausing processing for connection {}", context.id);
                // 实际实现中可能需要更复杂的暂停逻辑
                Ok(())
            }
            ActionType::Reject => {
                debug!("Rejecting request for connection {}", context.id);
                Err(AppError::Config("Request rejected by user intervention".to_string()))
            }
            ActionType::Redirect => {
                debug!("Redirecting request for connection {}", context.id);
                // 实际实现中需要处理重定向逻辑
                Ok(())
            }
            ActionType::ModifyRequest => {
                debug!("Modifying request for connection {}", context.id);
                // 请求修改逻辑
                Ok(())
            }
            ActionType::ModifyResponse => {
                debug!("Modifying response for connection {}", context.id);
                // 响应修改逻辑
                Ok(())
            }
            ActionType::ReplayRequest => {
                debug!("Replaying request for connection {}", context.id);
                // 请求重放逻辑
                Ok(())
            }
            ActionType::Custom(_) => {
                debug!("Applying custom action for connection {}", context.id);
                Ok(())
            }
        }
    }
    
    /// 验证用户权限
    async fn validate_user_permissions(
        &self,
        session_id: &str,
        decision: &InterventionDecision,
    ) -> Result<(), AppError> {
        let user_sessions = self.user_sessions.read().await;
        
        if let Some(session) = user_sessions.get(session_id) {
            if !session.authenticated {
                return Err(AppError::Config("User session not authenticated".to_string()));
            }
            
            // 检查特定操作权限
            match &decision.action {
                ActionType::ModifyRequest | ActionType::ModifyResponse => {
                    if !session.permissions.contains(&"modify_requests".to_string()) {
                        return Err(AppError::Config("Insufficient permissions to modify requests".to_string()));
                    }
                }
                ActionType::Reject => {
                    if !session.permissions.contains(&"reject_requests".to_string()) {
                        return Err(AppError::Config("Insufficient permissions to reject requests".to_string()));
                    }
                }
                _ => {}
            }
            
            Ok(())
        } else {
            Err(AppError::Config("Invalid user session".to_string()))
        }
    }
    
    /// 更新用户会话活动
    async fn update_user_session_activity(&self, session_id: &str) {
        let mut user_sessions = self.user_sessions.write().await;
        if let Some(session) = user_sessions.get_mut(session_id) {
            session.last_activity = SystemTime::now();
        }
    }
    
    /// 从决策创建自动批准规则
    async fn create_auto_approval_rule_from_decision(
        &self,
        intervention: &PendingIntervention,
        decision: &InterventionDecision,
    ) -> Result<(), AppError> {
        let rule_name = format!("Auto-generated rule for {}", intervention.message);
        
        // 创建基于主机的条件
        let condition = if let Some(ref request) = intervention.original_request {
            if let Ok(url) = url::Url::parse(&request.url) {
                if let Some(host) = url.host_str() {
                    ConditionExpression::HostMatches(host.to_string())
                } else {
                    ConditionExpression::MethodEquals(request.method.clone())
                }
            } else {
                ConditionExpression::MethodEquals(request.method.clone())
            }
        } else {
            return Err(AppError::Config("Cannot create rule without request context".to_string()));
        };
        
        let rule = AutoApprovalRule {
            id: Uuid::new_v4(),
            name: rule_name,
            condition,
            action: decision.action.clone(),
            enabled: true,
            created_at: SystemTime::now(),
            usage_count: 0,
        };
        
        let mut auto_approval_rules = self.auto_approval_rules.write().await;
        auto_approval_rules.push(rule);
        
        info!("📝 Created auto-approval rule from user decision");
        Ok(())
    }
    
    /// 记录干预完成
    async fn record_intervention_completion(
        &self,
        intervention_id: InterventionId,
        connection_id: ConnectionId,
        intervention_type: InterventionType,
        decision: Option<InterventionDecision>,
        duration: Duration,
        timed_out: bool,
        handled_by: Option<String>,
    ) {
        let record = InterventionRecord {
            id: Uuid::new_v4(),
            intervention_id,
            connection_id,
            intervention_type,
            started_at: SystemTime::now() - duration,
            completed_at: Some(SystemTime::now()),
            duration: Some(duration),
            decision,
            timed_out,
            handled_by,
            auto_handled: false,
        };
        
        let mut history = self.intervention_history.write().await;
        history.push_front(record);
        
        // 保持历史记录数量限制
        while history.len() > self.config.history_retention_count {
            history.pop_back();
        }
        
        // 更新统计
        {
            let mut stats = self.statistics.write().await;
            if !timed_out {
                stats.successful_interventions += 1;
            }
            
            // 更新平均处理时间
            stats.avg_handling_time = match stats.avg_handling_time {
                Some(avg) => Some((avg + duration) / 2),
                None => Some(duration),
            };
        }
    }
    
    /// 获取待处理的干预列表
    pub async fn get_pending_interventions(&self) -> Vec<PendingIntervention> {
        let pending = self.pending_interventions.read().await;
        pending.values().map(|p| PendingIntervention {
            id: p.id,
            connection_id: p.connection_id.clone(),
            intervention_type: p.intervention_type.clone(),
            created_at: p.created_at,
            response_channel: None, // 不能克隆oneshot::Sender
            original_request: p.original_request.clone(),
            original_response: p.original_response.clone(),
            modified_request: p.modified_request.clone(),
            modified_response: p.modified_response.clone(),
            timeout_at: p.timeout_at,
            message: p.message.clone(),
            available_actions: p.available_actions.clone(),
            context_data: p.context_data.clone(),
            priority: p.priority.clone(),
            auto_suggestion: p.auto_suggestion.clone(),
        }).collect()
    }
    
    /// 获取干预统计
    pub async fn get_statistics(&self) -> InterventionStatistics {
        self.statistics.read().await.clone()
    }
    
    /// 添加断点
    pub async fn add_breakpoint(&self, breakpoint: Breakpoint) -> Result<(), AppError> {
        let mut breakpoints = self.breakpoints.write().await;
        breakpoints.push(breakpoint);
        Ok(())
    }
    
    /// 移除断点
    pub async fn remove_breakpoint(&self, breakpoint_id: Uuid) -> Result<(), AppError> {
        let mut breakpoints = self.breakpoints.write().await;
        if let Some(pos) = breakpoints.iter().position(|bp| bp.id == breakpoint_id) {
            breakpoints.remove(pos);
            Ok(())
        } else {
            Err(AppError::Config(format!("Breakpoint {} not found", breakpoint_id)))
        }
    }
    
    /// 获取断点列表
    pub async fn get_breakpoints(&self) -> Vec<Breakpoint> {
        self.breakpoints.read().await.clone()
    }
}

impl Default for UserInterventionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_timeout: Duration::from_secs(300), // 5分钟
            max_concurrent_interventions: 10,
            history_retention_count: 1000,
            require_authentication: true,
            allowed_roles: vec!["admin".to_string(), "operator".to_string()],
            auto_pause_conditions: Vec::new(),
            enable_breakpoints: true,
            detailed_history: true,
        }
    }
}

#[async_trait]
impl Middleware for UserInterventionMiddleware {
    fn name(&self) -> &str {
        "user_intervention"
    }
    
    async fn process(
        &self,
        context: &mut ConnectionContext,
        _core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, crate::error::TakeoverError> {
        debug!("User intervention middleware processing connection {}", context.id);
        
        if !self.config.enabled {
            return Ok(MiddlewareResult::Continue);
        }
        
        // 检查是否需要暂停
        if let Some((intervention_type, message, priority)) = self.should_pause(context).await {
            context.set_processing_state(ProcessingState::UserIntervention);
            
            info!("🛑 Triggering user intervention: {:?}", intervention_type);
            
            match self.create_intervention(
                intervention_type,
                context,
                message,
                priority,
                None,
            ).await {
                Ok(_intervention_id) => {
                    info!("✅ User intervention completed successfully");
                    Ok(MiddlewareResult::Continue)
                }
                Err(e) => {
                    error!("❌ User intervention failed: {}", e);
                    
                    // 根据配置决定是否继续处理
                    if self.config.auto_pause_conditions.is_empty() {
                        Ok(MiddlewareResult::Continue)
                    } else {
                        Err(crate::error::TakeoverError::Config(format!("User intervention failed: {}", e)))
                    }
                }
            }
        } else {
            Ok(MiddlewareResult::Continue)
        }
    }
    
    fn priority(&self) -> i32 {
        45 // 在脚本执行之后，修改之前
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
    
    #[tokio::test]
    async fn test_intervention_middleware_creation() {
        let config = UserInterventionConfig::default();
        let middleware = UserInterventionMiddleware::new(config);
        
        assert_eq!(middleware.name(), "user_intervention");
        assert!(middleware.enabled());
    }
    
    #[tokio::test]
    async fn test_user_session_management() {
        let middleware = UserInterventionMiddleware::new(UserInterventionConfig::default());
        
        let session_id = middleware.start_user_session(
            "test_user".to_string(),
            vec!["admin".to_string()],
            vec!["modify_requests".to_string(), "reject_requests".to_string()]
        ).await.unwrap();
        
        let user_sessions = middleware.user_sessions.read().await;
        let session = user_sessions.get(&session_id).unwrap();
        
        assert_eq!(session.user_id, "test_user");
        assert!(session.authenticated);
        assert!(session.permissions.contains(&"modify_requests".to_string()));
    }
    
    #[tokio::test]
    async fn test_condition_evaluation() {
        let middleware = UserInterventionMiddleware::new(UserInterventionConfig::default());
        
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        
        let mut context = ConnectionContext::new(connection, "example.com".to_string(), 80);
        context.request = Some(HttpRequest::new("GET", "http://example.com/api"));
        
        // 测试主机匹配
        let condition = ConditionExpression::HostMatches("example.com".to_string());
        assert!(middleware.evaluate_condition(&condition, &context).await);
        
        // 测试方法匹配
        let condition = ConditionExpression::MethodEquals("GET".to_string());
        assert!(middleware.evaluate_condition(&condition, &context).await);
        
        // 测试路径匹配
        let condition = ConditionExpression::PathMatches("/api".to_string());
        assert!(middleware.evaluate_condition(&condition, &context).await);
        
        // 测试复合条件
        let condition = ConditionExpression::And(vec![
            ConditionExpression::HostMatches("example.com".to_string()),
            ConditionExpression::MethodEquals("GET".to_string()),
        ]);
        assert!(middleware.evaluate_condition(&condition, &context).await);
    }
    
    #[tokio::test]
    async fn test_breakpoint_management() {
        let middleware = UserInterventionMiddleware::new(UserInterventionConfig::default());
        
        let breakpoint = Breakpoint {
            id: Uuid::new_v4(),
            name: "Test Breakpoint".to_string(),
            condition: ConditionExpression::HostMatches("example.com".to_string()),
            enabled: true,
            created_at: SystemTime::now(),
            created_by: "test_user".to_string(),
            hit_count: 0,
            description: Some("Test breakpoint".to_string()),
        };
        
        let breakpoint_id = breakpoint.id;
        
        // 添加断点
        middleware.add_breakpoint(breakpoint).await.unwrap();
        
        // 验证断点存在
        let breakpoints = middleware.get_breakpoints().await;
        assert_eq!(breakpoints.len(), 1);
        assert_eq!(breakpoints[0].id, breakpoint_id);
        
        // 移除断点
        middleware.remove_breakpoint(breakpoint_id).await.unwrap();
        
        // 验证断点已移除
        let breakpoints = middleware.get_breakpoints().await;
        assert_eq!(breakpoints.len(), 0);
    }
    
    #[tokio::test]
    async fn test_auto_suggestion_generation() {
        let middleware = UserInterventionMiddleware::new(UserInterventionConfig::default());
        
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        
        let mut context = ConnectionContext::new(connection, "example.com".to_string(), 80);
        context.request = Some(HttpRequest::new("GET", "http://example.com/api"));
        
        let suggestion = middleware.generate_auto_suggestion(&context, &InterventionType::RequestPause).await;
        
        assert!(suggestion.is_some());
        let suggestion = suggestion.unwrap();
        assert!(matches!(suggestion.action, ActionType::Continue));
        assert!(suggestion.reason.is_some());
    }
    
    #[tokio::test]
    async fn test_wildcard_matching() {
        let middleware = UserInterventionMiddleware::new(UserInterventionConfig::default());
        
        assert!(middleware.wildcard_match("example.com", "*.com"));
        assert!(middleware.wildcard_match("api.example.com", "api.*"));
        assert!(middleware.wildcard_match("anything", "*"));
        assert!(middleware.wildcard_match("example.com", "example.com"));
        assert!(!middleware.wildcard_match("example.org", "example.com"));
    }
}
