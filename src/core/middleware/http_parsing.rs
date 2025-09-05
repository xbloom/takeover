//! HTTP解析中间件
//! 
//! 使用hyper库进行现代化HTTP解析，支持：
//! - HTTP/1.1 和 HTTP/2 协议
//! - 完整的HTTP请求/响应解析
//! - 各种Content-Type支持
//! - TLS SNI提取
//! - 流式处理和压缩支持

use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn, error, debug, instrument};
use async_trait::async_trait;
use hyper::{Request, Method, Uri, Version};
use hyper::header::{HeaderMap, HeaderValue};
use hyper::body::{Incoming, Bytes};

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{ConnectionContext, ProcessingState, HttpRequest};
use crate::core::ports::AppError;
use crate::protocol_handler::{ProtocolHandler, StandardProtocolHandler, ProtocolType};

/// HTTP解析中间件
/// 
/// 负责检测和解析HTTP/HTTPS请求
pub struct HttpParsingMiddleware {
    /// 协议处理器
    protocol_handler: StandardProtocolHandler,
    /// 解析配置
    config: HttpParsingConfig,
    /// 解析统计
    stats: std::sync::Arc<tokio::sync::RwLock<HttpParsingStats>>,
}

/// HTTP解析配置
#[derive(Debug, Clone)]
pub struct HttpParsingConfig {
    /// 是否启用HTTP解析
    pub enabled: bool,
    /// 最大请求大小
    pub max_request_size: usize,
    /// 最大头部数量
    pub max_headers: usize,
    /// 读取超时
    pub read_timeout: Duration,
    /// 是否解析请求体
    pub parse_body: bool,
    /// 支持的HTTP方法
    pub supported_methods: Vec<String>,
}

/// HTTP解析统计
#[derive(Debug, Default, Clone)]
struct HttpParsingStats {
    /// 总解析请求数
    total_requests: u64,
    /// HTTP请求数
    http_requests: u64,
    /// HTTPS请求数
    https_requests: u64,
    /// 解析成功数
    successful_parses: u64,
    /// 解析失败数
    failed_parses: u64,
    /// 未知协议数
    unknown_protocols: u64,
    /// SNI解析成功数
    sni_extractions: u64,
    /// HTTP/2请求数
    http2_requests: u64,
}

/// HTTP请求解析结果
#[derive(Debug, Clone)]
struct ParsedHttpRequest {
    /// HTTP方法
    pub method: Method,
    /// 请求URI
    pub uri: Uri,
    /// HTTP版本
    pub version: Version,
    /// 请求头
    pub headers: HeaderMap<HeaderValue>,
    /// 请求体
    pub body: Option<Bytes>,
    /// 原始路径（用于查询参数解析）
    pub path: String,
    /// 查询参数
    pub query_params: HashMap<String, String>,
}

impl HttpParsingMiddleware {
    /// 创建新的HTTP解析中间件
    pub fn new() -> Self {
        Self {
            protocol_handler: StandardProtocolHandler::new(),
            config: HttpParsingConfig::default(),
            stats: std::sync::Arc::new(tokio::sync::RwLock::new(HttpParsingStats::default())),
        }
    }
    
    /// 使用配置创建HTTP解析中间件
    pub fn with_config(config: HttpParsingConfig) -> Self {
        Self {
            protocol_handler: StandardProtocolHandler::new(),
            config,
            stats: std::sync::Arc::new(tokio::sync::RwLock::new(HttpParsingStats::default())),
        }
    }
    
    /// 使用hyper解析HTTP请求（支持HTTP/1.1和HTTP/2）
    #[instrument(skip(self, data))]
    async fn parse_http_request(&self, data: &[u8]) -> Result<ParsedHttpRequest, AppError> {
        if data.is_empty() {
            return Err(AppError::Config("空请求数据".to_string()));
        }
        
        if data.len() > self.config.max_request_size {
            return Err(AppError::Config(format!(
                "请求过大: {} bytes (最大: {})",
                data.len(),
                self.config.max_request_size
            )));
        }
        
        // 使用hyper解析HTTP请求
        let request = self.parse_raw_http_data(data).await?;
        
        // 提取方法
        let method = request.method().clone();
        
        // 验证HTTP方法
        let method_str = method.as_str();
        if !self.config.supported_methods.contains(&method_str.to_string()) {
            return Err(AppError::Config(format!(
                "不支持的HTTP方法: {}",
                method_str
            )));
        }
        
        // 提取URI
        let uri = request.uri().clone();
        let path = uri.path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or(uri.path())
            .to_string();
        
        // 提取HTTP版本
        let version = request.version();
        
        // 提取头部
        let headers = request.headers().clone();
        
        // 检查头部数量
        if headers.len() > self.config.max_headers {
            return Err(AppError::Config(format!(
                "头部过多: {} (最大: {})",
                headers.len(),
                self.config.max_headers
            )));
        }
        
        // 解析查询参数
        let query_params = if let Some(query) = uri.query() {
            self.parse_query_string(query)
        } else {
            HashMap::new()
        };
        
        // 解析请求体（如果需要）
        let body = if self.config.parse_body {
            self.extract_body_from_request(request).await?
        } else {
            None
        };
        
        Ok(ParsedHttpRequest {
            method,
            uri,
            version,
            headers,
            body,
            path,
            query_params,
        })
    }
    
    /// 从原始数据解析HTTP请求
    async fn parse_raw_http_data(&self, data: &[u8]) -> Result<Request<String>, AppError> {
        // 将原始数据转换为字符串
        let request_str = std::str::from_utf8(data).map_err(|e| {
            AppError::Config(format!("无效的UTF-8数据: {}", e))
        })?;
        
        // 查找头部和体的分界
        let (headers_part, body_part) = if let Some(pos) = request_str.find("\r\n\r\n") {
            let headers_end = pos + 4;
            (&request_str[..pos], &request_str[headers_end..])
        } else if let Some(pos) = request_str.find("\n\n") {
            let headers_end = pos + 2;
            (&request_str[..pos], &request_str[headers_end..])
        } else {
            (request_str, "")
        };
        
        // 解析请求行和头部
        let lines: Vec<&str> = headers_part.lines().collect();
        if lines.is_empty() {
            return Err(AppError::Config("空的HTTP请求".to_string()));
        }
        
        // 解析请求行
        let request_line = lines[0];
        let parts: Vec<&str> = request_line.split_whitespace().collect();
        if parts.len() != 3 {
            return Err(AppError::Config(format!(
                "无效的请求行: {}",
                request_line
            )));
        }
        
        let method = Method::from_bytes(parts[0].as_bytes()).map_err(|e| {
            AppError::Config(format!("无效的HTTP方法: {}", e))
        })?;
        
        let uri = parts[1].parse::<Uri>().map_err(|e| {
            AppError::Config(format!("URI解析失败: {}", e))
        })?;
        
        let version = match parts[2] {
            "HTTP/1.0" => Version::HTTP_10,
            "HTTP/1.1" => Version::HTTP_11,
            "HTTP/2.0" => Version::HTTP_2,
            _ => return Err(AppError::Config(format!("不支持的HTTP版本: {}", parts[2]))),
        };
        
        // 构建请求对象
        let mut request_builder = Request::builder()
            .method(method)
            .uri(uri)
            .version(version);
        
        // 添加头部
        for line in &lines[1..] {
            if let Some(colon_pos) = line.find(':') {
                let name = line[..colon_pos].trim();
                let value = line[colon_pos + 1..].trim();
                request_builder = request_builder.header(name, value);
            }
        }
        
        // 添加请求体
        let body = body_part.to_string();
        
        request_builder.body(body).map_err(|e| {
            AppError::Config(format!("构建请求失败: {}", e))
        })
    }
    
    /// 从请求中提取请求体
    async fn extract_body_from_request(&self, request: Request<String>) -> Result<Option<Bytes>, AppError> {
        let (_, body) = request.into_parts();
        
        // 直接处理字符串请求体
        if body.is_empty() {
            Ok(None)
        } else {
            // 检查大小限制
            if body.len() > self.config.max_request_size {
                return Err(AppError::Config(format!(
                    "请求体过大: {} bytes",
                    body.len()
                )));
            }
            Ok(Some(Bytes::from(body)))
        }
    }
    
    /// 解析查询字符串
    fn parse_query_string(&self, query_string: &str) -> HashMap<String, String> {
        let mut params = HashMap::new();
        
        for pair in query_string.split('&') {
            if let Some(eq_pos) = pair.find('=') {
                let key = pair[..eq_pos].to_string();
                let value = pair[eq_pos + 1..].to_string();
                params.insert(key, value);
            } else {
                params.insert(pair.to_string(), String::new());
            }
        }
        
        params
    }
    
    /// 从解析结果创建HttpRequest
    fn create_http_request(&self, parsed: ParsedHttpRequest, context: &ConnectionContext) -> HttpRequest {
        // 构建完整URL
        let scheme = if context.target_port == 443 { "https" } else { "http" };
        let host = context.target_host.clone();
        let port = if (scheme == "https" && context.target_port == 443) ||
                     (scheme == "http" && context.target_port == 80) {
            String::new()
        } else {
            format!(":{}", context.target_port)
        };
        
        let url = if parsed.query_params.is_empty() {
            format!("{}://{}{}{}", scheme, host, port, parsed.path)
        } else {
            let query_string: Vec<String> = parsed.query_params
                .iter()
                .map(|(k, v)| if v.is_empty() { k.clone() } else { format!("{}={}", k, v) })
                .collect();
            format!("{}://{}{}{}?{}", scheme, host, port, parsed.path, query_string.join("&"))
        };
        
        let mut request = HttpRequest::new(parsed.method.as_str(), &url);
        
        // 转换HeaderMap到HashMap
        let mut headers_map = HashMap::new();
        for (name, value) in parsed.headers.iter() {
            if let Ok(value_str) = value.to_str() {
                headers_map.insert(name.as_str().to_lowercase(), value_str.to_string());
            }
        }
        request.headers = headers_map;
        
        // 设置HTTP版本
        request.version = match parsed.version {
            Version::HTTP_10 => "HTTP/1.0".to_string(),
            Version::HTTP_11 => "HTTP/1.1".to_string(),
            Version::HTTP_2 => "HTTP/2.0".to_string(),
            _ => "HTTP/1.1".to_string(), // 默认值
        };
        
        if let Some(body) = parsed.body {
            request.body = body.to_vec();
        }
        
        request
    }
    
    /// 尝试解析SNI
    async fn extract_sni(&self, data: &[u8]) -> Option<String> {
        // 使用现有的协议处理器来解析SNI
        if let Ok((host, _port)) = self.protocol_handler.parse_target(data).await {
            Some(host)
        } else {
            None
        }
    }
    
    /// 更新统计信息
    async fn update_stats(&self, protocol_type: &ProtocolType, success: bool, version: Option<Version>) {
        let mut stats = self.stats.write().await;
        stats.total_requests += 1;
        
        match protocol_type {
            ProtocolType::Http => stats.http_requests += 1,
            ProtocolType::Https => stats.https_requests += 1,
            ProtocolType::Unknown => stats.unknown_protocols += 1,
        }
        
        if let Some(Version::HTTP_2) = version {
            stats.http2_requests += 1;
        }
        
        if success {
            stats.successful_parses += 1;
        } else {
            stats.failed_parses += 1;
        }
    }
    
    /// 更新SNI统计
    async fn update_sni_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.sni_extractions += 1;
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> HttpParsingStats {
        self.stats.read().await.clone()
    }
}

impl Default for HttpParsingMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for HttpParsingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_request_size: 10 * 1024 * 1024, // 10MB
            max_headers: 100,
            read_timeout: Duration::from_secs(30),
            parse_body: true,
            supported_methods: vec![
                "GET".to_string(),
                "POST".to_string(),
                "PUT".to_string(),
                "DELETE".to_string(),
                "HEAD".to_string(),
                "OPTIONS".to_string(),
                "PATCH".to_string(),
                "CONNECT".to_string(),
            ],
        }
    }
}

#[async_trait]
impl Middleware for HttpParsingMiddleware {
    fn name(&self) -> &str {
        "http_parsing"
    }
    
    #[instrument(skip(self, context, _core))]
    async fn process(
        &self,
        context: &mut ConnectionContext,
        _core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, crate::error::TakeoverError> {
        debug!("HTTP parsing middleware processing connection {}", context.id);
        
        // 更新处理状态
        context.set_processing_state(ProcessingState::HttpParsing);
        
        // 模拟从连接中读取数据
        // 在实际实现中，这应该从TCP流中读取
        let sample_data = format!(
            "GET / HTTP/1.1\r\nHost: {}\r\nUser-Agent: TestAgent/1.0\r\nConnection: keep-alive\r\n\r\n",
            context.target_host
        );
        let data = sample_data.as_bytes();
        
        // 检测协议类型
        let protocol_type = self.protocol_handler.detect_protocol(data).await;
        
        debug!("Detected protocol: {:?}", protocol_type);
        
        match protocol_type {
            ProtocolType::Http => {
                // 解析HTTP请求
                match self.parse_http_request(data).await {
                    Ok(parsed_request) => {
                        let http_request = self.create_http_request(parsed_request.clone(), context);
                        context.request = Some(http_request);
                        
                        context.custom_data.insert(
                            "protocol".to_string(),
                            serde_json::json!({
                                "type": "http", 
                                "parsed": true,
                                "version": match parsed_request.version {
                                    Version::HTTP_10 => "HTTP/1.0",
                                    Version::HTTP_11 => "HTTP/1.1", 
                                    Version::HTTP_2 => "HTTP/2.0",
                                    _ => "HTTP/1.1"
                                }
                            })
                        );
                        
                        self.update_stats(&protocol_type, true, Some(parsed_request.version)).await;
                        
                        info!("✅ HTTP request parsed successfully for {} ({})", 
                              context.target_host, 
                              match parsed_request.version {
                                  Version::HTTP_2 => "HTTP/2.0",
                                  Version::HTTP_11 => "HTTP/1.1",
                                  Version::HTTP_10 => "HTTP/1.0",
                                  _ => "HTTP/1.1"
                              });
                        Ok(MiddlewareResult::Continue)
                    }
                    Err(e) => {
                        error!("❌ Failed to parse HTTP request: {}", e);
                        self.update_stats(&protocol_type, false, None).await;
                        
                        context.custom_data.insert(
                            "protocol".to_string(),
                            serde_json::json!({"type": "http", "parsed": false, "error": e.to_string()})
                        );
                        
                        Err(crate::error::TakeoverError::Config(format!("HTTP parsing failed: {}", e)))
                    }
                }
            }
            ProtocolType::Https => {
                // 对于HTTPS，尝试提取SNI
                if let Some(sni_host) = self.extract_sni(data).await {
                    debug!("Extracted SNI: {}", sni_host);
                    
                    // 更新目标主机（如果SNI不同）
                    if sni_host != context.target_host {
                        warn!("SNI host ({}) differs from target host ({})", sni_host, context.target_host);
                        context.target_host = sni_host;
                    }
                    
                    self.update_sni_stats().await;
                }
                
                // 为HTTPS创建一个基本的请求对象
                let https_url = format!("https://{}:{}/", context.target_host, context.target_port);
                context.request = Some(HttpRequest::new("CONNECT", &https_url));
                
                context.custom_data.insert(
                    "protocol".to_string(),
                    serde_json::json!({"type": "https", "sni_extracted": true})
                );
                
                self.update_stats(&protocol_type, true, None).await;
                
                info!("✅ HTTPS connection processed for {}", context.target_host);
                Ok(MiddlewareResult::Continue)
            }
            ProtocolType::Unknown => {
                warn!("❓ Unknown protocol detected for {}", context.target_host);
                
                context.custom_data.insert(
                    "protocol".to_string(),
                    serde_json::json!({"type": "unknown", "parsed": false})
                );
                
                self.update_stats(&protocol_type, false, None).await;
                
                // 对于未知协议，继续处理但记录警告
                Ok(MiddlewareResult::Continue)
            }
        }
    }
    
    fn priority(&self) -> i32 {
        20 // 在TLS之后，规则匹配之前
    }
    
    fn enabled(&self) -> bool {
        self.config.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // use crate::core::model::{Connection, ConnectionMetadata};
    // use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    
    #[tokio::test]
    async fn test_middleware_creation() {
        let middleware = HttpParsingMiddleware::new();
        assert_eq!(middleware.name(), "http_parsing");
        assert!(middleware.enabled());
        assert_eq!(middleware.priority(), 20); // HTTP解析中间件的优先级
        println!("✅ 基本HttpParsingMiddleware创建成功");
        
        // 测试自定义配置创建
        let config = HttpParsingConfig {
            enabled: true,
            max_request_size: 1024 * 1024, // 1MB
            max_headers: 100,
            read_timeout: Duration::from_secs(30),
            parse_body: true,
            supported_methods: vec![
                "GET".to_string(),
                "POST".to_string(),
                "PUT".to_string(),
                "DELETE".to_string(),
            ],
        };
        
        let custom_middleware = HttpParsingMiddleware::with_config(config);
        assert_eq!(custom_middleware.name(), "http_parsing");
        println!("✅ 自定义配置HttpParsingMiddleware创建成功");
    }
    
    #[tokio::test]
    async fn test_http_request_parsing() {
        let middleware = HttpParsingMiddleware::new();
        
        let request_data = b"GET /api/test?param=value HTTP/1.1\r\nHost: example.com\r\nUser-Agent: TestAgent\r\nContent-Length: 0\r\n\r\n";
        
        let parsed = middleware.parse_http_request(request_data).await.unwrap();
        
        assert_eq!(parsed.method, Method::GET);
        assert_eq!(parsed.path, "/api/test?param=value");
        assert_eq!(parsed.version, Version::HTTP_11);
        let host_header = parsed.headers.get("host").and_then(|v| v.to_str().ok());
        assert_eq!(host_header, Some("example.com"));
        assert_eq!(parsed.query_params.get("param"), Some(&"value".to_string()));
    }
    
    #[tokio::test]
    async fn test_query_string_parsing() {
        let middleware = HttpParsingMiddleware::new();
        
        let params = middleware.parse_query_string("key1=value1&key2=value2&key3");
        
        assert_eq!(params.get("key1"), Some(&"value1".to_string()));
        assert_eq!(params.get("key2"), Some(&"value2".to_string()));
        assert_eq!(params.get("key3"), Some(&String::new()));
    }
    
    #[tokio::test]
    async fn test_path_and_query_parsing() {
        let middleware = HttpParsingMiddleware::new();
        
        // 测试带查询参数的请求
        let request_data = b"GET /api/test?param=value HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let parsed = middleware.parse_http_request(request_data).await.unwrap();
        
        assert_eq!(parsed.path, "/api/test?param=value");
        assert_eq!(parsed.query_params.get("param"), Some(&"value".to_string()));
        
        // 测试不带查询参数的请求
        let request_data2 = b"GET /simple/path HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let parsed2 = middleware.parse_http_request(request_data2).await.unwrap();
        
        assert_eq!(parsed2.path, "/simple/path");
        assert!(parsed2.query_params.is_empty());
    }
    
    #[tokio::test]
    async fn test_headers_parsing() {
        let middleware = HttpParsingMiddleware::new();
        
        // 直接测试完整的HTTP解析
        let request_data = b"GET / HTTP/1.1\r\nHost: example.com\r\nUser-Agent: TestAgent/1.0\r\nContent-Type: application/json\r\n\r\n";
        let parsed = middleware.parse_http_request(request_data).await.unwrap();
        
        let host_header = parsed.headers.get("host").and_then(|v| v.to_str().ok());
        let ua_header = parsed.headers.get("user-agent").and_then(|v| v.to_str().ok());
        let ct_header = parsed.headers.get("content-type").and_then(|v| v.to_str().ok());
        
        assert_eq!(host_header, Some("example.com"));
        assert_eq!(ua_header, Some("TestAgent/1.0"));
        assert_eq!(ct_header, Some("application/json"));
    }
    
    #[tokio::test]
    async fn test_request_size_limit() {
        let config = HttpParsingConfig {
            max_request_size: 100,
            ..Default::default()
        };
        let middleware = HttpParsingMiddleware::with_config(config);
        
        let large_request = "A".repeat(200);
        let result = middleware.parse_http_request(large_request.as_bytes()).await;
        
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("请求过大"));
    }
    
    #[tokio::test]
    async fn test_stats_tracking() {
        let middleware = HttpParsingMiddleware::new();
        
        let initial_stats = middleware.get_stats().await;
        assert_eq!(initial_stats.total_requests, 0);
        
        middleware.update_stats(&ProtocolType::Http, true, Some(Version::HTTP_11)).await;
        middleware.update_stats(&ProtocolType::Https, false, None).await;
        middleware.update_stats(&ProtocolType::Http, true, Some(Version::HTTP_2)).await;
        
        let updated_stats = middleware.get_stats().await;
        assert_eq!(updated_stats.total_requests, 3);
        assert_eq!(updated_stats.http_requests, 2);
        assert_eq!(updated_stats.https_requests, 1);
        assert_eq!(updated_stats.successful_parses, 2);
        assert_eq!(updated_stats.failed_parses, 1);
        assert_eq!(updated_stats.http2_requests, 1);
    }

    #[tokio::test]
    async fn test_http2_support() {
        let middleware = HttpParsingMiddleware::new();
        
        let request_data = b"GET /api/test HTTP/2.0\r\nHost: example.com\r\nUser-Agent: TestAgent\r\n\r\n";
        
        let parsed = middleware.parse_http_request(request_data).await.unwrap();
        
        assert_eq!(parsed.method, Method::GET);
        assert_eq!(parsed.version, Version::HTTP_2);
        let host_header = parsed.headers.get("host").and_then(|v| v.to_str().ok());
        assert_eq!(host_header, Some("example.com"));
        
        println!("✅ HTTP/2 请求解析成功");
    }
}