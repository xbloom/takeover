//! HTTP/2流处理中间件
//! 
//! 基于HTTPS + HTTP/2架构设计的现代化中间件：
//! - 使用hyper处理HTTP/2协议复杂性
//! - 面向流的并发处理
//! - 分层架构：TLS → HTTP/2 → 业务逻辑
//! - 零拷贝优化关键路径

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, error, debug, instrument};
use async_trait::async_trait;

use hyper::{Request, Response, Version};
use hyper::body::Incoming;
use http_body_util::Full;
use bytes::Bytes;
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioExecutor};
use tokio_rustls::{server::TlsStream as InboundTlsStream};
use tokio::net::TcpStream;
use uuid::Uuid;

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{ConnectionContext, ProcessingState, HttpRequest};
use crate::error::TakeoverError;

/// HTTP/2流处理中间件
/// 
/// 负责处理HTTPS + HTTP/2连接的现代化中间件
pub struct Http2StreamMiddleware {
    /// 配置
    config: Http2Config,
    /// 统计信息
    stats: Arc<RwLock<Http2Stats>>,
    /// HTTP/2连接配置
    _phantom: std::marker::PhantomData<()>,
}

/// HTTP/2配置
#[derive(Debug, Clone)]
pub struct Http2Config {
    /// 是否启用
    pub enabled: bool,
    /// 最大并发流数
    pub max_concurrent_streams: u32,
    /// 初始窗口大小
    pub initial_window_size: u32,
    /// 最大帧大小
    pub max_frame_size: u32,
    /// 是否启用服务器推送
    pub enable_push: bool,
    /// 连接超时
    pub connection_timeout: std::time::Duration,
}

/// HTTP/2统计信息
#[derive(Debug, Default, Clone)]
pub struct Http2Stats {
    /// 总连接数
    total_connections: u64,
    /// 活跃连接数
    active_connections: u64,
    /// 总流数
    total_streams: u64,
    /// 活跃流数
    active_streams: u64,
    /// 成功处理的请求数
    successful_requests: u64,
    /// 失败的请求数
    failed_requests: u64,
    /// 服务器推送数
    #[allow(dead_code)]
    server_pushes: u64,
}

/// HTTP/2流上下文
#[derive(Debug, Clone)]
pub struct StreamContext {
    /// 连接ID
    pub connection_id: uuid::Uuid,
    /// 流ID (HTTP/2)
    pub stream_id: Option<u32>,
    /// 协议版本
    pub version: Version,
    /// 连接元数据
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Http2StreamMiddleware {
    /// 创建新的HTTP/2流中间件
    pub fn new() -> Self {
        Self {
            config: Http2Config::default(),
            stats: Arc::new(RwLock::new(Http2Stats::default())),
            _phantom: std::marker::PhantomData,
        }
    }
    
    /// 使用配置创建中间件
    pub fn with_config(config: Http2Config) -> Self {
        Self {
            config,
            stats: Arc::new(RwLock::new(Http2Stats::default())),
            _phantom: std::marker::PhantomData,
        }
    }
    
    /// 处理HTTPS + HTTP/2连接 - 现代化实现
    /// 
    /// 现在使用具体的rustls-tokio类型，完全兼容tokio::io traits
    #[instrument(skip(client_tls_stream, connection_context, core, stats))]
    pub async fn handle_https_connection(
        config: Http2Config,
        client_tls_stream: InboundTlsStream<TcpStream>,
        connection_context: ConnectionContext,
        core: Arc<crate::core::AppCore>,
        stats: Arc<RwLock<Http2Stats>>,
    ) -> Result<(), TakeoverError> {
        let connection_id = connection_context.id;
        
        // 更新连接统计
        {
            let mut stats = stats.write().await;
            stats.total_connections += 1;
            stats.active_connections += 1;
        }
        
        info!("🔗 开始处理HTTPS + HTTP/2连接: {}", connection_id);
        
        // 创建服务处理函数
        let core_clone = Arc::clone(&core);
        let stats_clone = Arc::clone(&stats);
        let context_clone = connection_context.clone();
        
        let service = service_fn(move |req: Request<Incoming>| {
            let core = Arc::clone(&core_clone);
            let stats = Arc::clone(&stats_clone);
            let connection_context = context_clone.clone();
            
            async move {
                Http2StreamMiddleware::handle_http2_stream_static(req, connection_context, core, stats).await
            }
        });
        
        // 使用hyper的HTTP/2处理连接
        let mut builder = http2::Builder::new(TokioExecutor::new());
        builder.max_concurrent_streams(config.max_concurrent_streams);
        builder.initial_stream_window_size(config.initial_window_size);
        builder.max_frame_size(config.max_frame_size);
        
        // 现在可以直接使用TLS流，因为它实现了AsyncRead + AsyncWrite
        let io = TokioIo::new(client_tls_stream);
        
        match builder.serve_connection(io, service).await {
            Ok(()) => {
                debug!("✅ HTTP/2连接处理完成: {}", connection_id);
            }
            Err(e) => {
                error!("❌ HTTP/2连接处理失败: {} - {}", connection_id, e);
                return Err(TakeoverError::Config(format!("HTTP/2 connection failed: {}", e)));
            }
        }
        
        // 更新连接统计
        {
            let mut stats_guard = stats.write().await;
            stats_guard.active_connections = stats_guard.active_connections.saturating_sub(1);
        }
        
        Ok(())
    }
    
    /// 处理单个HTTP/2流 - 静态方法
    #[instrument(skip(req, core, stats))]
    async fn handle_http2_stream_static(
        req: Request<Incoming>,
        connection_context: ConnectionContext,
        core: Arc<crate::core::AppCore>,
        stats: Arc<RwLock<Http2Stats>>,
    ) -> Result<Response<Full<Bytes>>, hyper::Error> {
        Self::process_http2_stream(req, connection_context, core, stats).await
    }
    
    /// 处理单个HTTP/2流
    #[allow(dead_code)]
    #[instrument(skip(self, req, core, stats))]
    async fn handle_http2_stream(
        &self,
        req: Request<Incoming>,
        connection_context: ConnectionContext,
        core: Arc<crate::core::AppCore>,
        stats: Arc<RwLock<Http2Stats>>,
    ) -> Result<Response<Full<Bytes>>, hyper::Error> {
        Self::process_http2_stream(req, connection_context, core, stats).await
    }
    
    /// 实际的HTTP/2流处理逻辑
    async fn process_http2_stream(
        req: Request<Incoming>,
        connection_context: ConnectionContext,
        _core: Arc<crate::core::AppCore>,
        stats: Arc<RwLock<Http2Stats>>,
    ) -> Result<Response<Full<Bytes>>, hyper::Error> {
        // 更新流统计
        {
            let mut stats = stats.write().await;
            stats.total_streams += 1;
            stats.active_streams += 1;
        }
        
        let method = req.method().clone();
        let uri = req.uri().clone();
        let version = req.version();
        
        debug!("🌊 处理HTTP/2流: {} {} {:?}", method, uri, version);
        
        // 创建流上下文
        let stream_context = StreamContext {
            connection_id: connection_context.id,
            stream_id: None, // hyper内部管理流ID
            version,
                            metadata: {
                    let mut metadata = std::collections::HashMap::new();
                metadata.insert("method".to_string(), serde_json::json!(method.as_str()));
                metadata.insert("uri".to_string(), serde_json::json!(uri.to_string()));
                metadata.insert("version".to_string(), serde_json::json!("HTTP/2.0"));
                metadata
            },
        };
        
        // 处理请求
        let response = match Self::process_request_static(req, stream_context, _core).await {
            Ok(resp) => {
                // 更新成功统计
                {
                    let mut stats = stats.write().await;
                    stats.successful_requests += 1;
                }
                resp
            }
            Err(e) => {
                error!("❌ 处理HTTP/2流失败: {}", e);
                
                // 更新失败统计
                {
                    let mut stats = stats.write().await;
                    stats.failed_requests += 1;
                }
                
                // 返回错误响应
                Response::builder()
                    .status(500)
                    .body(Full::new(Bytes::from(format!("Internal Server Error: {}", e))))
                    .unwrap()
            }
        };
        
        // 更新流统计
        {
            let mut stats = stats.write().await;
            stats.active_streams = stats.active_streams.saturating_sub(1);
        }
        
        Ok(response)
    }
    
    /// 处理单个请求（业务逻辑）- 静态方法
    async fn process_request_static(
        req: Request<Incoming>,
        stream_context: StreamContext,
        _core: Arc<crate::core::AppCore>,
    ) -> Result<Response<Full<Bytes>>, TakeoverError> {
        // 这里是业务逻辑处理
        // 可以调用其他中间件、规则匹配、脚本执行等
        
        debug!("📝 处理业务逻辑: {} {}", req.method(), req.uri());
        
        // 示例：简单的响应
        let response_body = format!(
            "HTTP/2 Stream Processed!\nMethod: {}\nURI: {}\nConnection: {}\n",
            req.method(),
            req.uri(),
            stream_context.connection_id
        );
        
        Ok(Response::builder()
            .status(200)
            .header("content-type", "text/plain")
            .header("server", "takeover-proxy/1.0")
            .body(Full::new(Bytes::from(response_body)))
            .map_err(|e| TakeoverError::Config(format!("Failed to build response: {}", e)))?)
    }
    
    /// 启动HTTP/2连接管理器 - 核心分裂逻辑
    /// 
    /// 这是复杂Pipeline的核心：父Pipeline在这里进入长期监听状态，
    /// 为每个HTTP/2流动态spawn子Pipeline
    #[instrument(skip(self, tls_stream, parent_context, core))]
    async fn start_http2_connection(
        &self,
        tls_stream: InboundTlsStream<TcpStream>,
        parent_context: ConnectionContext,
        core: &crate::core::AppCore,
    ) -> Result<(), TakeoverError> {
        let connection_id = parent_context.id;
        debug!("🚀 启动HTTP/2连接: {}", connection_id);
        
        // 更新连接统计
        {
            let mut stats = self.stats.write().await;
            stats.total_connections += 1;
            stats.active_connections += 1;
        }
        
        // 创建服务处理函数 - 为每个HTTP/2流spawn子Pipeline
        let parent_context_clone = parent_context.clone();
        let core_clone = core.clone();
        let stats_clone = Arc::clone(&self.stats);
        
        let service = service_fn(move |req: Request<Incoming>| {
            let parent_context = parent_context_clone.clone();
            let core = core_clone.clone();
            let stats = Arc::clone(&stats_clone);
            
            async move {
                // 🌊 关键：每收到一个HTTP/2流就spawn一个子Pipeline
                Self::spawn_child_pipeline_static(req, parent_context, core, stats).await
            }
        });
        
        // 使用hyper处理HTTP/2连接
        let mut builder = http2::Builder::new(TokioExecutor::new());
        builder.max_concurrent_streams(self.config.max_concurrent_streams);
        builder.initial_stream_window_size(self.config.initial_window_size);
        builder.max_frame_size(self.config.max_frame_size);
        
        let io = TokioIo::new(tls_stream);
        
        // 🔥 父Pipeline在这里进入长期监听状态，直到连接关闭
        match builder.serve_connection(io, service).await {
            Ok(()) => {
                debug!("HTTP/2 hyper连接关闭: {}", connection_id);
            }
            Err(e) => {
                warn!("⚠️ HTTP/2连接异常关闭: {} - {}", connection_id, e);
            }
        }
        
        // 更新连接统计
        {
            let mut stats = self.stats.write().await;
            stats.active_connections = stats.active_connections.saturating_sub(1);
        }
        
        info!("HTTP/2连接结束: {}", connection_id);
        Ok(())
    }
    
    /// 静态方法：为HTTP/2流spawn子Pipeline
    /// 
    /// 这是复杂Pipeline分裂的核心实现
    async fn spawn_child_pipeline_static(
        req: Request<Incoming>,
        parent_context: ConnectionContext,
        core: crate::core::AppCore,
        stats: Arc<RwLock<Http2Stats>>,
    ) -> Result<Response<Full<Bytes>>, hyper::Error> {
        // 更新流统计
        {
            let mut stats = stats.write().await;
            stats.total_streams += 1;
            stats.active_streams += 1;
        }
        
        let method = req.method().clone();
        let uri = req.uri().clone();
        let stream_id = Self::extract_stream_id_static(&req);
        
        info!("🌊 HTTP/2请求: {} {} (流ID: {:?})", method, uri, stream_id);
        
        // 转换hyper请求为我们的HttpRequest
        let http_request = match Self::convert_hyper_request_static(req).await {
            Ok(request) => request,
            Err(e) => {
                error!("❌ 转换HTTP请求失败: {}", e);
                // 更新失败统计
                {
                    let mut stats = stats.write().await;
                    stats.failed_requests += 1;
                    stats.active_streams = stats.active_streams.saturating_sub(1);
                }
                return Ok(Response::builder()
                    .status(400)
                    .body(Full::new(Bytes::from("Bad Request")))
                    .unwrap());
            }
        };
        
        // 🎯 复用ConnectionContext，只替换request字段
        let mut stream_context = parent_context.clone();
        stream_context.request = Some(http_request);
        
        // 添加stream特定的元数据
        stream_context.custom_data.insert(
            "stream_id".to_string(), 
            serde_json::json!(stream_id.unwrap_or(0))
        );
        stream_context.custom_data.insert(
            "http2_stream".to_string(), 
            serde_json::json!(true)
        );
        
        // 🔥 同步执行标准的业务pipeline处理（从优先级20开始）
        let stats_clone = Arc::clone(&stats);
        match Self::continue_standard_pipeline(stream_context, core).await {
            Ok(response) => {
                debug!("HTTP/2流处理成功: stream_id={:?}", stream_id);
                // 更新成功统计
                {
                    let mut stats = stats_clone.write().await;
                    stats.successful_requests += 1;
                    stats.active_streams = stats.active_streams.saturating_sub(1);
                }
                
                // 返回业务处理的真正响应
                Ok(response)
            }
            Err(e) => {
                error!("❌ HTTP/2流处理失败: stream_id={:?}, error={}", stream_id, e);
                // 更新失败统计
                {
                    let mut stats = stats_clone.write().await;
                    stats.failed_requests += 1;
                    stats.active_streams = stats.active_streams.saturating_sub(1);
                }
                
                // 返回错误响应
                Ok(Response::builder()
                    .status(500)
                    .header("content-type", "text/plain")
                    .header("server", "takeover-proxy/1.0")
                    .body(Full::new(Bytes::from(format!("Internal Server Error: {}", e))))
                    .unwrap())
            }
        }
    }
    
    /// 从HTTP/2请求中提取流ID
    fn extract_stream_id_static(_req: &Request<Incoming>) -> Option<u32> {
        // hyper内部管理流ID，这里我们模拟一个
        // 实际实现中可能需要从hyper的扩展中获取
        Some(fastrand::u32(1..=1000000))
    }
    
    /// 转换hyper请求为我们的HttpRequest
    async fn convert_hyper_request_static(req: Request<Incoming>) -> Result<HttpRequest, TakeoverError> {
        let method = req.method().to_string();
        let uri = req.uri().to_string();
        let version = format!("{:?}", req.version());
        
        // 转换头部
        let mut headers = std::collections::HashMap::new();
        for (name, value) in req.headers() {
            headers.insert(
                name.to_string(),
                value.to_str().unwrap_or("").to_string()
            );
        }
        
        // 读取请求体
        let body_bytes = match http_body_util::BodyExt::collect(req.into_body()).await {
            Ok(collected) => collected.to_bytes().to_vec(),
            Err(e) => {
                warn!("读取请求体失败: {}", e);
                Vec::new()
            }
        };
        
        Ok(HttpRequest {
            id: Some(Uuid::new_v4()),
            method,
            url: uri,
            version,
            headers,
            body: body_bytes,
            sent_at: Some(std::time::SystemTime::now()),
        })
    }
    
    /// 继续执行标准的业务pipeline（从指定优先级开始）
    /// 
    /// 这个方法会执行从优先级20开始的所有中间件，包括：
    /// - 规则匹配中间件 (30)
    /// - 脚本中间件 (35) 
    /// - 用户干预中间件 (40)
    /// - HTTP修改中间件 (50)
    /// - 转发中间件 (60) - 关键！
    /// - 响应合并中间件 (70)
    /// 
    /// 返回最终的HTTP响应
    async fn continue_standard_pipeline(
        mut context: ConnectionContext, 
        core: crate::core::AppCore
    ) -> Result<Response<Full<Bytes>>, TakeoverError> {
        // 从优先级20开始执行剩余的中间件
        let start_priority = 20;
        
        debug!("开始执行业务Pipeline(优先级{}) for stream: {}", start_priority, context.id);
        
        // 🚀 获取HTTP/2业务pipeline（跳过TLS和HTTP/2协议处理）
        let business_pipeline = core.engine().get_pipeline("http2_business");
        
        debug!("执行HTTP/2业务Pipeline({}个中间件)", business_pipeline.middlewares().len());
        
        // 执行业务pipeline
        match business_pipeline.process(&mut context, &core).await {
            Ok(crate::core::engine::MiddlewareResult::Continue) => {
                // pipeline执行完成，检查是否有响应生成
            }
            Ok(other_result) => {
                debug!("🎯 业务Pipeline返回: {:?}", other_result);
            }
            Err(e) => {
                error!("💥 业务Pipeline处理失败: {}", e);
                return Err(e);
            }
        }
        
        debug!("🎯 HTTP/2业务Pipeline执行完成 for stream: {}", context.id);
        
        // 检查是否有响应生成，并转换为hyper Response
        if let Some(response) = context.response {
            debug!("✅ HTTP/2流处理完成: {} ({}字节)", response.status_code, response.body.len());
            
            // 转换我们的HttpResponse为hyper Response
            let mut builder = Response::builder()
                .status(response.status_code as u16);
            
            // 添加头部
            for (key, value) in response.headers {
                builder = builder.header(key, value);
            }
            
            // 确保有必要的头部
            builder = builder.header("server", "takeover-proxy/1.0");
            
            let hyper_response = builder
                .body(Full::new(Bytes::from(response.body)))
                .map_err(|e| TakeoverError::Config(format!("Failed to build response: {}", e)))?;
            
            Ok(hyper_response)
        } else {
            warn!("⚠️ 没有生成响应，返回默认响应。可能需要检查转发中间件");
            
            // 生成默认响应
            let default_response = Response::builder()
                .status(502)
                .header("content-type", "text/plain")
                .header("server", "takeover-proxy/1.0")
                .body(Full::new(Bytes::from("Bad Gateway: No response generated by pipeline")))
                .map_err(|e| TakeoverError::Config(format!("Failed to build default response: {}", e)))?;
            
            Ok(default_response)
        }
    }

    // 注意：continue_standard_pipeline_manual() 方法已废弃
    // 现在使用简化的 Pipeline 架构，通过 engine.get_pipeline() 获取预构建的pipeline
    
    /// 检查是否为HTTP/2协议
    /// 
    /// 通过以下方式检测HTTP/2：
    /// 1. 检查ALPN协商结果 (优先)
    /// 2. 检查连接元数据中的协议信息
    /// 3. 检查端口号和TLS状态 (基本推断)
    #[instrument(skip(self, context))]
    async fn is_http2_protocol(&self, context: &ConnectionContext) -> bool {
        // 方法1：检查ALPN协商结果 - 最可靠的方式
        if let Some(alpn_result) = context.custom_data.get("alpn_protocol") {
            if let Some(protocol) = alpn_result.as_str() {
                debug!("🔍 ALPN协商协议: {}", protocol);
                return protocol == "h2";
            }
        }
        
        // 方法2：检查TLS元数据中的ALPN信息
        if let Some(tls_info) = context.custom_data.get("tls_intercepted") {
            if let Some(alpn_protocol) = tls_info.get("alpn_protocol") {
                if let Some(protocol) = alpn_protocol.as_str() {
                    debug!("🔍 TLS元数据ALPN协议: {}", protocol);
                    return protocol == "h2";
                }
            }
        }
        
        // 方法3：检查连接元数据
        if context.connection.metadata.protocol == "HTTP/2" || 
           context.connection.metadata.protocol == "h2" {
            debug!("🔍 连接元数据指示HTTP/2");
            return true;
        }
        
        // 方法4：基于端口和TLS状态的基本推断
        // 注意：这只是一个后备方案，不够精确
        if context.target_port == 443 && context.connection.has_tls_streams() {
            debug!("🔍 基于端口443+TLS推断可能为HTTP/2，但这不可靠");
            // 这种情况下我们假设是HTTP/2，但会在日志中明确说明这是推断
            warn!("⚠️ 无法确定协议类型，基于HTTPS端口推断为HTTP/2。建议TLS中间件实现ALPN协商！");
            return true;
        }
        
        debug!("🔍 协议检测结果：非HTTP/2");
        false
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> Http2Stats {
        self.stats.read().await.clone()
    }
}

impl Default for Http2StreamMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for Http2Config {
    fn default() -> Self {
        Self {
            enabled: true,
            max_concurrent_streams: 100,
            initial_window_size: 65535,
            max_frame_size: 16384,
            enable_push: false, // 默认关闭服务器推送
            connection_timeout: std::time::Duration::from_secs(300), // 5分钟
        }
    }
}

#[async_trait]
impl Middleware for Http2StreamMiddleware {
    fn name(&self) -> &str {
        "http2_stream"
    }
    
    #[instrument(skip(self, context, _core))]
    async fn process(
        &self,
        context: &mut ConnectionContext,
        _core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, TakeoverError> {
        debug!("HTTP/2 stream middleware processing connection {}", context.id);
        
        // 🔍 第一步：检查是否为HTTP/2协议
        if !self.is_http2_protocol(context).await {
            debug!("⏭️ 非HTTP/2协议，跳过HTTP/2中间件处理: {}", context.id);
            return Ok(MiddlewareResult::Continue);
        }
        
        info!("🔗 HTTP/2连接开始: {}", context.id);
        
        // 更新处理状态
        context.set_processing_state(ProcessingState::HttpParsing);
        
        // 检查是否有客户端TLS流可用（应该由TLS中间件提供）
        if context.connection.has_tls_streams() {
            debug!("🔗 检测到TLS流，启动HTTP/2复杂Pipeline模式: {}", context.id);
            
            // 取出TLS流
            let tls_stream = context.connection.inbound_tls_stream.take()
                .ok_or_else(|| TakeoverError::Config("Missing inbound TLS stream".to_string()))?;
            
            // 标记为HTTP/2处理状态
            context.custom_data.insert(
                "http2_processing".to_string(),
                serde_json::json!({
                    "mode": "complex_pipeline",
                    "connection_id": context.id,
                    "timestamp": chrono::Utc::now().to_rfc3339()
                })
            );
            
            // 🚀 启动HTTP/2连接（父Pipeline在这里进入长期监听状态）
            self.start_http2_connection(tls_stream, context.clone(), _core).await?;
            
            debug!("HTTP/2连接已启动，父Pipeline完成任务");
            
            // 父Pipeline在此结束，连接由HTTP/2连接接管
            Ok(MiddlewareResult::Stop)
        } else {
            debug!("⚠️ 没有可用的TLS流，可能不是HTTPS连接，跳过HTTP/2处理");
            Ok(MiddlewareResult::Continue) // 继续其他中间件处理
        }
    }
    
    fn priority(&self) -> i32 {
        15 // 在TLS处理之后，其他业务逻辑之前
    }
    
    fn enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_middleware_creation() {
        let middleware = Http2StreamMiddleware::new();
        assert_eq!(middleware.name(), "http2_stream");
        assert!(middleware.enabled());
        assert_eq!(middleware.priority(), 15);
        
        let stats = middleware.get_stats().await;
        assert_eq!(stats.total_connections, 0);
        assert_eq!(stats.active_connections, 0);
        
        println!("✅ HTTP/2流中间件创建成功");
    }
    
    #[tokio::test]
    async fn test_config_creation() {
        let config = Http2Config {
            enabled: true,
            max_concurrent_streams: 50,
            initial_window_size: 32768,
            max_frame_size: 8192,
            enable_push: true,
            connection_timeout: std::time::Duration::from_secs(120),
        };
        
        let middleware = Http2StreamMiddleware::with_config(config);
        assert_eq!(middleware.name(), "http2_stream");
        
        println!("✅ 自定义配置HTTP/2流中间件创建成功");
    }
    
    #[test]
    fn test_stream_context_creation() {
        let context = StreamContext {
            connection_id: uuid::Uuid::new_v4(),
            stream_id: Some(1),
            version: Version::HTTP_2,
            metadata: HashMap::new(),
        };
        
        assert_eq!(context.version, Version::HTTP_2);
        assert_eq!(context.stream_id, Some(1));
        
        println!("✅ 流上下文创建成功");
    }
    
    #[tokio::test]
    async fn test_protocol_detection() {
        use crate::core::model::{Connection, ConnectionContext};
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        
        let middleware = Http2StreamMiddleware::new();
        
        // 测试1：ALPN协商为h2的情况
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443)
        );
        let mut context = ConnectionContext::new(connection, "example.com".to_string(), 443);
        context.custom_data.insert("alpn_protocol".to_string(), serde_json::json!("h2"));
        
        assert!(middleware.is_http2_protocol(&context).await);
        
        // 测试2：ALPN协商为http/1.1的情况
        let connection2 = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443)
        );
        let mut context2 = ConnectionContext::new(connection2, "example.com".to_string(), 443);
        context2.custom_data.insert("alpn_protocol".to_string(), serde_json::json!("http/1.1"));
        
        assert!(!middleware.is_http2_protocol(&context2).await);
        
        // 测试3：无ALPN协商但连接元数据指示HTTP/2
        let connection3 = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443)
        );
        let mut context3 = ConnectionContext::new(connection3, "example.com".to_string(), 443);
        context3.connection.metadata.protocol = "HTTP/2".to_string();
        
        assert!(middleware.is_http2_protocol(&context3).await);
        
        // 测试4：普通HTTP连接
        let connection4 = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        let context4 = ConnectionContext::new(connection4, "example.com".to_string(), 80);
        
        assert!(!middleware.is_http2_protocol(&context4).await);
        
        println!("✅ 协议检测测试通过");
    }
}
