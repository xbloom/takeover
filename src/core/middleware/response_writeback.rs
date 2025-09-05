//! 响应写回中间件
//! 
//! 统一输出架构的关键组件，负责：
//! - 检测协议类型（HTTP/1.1 vs HTTP/2）
//! - 统一HTTP/1.1响应写回处理
//! - 处理连接生命周期和keep-alive

use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::io::{AsyncWriteExt};
use tracing::{info, warn, error, debug, instrument};
use async_trait::async_trait;

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{ConnectionContext, ProcessingState, HttpResponse};
use crate::error::TakeoverError;

/// 响应写回中间件
/// 
/// 负责统一的HTTP响应输出处理
pub struct ResponseWriteBackMiddleware {
    /// 配置
    config: ResponseWriteBackConfig,
    /// 统计信息
    stats: Arc<RwLock<ResponseWriteBackStats>>,
}

/// 响应写回配置
#[derive(Debug, Clone)]
pub struct ResponseWriteBackConfig {
    /// 是否启用响应写回
    pub enabled: bool,
    /// 写入缓冲区大小
    pub write_buffer_size: usize,
    /// 写入超时
    pub write_timeout: std::time::Duration,
    /// 是否自动添加服务器头部
    pub add_server_header: bool,
    /// 服务器头部值
    pub server_header_value: String,
    /// 是否支持keep-alive
    pub enable_keep_alive: bool,
    /// 连接超时（keep-alive模式）
    pub connection_timeout: std::time::Duration,
}

/// 响应写回统计信息
#[derive(Debug, Default, Clone)]
pub struct ResponseWriteBackStats {
    /// 总响应数
    total_responses: u64,
    /// HTTP/1.1响应数
    http1_responses: u64,
    /// HTTPS响应数
    https_responses: u64,
    /// HTTP响应数
    http_responses: u64,
    /// 成功写回数
    successful_writebacks: u64,
    /// 失败写回数
    failed_writebacks: u64,
    /// 写入超时数
    write_timeouts: u64,
    /// 传输字节数
    bytes_written: u64,
    /// Keep-alive连接数
    keep_alive_connections: u64,
}

impl ResponseWriteBackMiddleware {
    /// 创建新的响应写回中间件
    pub fn new() -> Self {
        Self {
            config: ResponseWriteBackConfig::default(),
            stats: Arc::new(RwLock::new(ResponseWriteBackStats::default())),
        }
    }
    
    /// 使用配置创建中间件
    pub fn with_config(config: ResponseWriteBackConfig) -> Self {
        Self {
            config,
            stats: Arc::new(RwLock::new(ResponseWriteBackStats::default())),
        }
    }
    
    /// 检测连接类型
    fn detect_connection_type(&self, context: &ConnectionContext) -> ConnectionType {
        // 检查是否是HTTP/2连接（已被hyper处理）
        if context.custom_data.contains_key("http2_processing") {
            return ConnectionType::Http2;
        }
        
        // 检查TLS流状态
        if context.connection.inbound_tls_stream.is_some() {
            ConnectionType::HttpsHttp1
        } else if context.connection.stream.is_some() {
            ConnectionType::HttpHttp1
        } else {
            ConnectionType::Unknown
        }
    }
    
    /// 写回HTTP/1.1响应到TLS流
    #[instrument(skip(self, tls_stream, response))]
    async fn write_https_response(
        &self,
        tls_stream: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
        response: &HttpResponse,
    ) -> Result<usize, TakeoverError> {
        debug!("写回HTTPS响应: {} {}", response.status_code, 
               response.status_text.as_deref().unwrap_or("OK"));
        
        let response_data = self.format_http_response(response)?;
        let total_bytes = response_data.len();
        
        // 超时写入
        match tokio::time::timeout(
            self.config.write_timeout,
            tls_stream.write_all(&response_data)
        ).await {
            Ok(Ok(())) => {
                tls_stream.flush().await
                    .map_err(|e| TakeoverError::Config(format!("Failed to flush HTTPS response: {}", e)))?;
                
                debug!("HTTPS响应写入完成: {} bytes", total_bytes);
                
                // 更新统计
                {
                    let mut stats = self.stats.write().await;
                    stats.https_responses += 1;
                    stats.successful_writebacks += 1;
                    stats.bytes_written += total_bytes as u64;
                }
                
                Ok(total_bytes)
            }
            Ok(Err(e)) => {
                error!("HTTPS响应写入失败: {}", e);
                {
                    let mut stats = self.stats.write().await;
                    stats.failed_writebacks += 1;
                }
                Err(TakeoverError::Config(format!("Failed to write HTTPS response: {}", e)))
            }
            Err(_) => {
                error!("HTTPS响应写入超时");
                {
                    let mut stats = self.stats.write().await;
                    stats.write_timeouts += 1;
                }
                Err(TakeoverError::Config("HTTPS response write timeout".to_string()))
            }
        }
    }
    
    /// 写回HTTP/1.1响应到TCP流
    #[instrument(skip(self, tcp_stream, response))]
    async fn write_http_response(
        &self,
        tcp_stream: &mut tokio::net::TcpStream,
        response: &HttpResponse,
    ) -> Result<usize, TakeoverError> {
        debug!("写回HTTP响应: {} {}", response.status_code, 
               response.status_text.as_deref().unwrap_or("OK"));
        
        let response_data = self.format_http_response(response)?;
        let total_bytes = response_data.len();
        
        // 超时写入
        match tokio::time::timeout(
            self.config.write_timeout,
            tcp_stream.write_all(&response_data)
        ).await {
            Ok(Ok(())) => {
                tcp_stream.flush().await
                    .map_err(|e| TakeoverError::Config(format!("Failed to flush HTTP response: {}", e)))?;
                
                debug!("HTTP响应写入完成: {} bytes", total_bytes);
                
                // 更新统计
                {
                    let mut stats = self.stats.write().await;
                    stats.http_responses += 1;
                    stats.successful_writebacks += 1;
                    stats.bytes_written += total_bytes as u64;
                }
                
                Ok(total_bytes)
            }
            Ok(Err(e)) => {
                error!("HTTP响应写入失败: {}", e);
                {
                    let mut stats = self.stats.write().await;
                    stats.failed_writebacks += 1;
                }
                Err(TakeoverError::Config(format!("Failed to write HTTP response: {}", e)))
            }
            Err(_) => {
                error!("HTTP响应写入超时");
                {
                    let mut stats = self.stats.write().await;
                    stats.write_timeouts += 1;
                }
                Err(TakeoverError::Config("HTTP response write timeout".to_string()))
            }
        }
    }
    
    /// 格式化HTTP响应
    fn format_http_response(&self, response: &HttpResponse) -> Result<Vec<u8>, TakeoverError> {
        let mut response_data = Vec::with_capacity(
            1024 + response.body.len() // 预估头部大小 + 响应体
        );
        
        // 状态行
        let status_line = format!(
            "HTTP/1.1 {} {}\r\n",
            response.status_code,
            response.status_text.as_deref().unwrap_or(
                self.get_default_status_text(response.status_code as u16)
            )
        );
        response_data.extend_from_slice(status_line.as_bytes());
        
        // 响应头部
        let mut headers = response.headers.clone();
        
        // 自动添加必要的头部
        if !headers.contains_key("content-length") {
            headers.insert("content-length".to_string(), response.body.len().to_string());
        }
        
        if self.config.add_server_header && !headers.contains_key("server") {
            headers.insert("server".to_string(), self.config.server_header_value.clone());
        }
        
        if !headers.contains_key("date") {
            headers.insert("date".to_string(), self.format_http_date());
        }
        
        // Keep-alive处理
        if self.config.enable_keep_alive {
            if !headers.contains_key("connection") {
                headers.insert("connection".to_string(), "keep-alive".to_string());
            }
            if !headers.contains_key("keep-alive") {
                headers.insert("keep-alive".to_string(), format!(
                    "timeout={}", 
                    self.config.connection_timeout.as_secs()
                ));
            }
        }
        
        // 写入头部
        for (name, value) in &headers {
            let header_line = format!("{}: {}\r\n", name, value);
            response_data.extend_from_slice(header_line.as_bytes());
        }
        
        // 头部结束标记
        response_data.extend_from_slice(b"\r\n");
        
        // 响应体
        response_data.extend_from_slice(&response.body);
        
        debug!("格式化HTTP响应完成: {} bytes (头部: {} bytes, 体: {} bytes)", 
               response_data.len(), 
               response_data.len() - response.body.len(),
               response.body.len());
        
        Ok(response_data)
    }
    
    /// 获取默认状态文本
    fn get_default_status_text(&self, status_code: u16) -> &'static str {
        match status_code {
            200 => "OK",
            201 => "Created",
            202 => "Accepted",
            204 => "No Content",
            301 => "Moved Permanently",
            302 => "Found",
            304 => "Not Modified",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            500 => "Internal Server Error",
            501 => "Not Implemented",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            _ => "Unknown",
        }
    }
    
    /// 格式化HTTP日期
    fn format_http_date(&self) -> String {
        // 简化实现，实际应使用RFC 2822格式
        chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string()
    }
    
    /// 更新总响应统计
    async fn update_total_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.total_responses += 1;
    }
    
    /// 更新HTTP/1.1响应统计
    async fn update_http1_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.http1_responses += 1;
    }
    
    /// 更新Keep-alive统计
    async fn update_keep_alive_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.keep_alive_connections += 1;
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> ResponseWriteBackStats {
        self.stats.read().await.clone()
    }
}

/// 连接类型
#[derive(Debug, Clone, PartialEq)]
enum ConnectionType {
    /// HTTP/2连接（已被hyper处理）
    Http2,
    /// HTTPS HTTP/1.1连接
    HttpsHttp1,
    /// HTTP HTTP/1.1连接
    HttpHttp1,
    /// 未知连接类型
    Unknown,
}

impl Default for ResponseWriteBackMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for ResponseWriteBackConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            write_buffer_size: 8192,
            write_timeout: std::time::Duration::from_secs(30),
            add_server_header: true,
            server_header_value: "takeover-proxy/1.0".to_string(),
            enable_keep_alive: false, // 默认禁用keep-alive
            connection_timeout: std::time::Duration::from_secs(60),
        }
    }
}

#[async_trait]
impl Middleware for ResponseWriteBackMiddleware {
    fn name(&self) -> &str {
        "response_writeback"
    }
    
    #[instrument(skip(self, context, _core))]
    async fn process(
        &self,
        context: &mut ConnectionContext,
        _core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, TakeoverError> {
        if !self.config.enabled {
            debug!("响应写回中间件已禁用，跳过");
            return Ok(MiddlewareResult::Continue);
        }
        
        debug!("响应写回中间件开始处理连接: {}", context.id);
        
        // 更新处理状态
        context.set_processing_state(ProcessingState::ResponseWriting);
        
        // 更新总响应统计
        self.update_total_stats().await;
        
        // 检测连接类型
        let connection_type = self.detect_connection_type(context);
        debug!("检测到连接类型: {:?}", connection_type);
        
        match connection_type {
            ConnectionType::Http2 => {
                debug!("HTTP/2连接已被hyper处理，跳过响应写回");
                Ok(MiddlewareResult::Continue)
            }
            ConnectionType::HttpsHttp1 | ConnectionType::HttpHttp1 => {
                // 检查是否有响应需要写回
                let response = match context.response.as_ref() {
                    Some(response) => response,
                    None => {
                        warn!("没有响应对象可写回，跳过");
                        return Ok(MiddlewareResult::Continue);
                    }
                };
                
                debug!("开始写回HTTP/1.1响应: {} {}", 
                       response.status_code, 
                       response.status_text.as_deref().unwrap_or("OK"));
                
                // 更新HTTP/1.1统计
                self.update_http1_stats().await;
                
                // 根据连接类型选择写回方式
                let bytes_written = match connection_type {
                    ConnectionType::HttpsHttp1 => {
                        // HTTPS响应写回
                        if let Some(ref mut tls_stream) = context.connection.inbound_tls_stream {
                            self.write_https_response(tls_stream, response).await?
                        } else {
                            error!("HTTPS连接但没有可用的TLS流");
                            return Err(TakeoverError::Config("Missing TLS stream for HTTPS response".to_string()));
                        }
                    }
                    ConnectionType::HttpHttp1 => {
                        // HTTP响应写回
                        if let Some(ref mut tcp_stream) = context.connection.stream {
                            self.write_http_response(tcp_stream, response).await?
                        } else {
                            error!("HTTP连接但没有可用的TCP流");
                            return Err(TakeoverError::Config("Missing TCP stream for HTTP response".to_string()));
                        }
                    }
                    _ => unreachable!()
                };
                
                info!("✅ HTTP/1.1响应写回完成: {} bytes (连接: {})", bytes_written, context.id);
                
                // Keep-alive处理
                if self.config.enable_keep_alive {
                    self.update_keep_alive_stats().await;
                    debug!("Keep-alive连接保持开启");
                }
                
                Ok(MiddlewareResult::Continue)
            }
            ConnectionType::Unknown => {
                warn!("未知连接类型，无法写回响应");
                Ok(MiddlewareResult::Continue)
            }
        }
    }
    
    fn priority(&self) -> i32 {
        75 // 最低优先级，最后执行
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
        let middleware = ResponseWriteBackMiddleware::new();
        assert_eq!(middleware.name(), "response_writeback");
        assert!(middleware.enabled());
        assert_eq!(middleware.priority(), 75);
        
        let stats = middleware.get_stats().await;
        assert_eq!(stats.total_responses, 0);
        assert_eq!(stats.successful_writebacks, 0);
        
        println!("✅ 响应写回中间件创建成功");
    }
    
    #[test]
    fn test_connection_type_detection() {
        let middleware = ResponseWriteBackMiddleware::new();
        
        // 测试HTTP/2连接
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443)
        );
        let mut context = ConnectionContext::new(connection, "example.com".to_string(), 443);
        context.custom_data.insert("http2_processing".to_string(), serde_json::json!(true));
        
        assert_eq!(middleware.detect_connection_type(&context), ConnectionType::Http2);
        
        println!("✅ 连接类型检测测试通过");
    }
    
    #[test]
    fn test_http_response_formatting() {
        let middleware = ResponseWriteBackMiddleware::new();
        
        let mut response = HttpResponse::new(200);
        response.headers.insert("content-type".to_string(), "text/plain".to_string());
        response.body = b"Hello, World!".to_vec();
        
        let formatted = middleware.format_http_response(&response).unwrap();
        let response_str = String::from_utf8_lossy(&formatted);
        
        assert!(response_str.contains("HTTP/1.1 200 OK"));
        assert!(response_str.contains("content-type: text/plain"));
        assert!(response_str.contains("content-length: 13"));
        assert!(response_str.contains("Hello, World!"));
        
        println!("✅ HTTP响应格式化测试通过");
    }
    
    #[test]
    fn test_status_text_mapping() {
        let middleware = ResponseWriteBackMiddleware::new();
        
        assert_eq!(middleware.get_default_status_text(200), "OK");
        assert_eq!(middleware.get_default_status_text(404), "Not Found");
        assert_eq!(middleware.get_default_status_text(500), "Internal Server Error");
        assert_eq!(middleware.get_default_status_text(999), "Unknown");
        
        println!("✅ 状态文本映射测试通过");
    }
    
    #[tokio::test]
    async fn test_stats_tracking() {
        let middleware = ResponseWriteBackMiddleware::new();
        
        let initial_stats = middleware.get_stats().await;
        assert_eq!(initial_stats.total_responses, 0);
        
        middleware.update_total_stats().await;
        middleware.update_http1_stats().await;
        middleware.update_keep_alive_stats().await;
        
        let updated_stats = middleware.get_stats().await;
        assert_eq!(updated_stats.total_responses, 1);
        assert_eq!(updated_stats.http1_responses, 1);
        assert_eq!(updated_stats.keep_alive_connections, 1);
        
        println!("✅ 统计跟踪测试通过");
    }
}
