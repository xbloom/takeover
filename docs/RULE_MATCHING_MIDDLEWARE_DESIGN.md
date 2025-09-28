# 规则匹配中间件设计文档

## 📋 文档概述

本文档描述了基于新架构设计的规则匹配中间件（RuleMatchingMiddleware）的完整实现方案，包括混合状态管理、自包含组件设计和类型安全的依赖注入系统。

**设计版本**: v3.0  
**最后更新**: 2024年12月  
**适用架构**: 精简内核 + 插件式中间件 + 外置API架构

---

## 🎯 设计目标

### 核心原则
1. **高性能热路径** - 规则匹配操作零序列化开销
2. **统一状态管理** - 统计和监控数据集中管理
3. **自包含设计** - 单文件包含所有相关功能
4. **类型安全** - TypeId驱动的依赖注入，编译时类型检查
5. **可选组合** - API层完全可选，内核保持精简

### 性能目标
- **规则匹配延迟**: < 50μs (微秒级别)
- **并发规则更新**: 支持10,000+ 规则无锁竞争
- **内存占用**: 每条规则 < 1KB 内存footprint
- **热重载**: 规则更新不影响正在进行的匹配

---

## 🏗️ 架构设计

### 整体架构图

```
┌─────────────────────────────────────────────────────────────────┐
│                   规则匹配中间件架构                              │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ┌──────────────────┐  ┌─────────────────┐  ┌─────────────────┐ │
│  │                  │  │                 │  │                 │ │
│  │  高频热路径存储   │  │  统一管理存储    │  │   外部API接口   │ │
│  │                  │  │                 │  │                 │ │
│  │ ┌──────────────┐ │  │ ┌─────────────┐ │  │ ┌─────────────┐ │ │
│  │ │ active_rules │ │  │ │StateManager │ │  │ │RuleControlApi│ │ │
│  │ │ (HashMap)    │ │  │ │统计数据      │ │  │ │(外置可选)    │ │ │
│  │ └──────────────┘ │  │ │历史记录      │ │  │ └─────────────┘ │ │
│  │ ┌──────────────┐ │  │ │审计日志      │ │  │                 │ │
│  │ │compiled_     │ │  │ │事件发布      │ │  │                 │ │
│  │ │patterns      │ │  │ └─────────────┘ │  │                 │ │
│  │ │(预编译缓存)   │ │  │                 │  │                 │ │
│  │ └──────────────┘ │  │                 │  │                 │ │
│  └──────────────────┘  └─────────────────┘  └─────────────────┘ │
│           ↑                       ↑                       ↑     │
│           │                       │                       │     │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │              RuleService (核心业务服务)                     │ │
│  │  - 高性能规则匹配 (直接内存访问)                             │ │
│  │  - 规则管理操作 (双写策略)                                  │ │
│  │  - 统计数据收集 (异步更新)                                  │ │
│  └─────────────────────────────────────────────────────────────┘ │
│                                 ↑                               │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │          RuleMatchingMiddleware (处理管道)                  │ │
│  │  - 中间件生命周期管理                                       │ │
│  │  - 请求处理协调                                             │ │
│  │  - 服务注册和初始化                                         │ │
│  └─────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

### 核心组件分层

```rust
// 文件: src/core/middleware/rule_matching.rs
// 自包含的三层架构：

┌─────────────────────────────────────────────┐
│            外部API层 (可选)                  │ ← RuleControlApi
│  - 权限控制、参数验证                       │
│  - 审计日志、速率限制                       │  
│  - 协议转换、错误处理                       │
└─────────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────────┐
│            业务服务层 (核心)                 │ ← RuleService
│  - 混合状态管理策略                         │
│  - 高性能规则匹配算法                       │
│  - 双写数据同步机制                         │
└─────────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────────┐
│            中间件层 (管道)                   │ ← RuleMatchingMiddleware  
│  - 生命周期管理                             │
│  - 服务注册协调                             │
│  - 请求处理编排                             │
└─────────────────────────────────────────────┘
```

---

## 🔧 技术实现

### 数据模型设计

#### 规则数据结构
```rust
/// 规则定义 - 编译时强类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    /// 规则唯一标识
    pub id: uuid::Uuid,
    /// 规则名称
    pub name: String,
    /// 规则描述
    pub description: String,
    /// 匹配模式
    pub pattern: RulePattern,
    /// 匹配条件
    pub conditions: Vec<RuleCondition>,
    /// 执行动作
    pub action: RuleAction,
    /// 是否启用
    pub enabled: bool,
    /// 优先级 (数字越小优先级越高)
    pub priority: i32,
    /// 规则元数据
    pub metadata: HashMap<String, serde_json::Value>,
}

/// 规则匹配模式
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RulePattern {
    /// 主机名匹配 (支持通配符)
    HostMatches(String),
    /// 路径匹配 (支持正则表达式)
    PathMatches(String),  
    /// 头部存在性检查
    HeaderExists(String),
    /// 头部值匹配
    HeaderValueMatches { header: String, value: String },
    /// 端口匹配
    PortMatches(u16),
    /// 进程名匹配
    ProcessMatches(String),
    /// 逻辑AND组合
    And(Vec<RulePattern>),
    /// 逻辑OR组合  
    Or(Vec<RulePattern>),
    /// 自定义Lua表达式
    Custom(String),
}

/// 规则执行动作
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RuleAction {
    /// 允许通过
    Allow,
    /// 直接连接
    Direct,  
    /// 拒绝连接
    Block,
    /// 通过指定代理
    ProxyByName(String),
    /// 修改请求
    ModifyRequest(RequestModifications),
    /// 修改响应
    ModifyResponse(ResponseModifications),
    /// 拦截等待审批
    InterceptForApproval,
    /// 执行脚本
    ExecuteScript(String),
    /// 记录日志
    Log(LogLevel),
}
```

#### 性能优化数据结构
```rust
/// 预编译的规则模式 - 热路径优化
#[derive(Debug)]
pub struct CompiledPattern {
    /// 原始模式
    pub original: RulePattern,
    /// 编译后的正则表达式 (如果适用)
    pub regex: Option<regex::Regex>,
    /// 通配符匹配器 (如果适用)
    pub glob: Option<globset::Glob>,
    /// 简单字符串匹配 (最快)
    pub exact_match: Option<String>,
    /// 匹配类型标识
    pub pattern_type: PatternType,
}

/// 模式类型 - 性能分级
#[derive(Debug, PartialEq)]
pub enum PatternType {
    /// 精确匹配 - 最快 (HashMap lookup)
    Exact,
    /// 前缀匹配 - 快速 (starts_with)
    Prefix,
    /// 后缀匹配 - 快速 (ends_with) 
    Suffix,
    /// 通配符匹配 - 中等 (glob matching)
    Wildcard,
    /// 正则表达式 - 较慢 (regex matching)
    Regex,
    /// 复合条件 - 最复杂 (recursive matching)
    Composite,
}
```

### 混合状态管理策略

#### 高性能内存存储
```rust
pub struct RuleService {
    // 🚀 高频热路径数据 - 分散管理 (性能优先)
    /// 活跃规则集合 - 直接内存访问
    active_rules: Arc<RwLock<HashMap<String, Rule>>>,
    /// 预编译的匹配模式 - 避免重复编译
    compiled_patterns: Arc<RwLock<HashMap<String, CompiledPattern>>>,
    /// 规则优先级索引 - 快速排序查找
    priority_index: Arc<RwLock<BTreeMap<i32, Vec<String>>>>,
    
    // 📊 统计和管理数据 - 统一管理 (便于监控)
    /// 状态管理器实例
    state_manager: Arc<StateManager>,
    
    // ⚡ 性能优化缓存
    /// 最近匹配缓存 (LRU)
    match_cache: Arc<Mutex<lru::LruCache<String, Option<String>>>>,
}

impl RuleService {
    /// 创建规则服务
    pub fn new(state_manager: Arc<StateManager>) -> Self {
        Self {
            active_rules: Arc::new(RwLock::new(HashMap::new())),
            compiled_patterns: Arc::new(RwLock::new(HashMap::new())),
            priority_index: Arc::new(RwLock::new(BTreeMap::new())),
            state_manager,
            match_cache: Arc::new(Mutex::new(lru::LruCache::new(1000))), // 缓存1000条
        }
    }
}
```

#### 双写数据同步策略
```rust
impl RuleService {
    /// 应用规则 - 双写策略
    pub async fn apply_rules(&self, rules: Vec<Rule>) -> Result<RuleApplyResult, RuleError> {
        let start_time = Instant::now();
        
        // 1. 数据验证和预编译
        let mut compiled_patterns = HashMap::new();
        let mut priority_map = BTreeMap::new();
        
        for rule in &rules {
            // 验证规则有效性
            self.validate_rule(rule)?;
            
            // 预编译匹配模式
            let compiled = self.compile_pattern(&rule.pattern).await?;
            compiled_patterns.insert(rule.id.to_string(), compiled);
            
            // 构建优先级索引
            priority_map.entry(rule.priority)
                .or_insert_with(Vec::new)
                .push(rule.id.to_string());
        }
        
        // 2. 原子更新内存状态 (性能关键路径)
        let backup_id = {
            let mut active_rules = self.active_rules.write().await;
            let mut patterns = self.compiled_patterns.write().await;
            let mut index = self.priority_index.write().await;
            
            // 备份当前状态用于回滚
            let backup_id = self.create_rules_backup(&active_rules).await?;
            
            // 原子替换
            active_rules.clear();
            patterns.clear();
            index.clear();
            
            for rule in &rules {
                active_rules.insert(rule.id.to_string(), rule.clone());
            }
            *patterns = compiled_patterns;
            *index = priority_map;
            
            backup_id
        };
        
        // 3. 清除匹配缓存
        self.match_cache.lock().await.clear();
        
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
        
        // 5. 返回操作结果
        let apply_duration = start_time.elapsed();
        Ok(RuleApplyResult {
            applied_count: rules.len(),
            backup_id,
            apply_duration,
            warnings: self.check_rule_conflicts(&rules).await,
        })
    }
}
```

### 高性能规则匹配算法

#### 多级匹配策略
```rust
impl RuleService {
    /// 高性能规则匹配 - 热路径优化
    pub async fn match_request(&self, context: &ConnectionContext) -> Option<MatchResult> {
        let match_start = Instant::now();
        
        // 1. 构建匹配键用于缓存
        let cache_key = self.build_cache_key(context);
        
        // 2. 尝试缓存命中
        if let Ok(mut cache) = self.match_cache.try_lock() {
            if let Some(cached_result) = cache.get(&cache_key) {
                // 异步更新统计 (不阻塞主路径)
                let state_manager = self.state_manager.clone();
                tokio::spawn(async move {
                    let _ = state_manager.increment_counter("rule_match_cache_hits").await;
                });
                
                return cached_result.as_ref().map(|rule_id| MatchResult {
                    rule_id: rule_id.clone(),
                    match_duration: match_start.elapsed(),
                    cache_hit: true,
                });
            }
        }
        
        // 3. 执行实际匹配
        let match_result = self.perform_rule_matching(context, match_start).await;
        
        // 4. 异步更新缓存和统计
        let cache_key_clone = cache_key.clone();
        let result_clone = match_result.as_ref().map(|r| r.rule_id.clone());
        let state_manager = self.state_manager.clone();
        let cache = self.match_cache.clone();
        
        tokio::spawn(async move {
            // 更新缓存
            if let Ok(mut cache) = cache.lock().await {
                cache.put(cache_key_clone, result_clone);
            }
            
            // 更新统计
            let _ = state_manager.increment_counter("rule_match_total").await;
            if match_result.is_some() {
                let _ = state_manager.increment_counter("rule_match_successful").await;
            }
        });
        
        match_result
    }
    
    /// 实际匹配执行 - 分级优化算法
    async fn perform_rule_matching(&self, context: &ConnectionContext, start_time: Instant) -> Option<MatchResult> {
        // 只读锁，支持高并发
        let active_rules = self.active_rules.read().await;
        let compiled_patterns = self.compiled_patterns.read().await;
        let priority_index = self.priority_index.read().await;
        
        // 按优先级顺序匹配
        for (_priority, rule_ids) in priority_index.iter() {
            for rule_id in rule_ids {
                if let Some(rule) = active_rules.get(rule_id) {
                    if !rule.enabled {
                        continue;
                    }
                    
                    if let Some(compiled_pattern) = compiled_patterns.get(rule_id) {
                        if self.evaluate_compiled_pattern(compiled_pattern, context).await {
                            // 异步更新匹配统计
                            let state_manager = self.state_manager.clone();
                            let rule_id_clone = rule_id.clone();
                            tokio::spawn(async move {
                                let counter_key = format!("rule_match_count_{}", rule_id_clone);
                                let _ = state_manager.increment_counter(&counter_key).await;
                            });
                            
                            return Some(MatchResult {
                                rule_id: rule_id.clone(),
                                match_duration: start_time.elapsed(),
                                cache_hit: false,
                            });
                        }
                    }
                }
            }
        }
        
        None
    }
    
    /// 高效模式评估 - 分级匹配策略
    async fn evaluate_compiled_pattern(&self, pattern: &CompiledPattern, context: &ConnectionContext) -> bool {
        match pattern.pattern_type {
            // 最快: 精确匹配
            PatternType::Exact => {
                if let Some(ref exact) = pattern.exact_match {
                    return context.target_host == *exact;
                }
            }
            
            // 快速: 前缀/后缀匹配
            PatternType::Prefix => {
                if let Some(ref exact) = pattern.exact_match {
                    return context.target_host.starts_with(exact);
                }
            }
            PatternType::Suffix => {
                if let Some(ref exact) = pattern.exact_match {
                    return context.target_host.ends_with(exact);
                }
            }
            
            // 中等: 通配符匹配
            PatternType::Wildcard => {
                if let Some(ref glob) = pattern.glob {
                    return glob.compile_matcher().is_match(&context.target_host);
                }
            }
            
            // 较慢: 正则表达式
            PatternType::Regex => {
                if let Some(ref regex) = pattern.regex {
                    return regex.is_match(&context.target_host);
                }
            }
            
            // 复杂: 递归评估
            PatternType::Composite => {
                return self.evaluate_composite_pattern(&pattern.original, context).await;
            }
        }
        
        false
    }
}
```

### 自包含中间件实现

#### 中间件生命周期管理
```rust
/// 规则匹配中间件 - 自包含设计
pub struct RuleMatchingMiddleware {
    config: RuleMatchingConfig,
}

#[derive(Debug, Clone)]
pub struct RuleMatchingConfig {
    /// 是否启用规则匹配
    pub enabled: bool,
    /// 默认动作
    pub default_action: DefaultAction,
    /// 规则匹配超时
    pub match_timeout: Duration,
    /// 缓存大小
    pub cache_size: usize,
    /// 是否启用性能监控
    pub enable_performance_monitoring: bool,
}

impl RuleMatchingConfig {
    pub fn production() -> Self {
        Self {
            enabled: true,
            default_action: DefaultAction::Direct,
            match_timeout: Duration::from_millis(100), // 100ms超时
            cache_size: 10000, // 缓存10K条记录
            enable_performance_monitoring: true,
        }
    }
    
    pub fn development() -> Self {
        Self {
            enabled: true,
            default_action: DefaultAction::Direct,
            match_timeout: Duration::from_secs(1), // 开发环境宽松超时
            cache_size: 1000,
            enable_performance_monitoring: false,
        }
    }
}

#[async_trait]
impl SelfContainedMiddleware for RuleMatchingMiddleware {
    fn name(&self) -> &str {
        "rule_matching"
    }
    
    fn priority(&self) -> i32 {
        30 // 在HTTP解析之后，转发之前
    }
    
    fn enabled(&self) -> bool {
        self.config.enabled
    }
    
    /// 注册核心服务到AppCore
    async fn register_services(&self, app_core: &AppCore) -> Result<(), AppError> {
        // 创建规则服务实例
        let rule_service = RuleService::new(app_core.state_manager.clone());
        
        // 注册到类型安全的服务容器
        app_core.manage(rule_service).await;
        
        info!("✅ RuleService registered to AppCore via TypeId");
        Ok(())
    }
    
    /// 初始化中间件
    async fn initialize(&self, app_core: &AppCore) -> Result<(), AppError> {
        // 验证依赖服务
        if let Some(rule_service) = app_core.state::<RuleService>().await {
            // 加载默认规则（如果需要）
            if self.should_load_default_rules() {
                let default_rules = self.create_default_rules();
                rule_service.apply_rules(default_rules).await
                    .map_err(|e| AppError::InitializationFailed(format!("Failed to load default rules: {}", e)))?;
            }
            
            // 启动性能监控（如果启用）
            if self.config.enable_performance_monitoring {
                self.start_performance_monitoring(rule_service).await?;
            }
            
            info!("🚀 RuleMatchingMiddleware initialized successfully");
            Ok(())
        } else {
            Err(AppError::ServiceNotAvailable("RuleService not found in AppCore".to_string()))
        }
    }
    
    /// 处理请求 - 主要业务逻辑
    async fn process(&self, context: &mut ConnectionContext, core: &AppCore) -> Result<MiddlewareResult, AppError> {
        let process_start = Instant::now();
        
        // 更新处理状态
        context.set_processing_state(ProcessingState::RuleMatching);
        
        // 获取规则服务
        let rule_service = core.state::<RuleService>().await
            .ok_or_else(|| AppError::ServiceNotAvailable("RuleService not available".to_string()))?;
        
        // 执行规则匹配
        let match_result = tokio::time::timeout(
            self.config.match_timeout,
            rule_service.match_request(context)
        ).await;
        
        match match_result {
            Ok(Some(match_result)) => {
                debug!("✅ Rule matched: {} ({}μs)", 
                    match_result.rule_id, 
                    match_result.match_duration.as_micros());
                
                // 应用匹配结果
                self.apply_match_result(context, &match_result, &rule_service).await?;
                
                // 异步记录性能指标
                let process_duration = process_start.elapsed();
                if process_duration > Duration::from_millis(10) {
                    warn!("⚠️ Rule matching took {}ms for {}", 
                        process_duration.as_millis(), context.target_host);
                }
                
                Ok(MiddlewareResult::Continue)
            }
            
            Ok(None) => {
                debug!("ℹ️ No rules matched for {}, using default action", context.target_host);
                self.apply_default_action(context).await
            }
            
            Err(_timeout) => {
                warn!("⏰ Rule matching timeout for {}", context.target_host);
                self.apply_default_action(context).await
            }
        }
    }
}
```

### 外置API设计

#### 控制接口实现
```rust
/// 规则控制API - 外置组件，完全可选
pub struct RuleControlApi {
    app_core: Arc<AppCore>,
    auth_service: Option<Arc<dyn AuthService>>,
    audit_logger: Option<Arc<dyn AuditLogger>>,
    rate_limiter: Option<Arc<RateLimiter>>,
}

impl RuleControlApi {
    /// 创建规则控制API
    pub fn new(app_core: Arc<AppCore>) -> Self {
        Self {
            app_core,
            auth_service: None,
            audit_logger: None,
            rate_limiter: None,
        }
    }
    
    /// 配置认证服务
    pub fn with_auth(mut self, auth_service: Arc<dyn AuthService>) -> Self {
        self.auth_service = Some(auth_service);
        self
    }
    
    /// 配置审计日志
    pub fn with_audit(mut self, audit_logger: Arc<dyn AuditLogger>) -> Self {
        self.audit_logger = Some(audit_logger);
        self
    }
    
    /// 配置速率限制
    pub fn with_rate_limiter(mut self, rate_limiter: Arc<RateLimiter>) -> Self {
        self.rate_limiter = Some(rate_limiter);
        self
    }
    
    /// 应用规则 - 完整的外部API处理
    pub async fn apply_rules(&self, 
        rules: Vec<Rule>, 
        caller_context: &CallerContext
    ) -> Result<ApiResponse<RuleApplyResult>, ApiError> {
        let operation_start = Instant::now();
        let operation_id = uuid::Uuid::new_v4();
        
        // 1. 权限检查
        if let Some(ref auth) = self.auth_service {
            if !auth.check_permission(&caller_context.caller_id, "rules:write").await? {
                return Err(ApiError::Unauthorized {
                    message: "Insufficient permissions to modify rules".to_string(),
                    required_permission: "rules:write".to_string(),
                });
            }
        }
        
        // 2. 速率限制检查
        if let Some(ref limiter) = self.rate_limiter {
            if !limiter.allow(&caller_context.caller_id).await {
                return Err(ApiError::RateLimited {
                    retry_after: limiter.retry_after(&caller_context.caller_id).await,
                });
            }
        }
        
        // 3. 输入验证
        self.validate_rules_input(&rules, caller_context).await?;
        
        // 4. 审计日志 - 操作开始
        if let Some(ref audit) = self.audit_logger {
            audit.log_operation_start(&AuditLogEntry {
                operation_id,
                caller_id: caller_context.caller_id.clone(),
                operation_type: "apply_rules".to_string(),
                resource_count: rules.len(),
                timestamp: SystemTime::now(),
                client_info: caller_context.client_info.clone(),
            }).await;
        }
        
        // 5. 调用内核服务
        let rule_service = self.app_core.state::<RuleService>().await
            .ok_or_else(|| ApiError::ServiceNotAvailable("RuleService not available".to_string()))?;
        
        match rule_service.apply_rules(rules.clone()).await {
            Ok(apply_result) => {
                // 6. 成功审计日志
                if let Some(ref audit) = self.audit_logger {
                    audit.log_operation_success(&AuditLogEntry {
                        operation_id,
                        caller_id: caller_context.caller_id.clone(),
                        operation_type: "apply_rules".to_string(),
                        resource_count: apply_result.applied_count,
                        timestamp: SystemTime::now(),
                        client_info: caller_context.client_info.clone(),
                    }).await;
                }
                
                // 7. 构建API响应
                Ok(ApiResponse {
                    success: true,
                    data: apply_result,
                    operation_id,
                    processing_time: operation_start.elapsed(),
                    timestamp: SystemTime::now(),
                    warnings: vec![], // TODO: 从apply_result提取警告
                })
            }
            
            Err(rule_error) => {
                // 6. 错误审计日志
                if let Some(ref audit) = self.audit_logger {
                    audit.log_operation_error(&AuditLogEntry {
                        operation_id,
                        caller_id: caller_context.caller_id.clone(),
                        operation_type: "apply_rules".to_string(),
                        resource_count: rules.len(),
                        timestamp: SystemTime::now(),
                        client_info: caller_context.client_info.clone(),
                    }, &format!("{:?}", rule_error)).await;
                }
                
                // 7. 转换业务错误为API错误
                Err(self.convert_rule_error(rule_error))
            }
        }
    }
    
    /// 获取活跃规则列表
    pub async fn get_active_rules(&self, caller_context: &CallerContext) -> Result<ApiResponse<Vec<Rule>>, ApiError> {
        // 权限检查
        if let Some(ref auth) = self.auth_service {
            if !auth.check_permission(&caller_context.caller_id, "rules:read").await? {
                return Err(ApiError::Unauthorized {
                    message: "Insufficient permissions to read rules".to_string(),
                    required_permission: "rules:read".to_string(),
                });
            }
        }
        
        // 获取规则
        let rule_service = self.app_core.state::<RuleService>().await
            .ok_or_else(|| ApiError::ServiceNotAvailable("RuleService not available".to_string()))?;
        
        let rules = rule_service.get_active_rules().await;
        
        Ok(ApiResponse {
            success: true,
            data: rules,
            operation_id: uuid::Uuid::new_v4(),
            processing_time: Duration::from_micros(1), // 非常快的操作
            timestamp: SystemTime::now(),
            warnings: vec![],
        })
    }
    
    /// 获取规则统计信息
    pub async fn get_rule_statistics(&self, caller_context: &CallerContext) -> Result<ApiResponse<RuleStatistics>, ApiError> {
        // 权限检查
        if let Some(ref auth) = self.auth_service {
            if !auth.check_permission(&caller_context.caller_id, "rules:stats").await? {
                return Err(ApiError::Unauthorized {
                    message: "Insufficient permissions to access rule statistics".to_string(),
                    required_permission: "rules:stats".to_string(),
                });
            }
        }
        
        // 获取统计信息（从StateManager）
        let state_manager = &self.app_core.state_manager;
        
        let statistics = RuleStatistics {
            active_rules_count: state_manager.get_counter("active_rules_count").await.unwrap_or(0),
            total_matches: state_manager.get_counter("rule_match_total").await.unwrap_or(0),
            successful_matches: state_manager.get_counter("rule_match_successful").await.unwrap_or(0),
            cache_hit_ratio: self.calculate_cache_hit_ratio(state_manager).await,
            average_match_time_us: self.calculate_average_match_time(state_manager).await,
            last_update: state_manager.get_global_data::<SystemTime>("rules_last_update").await
                .unwrap_or_else(SystemTime::now),
        };
        
        Ok(ApiResponse {
            success: true,
            data: statistics,
            operation_id: uuid::Uuid::new_v4(),
            processing_time: Duration::from_millis(1),
            timestamp: SystemTime::now(),
            warnings: vec![],
        })
    }
}
```

---

## 📊 性能基准和监控

### 性能指标定义

| 指标类型 | 指标名称 | 目标值 | 监控方式 |
|---------|---------|--------|---------|
| **延迟** | 规则匹配时间 | < 50μs | 每次匹配记录 |
| **吞吐量** | 每秒匹配次数 | > 100,000/s | 滑动窗口统计 |
| **内存** | 每条规则内存占用 | < 1KB | 定期采样 |
| **缓存** | 缓存命中率 | > 90% | 异步统计 |
| **并发** | 最大并发匹配 | 1000+ | 压力测试 |

### 监控和可观测性

#### 指标收集
```rust
/// 性能监控服务
impl RuleService {
    /// 启动性能监控
    async fn start_performance_monitoring(&self) -> Result<(), AppError> {
        let state_manager = self.state_manager.clone();
        
        // 启动指标收集任务
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            
            loop {
                interval.tick().await;
                
                // 收集性能指标
                let _ = state_manager.set_gauge(
                    "rule_matching_memory_usage", 
                    Self::estimate_memory_usage().await as f64
                ).await;
                
                // 计算缓存命中率
                let hits = state_manager.get_counter("rule_match_cache_hits").await.unwrap_or(0);
                let total = state_manager.get_counter("rule_match_total").await.unwrap_or(0);
                if total > 0 {
                    let hit_ratio = (hits as f64 / total as f64) * 100.0;
                    let _ = state_manager.set_gauge("rule_match_cache_hit_ratio", hit_ratio).await;
                }
                
                // 其他指标...
            }
        });
        
        Ok(())
    }
}
```

#### 告警规则
```yaml
# 监控告警配置 (示例)
alerts:
  - name: high_rule_matching_latency
    condition: rule_match_avg_time_us > 100
    severity: warning
    description: "Rule matching latency is above 100μs"
    
  - name: low_cache_hit_ratio  
    condition: rule_match_cache_hit_ratio < 80
    severity: warning
    description: "Rule matching cache hit ratio is below 80%"
    
  - name: rule_matching_errors
    condition: rule_match_errors > 10
    severity: critical
    description: "High number of rule matching errors"
```

---

## 🔧 部署和运维

### 配置最佳实践

#### 生产环境配置
```toml
# config.toml
[rule_matching]
enabled = true
default_action = "direct"
match_timeout_ms = 50
cache_size = 50000
enable_performance_monitoring = true
enable_debug_logging = false

[rule_matching.performance]
# 预编译池大小
pattern_compile_pool_size = 10
# 匹配并发限制
max_concurrent_matches = 1000
# 内存限制 (MB)
max_memory_usage_mb = 100
```

#### 开发环境配置
```toml
# config.dev.toml  
[rule_matching]
enabled = true
default_action = "direct"
match_timeout_ms = 1000  # 宽松超时
cache_size = 1000        # 较小缓存
enable_performance_monitoring = false
enable_debug_logging = true

[rule_matching.development]
# 热重载支持
hot_reload_enabled = true
# 详细日志
verbose_logging = true
```

### 故障排查指南

#### 常见问题和解决方案

| 问题类型 | 症状 | 可能原因 | 解决方案 |
|---------|------|---------|---------|
| **性能问题** | 匹配延迟高 | 复杂正则表达式过多 | 优化规则，使用精确匹配 |
| **内存问题** | 内存占用过高 | 缓存过大，规则过多 | 调整缓存大小，清理无用规则 |
| **匹配错误** | 规则不生效 | 优先级设置错误 | 检查规则优先级和条件 |
| **热重载失败** | 规则更新不生效 | 规则验证失败 | 检查规则格式和语法 |

#### 调试工具
```rust
// 调试辅助函数
impl RuleService {
    /// 导出调试信息
    pub async fn export_debug_info(&self) -> DebugInfo {
        let rules = self.active_rules.read().await;
        let patterns = self.compiled_patterns.read().await;
        let index = self.priority_index.read().await;
        
        DebugInfo {
            total_rules: rules.len(),
            compiled_patterns_count: patterns.len(),
            priority_levels: index.len(),
            cache_stats: self.get_cache_statistics().await,
            memory_usage: Self::estimate_memory_usage().await,
        }
    }
    
    /// 测试规则匹配
    pub async fn test_rule_matching(&self, test_input: &TestInput) -> TestResult {
        // 提供测试接口用于调试
    }
}
```

---

## 📋 总结

### 架构优势

1. **🚀 极致性能**
   - 热路径零序列化开销
   - 多级缓存和预编译优化
   - 微秒级匹配延迟

2. **🔧 开发友好**
   - 自包含中间件设计
   - 一个文件包含所有功能
   - TypeId类型安全依赖注入

3. **📊 运维友好**
   - 统一的状态管理和监控
   - 完善的性能指标和告警
   - 可选的外部API控制层

4. **🛡️ 生产就绪**
   - 权限控制和审计日志
   - 优雅的错误处理和恢复
   - 热重载和零停机更新

### 后续发展

1. **功能扩展**
   - 支持更多匹配模式
   - 机器学习规则优化
   - 分布式规则同步

2. **性能优化**  
   - SIMD并行匹配
   - 规则编译优化
   - 智能缓存预热

3. **生态集成**
   - Prometheus指标导出
   - Grafana监控面板
   - Kubernetes Operator

这个设计实现了**高性能**、**高可用**、**易维护**的规则匹配中间件，完全符合新架构的精简内核和插件化设计理念。

---

## 🚀 实现步骤分解

### 实施策略：简化渐进式开发

根据实际需求，采用**简化的增量式**开发策略，每个步骤都能直接在 `direct_demo` 中测试验证。

### Phase 1: 基础设施准备 (1天)

#### Step 1.1: 为 AppCore 添加服务容器 ⭐⭐⭐
```rust
// 文件: src/core/mod.rs
// 目标: 为现有 AppCore 添加 TypeId 服务容器

use std::any::{Any, TypeId};
use std::collections::HashMap;

// 1.1.1 定义服务容器
pub struct ServiceContainer {
    services: Arc<RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>>,
}

impl ServiceContainer {
    pub fn new() -> Self {
        Self {
            services: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    pub async fn register<T: Any + Send + Sync>(&self, service: T) {
        let type_id = TypeId::of::<T>();
        let mut services = self.services.write().await;
        services.insert(type_id, Arc::new(service));
    }
    
    pub async fn get<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        let type_id = TypeId::of::<T>();
        let services = self.services.read().await;
        services.get(&type_id)
            .and_then(|service| service.clone().downcast::<T>().ok())
    }
}

// 1.1.2 扩展现有 AppCore 结构
pub struct AppCore {
    // 现有字段保持不变...
    
    // 新增：服务容器 (默认就有，不是可选)
    services: ServiceContainer,
}

// 1.1.3 扩展 AppCore impl
impl AppCore {
    /// 现有构造器，添加服务容器初始化
    pub fn new(
        config: AppConfig,
        inbound_port: Arc<dyn InboundPort>,
        outbound_port: Arc<dyn OutboundPort>,
        control_port: Arc<dyn ControlPort>,
        event_port: Arc<dyn EventPort>,
    ) -> Self {
        // 现有逻辑保持不变...
        // 只添加服务容器初始化
        let services = ServiceContainer::new();
        
        Self {
            // 现有字段...
            services,
        }
    }
    
    /// Tauri 风格服务注册
    pub async fn manage<T: Any + Send + Sync>(&self, service: T) {
        self.services.register(service).await;
    }
    
    /// Tauri 风格服务查询  
    pub async fn state<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.services.get::<T>().await
    }
}
```

**测试要点**:
- ✅ 现有 AppCore 功能完全不变
- ✅ 新的服务注册和查询API工作正常
- ✅ direct_demo 正常运行

#### Step 1.2: 扩展现有 Middleware trait ⭐⭐
```rust
// 文件: src/core/engine.rs
// 目标: 为现有 Middleware 添加可选的服务注册功能

#[async_trait]  
pub trait Middleware: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> i32;
    fn enabled(&self) -> bool;
    
    /// 现有的处理方法
    async fn process(&self, context: &mut ConnectionContext, core: &AppCore) -> Result<MiddlewareResult, AppError>;
    
    /// 🆕 新增：可选的服务注册 (默认实现，兼容现有中间件)
    async fn register_services(&self, _app_core: &AppCore) -> Result<(), AppError> {
        Ok(()) // 默认什么也不做
    }
    
    /// 🆕 新增：可选的初始化 (默认实现)
    async fn initialize(&self, _app_core: &AppCore) -> Result<(), AppError> {
        Ok(()) // 默认什么也不做  
    }
}
```

**测试要点**:
- ✅ 所有现有中间件继续正常工作  
- ✅ 新中间件可以重写这些方法实现服务注册

### Phase 2: 完整版规则数据模型 (1-2天)

#### Step 2.1: 定义完整规则数据结构 ⭐⭐⭐
```rust
// 文件: src/core/middleware/rule_matching.rs (新建)
// 目标: 完整的规则数据模型

use crate::utils::generate_short_connection_id; // 使用现有的ID生成
use std::collections::HashMap;
use serde::{Serialize, Deserialize};

/// 规则定义 - 完整版本
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    pub id: String,  // 使用 utils::generate_short_connection_id()
    pub name: String,
    pub description: String,
    pub pattern: RulePattern,
    pub conditions: Vec<RuleCondition>,
    pub action: RuleAction,
    pub enabled: bool,
    pub priority: i32,
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Rule {
    pub fn new(name: &str, description: &str, pattern: RulePattern, action: RuleAction, priority: i32) -> Self {
        Self {
            id: generate_short_connection_id(),  // 使用现有工具方法
            name: name.to_string(),
            description: description.to_string(),
            pattern,
            conditions: Vec::new(),
            action,
            enabled: true,
            priority,
            metadata: HashMap::new(),
        }
    }
    
    /// 便捷构造器 - 兼容现有代码
    pub fn simple(name: &str, pattern: RulePattern, action: RuleAction, priority: i32) -> Self {
        Self::new(name, name, pattern, action, priority)
    }
}

/// 规则匹配模式 - 完整版本
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RulePattern {
    // 基础匹配
    HostMatches(String),              // 主机匹配：*.google.com
    PathMatches(String),              // 路径匹配：/api/*
    HeaderExists(String),             // 请求头存在：Authorization
    HeaderValueMatches { 
        header: String, 
        value: String 
    },                                // 请求头值匹配：User-Agent: *Chrome*
    PortMatches(u16),                 // 端口匹配：443
    ProcessMatches(String),           // 进程匹配：*curl*
    UserMatches(String),              // 用户匹配：john
    
    // 时间匹配
    TimeRange { 
        start: String,                // HH:MM 格式：09:00
        end: String                   // HH:MM 格式：17:00
    },
    WeekdayMatches(Vec<u8>),         // 星期几：[1,2,3,4,5] (Mon-Fri)
    
    // 复合匹配
    And(Vec<RulePattern>),           // 逻辑与
    Or(Vec<RulePattern>),            // 逻辑或
    Not(Box<RulePattern>),           // 逻辑非
    
    // 高级匹配
    Custom(String),                  // 自定义Lua脚本匹配
}

/// 规则条件 - 补充匹配条件
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuleCondition {
    pub name: String,
    pub condition_type: RuleConditionType,
    pub value: serde_json::Value,
    pub negated: bool,  // 是否取反
}

/// 规则条件类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RuleConditionType {
    // 网络条件
    SourceIPInRange,     // 源IP范围
    TargetIPInRange,     // 目标IP范围
    ConnectionCount,     // 连接数量限制
    BandwidthUsage,      // 带宽使用情况
    
    // 请求条件  
    RequestSize,         // 请求大小
    RequestMethod,       // HTTP方法
    ContentType,         // Content-Type
    
    // 应用条件
    ProcessPID,          // 进程PID
    ProcessParent,       // 父进程
    ProcessCPUUsage,     // CPU使用率
    ProcessMemoryUsage,  // 内存使用率
    
    // 自定义条件
    CustomScript,        // 自定义脚本
}

/// 规则动作 - 完整版本  
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RuleAction {
    // 基础动作
    Allow,                           // 允许通过 (无修改)
    Direct,                          // 直连
    Block,                           // 拒绝连接
    ProxyByName(String),            // 通过指定代理
    
    // 修改动作
    ModifyRequest(RequestModifications),      // 修改请求
    ModifyResponse(ResponseModifications),    // 修改响应
    RedirectTo(String),                       // 重定向到指定URL
    
    // 交互动作  
    InterceptForApproval,            // 拦截等待人工批准
    PauseForInspection(u64),        // 暂停指定秒数供检查
    
    // 脚本动作
    ExecuteScript(String),           // 执行指定脚本
    CompositeRequest(CompositeRequestConfig), // 复合请求处理
    
    // 日志动作
    Log(LogLevel),                   // 记录日志
    Alert(AlertConfig),             // 发送告警
    
    // 自定义动作
    Custom(HashMap<String, serde_json::Value>),
}

/// 请求修改配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestModifications {
    pub add_headers: Option<HashMap<String, String>>,
    pub remove_headers: Option<Vec<String>>,
    pub modify_headers: Option<HashMap<String, String>>,
    pub change_method: Option<String>,
    pub change_path: Option<String>,
    pub change_body: Option<String>,
}

/// 响应修改配置  
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResponseModifications {
    pub change_status: Option<u16>,
    pub add_headers: Option<HashMap<String, String>>,
    pub remove_headers: Option<Vec<String>>,
    pub modify_headers: Option<HashMap<String, String>>,
    pub change_body: Option<String>,
}

/// 复合请求配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompositeRequestConfig {
    pub additional_requests: Vec<HttpRequestConfig>,
    pub merge_strategy: MergeStrategy,
    pub timeout: u64, // 秒
}

/// HTTP请求配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HttpRequestConfig {
    pub url: String,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
}

/// 合并策略
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MergeStrategy {
    Replace,           // 使用新响应替换
    Merge,            // 合并响应内容
    Append,           // 追加到原响应
    Custom(String),   // 自定义合并脚本
}

/// 日志级别
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LogLevel {
    Debug,
    Info, 
    Warn,
    Error,
}

/// 告警配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertConfig {
    pub alert_type: AlertType,
    pub message: String,
    pub severity: AlertSeverity,
    pub channels: Vec<String>, // 告警渠道：email, slack, webhook等
}

/// 告警类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]  
pub enum AlertType {
    SecurityThreat,
    PerformanceIssue,  
    PolicyViolation,
    SystemAnomaly,
    Custom(String),
}

/// 告警严重级别
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AlertSeverity {
    Low,
    Medium, 
    High,
    Critical,
}

/// 匹配结果 - 完整版本
#[derive(Debug, Clone)]
pub struct MatchResult {
    pub rule_id: String,
    pub rule_name: String,
    pub rule_description: String,
    pub action: RuleAction,
    pub match_duration: Duration,
    pub matched_patterns: Vec<String>, // 哪些模式匹配了
    pub matched_conditions: Vec<String>, // 哪些条件匹配了
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
```

**测试要点**:
- ✅ 完整的数据结构可序列化/反序列化
- ✅ ID 生成方法正常工作
- ✅ 复杂规则模式的创建和验证
- ✅ 嵌套模式 (And/Or/Not) 处理正确
- ✅ 条件和动作的复杂配置
- ✅ 便捷构造器兼容现有代码

### Phase 3: 完整版规则服务和中间件 (2-3天)

#### Step 3.1: 完整版 RuleService ⭐⭐⭐
```rust
// 文件: src/core/middleware/rule_matching.rs
// 目标: 完整功能的规则服务

use std::time::{SystemTime, Duration, Instant};
use regex::Regex;
use globset::Glob;

pub struct RuleService {
    // 规则存储 - 按优先级排序
    active_rules: Arc<RwLock<Vec<Rule>>>,
    
    // 预编译缓存 - 性能优化
    compiled_rules: Arc<RwLock<HashMap<String, CompiledRule>>>,
    
    // 匹配缓存 - LRU缓存
    match_cache: Arc<Mutex<lru::LruCache<String, Option<MatchResult>>>>,
    
    // 统计信息
    stats: Arc<RwLock<RuleServiceStats>>,
}

#[derive(Debug, Default, Clone)]
pub struct RuleServiceStats {
    pub total_matches: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub average_match_time: Duration,
    pub rules_count: usize,
    pub compiled_rules_count: usize,
}

impl RuleService {
    pub fn new() -> Self {
        Self {
            active_rules: Arc::new(RwLock::new(Vec::new())),
            compiled_rules: Arc::new(RwLock::new(HashMap::new())),
            match_cache: Arc::new(Mutex::new(lru::LruCache::new(1000))), // 缓存1000个匹配结果
            stats: Arc::new(RwLock::new(RuleServiceStats::default())),
        }
    }
    
    /// 应用规则 (完整版) - 包含预编译和缓存清理
    pub async fn apply_rules(&self, mut rules: Vec<Rule>) -> Result<(), RuleServiceError> {
        info!("🔧 Applying {} rules to RuleService", rules.len());
        
        // 按优先级排序
        rules.sort_by_key(|r| r.priority);
        
        // 预编译所有规则
        let mut compiled_rules = HashMap::new();
        for rule in &rules {
            match self.compile_rule(rule).await {
                Ok(compiled) => {
                    compiled_rules.insert(rule.id.clone(), compiled);
                }
                Err(e) => {
                    warn!("⚠️ Failed to compile rule {}: {}", rule.name, e);
                    // 继续处理其他规则
                }
            }
        }
        
        // 更新规则存储
        {
            let mut active_rules = self.active_rules.write().await;
            *active_rules = rules;
        }
        
        // 更新编译缓存
        {
            let mut compiled = self.compiled_rules.write().await;
            *compiled = compiled_rules;
        }
        
        // 清理匹配缓存
        {
            let mut cache = self.match_cache.lock().await;
            cache.clear();
        }
        
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.rules_count = rules.len();
            stats.compiled_rules_count = compiled_rules.len();
        }
        
        info!("✅ Rules applied successfully: {} rules, {} compiled", 
              stats.rules_count, stats.compiled_rules_count);
        Ok(())
    }
    
    /// 获取活跃规则
    pub async fn get_active_rules(&self) -> Vec<Rule> {
        self.active_rules.read().await.clone()
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
                // 更新缓存命中统计
                {
                    let mut stats = self.stats.write().await;
                    stats.cache_hits += 1;
                }
                
                debug!("🎯 Cache hit for request matching");
                return cached_result.clone();
            }
        }
        
        // 缓存未命中，执行匹配
        let match_result = self.execute_match(context).await;
        let match_duration = start_time.elapsed();
        
        // 存储到缓存
        {
            let mut cache = self.match_cache.lock().await;
            cache.put(cache_key, match_result.clone());
        }
        
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.total_matches += 1;
            stats.cache_misses += 1;
            stats.average_match_time = Duration::from_nanos(
                ((stats.average_match_time.as_nanos() * (stats.total_matches - 1) as u128) 
                 + match_duration.as_nanos()) / stats.total_matches as u128
            );
        }
        
        match_result
    }
    
    /// 执行实际的规则匹配
    async fn execute_match(&self, context: &ConnectionContext) -> Option<MatchResult> {
        let compiled_rules = self.compiled_rules.read().await;
        let active_rules = self.active_rules.read().await;
        
        for rule in active_rules.iter() {
            if !rule.enabled {
                continue;
            }
            
            let start_time = Instant::now();
            let mut matched_patterns = Vec::new();
            let mut matched_conditions = Vec::new();
            
            // 匹配主模式
            if self.match_pattern(&rule.pattern, context, &compiled_rules, &mut matched_patterns).await {
                // 匹配附加条件
                if self.match_conditions(&rule.conditions, context, &mut matched_conditions).await {
                    let match_duration = start_time.elapsed();
                    
                    debug!("✅ Rule matched: {} -> {:?} ({}μs)", 
                           rule.name, rule.action, match_duration.as_micros());
                    
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
        
        debug!("ℹ️ No rules matched for this request");
        None
    }
    
    /// 匹配规则模式 - 支持所有模式类型
    async fn match_pattern(
        &self, 
        pattern: &RulePattern, 
        context: &ConnectionContext,
        compiled_rules: &HashMap<String, CompiledRule>,
        matched_patterns: &mut Vec<String>
    ) -> bool {
        match pattern {
            // 基础匹配
            RulePattern::HostMatches(host_pattern) => {
                let matches = self.wildcard_match(&context.target_host, host_pattern);
                if matches { matched_patterns.push(format!("host:{}", host_pattern)); }
                matches
            }
            
            RulePattern::PathMatches(path_pattern) => {
                if let Some(ref request) = context.request {
                    let matches = self.wildcard_match(&request.path, path_pattern);
                    if matches { matched_patterns.push(format!("path:{}", path_pattern)); }
                    matches
                } else { false }
            }
            
            RulePattern::HeaderExists(header_name) => {
                if let Some(ref request) = context.request {
                    let matches = request.headers.contains_key(header_name);
                    if matches { matched_patterns.push(format!("header_exists:{}", header_name)); }
                    matches
                } else { false }
            }
            
            RulePattern::HeaderValueMatches { header, value } => {
                if let Some(ref request) = context.request {
                    if let Some(header_value) = request.headers.get(header) {
                        let matches = self.wildcard_match(header_value, value);
                        if matches { matched_patterns.push(format!("header_value:{}={}", header, value)); }
                        matches
                    } else { false }
                } else { false }
            }
            
            RulePattern::PortMatches(port) => {
                let matches = context.target_port == *port;
                if matches { matched_patterns.push(format!("port:{}", port)); }
                matches
            }
            
            RulePattern::ProcessMatches(process_pattern) => {
                if let Some(ref process_name) = context.connection.metadata.process_name {
                    let matches = self.wildcard_match(process_name, process_pattern);
                    if matches { matched_patterns.push(format!("process:{}", process_pattern)); }
                    matches
                } else { false }
            }
            
            RulePattern::UserMatches(user_pattern) => {
                if let Some(ref user_name) = context.connection.metadata.user_name {
                    let matches = self.wildcard_match(user_name, user_pattern);
                    if matches { matched_patterns.push(format!("user:{}", user_pattern)); }
                    matches
                } else { false }
            }
            
            // 时间匹配
            RulePattern::TimeRange { start, end } => {
                let matches = self.match_time_range(start, end);
                if matches { matched_patterns.push(format!("time:{}−{}", start, end)); }
                matches
            }
            
            RulePattern::WeekdayMatches(weekdays) => {
                let matches = self.match_weekday(weekdays);
                if matches { matched_patterns.push(format!("weekday:{:?}", weekdays)); }
                matches
            }
            
            // 复合匹配
            RulePattern::And(patterns) => {
                let mut all_match = true;
                for sub_pattern in patterns {
                    if !self.match_pattern(sub_pattern, context, compiled_rules, matched_patterns).await {
                        all_match = false;
                        break;
                    }
                }
                all_match
            }
            
            RulePattern::Or(patterns) => {
                for sub_pattern in patterns {
                    if self.match_pattern(sub_pattern, context, compiled_rules, matched_patterns).await {
                        return true;
                    }
                }
                false
            }
            
            RulePattern::Not(pattern) => {
                !self.match_pattern(pattern, context, compiled_rules, matched_patterns).await
            }
            
            // 高级匹配
            RulePattern::Custom(script_code) => {
                self.execute_custom_match(script_code, context).await
            }
        }
    }
    
    /// 匹配规则条件
    async fn match_conditions(
        &self, 
        conditions: &[RuleCondition], 
        context: &ConnectionContext, 
        matched_conditions: &mut Vec<String>
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
    async fn match_single_condition(&self, condition: &RuleCondition, context: &ConnectionContext) -> bool {
        match condition.condition_type {
            RuleConditionType::SourceIPInRange => {
                // TODO: 实现IP范围匹配
                false
            }
            RuleConditionType::RequestSize => {
                // TODO: 实现请求大小匹配
                false  
            }
            RuleConditionType::ProcessPID => {
                // TODO: 实现PID匹配
                false
            }
            // ... 其他条件类型
            _ => {
                warn!("⚠️ Condition type {:?} not implemented yet", condition.condition_type);
                false
            }
        }
    }
    
    /// 高级通配符匹配 (支持 *, ?, [...])
    fn wildcard_match(&self, text: &str, pattern: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        
        // 基础通配符匹配
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
            text == pattern
        }
    }
    
    /// 时间范围匹配
    fn match_time_range(&self, start: &str, end: &str) -> bool {
        use chrono::{Local, Timelike};
        
        let now = Local::now();
        let current_time = format!("{:02}:{:02}", now.hour(), now.minute());
        
        current_time >= start && current_time <= end
    }
    
    /// 星期匹配 (1=Monday, 7=Sunday)
    fn match_weekday(&self, weekdays: &[u8]) -> bool {
        use chrono::{Local, Datelike, Weekday};
        
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
    fn compile_pattern(&self, pattern: &RulePattern, compiled_patterns: &mut Vec<CompiledPattern>) -> Result<(), RuleServiceError> {
        match pattern {
            RulePattern::HostMatches(host_pattern) => {
                if host_pattern.contains('*') || host_pattern.contains('?') {
                    // 通配符模式，转换为 glob
                    if let Ok(glob) = Glob::new(host_pattern) {
                        compiled_patterns.push(CompiledPattern {
                            original_pattern: pattern.clone(),
                            regex: None,
                            glob: Some(glob),
                            exact_match: None,
                            pattern_type: PatternType::Wildcard,
                        });
                    }
                } else {
                    // 精确匹配
                    compiled_patterns.push(CompiledPattern {
                        original_pattern: pattern.clone(),
                        regex: None,
                        glob: None,
                        exact_match: Some(host_pattern.clone()),
                        pattern_type: PatternType::Exact,
                    });
                }
            }
            
            // 复合模式递归编译
            RulePattern::And(patterns) | RulePattern::Or(patterns) => {
                for sub_pattern in patterns {
                    self.compile_pattern(sub_pattern, compiled_patterns)?;
                }
            }
            
            RulePattern::Not(pattern) => {
                self.compile_pattern(pattern, compiled_patterns)?;
            }
            
            // 其他模式...
            _ => {
                // 暂时使用默认编译
                compiled_patterns.push(CompiledPattern {
                    original_pattern: pattern.clone(),
                    regex: None,
                    glob: None,  
                    exact_match: None,
                    pattern_type: PatternType::Wildcard,
                });
            }
        }
        
        Ok(())
    }
    
    /// 生成缓存键
    fn generate_cache_key(&self, context: &ConnectionContext) -> String {
        format!("{}:{}:{}", 
                context.target_host, 
                context.target_port,
                context.connection.metadata.process_name.as_ref().unwrap_or(&"unknown".to_string())
        )
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> RuleServiceStats {
        self.stats.read().await.clone()
    }
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
```

#### Step 3.2: 完整版规则匹配中间件 ⭐⭐⭐
```rust
// 文件: src/core/middleware/rule_matching.rs  
// 目标: 实现完整的规则匹配中间件，处理所有动作类型

pub struct RuleMatchingMiddleware {
    enabled: bool,
    default_action: DefaultAction,
}

#[derive(Debug, Clone)]
pub enum DefaultAction {
    Allow,   // 无规则匹配时允许通过
    Block,   // 无规则匹配时阻止
    Direct,  // 无规则匹配时直连
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
    async fn register_services(&self, app_core: &AppCore) -> Result<(), AppError> {
        let rule_service = RuleService::new();
        app_core.manage(rule_service).await;
        info!("✅ RuleService registered to AppCore");
        Ok(())
    }
    
    /// 处理请求 - 完整的规则匹配和动作执行
    async fn process(&self, context: &mut ConnectionContext, core: &AppCore) -> Result<MiddlewareResult, AppError> {
        if let Some(rule_service) = core.state::<RuleService>().await {
            match rule_service.match_request(context).await {
                Some(match_result) => {
                    info!("✅ Rule matched: {} -> {:?} ({}μs)", 
                          match_result.rule_name, 
                          match_result.action,
                          match_result.match_duration.as_micros());
                    
                    // 存储详细匹配信息到context
                    context.custom_data.insert("matched_rule_id".to_string(), 
                        serde_json::json!(match_result.rule_id));
                    context.custom_data.insert("matched_rule_name".to_string(), 
                        serde_json::json!(match_result.rule_name));
                    context.custom_data.insert("matched_patterns".to_string(), 
                        serde_json::json!(match_result.matched_patterns));
                    context.custom_data.insert("matched_conditions".to_string(), 
                        serde_json::json!(match_result.matched_conditions));
                    context.custom_data.insert("match_duration_micros".to_string(), 
                        serde_json::json!(match_result.match_duration.as_micros()));
                    
                    // 执行规则动作
                    self.execute_rule_action(match_result.action, context, core).await
                }
                None => {
                    debug!("ℹ️ No rules matched, applying default action: {:?}", self.default_action);
                    
                    // 应用默认动作
                    match self.default_action {
                        DefaultAction::Allow | DefaultAction::Direct => {
                            context.custom_data.insert("rule_decision".to_string(), 
                                serde_json::json!("default_direct"));
                            Ok(MiddlewareResult::Continue)
                        }
                        DefaultAction::Block => {
                            warn!("❌ Connection blocked by default action");
                            Ok(MiddlewareResult::Reject("Blocked by default rule".to_string()))
                        }
                    }
                }
            }
        } else {
            warn!("⚠️ RuleService not available, skipping rule matching");
            Ok(MiddlewareResult::Continue)
        }
    }
    
    /// 执行规则动作 - 处理所有动作类型
    async fn execute_rule_action(
        &self, 
        action: RuleAction, 
        context: &mut ConnectionContext, 
        core: &AppCore
    ) -> Result<MiddlewareResult, AppError> {
        match action {
            // 基础动作
            RuleAction::Allow => {
                context.custom_data.insert("rule_decision".to_string(), 
                    serde_json::json!("allow"));
                Ok(MiddlewareResult::Continue)
            }
            
            RuleAction::Direct => {
                context.custom_data.insert("rule_decision".to_string(), 
                    serde_json::json!("direct"));
                Ok(MiddlewareResult::Continue)
            }
            
            RuleAction::Block => {
                warn!("❌ Connection blocked by rule");
                Ok(MiddlewareResult::Reject("Blocked by security rule".to_string()))
            }
            
            RuleAction::ProxyByName(proxy_name) => {
                context.custom_data.insert("rule_decision".to_string(), 
                    serde_json::json!("proxy"));
                context.custom_data.insert("routing_decision".to_string(), 
                    serde_json::json!({
                        "decision": "proxy_by_name", 
                        "proxy_name": proxy_name
                    }));
                Ok(MiddlewareResult::Continue)
            }
            
            // 修改动作
            RuleAction::ModifyRequest(modifications) => {
                info!("🔧 Applying request modifications");
                self.apply_request_modifications(modifications, context).await?;
                context.custom_data.insert("rule_decision".to_string(), 
                    serde_json::json!("modify_request"));
                Ok(MiddlewareResult::Continue)
            }
            
            RuleAction::ModifyResponse(modifications) => {
                info!("🔧 Scheduling response modifications");
                // 存储修改配置，在响应阶段应用
                context.custom_data.insert("response_modifications".to_string(), 
                    serde_json::json!(modifications));
                context.custom_data.insert("rule_decision".to_string(), 
                    serde_json::json!("modify_response"));
                Ok(MiddlewareResult::Continue)
            }
            
            RuleAction::RedirectTo(target_url) => {
                info!("🔀 Redirecting to: {}", target_url);
                // 构造重定向响应
                let redirect_response = HttpResponse::redirect(302, &target_url);
                context.response = Some(redirect_response);
                Ok(MiddlewareResult::Stop) // 停止后续处理，直接返回重定向
            }
            
            // 交互动作
            RuleAction::InterceptForApproval => {
                info!("✋ Intercepting request for manual approval");
                context.custom_data.insert("rule_decision".to_string(), 
                    serde_json::json!("intercept_for_approval"));
                
                // 触发用户干预流程
                if let Some(control_port) = core.control_port() {
                    let request_id = generate_short_connection_id();
                    control_port.intercept_connection(context.connection.id.clone()).await
                        .map_err(|e| AppError::Processing(format!("Failed to intercept connection: {}", e)))?;
                }
                
                Ok(MiddlewareResult::Pause("Waiting for manual approval".to_string()))
            }
            
            RuleAction::PauseForInspection(seconds) => {
                info!("⏸️ Pausing request for inspection: {}s", seconds);
                context.custom_data.insert("rule_decision".to_string(), 
                    serde_json::json!("pause_for_inspection"));
                context.custom_data.insert("pause_duration".to_string(), 
                    serde_json::json!(seconds));
                
                // 实现暂停逻辑
                tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
                Ok(MiddlewareResult::Continue)
            }
            
            // 脚本动作
            RuleAction::ExecuteScript(script_name) => {
                info!("📜 Executing script: {}", script_name);
                self.execute_rule_script(&script_name, context, core).await
            }
            
            RuleAction::CompositeRequest(composite_config) => {
                info!("🔀 Executing composite request");
                self.execute_composite_request(composite_config, context, core).await
            }
            
            // 日志动作
            RuleAction::Log(log_level) => {
                self.log_request(log_level, context);
                context.custom_data.insert("rule_decision".to_string(), 
                    serde_json::json!("log"));
                Ok(MiddlewareResult::Continue)
            }
            
            RuleAction::Alert(alert_config) => {
                info!("🚨 Triggering alert: {:?}", alert_config.alert_type);
                self.trigger_alert(alert_config, context, core).await?;
                context.custom_data.insert("rule_decision".to_string(), 
                    serde_json::json!("alert"));
                Ok(MiddlewareResult::Continue)
            }
            
            // 自定义动作
            RuleAction::Custom(custom_data) => {
                info!("🔧 Executing custom action");
                context.custom_data.insert("rule_decision".to_string(), 
                    serde_json::json!("custom"));
                context.custom_data.insert("custom_action_data".to_string(), 
                    serde_json::json!(custom_data));
                Ok(MiddlewareResult::Continue)
            }
        }
    }
    
    /// 应用请求修改
    async fn apply_request_modifications(
        &self, 
        modifications: RequestModifications, 
        context: &mut ConnectionContext
    ) -> Result<(), AppError> {
        if let Some(ref mut request) = context.request {
            // 添加头部
            if let Some(add_headers) = modifications.add_headers {
                for (key, value) in add_headers {
                    request.headers.insert(key, value);
                }
            }
            
            // 移除头部
            if let Some(remove_headers) = modifications.remove_headers {
                for header in remove_headers {
                    request.headers.remove(&header);
                }
            }
            
            // 修改头部
            if let Some(modify_headers) = modifications.modify_headers {
                for (key, value) in modify_headers {
                    request.headers.insert(key, value);
                }
            }
            
            // 修改方法
            if let Some(new_method) = modifications.change_method {
                request.method = new_method;
            }
            
            // 修改路径
            if let Some(new_path) = modifications.change_path {
                request.path = new_path;
            }
            
            // 修改请求体
            if let Some(new_body) = modifications.change_body {
                request.body = Some(new_body.into_bytes());
            }
        }
        
        Ok(())
    }
    
    /// 执行规则脚本
    async fn execute_rule_script(
        &self, 
        script_name: &str, 
        context: &mut ConnectionContext, 
        core: &AppCore
    ) -> Result<MiddlewareResult, AppError> {
        // TODO: 集成脚本引擎
        warn!("⚠️ Script execution not implemented yet: {}", script_name);
        context.custom_data.insert("rule_decision".to_string(), 
            serde_json::json!("script_execution"));
        Ok(MiddlewareResult::Continue)
    }
    
    /// 执行复合请求
    async fn execute_composite_request(
        &self, 
        composite_config: CompositeRequestConfig, 
        context: &mut ConnectionContext, 
        core: &AppCore
    ) -> Result<MiddlewareResult, AppError> {
        // TODO: 实现复合请求处理
        warn!("⚠️ Composite request not implemented yet");
        context.custom_data.insert("rule_decision".to_string(), 
            serde_json::json!("composite_request"));
        Ok(MiddlewareResult::Continue)
    }
    
    /// 记录请求日志
    fn log_request(&self, log_level: LogLevel, context: &ConnectionContext) {
        let message = format!("Rule-triggered log: {} -> {}", 
                            context.target_host, context.target_port);
        
        match log_level {
            LogLevel::Debug => debug!("{}", message),
            LogLevel::Info => info!("{}", message),
            LogLevel::Warn => warn!("{}", message),
            LogLevel::Error => error!("{}", message),
        }
    }
    
    /// 触发告警
    async fn trigger_alert(
        &self, 
        alert_config: AlertConfig, 
        context: &ConnectionContext, 
        core: &AppCore
    ) -> Result<(), AppError> {
        // TODO: 实现告警系统
        warn!("🚨 ALERT [{}]: {} - {}",
              alert_config.severity,
              alert_config.alert_type,
              alert_config.message);
        Ok(())
    }
}
```

### Phase 4: 集成到 direct_demo (0.5天)

#### Step 4.1: StateManager 集成到 direct_demo ⭐⭐⭐
```rust  
// 文件: examples/http2_direct_demo.rs
// 目标: 将 StateManager 集成到 demo 中，使用完整规则系统

use takeover::core::services::StateManager;
use takeover::core::middleware::rule_matching::{
    RuleMatchingMiddleware, RuleService, Rule, RulePattern, RuleAction, 
    RuleCondition, RuleConditionType, RequestModifications, DefaultAction
};

async fn build_demo_app(config: AppConfig) -> Result<AppCore> {
    info!("🏗️ 构建HTTP/2 Direct Demo应用程序 (with StateManager + 完整规则系统)");
    
    let config = Arc::new(config);
    
    // 1. 创建 StateManager
    let state_manager_config = StateManagerConfig {
        max_connections: 1000,
        max_sessions: 500,
        max_cache_entries: 2000,
        max_temp_data: 1000,
        connection_timeout: Duration::from_secs(300),
        session_timeout: Duration::from_secs(1800),
        cache_timeout: Duration::from_secs(600),
        temp_data_timeout: Duration::from_secs(300),
        cleanup_interval: Duration::from_secs(60),
        enable_statistics: true,
        statistics_retention: Duration::from_secs(3600),
    };
    
    let state_manager = Arc::new(StateManager::new(state_manager_config).await?);
    
    // 2. 创建适配器
    let inbound_adapter = create_inbound_adapter(&config).await?;
    let outbound_adapter = create_outbound_adapter(&config).await?;
    let control_adapter = create_control_adapter().await?;
    let event_port = Arc::new(SimpleEventPort::new());
    
    // 3. 组装应用核心
    let mut app_core = AppCore::new(
        (*config).clone(),
        inbound_adapter,
        outbound_adapter,
        control_adapter,
        event_port,
    );
    
    // 4. 注册 StateManager 到 AppCore 服务容器
    app_core.manage(state_manager).await;
    info!("✅ StateManager registered to AppCore");
    
    // 5. 添加规则匹配中间件
    let rule_middleware = RuleMatchingMiddleware::new()
        .with_default_action(DefaultAction::Direct); // 默认直连
    rule_middleware.register_services(&app_core).await?;
    // 注意：这里需要 AppCore 支持中间件注册，可能需要修改现有的中间件管理
    
    // 6. 配置完整的测试规则
    if let Some(rule_service) = app_core.state::<RuleService>().await {
        let test_rules = vec![
            // Rule 1: Google域名代理规则 (带时间和星期限制)
            Rule {
                id: generate_short_connection_id(),
                name: "Google域名工作时间代理".to_string(),
                description: "工作时间访问Google域名通过代理".to_string(),
                pattern: RulePattern::And(vec![
                    RulePattern::HostMatches("*.google.com".to_string()),
                    RulePattern::TimeRange { start: "09:00".to_string(), end: "18:00".to_string() },
                    RulePattern::WeekdayMatches(vec![1, 2, 3, 4, 5]), // 周一到周五
                ]),
                conditions: vec![],
                action: RuleAction::ProxyByName("default".to_string()),
                enabled: true,
                priority: 10,
                metadata: HashMap::new(),
            },
            
            // Rule 2: curl程序代理规则 (带请求修改)
            Rule {
                id: generate_short_connection_id(),
                name: "curl程序代理和标记".to_string(),
                description: "curl程序请求通过代理，并添加自定义头部".to_string(),
                pattern: RulePattern::ProcessMatches("*curl*".to_string()),
                conditions: vec![],
                action: RuleAction::ModifyRequest(RequestModifications {
                    add_headers: Some(HashMap::from([
                        ("X-Proxy-By".to_string(), "Takeover-Proxy".to_string()),
                        ("X-Request-Time".to_string(), chrono::Utc::now().to_rfc3339()),
                    ])),
                    remove_headers: None,
                    modify_headers: None,
                    change_method: None,
                    change_path: None,
                    change_body: None,
                }),
                enabled: true,
                priority: 15,
                metadata: HashMap::new(),
            },
            
            // Rule 3: 安全域名拦截规则
            Rule {
                id: generate_short_connection_id(),
                name: "恶意域名拦截".to_string(),
                description: "拦截已知恶意域名访问".to_string(),
                pattern: RulePattern::Or(vec![
                    RulePattern::HostMatches("*.malware.com".to_string()),
                    RulePattern::HostMatches("*.phishing.net".to_string()),
                    RulePattern::HostMatches("*suspicious*".to_string()),
                ]),
                conditions: vec![],
                action: RuleAction::Block,
                enabled: true,
                priority: 5, // 高优先级
                metadata: HashMap::new(),
            },
            
            // Rule 4: API请求日志规则
            Rule {
                id: generate_short_connection_id(),
                name: "API请求日志记录".to_string(),
                description: "记录所有API请求访问".to_string(),
                pattern: RulePattern::PathMatches("/api/*".to_string()),
                conditions: vec![],
                action: RuleAction::Log(LogLevel::Info),
                enabled: true,
                priority: 25,
                metadata: HashMap::new(),
            },
            
            // Rule 5: 大文件下载暂停检查
            Rule {
                id: generate_short_connection_id(), 
                name: "大文件下载检查".to_string(),
                description: "大文件下载暂停3秒供检查".to_string(),
                pattern: RulePattern::And(vec![
                    RulePattern::PathMatches("*.zip".to_string()),
                    RulePattern::Or(vec![
                        RulePattern::PathMatches("*.exe".to_string()),
                        RulePattern::PathMatches("*.dmg".to_string()),
                        RulePattern::PathMatches("*.pkg".to_string()),
                    ]),
                ]),
                conditions: vec![],
                action: RuleAction::PauseForInspection(3), // 暂停3秒
                enabled: true,
                priority: 20,
                metadata: HashMap::new(),
            },
            
            // Rule 6: 开发环境重定向
            Rule {
                id: generate_short_connection_id(),
                name: "开发环境重定向".to_string(),
                description: "将开发域名重定向到本地".to_string(),
                pattern: RulePattern::HostMatches("dev.example.com".to_string()),
                conditions: vec![],
                action: RuleAction::RedirectTo("http://localhost:3000".to_string()),
                enabled: true,
                priority: 12,
                metadata: HashMap::new(),
            },
            
            // Rule 7: 默认直连规则 (兜底)
            Rule {
                id: generate_short_connection_id(),
                name: "默认直连规则".to_string(),
                description: "所有其他请求默认直连".to_string(),
                pattern: RulePattern::HostMatches("*".to_string()),
                conditions: vec![],
                action: RuleAction::Direct,
                enabled: true,
                priority: 100, // 最低优先级
                metadata: HashMap::new(),
            },
        ];
        
        rule_service.apply_rules(test_rules).await.map_err(|e| 
            anyhow::anyhow!("Failed to apply rules: {}", e))?;
            
        info!("✅ 完整测试规则已配置:");
        info!("   1. Google域名工作时间代理 (带时间和星期限制)");
        info!("   2. curl程序代理和请求标记");
        info!("   3. 恶意域名拦截 (安全规则)");
        info!("   4. API请求日志记录");
        info!("   5. 大文件下载检查 (暂停3秒)");
        info!("   6. 开发环境重定向 (dev.example.com → localhost:3000)");
        info!("   7. 默认直连规则 (兜底)");
        
        // 显示规则服务统计
        let stats = rule_service.get_stats().await;
        info!("📊 RuleService统计: {} 条规则, {} 条已编译", 
              stats.rules_count, stats.compiled_rules_count);
    } else {
        warn!("⚠️ RuleService not available, skipping rule configuration");
    }
    
    info!("✅ Demo应用程序构建完成 (StateManager + 完整规则系统)");
    Ok(app_core)
}
```

#### Step 4.2: 在 direct_demo 中测试 ⭐⭐
```bash
# 启动 direct_demo
cargo run --example http2_direct_demo

# 测试规则匹配
curl https://www.google.com/  # 应该走代理
curl https://httpbin.org/ip   # 应该直连

# 查看日志输出，确认：
# ✅ StateManager 正常初始化
# ✅ RuleService 注册成功  
# ✅ 规则匹配正常工作
# ✅ 路由决策正确
```

**测试要点**:
- ✅ StateManager 正确集成并运行
- ✅ RuleService 注册和查询正常
- ✅ 规则匹配逻辑正确工作
- ✅ 与现有中间件协同正常

## 📋 **简化实施时间线**

### 🎯 Sprint 规划 (总计4-5天)

| 阶段 | 时间 | 目标 | 可交付成果 |
|-----|------|------|-----------|
| **Phase 1** | 1天 | 基础设施 | ✅ AppCore 服务容器，Middleware trait 扩展 |
| **Phase 2** | 1-2天 | 完整数据模型 | ✅ 完整规则数据结构，支持所有模式和动作 |
| **Phase 3** | 2-3天 | 完整核心服务 | ✅ 完整版 RuleService + RuleMatchingMiddleware |
| **Phase 4** | 0.5天 | Demo 集成 | ✅ StateManager 集成，完整规则系统测试 |

### 🏆 **唯一里程碑: 基础可用版本** 

**验收标准** (4-5天内完成):
- [ ] AppCore 服务容器正常工作
- [ ] RuleService 注册和查询正常  
- [ ] 完整版规则匹配功能正确
- [ ] 支持所有规则模式: 主机、路径、头部、端口、进程、用户、时间、星期、复合模式
- [ ] 支持所有规则动作: 基础动作、修改动作、交互动作、脚本动作、日志动作
- [ ] StateManager 集成到 direct_demo
- [ ] 通过 direct_demo 手动测试验证完整规则系统
- [ ] 规则编译和缓存系统工作正常
- [ ] 性能统计和日志输出正常

### 🔧 **开发建议**

#### 实施顺序
1. **Step 1.1-1.2** - 先搞定服务容器和 Middleware trait 扩展
2. **Step 2.1** - 定义简化的规则数据结构  
3. **Step 3.1-3.2** - 实现 RuleService 和中间件逻辑
4. **Step 4.1-4.2** - 集成到 direct_demo 并测试

#### 测试策略
- **不写单独的集成测试** - 直接用 `direct_demo` 验证
- **手动测试为主** - `curl` 命令验证规则匹配
- **日志观察** - 通过日志确认服务注册和匹配逻辑

#### 质量保证
- **保持向后兼容** - 现有功能完全不受影响
- **增量验证** - 每个阶段都要在 direct_demo 中测试
- **简化为王** - 先实现基础功能，不追求复杂特性

### 📊 **成功标志**

完成后，你应该能够：
1. ✅ `cargo run --example http2_direct_demo` 正常启动
2. ✅ 日志显示 "StateManager registered to AppCore"
3. ✅ 日志显示 "RuleService registered to AppCore"
4. ✅ 日志显示 "完整测试规则已配置: 7条规则"
5. ✅ 日志显示 "RuleService统计: 7 条规则, 7 条已编译"
6. ✅ **完整规则测试**:
   - `curl https://www.google.com/` → 根据时间判断是否走代理 (工作时间规则)
   - `curl https://httpbin.org/ip` → 显示 "Rule matched: 默认直连规则"
   - `curl https://malware.com/` → 显示 "Connection blocked by rule: 恶意域名拦截"  
   - `curl http://dev.example.com/` → 重定向到 `localhost:3000`
   - `curl https://api.example.com/users` → 显示 "Rule matched: API请求日志记录" 
   - 测试请求修改: curl 程序自动添加 `X-Proxy-By: Takeover-Proxy` 头部

这样的实施策略确保了：
- ✅ **功能完整** - 4-5天完成完整规则系统
- ✅ **风险可控** - 每步都在已知环境中测试
- ✅ **向后兼容** - 不破坏任何现有功能
- ✅ **强大灵活** - 支持复杂规则模式和多种动作类型
