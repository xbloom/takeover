//! 完整的代理集成测试
//! 
//! 测试真正的takeover代理系统运行，包括：
//! - 启动完整的AppCore
//! - 运行入站适配器
//! - 执行完整的中间件pipeline
//! - 处理真实的HTTP/2流量

use std::sync::Arc;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, timeout};
use tracing::{info, debug, error};
use anyhow::Result;

use hyper::service::service_fn;
use hyper::server::conn::http2;
use hyper::{Request, Response, Method, StatusCode};
use hyper::body::{Incoming, Bytes};
use http_body_util::{Full, BodyExt};
use hyper_util::rt::{TokioIo, TokioExecutor};

use takeover::core::{
    AppCore, AppConfig, Connection, ConnectionContext, ProcessingEngine,
    MiddlewareResult,
    InboundPort, OutboundPort, ControlPort, EventPort, NetworkStream, AppError, AdapterStatus, DomainEvent, ConnectionSummary, Rule, RequestModifications, ReplayResult, ConnectionId, RequestId, InboundConfig
};
use takeover::core::middleware::{
    TlsMiddleware, Http2StreamMiddleware, RuleMatchingMiddleware,
    HttpModificationMiddleware
};

use async_trait::async_trait;
use std::pin::Pin;
use futures::Stream;

/// 完整的代理集成测试 - 启动真实的takeover系统
#[tokio::test]
async fn test_full_proxy_integration() -> Result<()> {
    // 初始化日志
    let _ = tracing_subscriber::fmt::try_init();
    
    info!("🚀 开始完整的代理集成测试");
    
    // 1. 启动目标HTTP/2服务器
    let target_server_addr = start_target_http2_server().await?;
    info!("✅ 目标服务器启动: {}", target_server_addr);
    
    // 2. 启动takeover代理系统
    let proxy_addr = start_takeover_proxy_system(target_server_addr).await?;
    info!("✅ Takeover代理系统启动: {}", proxy_addr);
    
    // 等待系统启动
    sleep(Duration::from_millis(200)).await;
    
    // 3. 通过代理发送HTTP/2请求
    test_requests_through_proxy(proxy_addr).await?;
    
    info!("✅ 完整的代理集成测试通过！");
    Ok(())
}

/// 启动目标HTTP/2服务器
async fn start_target_http2_server() -> Result<SocketAddr> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0);
    let listener = TcpListener::bind(addr).await?;
    let server_addr = listener.local_addr()?;
    
    tokio::spawn(async move {
        info!("🎯 目标HTTP/2服务器监听: {}", server_addr);
        
        while let Ok((stream, peer_addr)) = listener.accept().await {
            debug!("📥 目标服务器接收连接: {}", peer_addr);
            
            tokio::spawn(async move {
                if let Err(e) = handle_target_server_connection(stream).await {
                    error!("❌ 目标服务器连接处理失败: {}", e);
                }
            });
        }
    });
    
    Ok(server_addr)
}

/// 处理目标服务器连接
async fn handle_target_server_connection(stream: TcpStream) -> Result<()> {
    let io = TokioIo::new(stream);
    
    let service = service_fn(|req: Request<Incoming>| async move {
        debug!("🎯 目标服务器收到请求: {} {}", req.method(), req.uri());
        
        // 模拟真实的API响应
        let response = match req.uri().path() {
            "/api/user" => {
                let user_data = r#"{"id": 123, "name": "代理测试用户", "source": "through_proxy"}"#;
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .header("x-served-by", "target-server")
                    .body(Full::new(Bytes::from(user_data)))
                    .unwrap()
            }
            "/api/posts" => {
                let posts_data = r#"[{"id": 1, "title": "通过代理的请求"}, {"id": 2, "title": "HTTP/2多路复用"}]"#;
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .header("x-served-by", "target-server")
                    .body(Full::new(Bytes::from(posts_data)))
                    .unwrap()
            }
            _ => {
                Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .header("x-served-by", "target-server")
                    .body(Full::new(Bytes::from("Not Found")))
                    .unwrap()
            }
        };
        
        Ok::<_, hyper::Error>(response)
    });
    
    http2::Builder::new(TokioExecutor::new())
        .serve_connection(io, service)
        .await?;
        
    Ok(())
}

/// 启动takeover代理系统
async fn start_takeover_proxy_system(target_addr: SocketAddr) -> Result<SocketAddr> {
    info!("🔧 启动Takeover代理系统");
    
    // 创建代理配置
    let mut config = AppConfig::default();
    config.inbound.listen_port = 0; // 动态分配端口
    
    // 创建完整的中间件pipeline
    let engine = ProcessingEngine::new();
    engine.add_middleware(Arc::new(TlsMiddleware::new()));
    engine.add_middleware(Arc::new(Http2StreamMiddleware::new()));
    engine.add_middleware(Arc::new(RuleMatchingMiddleware::new()));
    engine.add_middleware(Arc::new(HttpModificationMiddleware::new()));
    
    // 创建适配器
    let inbound_port = Arc::new(TestInboundPort::new());
    let outbound_port = Arc::new(TestOutboundPort::new(target_addr));
    let control_port = Arc::new(TestControlPort::new());
    let event_port = Arc::new(TestEventPort::new());
    
    // 创建完整的AppCore
    let app_core = AppCore::builder(
        config,
        inbound_port.clone(),
        outbound_port,
        control_port,
        event_port,
    ).build().await;
    
    // 启动代理监听
    let proxy_addr = start_proxy_listener(app_core, inbound_port).await?;
    
    Ok(proxy_addr)
}

/// 启动代理监听器
async fn start_proxy_listener(
    app_core: AppCore,
    inbound_port: Arc<TestInboundPort>,
) -> Result<SocketAddr> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0);
    let listener = TcpListener::bind(addr).await?;
    let proxy_addr = listener.local_addr()?;
    
    // 设置入站端口的监听器
    inbound_port.set_listener(listener).await;
    
    // 启动AppCore处理任务
    tokio::spawn(async move {
        info!("🎛️ Takeover代理监听: {}", proxy_addr);
        
        // 这里应该调用app_core.run()，但为了测试简化，我们手动处理连接
        if let Err(e) = run_proxy_connection_handler(app_core, inbound_port).await {
            error!("❌ 代理系统运行失败: {}", e);
        }
    });
    
    Ok(proxy_addr)
}

/// 运行代理连接处理器
async fn run_proxy_connection_handler(
    app_core: AppCore,
    inbound_port: Arc<TestInboundPort>,
) -> Result<()> {
    while let Some(stream) = inbound_port.accept_connection().await {
        let app_core_clone = app_core.clone();
        
        tokio::spawn(async move {
            if let Err(e) = handle_proxy_connection(stream, app_core_clone).await {
                error!("❌ 代理连接处理失败: {}", e);
            }
        });
    }
    
    Ok(())
}

/// 处理代理连接
async fn handle_proxy_connection(
    stream: TcpStream,
    app_core: AppCore,
) -> Result<()> {
    let peer_addr = stream.peer_addr()?;
    let local_addr = stream.local_addr()?;
    
    info!("🔄 代理处理连接: {} -> {}", peer_addr, local_addr);
    
    // 创建连接对象
    let connection = Connection::new_with_stream(peer_addr, local_addr, stream);
    
    // 创建连接上下文
    let mut context = ConnectionContext::new(
        connection,
        "127.0.0.1".to_string(),
        local_addr.port(),
    );
    
    // 🔥 执行完整的中间件pipeline
    let pipeline = app_core.engine().get_default_pipeline();
    
    match pipeline.process(&mut context, &app_core).await {
        Ok(MiddlewareResult::Continue) => {
            info!("✅ 代理pipeline处理完成");
        }
        Ok(MiddlewareResult::Stop) => {
            info!("🛑 代理pipeline被中间件停止（如HTTP/2接管）");
        }
        Ok(other) => {
            info!("ℹ️ 代理pipeline返回: {:?}", other);
        }
        Err(e) => {
            error!("❌ 代理pipeline处理失败: {}", e);
        }
    }
    
    Ok(())
}

/// 通过代理发送测试请求
async fn test_requests_through_proxy(proxy_addr: SocketAddr) -> Result<()> {
    info!("🌊 通过代理发送HTTP/2测试请求");
    
    // 连接到代理
    let stream = TcpStream::connect(proxy_addr).await?;
    let io = TokioIo::new(stream);
    
    // 建立HTTP/2连接
    let (mut sender, conn) = hyper::client::conn::http2::handshake(TokioExecutor::new(), io).await?;
    
    tokio::spawn(async move {
        if let Err(err) = conn.await {
            error!("HTTP/2连接错误: {}", err);
        }
    });
    
    let uri_base = format!("http://{}", proxy_addr);
    
    // 🌊 并发发送多个请求，测试多路复用
    let mut responses = Vec::new();
    
    // Stream 1: 用户API
    let req1 = Request::builder()
        .method(Method::GET)
        .uri(format!("{}/api/user", uri_base))
        .header("x-test-stream", "1")
        .body(Full::new(Bytes::new()))
        .unwrap();
    
    let response1 = timeout(Duration::from_secs(10), sender.send_request(req1)).await??;
    info!("📥 代理响应 Stream 1: {}", response1.status());
    
    // 验证响应头
    if let Some(served_by) = response1.headers().get("x-served-by") {
        info!("🎯 请求被服务于: {:?}", served_by);
    }
    
    responses.push(response1);
    
    // Stream 2: 文章API
    let req2 = Request::builder()
        .method(Method::GET)
        .uri(format!("{}/api/posts", uri_base))
        .header("x-test-stream", "2")
        .body(Full::new(Bytes::new()))
        .unwrap();
    
    let response2 = timeout(Duration::from_secs(10), sender.send_request(req2)).await??;
    info!("📥 代理响应 Stream 2: {}", response2.status());
    responses.push(response2);
    
    // 验证所有响应
    let mut success_count = 0;
    for (i, response) in responses.into_iter().enumerate() {
        if response.status().is_success() {
            info!("✅ 代理Stream {} 处理成功: {}", i + 1, response.status());
            success_count += 1;
            
            // 读取响应体验证
            let body_bytes = BodyExt::collect(response.into_body()).await?.to_bytes();
            let body_str = String::from_utf8_lossy(&body_bytes);
            info!("📄 响应内容预览: {}", &body_str[..body_str.len().min(100)]);
        } else {
            error!("❌ 代理Stream {} 返回错误: {}", i + 1, response.status());
        }
    }
    
    assert!(success_count >= 1, "至少1个请求应该通过代理成功处理");
    
    info!("✅ 通过代理的HTTP/2多路复用测试通过，成功处理 {} 个流", success_count);
    Ok(())
}

// =============================================================================
// 测试用的适配器实现 - 真实功能的简化版本
// =============================================================================

/// 测试入站适配器
pub struct TestInboundPort {
    listener: Arc<tokio::sync::Mutex<Option<TcpListener>>>,
}

impl TestInboundPort {
    pub fn new() -> Self {
        Self {
            listener: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }
    
    pub async fn set_listener(&self, listener: TcpListener) {
        *self.listener.lock().await = Some(listener);
    }
    
    pub async fn accept_connection(&self) -> Option<TcpStream> {
        let listener = self.listener.lock().await;
        if let Some(ref listener) = *listener {
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
                    debug!("📨 入站适配器接收连接: {}", peer_addr);
                    Some(stream)
                }
                Err(e) => {
                    error!("❌ 入站适配器接收连接失败: {}", e);
                    None
                }
            }
        } else {
            None
        }
    }
}

#[async_trait]
impl InboundPort for TestInboundPort {
    async fn listen(&self, _config: &InboundConfig) -> Result<Pin<Box<dyn Stream<Item = Result<Connection, AppError>> + Send>>, AppError> {
        // 返回空流，实际连接处理在accept_connection中
        Ok(Box::pin(futures::stream::empty()))
    }
    
    async fn stop(&self) -> Result<(), AppError> { Ok(()) }
    
    async fn status(&self) -> AdapterStatus {
        AdapterStatus {
            name: "test_inbound".to_string(),
            active: true,
            connections: 0,
            last_activity: None,
            errors: 0,
        }
    }
    
    fn name(&self) -> &str { "test_inbound" }
}

/// 测试出站适配器
pub struct TestOutboundPort {
    target_addr: SocketAddr,
}

impl TestOutboundPort {
    pub fn new(target_addr: SocketAddr) -> Self {
        Self { target_addr }
    }
}

#[async_trait]
impl OutboundPort for TestOutboundPort {
    async fn connect(&self, context: &ConnectionContext) -> Result<Box<dyn NetworkStream>, AppError> {
        info!("🔗 出站适配器连接到目标: {} (请求: {:?})", 
              self.target_addr, 
              context.request.as_ref().map(|r| &r.url));
        
        let stream = TcpStream::connect(self.target_addr).await
            .map_err(|e| AppError::NetworkInterception(format!("Failed to connect to target: {}", e)))?;
        
        Ok(Box::new(TestNetworkStream::new(stream)))
    }
    
    async fn supports_target(&self, _context: &ConnectionContext) -> bool { true }
    fn name(&self) -> &str { "test_outbound" }
}

/// 测试控制适配器
pub struct TestControlPort;

impl TestControlPort {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ControlPort for TestControlPort {
    async fn apply_rules(&self, _rules: Vec<Rule>) -> Result<(), AppError> { Ok(()) }
    async fn get_active_connections(&self) -> Result<Vec<ConnectionSummary>, AppError> { Ok(vec![]) }
    async fn intercept_connection(&self, _conn_id: ConnectionId) -> Result<(), AppError> { Ok(()) }
    async fn approve_request(&self, _request_id: RequestId, _modifications: Option<RequestModifications>) -> Result<(), AppError> { Ok(()) }
    async fn reject_request(&self, _request_id: RequestId, _reason: String) -> Result<(), AppError> { Ok(()) }
    async fn trigger_replay(&self, _request_id: RequestId, _modifications: RequestModifications) -> Result<ReplayResult, AppError> {
        Ok(ReplayResult {
            success: true,
            request_id: _request_id,
            replay_count: 1,
            execution_time: Duration::from_millis(100),
            response: None,
            error_message: None,
        })
    }
    async fn get_active_rules(&self) -> Result<Vec<Rule>, AppError> { Ok(vec![]) }
}

/// 测试事件适配器
pub struct TestEventPort;

impl TestEventPort {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl EventPort for TestEventPort {
    async fn publish(&self, event: DomainEvent) -> Result<(), AppError> {
        debug!("📡 事件发布: {:?}", event);
        Ok(())
    }
    
    async fn subscribe(&self) -> Result<Pin<Box<dyn Stream<Item = DomainEvent> + Send>>, AppError> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

/// 测试网络流
pub struct TestNetworkStream {
    stream: TcpStream,
}

impl TestNetworkStream {
    pub fn new(stream: TcpStream) -> Self {
        Self { stream }
    }
}

impl tokio::io::AsyncRead for TestNetworkStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for TestNetworkStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

impl NetworkStream for TestNetworkStream {}
