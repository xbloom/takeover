//! HTTP/1.1流处理中间件
//! 
//! 专门处理HTTP/1.1 over TLS连接的中间件：
//! - 检测HTTP/1.1协议（通过ALPN协商结果）
//! - 从TLS流读取和解析HTTP/1.1请求
//! - 转换为标准HttpRequest对象
//! - 与HTTP/2中间件并行工作

use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::io::{AsyncReadExt, AsyncBufReadExt, BufReader};
use tracing::{warn, error, debug, instrument};
use async_trait::async_trait;

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{ConnectionContext, ProcessingState, HttpRequest};
use crate::error::TakeoverError;

/// HTTP/1.1流处理中间件
/// 
/// 专门负责处理HTTP/1.1 over TLS连接
pub struct Http1StreamMiddleware {
    /// 配置
    config: Http1Config,
    /// 统计信息
    stats: Arc<RwLock<Http1Stats>>,
}

/// HTTP/1.1配置
#[derive(Debug, Clone)]
pub struct Http1Config {
    /// 是否启用
    pub enabled: bool,
    /// 最大请求头大小
    pub max_header_size: usize,
    /// 最大请求体大小
    pub max_body_size: usize,
    /// 读取超时
    pub read_timeout: std::time::Duration,
    /// 是否启用管道化
    pub enable_pipelining: bool,
}

/// HTTP/1.1统计信息
#[derive(Debug, Default, Clone)]
pub struct Http1Stats {
    /// 总连接数
    total_connections: u64,
    /// 成功处理的请求数
    successful_requests: u64,
    /// 失败的请求数
    failed_requests: u64,
    /// 解析错误数
    parse_errors: u64,
    /// 超时数
    timeout_errors: u64,
}

impl Http1StreamMiddleware {
    /// 创建新的HTTP/1.1流中间件
    pub fn new() -> Self {
        Self {
            config: Http1Config::default(),
            stats: Arc::new(RwLock::new(Http1Stats::default())),
        }
    }
    
    /// 使用配置创建中间件
    pub fn with_config(config: Http1Config) -> Self {
        Self {
            config,
            stats: Arc::new(RwLock::new(Http1Stats::default())),
        }
    }
    
    /// 检查是否为HTTP/1.1协议
    /// 
    /// 通过以下方式检测HTTP/1.1：
    /// 0. 检查协议检测中间件的结果 (最优先) - 最可靠的协议识别
    /// 1. 检查ALPN协商结果 (优先) - 用于HTTPS
    /// 2. 检查连接元数据中的协议信息
    /// 3. 检查端口号和TLS状态 (基本推断)
    /// 4. 检查普通HTTP端口 (80等) - 用于HTTP
    #[instrument(skip(self, context))]
    async fn is_http1_protocol(&self, context: &ConnectionContext) -> bool {
        // 方法0：检查协议检测中间件的结果 - 最优先和最可靠
        if let Some(detected_protocol) = context.custom_data.get("detected_protocol") {
            if let Some(protocol) = detected_protocol.as_str() {
                debug!("🔍 协议检测中间件结果: {}", protocol);
                if protocol == "httpcleartext" {
                    debug!("🔍 协议检测中间件确认为HTTP明文协议");
                    return true;
                }
                if protocol == "tls" {
                    debug!("🔍 协议检测中间件检测到TLS，继续检查ALPN");
                    // 继续后续检查ALPN结果
                }
            }
        }
        
        // 方法1：检查ALPN协商结果 - 最可靠的方式（仅用于HTTPS）
        if let Some(alpn_result) = context.custom_data.get("alpn_protocol") {
            if let Some(protocol) = alpn_result.as_str() {
                debug!("🔍 ALPN协商协议: {}", protocol);
                return protocol == "http/1.1";
            }
        }
        
        // 方法2：检查TLS元数据中的ALPN信息（仅用于HTTPS）
        if let Some(tls_info) = context.custom_data.get("tls_intercepted") {
            if let Some(alpn_protocol) = tls_info.get("alpn_protocol") {
                if let Some(protocol) = alpn_protocol.as_str() {
                    debug!("🔍 TLS元数据ALPN协议: {}", protocol);
                    return protocol == "http/1.1";
                }
            }
        }
        
        // 方法3：检查连接元数据
        if context.connection.metadata.protocol == "HTTP/1.1" {
            debug!("🔍 连接元数据指示HTTP/1.1");
            return true;
        }
        
        // 方法4：基于端口和TLS状态的基本推断（HTTPS）
        // 如果有TLS流但不是HTTP/2，推断为HTTP/1.1
        if context.target_port == 443 && context.connection.has_tls_streams() {
            // 先确认不是HTTP/2
            if let Some(alpn_result) = context.custom_data.get("alpn_protocol") {
                if let Some(protocol) = alpn_result.as_str() {
                    if protocol != "h2" {
                        debug!("🔍 基于TLS+非h2推断为HTTP/1.1");
                        return true;
                    }
                }
            } else {
                // 没有ALPN信息，但有TLS，可能是HTTP/1.1
                debug!("🔍 基于HTTPS端口+TLS推断可能为HTTP/1.1");
                warn!("⚠️ 无法确定协议类型，基于HTTPS端口推断为HTTP/1.1。建议检查ALPN协商！");
                return true;
            }
        }
        
        // 方法5：检查普通HTTP端口
        // 如果没有TLS流但是HTTP端口，推断为普通HTTP/1.1
        if matches!(context.target_port, 80 | 8080 | 8000 | 3000) && !context.connection.has_tls_streams() {
            debug!("🔍 检测到普通HTTP端口: {}", context.target_port);
            return true;
        }
        
        // 方法6：最后的peek检测
        // 如果既没有明确的协议信息，也没有TLS流，但有TCP流，尝试peek检测HTTP特征
        if !context.connection.has_tls_streams() && context.connection.stream.is_some() {
            debug!("🔍 检测到TCP流但无TLS，尝试peek检测HTTP/1.1特征");
            // 可以尝试peek一些数据来检测HTTP请求特征
            if let Some(ref stream) = context.connection.stream {
                if let Ok(is_http) = self.peek_http_request(stream).await {
                    if is_http {
                        debug!("🔍 通过peek检测到HTTP/1.1请求特征");
                        return true;
                    }
                }
            }
        }
        
        debug!("🔍 协议检测结果：非HTTP/1.1");
        false
    }
    
    /// 使用peek方法检测HTTP请求（非侵入式）
    /// 
    /// 检测HTTP请求的特征：
    /// - 常见的HTTP方法开头 (GET, POST, PUT, DELETE等)
    /// - HTTP版本信息
    async fn peek_http_request(&self, stream: &tokio::net::TcpStream) -> Result<bool, std::io::Error> {
        let mut buf = [0u8; 16]; // 读取前16个字节检测
        
        // 使用peek读取前16个字节，不消耗数据
        let n = stream.peek(&mut buf).await?;
        
        if n < 3 {
            debug!("HTTP检测：数据不足3字节 ({}字节)", n);
            return Ok(false);
        }
        
        // 转换为字符串检测
        let data = String::from_utf8_lossy(&buf[..n]);
        
        // 检测常见的HTTP方法
        let http_methods = ["GET ", "POST ", "PUT ", "DELETE ", "HEAD ", "OPTIONS ", "PATCH ", "CONNECT "];
        let is_http = http_methods.iter().any(|method| data.starts_with(method));
        
        if is_http {
            debug!("HTTP检测：检测到HTTP请求方法: {}", &data[..n.min(16)]);
        } else {
            debug!("HTTP检测：非HTTP请求数据: {}", &data[..n.min(16)]);
        }
        
        Ok(is_http)
    }
    
    /// 从流读取HTTP/1.1请求（支持TLS和TCP流）
    #[instrument(skip(self, context))]
    async fn read_http1_request(&self, context: &mut ConnectionContext) -> Result<HttpRequest, TakeoverError> {
        // 检查是HTTPS还是HTTP连接
        let is_tls_connection = context.connection.has_tls_streams();
        
        if is_tls_connection {
            debug!("📖 开始从TLS流读取HTTPS/1.1请求");
            self.read_http1_from_tls(context).await
        } else {
            debug!("📖 开始从TCP流读取HTTP/1.1请求");
            self.read_http1_from_tcp(context).await
        }
    }
    
    /// 从TLS流读取HTTPS/1.1请求
    async fn read_http1_from_tls(&self, context: &mut ConnectionContext) -> Result<HttpRequest, TakeoverError> {
        // 获取客户端TLS流
        let mut tls_stream = context.connection.inbound_tls_stream.take()
            .ok_or_else(|| TakeoverError::Config("Missing inbound TLS stream".to_string()))?;
        
        // 设置读取超时
        let timeout_duration = self.config.read_timeout;
        
        // 使用BufReader进行高效的行读取
        let mut reader = BufReader::new(&mut tls_stream);
        
        // 读取请求行和头部
        let (request_line, headers) = match tokio::time::timeout(timeout_duration, self.read_headers(&mut reader)).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                debug!("❌ 读取HTTP头部失败: {}", e);
                // 将TLS流放回
                context.connection.inbound_tls_stream = Some(tls_stream);
                return Err(e);
            }
            Err(_) => {
                error!("❌ 读取HTTP头部超时");
                // 将TLS流放回
                context.connection.inbound_tls_stream = Some(tls_stream);
                self.update_timeout_stats().await;
                return Err(TakeoverError::Config("HTTP request read timeout".to_string()));
            }
        };
        
        debug!("📋 HTTP请求行: {}", request_line);
        debug!("📋 HTTP头部数量: {}", headers.len());
        
        // 解析请求行
        let (method, path, version) = self.parse_request_line(&request_line)?;
        
        // 读取请求体（如果有）
        let body = self.read_request_body(&mut reader, &headers).await?;
        
        // 构建完整URL
        let url = self.build_full_url(&context.target_host, context.target_port, &path);
        
        // 创建HttpRequest对象
        let mut http_request = HttpRequest::new(&method, &url);
        http_request.version = version;
        http_request.headers = headers;
        http_request.body = body;
        http_request.sent_at = Some(std::time::SystemTime::now());
        
        // 将TLS流放回（重要！）
        context.connection.inbound_tls_stream = Some(tls_stream);
        
        debug!("✅ HTTP/1.1请求解析完成: {} {}", method, path);
        self.update_success_stats().await;
        
        Ok(http_request)
    }
    
    /// 从TCP流读取HTTP/1.1请求
    async fn read_http1_from_tcp(&self, context: &mut ConnectionContext) -> Result<HttpRequest, TakeoverError> {
        // 获取TCP流
        let mut tcp_stream = context.connection.stream.take()
            .ok_or_else(|| TakeoverError::Config("Missing TCP stream".to_string()))?;
        
        // 设置读取超时
        let timeout_duration = self.config.read_timeout;
        
        // 使用BufReader进行高效的行读取
        let mut reader = BufReader::new(&mut tcp_stream);
        
        // 读取请求行和头部
        let (request_line, headers) = match tokio::time::timeout(timeout_duration, self.read_headers(&mut reader)).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                debug!("❌ 读取HTTP头部失败: {}", e);
                // 将TCP流放回
                context.connection.stream = Some(tcp_stream);
                return Err(e);
            }
            Err(_) => {
                error!("❌ 读取HTTP头部超时");
                // 将TCP流放回
                context.connection.stream = Some(tcp_stream);
                self.update_timeout_stats().await;
                return Err(TakeoverError::Config("HTTP request read timeout".to_string()));
            }
        };
        
        debug!("📋 HTTP请求行: {}", request_line);
        debug!("📋 HTTP头部数量: {}", headers.len());
        
        // 解析请求行
        let (method, path, version) = self.parse_request_line(&request_line)?;
        
        // 读取请求体（如果有）
        let body = self.read_request_body(&mut reader, &headers).await?;
        
        // 构建完整URL
        let url = self.build_full_url(&context.target_host, context.target_port, &path);
        
        // 创建HttpRequest对象
        let mut http_request = HttpRequest::new(&method, &url);
        http_request.version = version;
        http_request.headers = headers;
        http_request.body = body;
        http_request.sent_at = Some(std::time::SystemTime::now());
        
        // 将TCP流放回（重要！）
        context.connection.stream = Some(tcp_stream);
        
        debug!("✅ HTTP/1.1请求解析完成: {} {}", method, path);
        self.update_success_stats().await;
        
        Ok(http_request)
    }
    
    /// 读取HTTP头部
    async fn read_headers<R>(&self, reader: &mut BufReader<R>) -> Result<(String, std::collections::HashMap<String, String>), TakeoverError>
    where
        R: AsyncReadExt + Unpin,
    {
        let mut headers = std::collections::HashMap::new();
        let mut total_size = 0;
        let mut request_line = String::new();
        let mut first_line = true;
        
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break, // EOF
                Ok(n) => {
                    total_size += n;
                    if total_size > self.config.max_header_size {
                        return Err(TakeoverError::Config("HTTP headers too large".to_string()));
                    }
                }
                Err(e) => {
                    return Err(TakeoverError::Config(format!("Failed to read line: {}", e)));
                }
            }
            
            // 去除行尾换行符
            let line = line.trim_end();
            
            if first_line {
                request_line = line.to_string();
                first_line = false;
                continue;
            }
            
            // 空行表示头部结束
            if line.is_empty() {
                break;
            }
            
            // 解析头部字段
            if let Some(colon_pos) = line.find(':') {
                let name = line[..colon_pos].trim().to_lowercase();
                let value = line[colon_pos + 1..].trim().to_string();
                headers.insert(name, value);
            }
        }
        
        Ok((request_line, headers))
    }
    
    /// 解析请求行
    fn parse_request_line(&self, request_line: &str) -> Result<(String, String, String), TakeoverError> {
        let parts: Vec<&str> = request_line.split_whitespace().collect();
        
        if parts.len() != 3 {
            return Err(TakeoverError::Config(format!("Invalid request line: {}", request_line)));
        }
        
        let method = parts[0].to_string();
        let path = parts[1].to_string();
        let version = match parts[2] {
            "HTTP/1.0" => "HTTP/1.0".to_string(),
            "HTTP/1.1" => "HTTP/1.1".to_string(),
            _ => {
                return Err(TakeoverError::Config(format!("Unsupported HTTP version: {}", parts[2])));
            }
        };
        
        Ok((method, path, version))
    }
    
    /// 读取请求体
    async fn read_request_body<R>(&self, reader: &mut BufReader<R>, headers: &std::collections::HashMap<String, String>) -> Result<Vec<u8>, TakeoverError>
    where
        R: AsyncReadExt + Unpin,
    {
        // 检查Content-Length头部
        if let Some(content_length_str) = headers.get("content-length") {
            let content_length: usize = content_length_str.parse()
                .map_err(|_| TakeoverError::Config("Invalid Content-Length header".to_string()))?;
            
            if content_length == 0 {
                return Ok(Vec::new());
            }
            
            if content_length > self.config.max_body_size {
                return Err(TakeoverError::Config("Request body too large".to_string()));
            }
            
            // 读取指定长度的请求体
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).await
                .map_err(|e| TakeoverError::Config(format!("Failed to read request body: {}", e)))?;
            
            Ok(body)
        } else {
            // 没有Content-Length，假设没有请求体
            Ok(Vec::new())
        }
    }
    
    /// 构建完整URL
    fn build_full_url(&self, host: &str, port: u16, path: &str) -> String {
        let scheme = if port == 443 { "https" } else { "http" };
        
        let port_suffix = if (scheme == "https" && port == 443) || (scheme == "http" && port == 80) {
            String::new()
        } else {
            format!(":{}", port)
        };
        
        format!("{}://{}{}{}", scheme, host, port_suffix, path)
    }
    
    /// 更新成功统计
    async fn update_success_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.successful_requests += 1;
    }
    
    /// 更新失败统计
    async fn update_failure_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.failed_requests += 1;
    }
    
    /// 更新解析错误统计
    async fn update_parse_error_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.parse_errors += 1;
    }
    
    /// 更新超时统计
    async fn update_timeout_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.timeout_errors += 1;
    }
    
    /// 更新连接统计
    async fn update_connection_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.total_connections += 1;
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> Http1Stats {
        self.stats.read().await.clone()
    }
}

impl Default for Http1StreamMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for Http1Config {
    fn default() -> Self {
        Self {
            enabled: true,
            max_header_size: 8192,      // 8KB for headers
            max_body_size: 10 * 1024 * 1024, // 10MB for body
            read_timeout: std::time::Duration::from_secs(30),
            enable_pipelining: false,    // 默认禁用管道化
        }
    }
}

#[async_trait]
impl Middleware for Http1StreamMiddleware {
    fn name(&self) -> &str {
        "http1_stream"
    }
    
    #[instrument(skip(self, context, _core))]
    async fn process(
        &self,
        context: &mut ConnectionContext,
        _core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, TakeoverError> {
        debug!("HTTP/1.1 stream middleware processing connection {}", context.id);
        
        // 🔍 第一步：检查是否为HTTP/1.1协议
        if !self.is_http1_protocol(context).await {
            debug!("⏭️ 非HTTP/1.1协议，跳过HTTP/1.1中间件处理: {}", context.id);
            return Ok(MiddlewareResult::Continue);
        }
        
        debug!("🔗 HTTP/1.1连接开始: {}", context.id);
        
        // 更新处理状态
        context.set_processing_state(ProcessingState::HttpParsing);
        
        // 更新连接统计
        self.update_connection_stats().await;
        
        // 检查连接类型：HTTPS(有TLS流) 还是 HTTP明文(有TCP流)
        let has_tls = context.connection.has_tls_streams();
        let has_tcp = context.connection.stream.is_some();
        
        if has_tls {
            debug!("🔗 检测到TLS流，开始HTTPS/1.1请求解析: {}", context.id);
        } else if has_tcp {
            debug!("🔗 检测到TCP流，开始HTTP/1.1请求解析: {}", context.id);
        } else {
            warn!("⚠️ 没有可用的TLS流或TCP流，跳过HTTP/1.1处理");
            return Ok(MiddlewareResult::Continue);
        }
        
        // 读取并解析HTTP/1.1请求（支持TLS和TCP）
        match self.read_http1_request(context).await {
            Ok(http_request) => {
                debug!("✅ HTTP/1.1请求解析成功: {} {}", http_request.method, http_request.url);
                
                // 存储解析的请求到context
                context.request = Some(http_request);
                
                // 标记为HTTP/1.1处理状态
                context.custom_data.insert(
                    "http1_processing".to_string(),
                    serde_json::json!({
                        "success": true,
                        "connection_id": context.id,
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    })
                );
                
                debug!("HTTP/1.1请求解析完成，继续后续中间件处理");
                Ok(MiddlewareResult::Continue)
            }
            Err(e) => {
                debug!("❌ HTTP/1.1请求解析失败: {}", e);
                
                self.update_failure_stats().await;
                self.update_parse_error_stats().await;
                
                // 标记为解析失败
                context.custom_data.insert(
                    "http1_processing".to_string(),
                    serde_json::json!({
                        "success": false,
                        "error": e.to_string(),
                        "connection_id": context.id,
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    })
                );
                
                Err(e)
            }
        }
    }
    
    fn priority(&self) -> i32 {
        16 // 在TLS(10)和HTTP/2(15)之后，规则匹配(30)之前
    }
    
    fn enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Connection, ConnectionContext};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    
    #[tokio::test]
    async fn test_middleware_creation() {
        let middleware = Http1StreamMiddleware::new();
        assert_eq!(middleware.name(), "http1_stream");
        assert!(middleware.enabled());
        assert_eq!(middleware.priority(), 16);
        
        let stats = middleware.get_stats().await;
        assert_eq!(stats.total_connections, 0);
        assert_eq!(stats.successful_requests, 0);
        
        println!("✅ HTTP/1.1流中间件创建成功");
    }
    
    #[tokio::test]
    async fn test_config_creation() {
        let config = Http1Config {
            enabled: true,
            max_header_size: 4096,
            max_body_size: 5 * 1024 * 1024, // 5MB
            read_timeout: std::time::Duration::from_secs(60),
            enable_pipelining: true,
        };
        
        let middleware = Http1StreamMiddleware::with_config(config);
        assert_eq!(middleware.name(), "http1_stream");
        
        println!("✅ 自定义配置HTTP/1.1流中间件创建成功");
    }
    
    #[tokio::test]
    async fn test_protocol_detection() {
        let middleware = Http1StreamMiddleware::new();
        
        // 测试1：ALPN协商为http/1.1的情况
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443)
        );
        let mut context = ConnectionContext::new(connection, "example.com".to_string(), 443);
        context.custom_data.insert("alpn_protocol".to_string(), serde_json::json!("http/1.1"));
        
        assert!(middleware.is_http1_protocol(&context).await);
        
        // 测试2：ALPN协商为h2的情况
        let connection2 = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443)
        );
        let mut context2 = ConnectionContext::new(connection2, "example.com".to_string(), 443);
        context2.custom_data.insert("alpn_protocol".to_string(), serde_json::json!("h2"));
        
        assert!(!middleware.is_http1_protocol(&context2).await);
        
        // 测试3：普通HTTP连接
        let connection3 = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        let context3 = ConnectionContext::new(connection3, "example.com".to_string(), 80);
        
        assert!(!middleware.is_http1_protocol(&context3).await);
        
        println!("✅ 协议检测测试通过");
    }
    
    #[test]
    fn test_request_line_parsing() {
        let middleware = Http1StreamMiddleware::new();
        
        let (method, path, version) = middleware.parse_request_line("GET /api/test?param=value HTTP/1.1").unwrap();
        assert_eq!(method, "GET");
        assert_eq!(path, "/api/test?param=value");
        assert_eq!(version, "HTTP/1.1");
        
        let (method2, path2, version2) = middleware.parse_request_line("POST /data HTTP/1.0").unwrap();
        assert_eq!(method2, "POST");
        assert_eq!(path2, "/data");
        assert_eq!(version2, "HTTP/1.0");
        
        println!("✅ 请求行解析测试通过");
    }
    
    #[test]
    fn test_url_building() {
        let middleware = Http1StreamMiddleware::new();
        
        let url1 = middleware.build_full_url("example.com", 443, "/api/test");
        assert_eq!(url1, "https://example.com/api/test");
        
        let url2 = middleware.build_full_url("example.com", 80, "/api/test");
        assert_eq!(url2, "http://example.com/api/test");
        
        let url3 = middleware.build_full_url("example.com", 8080, "/api/test");
        assert_eq!(url3, "http://example.com:8080/api/test");
        
        let url4 = middleware.build_full_url("example.com", 8443, "/api/test");
        assert_eq!(url4, "https://example.com:8443/api/test");
        
        println!("✅ URL构建测试通过");
    }
    
    #[tokio::test]
    async fn test_stats_tracking() {
        let middleware = Http1StreamMiddleware::new();
        
        let initial_stats = middleware.get_stats().await;
        assert_eq!(initial_stats.total_connections, 0);
        
        middleware.update_connection_stats().await;
        middleware.update_success_stats().await;
        middleware.update_failure_stats().await;
        
        let updated_stats = middleware.get_stats().await;
        assert_eq!(updated_stats.total_connections, 1);
        assert_eq!(updated_stats.successful_requests, 1);
        assert_eq!(updated_stats.failed_requests, 1);
        
        println!("✅ 统计跟踪测试通过");
    }
    
    #[tokio::test]
    async fn test_http_cleartext_protocol_detection() {
        let middleware = Http1StreamMiddleware::new();
        
        // 测试协议检测中间件设置的HTTP明文协议标记
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        let mut context = ConnectionContext::new(connection, "example.com".to_string(), 80);
        
        // 模拟协议检测中间件的结果
        context.custom_data.insert(
            "detected_protocol".to_string(),
            serde_json::json!("httpcleartext")
        );
        
        assert!(middleware.is_http1_protocol(&context).await);
        
        // 测试TLS协议标记
        let mut context_tls = context.clone();
        context_tls.custom_data.insert(
            "detected_protocol".to_string(),
            serde_json::json!("tls")
        );
        context_tls.custom_data.insert(
            "alpn_protocol".to_string(),
            serde_json::json!("http/1.1")
        );
        
        assert!(middleware.is_http1_protocol(&context_tls).await);
        
        // 测试非HTTP协议
        let mut context_other = context.clone();
        context_other.custom_data.insert(
            "detected_protocol".to_string(),
            serde_json::json!("ssh")
        );
        
        assert!(!middleware.is_http1_protocol(&context_other).await);
        
        println!("✅ HTTP明文协议检测测试通过");
    }
}
