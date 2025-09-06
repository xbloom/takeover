//! 处理引擎模块
//! 
//! 这个模块实现了中间件管道架构，负责：
//! - 构建处理管道
//! - 执行中间件链
//! - 管理处理流程
//! - 处理错误和异常情况

use std::sync::Arc;
use async_trait::async_trait;
use tracing::{debug, error, instrument};

use super::model::*;

use crate::error::TakeoverError;

/// 中间件处理结果
#[derive(Debug)]
pub enum MiddlewareResult {
    /// 继续处理下一个中间件
    Continue,
    /// 暂停处理，等待用户干预
    Pause(String),
    /// 拒绝请求
    Reject(String),
    /// 重定向到其他URL
    Redirect(String),
    /// 停止处理，中间件已接管控制（用于HTTP/2复杂Pipeline）
    Stop,
}

/// 中间件接口
/// 
/// 所有中间件都必须实现这个接口
#[async_trait]
pub trait Middleware: Send + Sync {
    /// 中间件名称
    fn name(&self) -> &str;
    
    /// 处理连接上下文
    /// 
    /// # 参数
    /// - `context`: 连接上下文，可以被修改
    /// - `core`: AppCore引用，用于访问端口和服务
    /// 
    /// # 返回
    /// 返回处理结果，决定是否继续处理
    async fn process(
        &self,
        context: &mut ConnectionContext,
        core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, TakeoverError>;
    
    /// 中间件优先级（数字越小优先级越高）
    fn priority(&self) -> i32 {
        0
    }
    
    /// 是否启用
    fn enabled(&self) -> bool {
        true
    }
}

/// 处理管道
/// 
/// 管道按顺序执行一系列中间件
pub struct Pipeline {
    middlewares: Vec<Arc<dyn Middleware>>,
}

impl Pipeline {
    /// 创建新的管道
    pub fn new() -> Self {
        Self {
            middlewares: Vec::new(),
        }
    }
    
    /// 从middleware列表创建pipeline（推荐使用）
    pub fn from_middlewares(middlewares: Vec<Arc<dyn Middleware>>) -> Self {
        Self { middlewares }
    }
    
    /// 添加中间件（用于构建器模式）
    pub fn add_middleware(mut self, middleware: Arc<dyn Middleware>) -> Self {
        self.middlewares.push(middleware);
        self
    }
    
    /// 执行管道
    #[instrument(skip(self, context, core), fields(connection_id = %context.id))]
    pub async fn process(
        &self,
        context: &mut ConnectionContext,
        core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, TakeoverError> {
        debug!("开始Pipeline执行");
        
        for middleware in &self.middlewares {
            if !middleware.enabled() {
                debug!("Skipping disabled middleware: {}", middleware.name());
                continue;
            }
            
            debug!("Executing middleware: {}", middleware.name());
            
            match middleware.process(context, core).await {
                Ok(MiddlewareResult::Continue) => {
                    debug!("Middleware {} completed successfully", middleware.name());
                    continue;
                }
                Ok(result) => {
                    debug!("Middleware {} returned: {:?}", middleware.name(), result);
                    return Ok(result);
                }
                Err(e) => {
                    error!("Middleware {} failed: {}", middleware.name(), e);
                    return Err(e);
                }
            }
        }
        
        debug!("Pipeline execution completed successfully");
        Ok(MiddlewareResult::Continue)
    }
    
    /// 获取中间件列表
    pub fn middlewares(&self) -> &[Arc<dyn Middleware>] {
        &self.middlewares
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

/// 管道构建器
/// 
/// 用于构建和配置处理管道
pub struct PipelineBuilder {
    middlewares: Vec<Arc<dyn Middleware>>,
}

impl PipelineBuilder {
    /// 创建新的构建器
    pub fn new() -> Self {
        Self {
            middlewares: Vec::new(),
        }
    }
    
    /// 添加中间件
    pub fn add(mut self, middleware: Arc<dyn Middleware>) -> Self {
        self.middlewares.push(middleware);
        self
    }
    
    /// 按优先级排序中间件
    pub fn sort_by_priority(mut self) -> Self {
        self.middlewares.sort_by_key(|m| m.priority());
        self
    }
    
    /// 构建管道
    pub fn build(self) -> Pipeline {
        let mut pipeline = Pipeline::new();
        for middleware in self.middlewares {
            pipeline = pipeline.add_middleware(middleware);
        }
        pipeline
    }
}

impl Default for PipelineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// 处理引擎
/// 
/// 负责管理和执行处理管道
/// 
/// **正确的架构设计**：
/// - middleware是全局单例，在引擎中注册一次
/// - pipeline是预构建的执行序列，可以缓存复用
/// - 避免每个连接都重建pipeline
pub struct ProcessingEngine {
    /// 全局middleware注册表 - 所有middleware的唯一存储位置
    middlewares: Vec<Arc<dyn Middleware>>,
    // 注意：移除了cached_pipelines和middleware_templates字段
    // 现在采用简化架构：middleware直接注册，pipeline按需构建
}

impl ProcessingEngine {
    /// 添加中间件到全局注册表
    pub fn add_middleware(&mut self, middleware: Arc<dyn Middleware>) {
        tracing::debug!("📋 注册中间件: {} (优先级: {})", middleware.name(), middleware.priority());
        self.middlewares.push(middleware);
        // 按优先级排序
        self.middlewares.sort_by_key(|m| m.priority());
    }
    
    /// 获取所有中间件（仅供调试，不推荐直接使用）
    pub fn get_middlewares(&self) -> &Vec<Arc<dyn Middleware>> {
        &self.middlewares
    }
    
    /// 获取预构建的默认pipeline（推荐使用）
    /// 
    /// 注意：当前实现每次都构建新的pipeline，后续应该优化为缓存
    pub fn get_default_pipeline(&self) -> Pipeline {
        self.build_default_pipeline()
    }
    
    /// 获取指定名称的pipeline（如果不存在则动态构建）
    pub fn get_pipeline(&self, name: &str) -> Pipeline {
        match name {
            "default" => self.build_default_pipeline(),
            "http2_business" => self.create_child_pipeline(20),
            _ => {
                tracing::warn!("未知的pipeline名称: {}，返回默认pipeline", name);
                self.build_default_pipeline()
            }
        }
    }
    
    /// 创建子Pipeline - 工厂方法（用于HTTP/2等特殊场景）
    /// 
    /// 用于HTTP/2中间件动态创建包含指定优先级及以上中间件的子Pipeline
    /// 
    /// # 参数
    /// - `start_priority`: 起始优先级，只包含此优先级及以上的中间件
    /// 
    /// # 示例
    /// ```rust
    /// // 创建从Rule中间件开始的子Pipeline（跳过TLS和HTTP/2）
    /// let child_pipeline = engine.create_child_pipeline(30);
    /// ```
    pub fn create_child_pipeline(&self, start_priority: i32) -> Pipeline {
        debug!("创建子Pipeline，起始优先级: {}", start_priority);
        
        let mut filtered_middlewares = Vec::new();
        let mut included_count = 0;
        
        for middleware in &self.middlewares {
            if middleware.priority() >= start_priority {
                tracing::debug!("  ✅ 包含中间件: {} (优先级: {})", middleware.name(), middleware.priority());
                filtered_middlewares.push(Arc::clone(middleware));
                included_count += 1;
            } else {
                tracing::debug!("  ⏭️  跳过中间件: {} (优先级: {})", middleware.name(), middleware.priority());
            }
        }
        
        debug!("子Pipeline创建完成，包含 {} 个中间件", included_count);
        Pipeline::from_middlewares(filtered_middlewares)
    }
    
    // 注意：prebuild_pipelines() 方法已废弃
    // 现在pipeline按需动态构建，性能足够且简化了架构
    
    /// 构建默认pipeline
    fn build_default_pipeline(&self) -> Pipeline {
        let enabled_middlewares: Vec<_> = self.middlewares
            .iter()
            .filter(|m| m.enabled())
            .cloned()
            .collect();
        
        Pipeline::from_middlewares(enabled_middlewares)
    }
}

// 注意：MiddlewareTemplate、MiddlewareConfig、TlsInterceptionMode 已废弃
// 现在middleware直接创建，不再使用模板模式

impl ProcessingEngine {
    /// 创建新的处理引擎
    pub fn new() -> Self {
        Self {
            middlewares: Vec::new(),
        }
    }
    
    /// 创建默认中间件列表
    /// 
    /// 只创建不需要端口依赖的核心中间件
    /// 其他需要端口的中间件在后续通过`add_middleware`添加
    pub fn create_default_middlewares() -> Vec<Arc<dyn Middleware>> {
        use crate::core::middleware;
        
        vec![
            // 🔍 协议检测中间件 (优先级5) - 最早执行，协议分流
            Arc::new(middleware::protocol_detection::ProtocolDetectionMiddleware::new()),
            
            // 🔐 TLS中间件 (优先级10) - 处理HTTPS拦截和ALPN协商
            Arc::new(middleware::tls::TlsMiddleware::with_config(
                middleware::tls::TlsMiddlewareConfig::default()
            )),
            
            // 🌐 HTTP/2流中间件 (优先级15) - HTTP/2协议完整生命周期
            Arc::new(middleware::http2_stream::Http2StreamMiddleware::new()),
            
            // 🌐 HTTP/1.1流中间件 (优先级16) - HTTP/1.1协议解析
            Arc::new(middleware::http1_stream::Http1StreamMiddleware::new()),
            
            // 🎯 规则匹配中间件 (优先级30) - 业务逻辑开始
            Arc::new(middleware::rule_matching::RuleMatchingMiddleware::new()),
            
            // 👤 用户干预中间件 (优先级40) - 人工审批
            Arc::new(middleware::user_intervention::UserInterventionMiddleware::new(
                middleware::user_intervention::UserInterventionConfig::default()
            )),
            
            // ✏️ HTTP修改中间件 (优先级50) - 请求/响应修改
            Arc::new(middleware::http_modification::HttpModificationMiddleware::new()),
            
            // 🔄 转发中间件 (优先级60) - 关键！负责实际的网络请求
            Arc::new(middleware::forwarding::ForwardingMiddleware::new()),
            
            // 📤 响应写回中间件 (优先级75) - 统一HTTP/1.1响应输出
            Arc::new(middleware::response_writeback::ResponseWriteBackMiddleware::new()),
        ]
        
        // 注意：以下中间件需要端口依赖，在new_with_all_ports中单独添加：
        // - ScriptMiddleware::new(script_port, api_port)
        // - ResponseMergeMiddleware::new(api_port, config).await
    }
    
    /// 创建带有默认中间件的新引擎
    pub fn new_with_defaults() -> Self {
        let mut engine = Self::new();
        let default_middlewares = Self::create_default_middlewares();
        for middleware in default_middlewares {
            engine.add_middleware(middleware);
        }
        engine
    }
    
    // 注意：create_default_template() 方法已废弃
    // 现在middleware直接在 ProcessingEngine::create_default_middlewares() 中创建
    
    // 注意：build_pipeline() 方法已废弃
    // 现在直接使用 get_default_pipeline() 或 get_pipeline() 方法
    
    // 注意：create_middleware_from_template() 方法已废弃
    // 现在直接在 ProcessingEngine::create_default_middlewares() 中创建middleware
    
    // 注意：template相关方法已废弃
    // 现在middleware直接在ProcessingEngine创建时注册，不再使用模板模式
}

impl Default for ProcessingEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ================================
// 注意：占位符中间件和工厂函数已废弃
// 现在middleware直接在AppCore中创建和注册
// ================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    
    // ================================
    // 测试用的简单中间件
    // ================================
    
    /// 测试用的简单中间件
    struct TestMiddleware {
        name: String,
        test_priority: i32,
    }
    
    impl TestMiddleware {
        pub fn new(name: impl Into<String>) -> Self {
            Self {
                name: name.into(),
                test_priority: 50, // 默认优先级
            }
        }
        
        pub fn with_priority(name: impl Into<String>, priority: i32) -> Self {
            Self {
                name: name.into(),
                test_priority: priority,
            }
        }
    }
    
    #[async_trait]
    impl Middleware for TestMiddleware {
        fn name(&self) -> &str {
            &self.name
        }
        
        async fn process(
            &self,
            _context: &mut ConnectionContext,
            _core: &crate::core::AppCore,
        ) -> Result<MiddlewareResult, TakeoverError> {
            // 简单的测试处理
            Ok(MiddlewareResult::Continue)
        }
        
        fn priority(&self) -> i32 {
            self.test_priority
        }
        
        fn enabled(&self) -> bool {
            true
        }
    }
    
    #[tokio::test]
    async fn test_pipeline_creation() {
        let pipeline = PipelineBuilder::new()
            .add(Arc::new(TestMiddleware::new("test1")))
            .add(Arc::new(TestMiddleware::new("test2")))
            .build();
        assert_eq!(pipeline.middlewares().len(), 2);
    }
    
    #[tokio::test]
    async fn test_middleware_execution() {
        let middleware = TestMiddleware::new("test");
        
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        
        let _context = ConnectionContext::new(
            connection,
            "example.com".to_string(),
            80
        );
        
        // 测试基本属性
        assert_eq!(middleware.name(), "test");
        assert!(middleware.enabled());
        assert_eq!(middleware.priority(), 50);
    }
    
    #[test]
    fn test_processing_engine_new_architecture() {
        // 测试新的简化架构
        let mut engine = ProcessingEngine::new();
        
        // 验证引擎初始状态
        assert_eq!(engine.get_middlewares().len(), 0);
        
        // 添加几个测试middleware
        engine.add_middleware(Arc::new(TestMiddleware::new("test1")));
        engine.add_middleware(Arc::new(TestMiddleware::new("test2")));
        
        // 验证middleware被正确注册
        assert_eq!(engine.get_middlewares().len(), 2);
        
        // 验证pipeline可以正确创建
        let pipeline = engine.get_default_pipeline();
        assert_eq!(pipeline.middlewares().len(), 2);
        
        println!("✅ 新架构引擎测试通过");
    }
    
    #[test]
    fn test_pipeline_builder() {
        let pipeline = PipelineBuilder::new()
            .add(Arc::new(TestMiddleware::new("first")))
            .add(Arc::new(TestMiddleware::new("second")))
            .build();
        
        assert_eq!(pipeline.middlewares().len(), 2);
    }
    
    #[test]
    fn test_middleware_priority_sorting() {
        struct HighPriorityMiddleware;
        struct LowPriorityMiddleware;
        
        #[async_trait]
        impl Middleware for HighPriorityMiddleware {
            fn name(&self) -> &str { "high" }
            fn priority(&self) -> i32 { 1 }
            async fn process(&self, _context: &mut ConnectionContext, _core: &crate::core::AppCore) -> Result<MiddlewareResult, TakeoverError> {
                Ok(MiddlewareResult::Continue)
            }
        }
        
        #[async_trait]
        impl Middleware for LowPriorityMiddleware {
            fn name(&self) -> &str { "low" }
            fn priority(&self) -> i32 { 10 }
            async fn process(&self, _context: &mut ConnectionContext, _core: &crate::core::AppCore) -> Result<MiddlewareResult, TakeoverError> {
                Ok(MiddlewareResult::Continue)
            }
        }
        
        let pipeline = PipelineBuilder::new()
            .add(Arc::new(LowPriorityMiddleware))
            .add(Arc::new(HighPriorityMiddleware))
            .sort_by_priority()
            .build();
        
        assert_eq!(pipeline.middlewares()[0].name(), "high");
        assert_eq!(pipeline.middlewares()[1].name(), "low");
    }
}
