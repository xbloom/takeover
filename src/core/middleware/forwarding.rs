//! 转发中间件
//! 
//! 这是透明代理的核心组件，负责：
//! - 将解析后的HTTP请求转发到目标服务器
//! - 通过OutboundPort进行实际的网络连接
//! - 处理响应并返回给客户端
//! - 管理连接池和重试逻辑

use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{warn, error, debug, instrument};
use async_trait::async_trait;
use tokio::io::{AsyncWriteExt, AsyncReadExt, AsyncBufReadExt, BufReader};

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{ConnectionContext, ProcessingState, HttpRequest, HttpResponse};
use crate::core::ports::OutboundPort;
use crate::error::TakeoverError;

/// 转发中间件
/// 
/// 负责实际的请求转发逻辑，是透明代理的核心
pub struct ForwardingMiddleware {
    /// 配置
    config: ForwardingConfig,
    /// 统计信息
    stats: Arc<tokio::sync::RwLock<ForwardingStats>>,
}

/// 转发配置
#[derive(Debug, Clone)]
pub struct ForwardingConfig {
    /// 是否启用转发
    pub enabled: bool,
    /// 连接超时
    pub connect_timeout: Duration,
    /// 读取超时
    pub read_timeout: Duration,
    /// 写入超时
    pub write_timeout: Duration,
    /// 最大重试次数
    pub max_retries: usize,
    /// 重试延迟
    pub retry_delay: Duration,
    /// 是否保持连接活跃
    pub keep_alive: bool,
    /// 最大重定向次数
    pub max_redirects: usize,
    /// 是否强制使用TLS
    pub force_tls: bool,
    /// TLS验证设置
    pub tls_verify: bool,
}

/// 协议类型
#[derive(Debug, Clone, PartialEq)]
pub enum Protocol {
    Http,
    Https,
    Auto, // 根据端口或URL自动检测
}

/// 转发统计信息
#[derive(Debug, Default, Clone)]
pub struct ForwardingStats {
    /// 总请求数
    pub total_requests: u64,
    /// 成功转发数
    pub successful_forwards: u64,
    /// 失败转发数
    pub failed_forwards: u64,
    /// 重试次数
    pub total_retries: u64,
    /// 平均响应时间
    pub avg_response_time: Option<Duration>,
    /// 总传输字节数
    pub bytes_transferred: u64,
    /// 当前活跃转发数
    pub active_forwards: u64,
}

impl ForwardingMiddleware {
    /// 创建新的转发中间件
    pub fn new() -> Self {
        Self {
            config: ForwardingConfig::default(),
            stats: Arc::new(tokio::sync::RwLock::new(ForwardingStats::default())),
        }
    }
    
    /// 使用配置创建中间件
    pub fn with_config(config: ForwardingConfig) -> Self {
        Self {
            config,
            stats: Arc::new(tokio::sync::RwLock::new(ForwardingStats::default())),
        }
    }
    
    /// 执行HTTP请求转发
    #[instrument(skip(self, context, core))]
    async fn forward_request(
        &self,
        context: &mut ConnectionContext,
        core: &crate::core::AppCore,
    ) -> Result<HttpResponse, TakeoverError> {
        let start_time = Instant::now();
        
        // 更新活跃转发计数
        {
            let mut stats = self.stats.write().await;
            stats.total_requests += 1;
            stats.active_forwards += 1;
        }
        
        let request = context.request.as_ref()
            .ok_or_else(|| TakeoverError::Config("No request found in context".to_string()))?;
        
        debug!("🚀 转发请求: {} {}", request.method, request.url);
        
        // 选择出站适配器
        let outbound_port = core.outbound_port();
        
        // 检查适配器是否支持目标
        if !outbound_port.supports_target(context).await {
            error!("❌ 出站适配器不支持目标: {}", context.target_host);
            return Err(TakeoverError::Config("Outbound adapter does not support target".to_string()));
        }
        
        // 执行转发（带重试机制）
        let mut last_error = None;
        
        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                debug!("🔄 重试转发 (第{}次): {} {}", attempt, request.method, request.url);
                tokio::time::sleep(self.config.retry_delay).await;
                
                // 更新重试统计
                {
                    let mut stats = self.stats.write().await;
                    stats.total_retries += 1;
                }
            }
            
            match self.execute_forward_attempt(context, outbound_port.as_ref()).await {
                Ok(response) => {
                    let elapsed = start_time.elapsed();
                    debug!("✅ 转发成功: {} {} ({}ms)", request.method, request.url, elapsed.as_millis());
                    
                    // 更新成功统计
                    {
                        let mut stats = self.stats.write().await;
                        stats.successful_forwards += 1;
                        stats.active_forwards = stats.active_forwards.saturating_sub(1);
                        stats.bytes_transferred += response.body.len() as u64;
                        
                        // 更新平均响应时间
                        stats.avg_response_time = Some(match stats.avg_response_time {
                            Some(avg) => Duration::from_nanos(
                                ((avg.as_nanos() + elapsed.as_nanos()) / 2) as u64
                            ),
                            None => elapsed,
                        });
                    }
                    
                    // 记录字节传输
                    core.update_connection_bytes(
                        context.id.clone(),
                        request.body.len() as u64,
                        response.body.len() as u64
                    ).await;
                    
                    return Ok(response);
                }
                Err(e) => {
                    debug!("⚠️ 转发尝试失败 (第{}次): {}", attempt + 1, e);
                    last_error = Some(e);
                }
            }
        }
        
        // 所有重试都失败
        let error = last_error.unwrap_or_else(|| 
            TakeoverError::Config("All forwarding attempts failed".to_string())
        );
        
        debug!("⚠️ 转发失败: {} {} - {}", request.method, request.url, error);
        
        // 更新失败统计
        {
            let mut stats = self.stats.write().await;
            stats.failed_forwards += 1;
            stats.active_forwards = stats.active_forwards.saturating_sub(1);
        }
        
        Err(error)
    }
    
    /// 执行单次转发尝试
    async fn execute_forward_attempt(
        &self,
        context: &ConnectionContext,
        outbound_port: &dyn OutboundPort,
    ) -> Result<HttpResponse, TakeoverError> {
        let request = context.request.as_ref().unwrap();
        
        // 检测目标协议
        let protocol = self.detect_protocol(&request.url, context.target_port, context)?;
        debug!("🔗 建立出站连接到: {}:{} (协议: {:?})", context.target_host, context.target_port, protocol);
        
        // 告诉Outbound适配器原始目标协议，以便决定是否使用TLS
        let mut context_with_protocol = context.clone();
        context_with_protocol.custom_data.insert(
            "original_target_protocol".to_string(),
            serde_json::json!(format!("{:?}", protocol).to_lowercase())
        );
        
        // 建立出站连接
        let mut stream = outbound_port.connect(&context_with_protocol).await
            .map_err(|e| TakeoverError::Config(format!("Failed to connect: {}", e)))?;
        
        // 构建HTTP请求
        let http_request = self.build_http_request(request, context, &protocol)?;
        
        // 发送请求
        debug!("📤 发送HTTP请求 (协议: {:?})", protocol);
        stream.write_all(http_request.as_bytes()).await
            .map_err(|e| TakeoverError::Config(format!("Failed to send request: {}", e)))?;
        
        stream.flush().await
            .map_err(|e| TakeoverError::Config(format!("Failed to flush request: {}", e)))?;
        
        // 读取响应
        debug!("📥 读取HTTP响应");
        let response = self.read_http_response(&mut *stream).await?;
        
        Ok(response)
    }
    
    /// 检测目标协议
    /// 
    /// 根据TLS拦截状态、URL scheme和端口判断目标协议
    fn detect_protocol(&self, url: &str, target_port: u16, context: &ConnectionContext) -> Result<Protocol, TakeoverError> {
        // 检查TLS中间件是否已经处理过
        if let Some(tls_info) = context.custom_data.get("tls_intercepted") {
            if let Some(success) = tls_info.get("success").and_then(|v| v.as_bool()) {
                if success {
                    debug!("🔐 TLS中间件已拦截，原始协议为HTTPS");
                    return Ok(Protocol::Https);
                }
            }
        }
        
        // 从URL解析协议
        if let Ok(parsed_url) = url::Url::parse(url) {
            return match parsed_url.scheme() {
                "https" => Ok(Protocol::Https),
                "http" => Ok(Protocol::Http),
                scheme => {
                    debug!("⚠️ 未知scheme: {}，根据端口推断", scheme);
                    self.detect_protocol_by_port(target_port)
                }
            };
        } else {
            debug!("⚠️ URL解析失败，根据端口推断");
        }
        
        // 根据端口推断
        self.detect_protocol_by_port(target_port)
    }
    
    /// 根据端口检测协议
    fn detect_protocol_by_port(&self, port: u16) -> Result<Protocol, TakeoverError> {
        match port {
            443 | 8443 => Ok(Protocol::Https),
            80 | 8080 | 3000 | 8000 => Ok(Protocol::Http),
            _ => {
                if self.config.force_tls {
                    debug!("🔒 强制使用TLS (端口: {})", port);
                    Ok(Protocol::Https)
                } else {
                    debug!("🌐 默认使用HTTP (端口: {})", port);
                    Ok(Protocol::Http)
                }
            }
        }
    }
    
    /// 构建HTTP请求字符串
    fn build_http_request(
        &self,
        request: &HttpRequest,
        context: &ConnectionContext,
        protocol: &Protocol,
    ) -> Result<String, TakeoverError> {
        let mut http_request = String::new();
        
        // 请求行
        let path = self.extract_path_from_url(&request.url)?;
        http_request.push_str(&format!("{} {} HTTP/1.1\r\n", request.method, path));
        
        // 🌐 Host头部 - 根据协议和端口智能处理
        let host_header = self.build_host_header(context, protocol);
        http_request.push_str(&format!("Host: {}\r\n", host_header));
        
        // 添加其他头部
        for (name, value) in &request.headers {
            // 跳过某些头部，避免冲突
            match name.to_lowercase().as_str() {
                "host" | "content-length" => continue,
                _ => http_request.push_str(&format!("{}: {}\r\n", name, value)),
            }
        }
        
        // Content-Length头部（如果有请求体）
        if !request.body.is_empty() {
            http_request.push_str(&format!("Content-Length: {}\r\n", request.body.len()));
        }
        
        // 连接控制
        if self.config.keep_alive {
            http_request.push_str("Connection: keep-alive\r\n");
        } else {
            http_request.push_str("Connection: close\r\n");
        }
        
        // 请求头结束
        http_request.push_str("\r\n");
        
        // 请求体
        if !request.body.is_empty() {
            http_request.push_str(&String::from_utf8_lossy(&request.body));
        }
        
        Ok(http_request)
    }
    
    /// 构建Host头部
    /// 
    /// 根据协议智能处理端口显示：
    /// - HTTP: 80端口不显示，其他端口显示
    /// - HTTPS: 443端口不显示，其他端口显示
    fn build_host_header(&self, context: &ConnectionContext, protocol: &Protocol) -> String {
        let default_port = match protocol {
            Protocol::Https => 443,
            Protocol::Http => 80,
            Protocol::Auto => 80,
        };
        
        if context.target_port == default_port {
            context.target_host.clone()
        } else {
            format!("{}:{}", context.target_host, context.target_port)
        }
    }
    
    /// 从URL提取路径
    fn extract_path_from_url(&self, url: &str) -> Result<String, TakeoverError> {
        if let Ok(parsed_url) = url::Url::parse(url) {
            let mut path = parsed_url.path().to_string();
            if let Some(query) = parsed_url.query() {
                path.push('?');
                path.push_str(query);
            }
            Ok(if path.is_empty() { "/".to_string() } else { path })
        } else {
            // 如果不是完整URL，假设是路径
            Ok(if url.starts_with('/') { url.to_string() } else { "/".to_string() })
        }
    }
    
    /// 读取HTTP响应 - 完整的HTTP/1.1协议支持
    async fn read_http_response(
        &self,
        stream: &mut dyn crate::core::ports::NetworkStream,
    ) -> Result<HttpResponse, TakeoverError> {
        
        let mut reader = BufReader::new(stream);
        let mut headers = std::collections::HashMap::new();
        let mut status_code = 200;
        let mut status_text = "OK".to_string();
        
        // 读取状态行
        let mut status_line = String::new();
        reader.read_line(&mut status_line).await
            .map_err(|e| TakeoverError::Config(format!("Failed to read status line: {}", e)))?;
        
        let status_line = status_line.trim();
        debug!("📥 收到HTTP状态行: {}", status_line);
        
        // 解析状态行: HTTP/1.1 200 OK
        let status_parts: Vec<&str> = status_line.split_whitespace().collect();
        if status_parts.len() >= 2 {
            status_code = status_parts[1].parse().unwrap_or(200);
            if status_parts.len() >= 3 {
                status_text = status_parts[2..].join(" ");
            }
        }
        
        // 读取头部并分析传输方式
        let mut content_length: Option<usize> = None;
        let mut is_chunked = false;
        let mut connection_close = false;
        
        loop {
            let mut header_line = String::new();
            reader.read_line(&mut header_line).await
                .map_err(|e| TakeoverError::Config(format!("Failed to read header: {}", e)))?;
            
            let header_line = header_line.trim();
            if header_line.is_empty() {
                break; // 头部结束
            }
            
            if let Some(colon_pos) = header_line.find(':') {
                let name = header_line[..colon_pos].trim().to_string();
                let value = header_line[colon_pos + 1..].trim().to_string();
                
                // 分析关键头部
                match name.to_lowercase().as_str() {
                    "content-length" => {
                        content_length = value.parse().ok();
                        debug!("📏 Content-Length: {:?}", content_length);
                    }
                    "transfer-encoding" => {
                        if value.to_lowercase().contains("chunked") {
                            is_chunked = true;
                            debug!("🔀 检测到Chunked编码");
                        }
                    }
                    "connection" => {
                        if value.to_lowercase().contains("close") {
                            connection_close = true;
                            debug!("🔌 连接将关闭");
                        }
                    }
                    _ => {}
                }
                
                headers.insert(name, value);
            }
        }
        
        // 🎯 根据不同的传输方式读取响应体
        let body = if is_chunked {
            debug!("📦 读取Chunked响应体");
            self.read_chunked_body(&mut reader).await?
        } else if let Some(length) = content_length {
            debug!("📏 读取Content-Length响应体: {}字节", length);
            self.read_content_length_body(&mut reader, length).await?
        } else if connection_close {
            debug!("🔄 读取连接关闭式响应体");
            self.read_until_close_body(&mut reader).await?
        } else {
            debug!("⚠️ 无法确定响应体长度，假设无体或尝试读取剩余数据");
            // 尝试读取一些数据，如果有的话
            match self.read_until_close_body(&mut reader).await {
                Ok(data) => {
                    if !data.is_empty() {
                        debug!("📊 读取到意外响应体: {} 字节", data.len());
                    }
                    data
                }
                Err(_) => Vec::new() // 忽略错误，返回空体
            }
        };
        
        debug!("✅ 读取到HTTP响应: {} {} ({}字节)", status_code, status_text, body.len());
        
        Ok(HttpResponse {
            id: Some(uuid::Uuid::new_v4()),
            status_code,
            status_text: Some(status_text),
            headers,
            body,
            sent_at: Some(std::time::SystemTime::now()),
        })
    }
    
    /// 读取Chunked编码的响应体
    async fn read_chunked_body(
        &self, 
        reader: &mut BufReader<&mut dyn crate::core::ports::NetworkStream>
    ) -> Result<Vec<u8>, TakeoverError> {
        let mut body = Vec::new();
        
        loop {
            // 读取chunk大小行
            let mut size_line = String::new();
            reader.read_line(&mut size_line).await
                .map_err(|e| TakeoverError::Config(format!("Failed to read chunk size: {}", e)))?;
            
            let size_line = size_line.trim();
            if size_line.is_empty() {
                break;
            }
            
            // 解析chunk大小（16进制）
            let chunk_size = if let Some(semicolon_pos) = size_line.find(';') {
                // 忽略chunk扩展
                &size_line[..semicolon_pos]
            } else {
                size_line
            };
            
            let chunk_size = usize::from_str_radix(chunk_size, 16)
                .map_err(|e| TakeoverError::Config(format!("Invalid chunk size: {} - {}", chunk_size, e)))?;
            
            debug!("📦 Chunk大小: {} 字节", chunk_size);
            
            if chunk_size == 0 {
                // 最后一个chunk，读取trailer头部
                self.read_trailer_headers(reader).await?;
                break;
            }
            
            // 读取chunk数据
            let mut chunk_data = vec![0u8; chunk_size];
            reader.read_exact(&mut chunk_data).await
                .map_err(|e| TakeoverError::Config(format!("Failed to read chunk data: {}", e)))?;
            
            body.extend_from_slice(&chunk_data);
            
            // 读取chunk末尾的CRLF
            let mut crlf = String::new();
            reader.read_line(&mut crlf).await
                .map_err(|e| TakeoverError::Config(format!("Failed to read chunk CRLF: {}", e)))?;
        }
        
        debug!("📦 Chunked响应体读取完成: {} 字节", body.len());
        Ok(body)
    }
    
    /// 读取trailer头部（chunked编码末尾）
    async fn read_trailer_headers(
        &self, 
        reader: &mut BufReader<&mut dyn crate::core::ports::NetworkStream>
    ) -> Result<(), TakeoverError> {
        loop {
            let mut trailer_line = String::new();
            reader.read_line(&mut trailer_line).await
                .map_err(|e| TakeoverError::Config(format!("Failed to read trailer: {}", e)))?;
            
            let trailer_line = trailer_line.trim();
            if trailer_line.is_empty() {
                break; // trailer结束
            }
            
            debug!("📝 Trailer头部: {}", trailer_line);
        }
        
        Ok(())
    }
    
    /// 读取指定Content-Length的响应体
    async fn read_content_length_body(
        &self, 
        reader: &mut BufReader<&mut dyn crate::core::ports::NetworkStream>, 
        length: usize
    ) -> Result<Vec<u8>, TakeoverError> {
        if length == 0 {
            return Ok(Vec::new());
        }
        
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).await
            .map_err(|e| TakeoverError::Config(format!("Failed to read response body: {}", e)))?;
        
        debug!("📏 Content-Length响应体读取完成: {} 字节", body.len());
        Ok(body)
    }
    
    /// 读取直到连接关闭的响应体（HTTP/1.0方式）
    async fn read_until_close_body(
        &self, 
        reader: &mut BufReader<&mut dyn crate::core::ports::NetworkStream>
    ) -> Result<Vec<u8>, TakeoverError> {
        let mut body = Vec::new();
        
        // 读取所有剩余数据直到EOF
        match reader.read_to_end(&mut body).await {
            Ok(bytes_read) => {
                debug!("🔄 连接关闭式响应体读取完成: {} 字节", bytes_read);
                Ok(body)
            }
            Err(e) => {
                // 连接关闭是正常的，但其他错误需要报告
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    debug!("🔌 连接正常关闭，响应体: {} 字节", body.len());
                    Ok(body)
                } else {
                    warn!("⚠️ 读取响应体时出现错误: {}, 返回已读取的 {} 字节", e, body.len());
                    Ok(body) // 仍然返回已读取的数据
                }
            }
        }
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> ForwardingStats {
        self.stats.read().await.clone()
    }
}

impl Default for ForwardingMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for ForwardingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(10),
            max_retries: 1,
            retry_delay: Duration::from_millis(500),
            keep_alive: false,
            max_redirects: 5,
            force_tls: false,
            tls_verify: true,
        }
    }
}

#[async_trait]
impl Middleware for ForwardingMiddleware {
    fn name(&self) -> &str {
        "forwarding"
    }
    
    #[instrument(skip(self, context, core), fields(connection_id = %context.id))]
    async fn process(
        &self,
        context: &mut ConnectionContext,
        core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, TakeoverError> {
        if !self.config.enabled {
            debug!("转发中间件已禁用，跳过处理");
            return Ok(MiddlewareResult::Continue);
        }
        
        debug!("转发中间件开始处理");
        
        // 更新处理状态
        context.set_processing_state(ProcessingState::Forwarding);
        
        // 检查是否有请求需要转发
        if context.request.is_none() {
            warn!("没有请求对象可转发，跳过");
            return Ok(MiddlewareResult::Continue);
        }
        
        // 执行转发
        match self.forward_request(context, core).await {
            Ok(response) => {
                debug!("请求转发成功: {}", context.id);
                
                // 将响应存储到上下文中
                context.response = Some(response);
                context.set_processing_state(ProcessingState::ResponseProcessing);
                
                // 记录处理完成
                core.record_request_processed().await;
                
                Ok(MiddlewareResult::Continue)
            }
            Err(e) => {
                error!("⚠️ 转发失败: {} - {}", context.id, e);
                // 记录错误
                core.record_error().await;
                
                Err(e)
            }
        }
    }
    
    fn priority(&self) -> i32 {
        60 // 在规则匹配、脚本执行、HTTP修改之后
    }
    
    fn enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use crate::core::model::Connection;
    
    #[tokio::test]
    async fn test_middleware_creation() {
        let middleware = ForwardingMiddleware::new();
        assert_eq!(middleware.name(), "forwarding");
        assert!(middleware.enabled());
        assert_eq!(middleware.priority(), 60);
        
        let stats = middleware.get_stats().await;
        assert_eq!(stats.total_requests, 0);
        assert_eq!(stats.successful_forwards, 0);
        
        println!("✅ 转发中间件创建成功");
    }
    
    #[test]
    fn test_url_path_extraction() {
        let middleware = ForwardingMiddleware::new();
        
        assert_eq!(
            middleware.extract_path_from_url("http://example.com/path?query=1").unwrap(),
            "/path?query=1"
        );
        
        assert_eq!(
            middleware.extract_path_from_url("https://example.com/").unwrap(),
            "/"
        );
        
        assert_eq!(
            middleware.extract_path_from_url("/api/users").unwrap(),
            "/api/users"
        );
        
        println!("✅ URL路径提取测试通过");
    }
    
    #[test]
    fn test_http_request_building() {
        let middleware = ForwardingMiddleware::new();
        
        let mut request = HttpRequest::new("GET", "http://example.com/api/test?param=1");
        request.headers.insert("User-Agent".to_string(), "test-agent".to_string());
        request.headers.insert("Accept".to_string(), "application/json".to_string());
        
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        
        let context = ConnectionContext::new(connection, "example.com".to_string(), 80);
        
        let protocol = Protocol::Http; // 测试使用HTTP协议
        let http_request = middleware.build_http_request(&request, &context, &protocol).unwrap();
        
        assert!(http_request.contains("GET /api/test?param=1 HTTP/1.1"));
        assert!(http_request.contains("Host: example.com")); // 80端口不显示
        assert!(!http_request.contains("Host: example.com:80")); // 确保80端口不显示
        assert!(http_request.contains("User-Agent: test-agent"));
        assert!(http_request.contains("Accept: application/json"));
        assert!(http_request.contains("Connection: close"));
        
        println!("✅ HTTP请求构建测试通过");
    }
}
