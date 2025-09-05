//! 协议检测中间件
//! 
//! 早期分流架构的核心组件，负责：
//! - 非侵入式协议检测（使用TCP peek）
//! - 分流非HTTP协议到Copy Pipe透传
//! - 让HTTP协议继续后续处理管道

use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn, error, debug, instrument};
use async_trait::async_trait;

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{ConnectionContext, ProcessingState};
use crate::error::TakeoverError;

/// 协议检测中间件
/// 
/// 透明代理的第一道防线，负责早期协议识别和流量分流
pub struct ProtocolDetectionMiddleware {
    /// 配置
    config: ProtocolDetectionConfig,
    /// 统计信息
    stats: Arc<RwLock<ProtocolDetectionStats>>,
}

/// 协议检测配置
#[derive(Debug, Clone)]
pub struct ProtocolDetectionConfig {
    /// 是否启用协议检测
    pub enabled: bool,
    /// Peek缓冲区大小
    pub peek_buffer_size: usize,
    /// 检测超时
    pub detection_timeout: std::time::Duration,
    /// 是否启用Copy Pipe透传
    pub enable_copy_pipe: bool,
    /// Copy Pipe缓冲区大小
    pub copy_pipe_buffer_size: usize,
}

/// 协议检测统计信息
#[derive(Debug, Default, Clone)]
pub struct ProtocolDetectionStats {
    /// 总检测连接数
    total_connections: u64,
    /// TLS连接数
    tls_connections: u64,
    /// HTTP明文连接数
    http_cleartext_connections: u64,
    /// SSH连接数
    ssh_connections: u64,
    /// 其他已知协议连接数
    other_known_protocols: u64,
    /// 未知协议连接数
    unknown_protocol_connections: u64,
    /// Copy Pipe透传连接数
    copy_pipe_connections: u64,
    /// 检测失败数
    detection_failures: u64,
}

/// 检测到的协议类型
#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolType {
    /// TLS握手
    Tls,
    /// HTTP明文
    HttpCleartext,
    /// SSH协议
    Ssh,
    /// MySQL协议
    Mysql,
    /// PostgreSQL协议
    Postgresql,
    /// Redis协议
    Redis,
    /// 未知协议（需要透传）
    Unknown,
}

impl ProtocolDetectionMiddleware {
    /// 创建新的协议检测中间件
    pub fn new() -> Self {
        Self {
            config: ProtocolDetectionConfig::default(),
            stats: Arc::new(RwLock::new(ProtocolDetectionStats::default())),
        }
    }
    
    /// 使用配置创建中间件
    pub fn with_config(config: ProtocolDetectionConfig) -> Self {
        Self {
            config,
            stats: Arc::new(RwLock::new(ProtocolDetectionStats::default())),
        }
    }
    
    /// 检测TCP流的协议类型
    #[instrument(skip(self, stream))]
    async fn detect_protocol(&self, stream: &TcpStream) -> Result<ProtocolType, TakeoverError> {
        let mut buffer = vec![0u8; self.config.peek_buffer_size];
        
        // 使用peek非侵入式读取数据
        let bytes_read = match tokio::time::timeout(
            self.config.detection_timeout,
            stream.peek(&mut buffer)
        ).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                warn!("协议检测Peek失败: {}", e);
                return Err(TakeoverError::Config(format!("Protocol peek failed: {}", e)));
            }
            Err(_) => {
                warn!("协议检测超时");
                return Err(TakeoverError::Config("Protocol detection timeout".to_string()));
            }
        };
        
        if bytes_read < 3 {
            debug!("数据不足，推断为未知协议: {} bytes", bytes_read);
            return Ok(ProtocolType::Unknown);
        }
        
        let data = &buffer[..bytes_read];
        debug!("协议检测: 读取{}字节: {:02x?}", bytes_read, &data[..bytes_read.min(8)]);
        
        // 1. 检测TLS握手
        if self.is_tls_handshake(data) {
            debug!("🔐 检测到TLS握手");
            return Ok(ProtocolType::Tls);
        }
        
        // 2. 检测HTTP明文
        if self.is_http_cleartext(data) {
            debug!("🌐 检测到HTTP明文协议");
            return Ok(ProtocolType::HttpCleartext);
        }
        
        // 3. 检测SSH
        if self.is_ssh_protocol(data) {
            debug!("🔑 检测到SSH协议");
            return Ok(ProtocolType::Ssh);
        }
        
        // 4. 检测数据库协议
        if let Some(db_protocol) = self.detect_database_protocol(data) {
            debug!("🗃️ 检测到数据库协议: {:?}", db_protocol);
            return Ok(db_protocol);
        }
        
        debug!("❓ 未识别协议，标记为未知");
        Ok(ProtocolType::Unknown)
    }
    
    /// 检测TLS握手包
    /// 
    /// TLS握手特征:
    /// - 第1字节: 0x16 (TLS Handshake)
    /// - 第2-3字节: 版本号 (0x03, 0x01-0x04)
    /// - 第6字节: 0x01 (Client Hello)
    fn is_tls_handshake(&self, data: &[u8]) -> bool {
        if data.len() < 6 {
            return false;
        }
        
        data[0] == 0x16 &&                    // TLS Handshake
        data[1] == 0x03 &&                    // TLS Version Major
        (data[2] >= 0x01 && data[2] <= 0x04) && // TLS Version Minor (1.0-1.3)
        data[5] == 0x01                       // Client Hello
    }
    
    /// 检测HTTP明文协议
    /// 
    /// HTTP特征: 以HTTP方法开头
    fn is_http_cleartext(&self, data: &[u8]) -> bool {
        if data.len() < 4 {
            return false;
        }
        
        let text = String::from_utf8_lossy(data);
        let http_methods = [
            "GET ", "POST ", "PUT ", "DELETE ", "HEAD ", 
            "OPTIONS ", "PATCH ", "CONNECT ", "TRACE "
        ];
        
        http_methods.iter().any(|method| text.starts_with(method))
    }
    
    /// 检测SSH协议
    /// 
    /// SSH特征: 以"SSH-"开头
    fn is_ssh_protocol(&self, data: &[u8]) -> bool {
        if data.len() < 4 {
            return false;
        }
        
        let text = String::from_utf8_lossy(data);
        text.starts_with("SSH-")
    }
    
    /// 检测数据库协议
    fn detect_database_protocol(&self, data: &[u8]) -> Option<ProtocolType> {
        if data.len() < 8 {
            return None;
        }
        
        // MySQL协议检测
        // MySQL握手包特征: 第5字节是0x0a (协议版本10)
        if data.len() >= 5 && data[4] == 0x0a {
            return Some(ProtocolType::Mysql);
        }
        
        // PostgreSQL协议检测
        // PostgreSQL启动包特征: 长度字段 + 协议版本号
        if data.len() >= 8 {
            let length = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
            let version = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
            
            // PostgreSQL 3.0协议版本号: 0x00030000
            if length > 8 && length < 10000 && version == 0x00030000 {
                return Some(ProtocolType::Postgresql);
            }
        }
        
        // Redis协议检测
        // Redis RESP协议特征: 以'*'或'$'开头
        if data[0] == b'*' || data[0] == b'$' {
            return Some(ProtocolType::Redis);
        }
        
        None
    }
    
    /// 启动Copy Pipe透传
    #[instrument(skip(self, client_stream, context, core))]
    async fn start_copy_pipe(
        &self,
        client_stream: TcpStream,
        context: &ConnectionContext,
        core: &crate::core::AppCore,
    ) -> Result<(), TakeoverError> {
        info!("🔗 启动Copy Pipe透传: {}:{}", context.target_host, context.target_port);
        
        // 连接到目标服务器
        let server_stream = self.connect_to_target(context, core).await?;
        
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.copy_pipe_connections += 1;
        }
        
        // 启动双向数据转发
        self.bidirectional_copy(client_stream, server_stream, context).await?;
        
        Ok(())
    }
    
    /// 连接到目标服务器
    async fn connect_to_target(
        &self,
        context: &ConnectionContext,
        core: &crate::core::AppCore,
    ) -> Result<TcpStream, TakeoverError> {
        debug!("连接到目标服务器: {}:{}", context.target_host, context.target_port);
        
        // 使用出站适配器建立连接
        let outbound_port = core.outbound_port();
        let stream = outbound_port.connect(context).await
            .map_err(|e| TakeoverError::Config(format!("Failed to connect to target: {}", e)))?;
        
        // 转换为TcpStream（需要适配器支持）
        // 注意：这里简化处理，实际可能需要更复杂的适配
        match stream.try_into_tcp_stream() {
            Ok(tcp_stream) => Ok(tcp_stream),
            Err(_) => {
                error!("出站适配器返回的流无法转换为TcpStream");
                Err(TakeoverError::Config("Cannot convert outbound stream to TcpStream".to_string()))
            }
        }
    }
    
    /// 双向数据拷贝
    #[instrument(skip(self, client_stream, server_stream))]
    async fn bidirectional_copy(
        &self,
        mut client_stream: TcpStream,
        mut server_stream: TcpStream,
        context: &ConnectionContext,
    ) -> Result<(), TakeoverError> {
        let connection_id = context.id;
        info!("开始双向数据转发: {}", connection_id);
        
        let (mut client_read, mut client_write) = client_stream.split();
        let (mut server_read, mut server_write) = server_stream.split();
        
        // 客户端→服务器方向
        let c2s_task = async {
            let mut buffer = vec![0u8; self.config.copy_pipe_buffer_size];
            let mut total_bytes = 0u64;
            
            loop {
                match client_read.read(&mut buffer).await {
                    Ok(0) => {
                        debug!("客户端连接关闭 (C→S): {} bytes", total_bytes);
                        break;
                    }
                    Ok(n) => {
                        if let Err(e) = server_write.write_all(&buffer[..n]).await {
                            warn!("写入服务器失败: {}", e);
                            break;
                        }
                        total_bytes += n as u64;
                    }
                    Err(e) => {
                        warn!("读取客户端数据失败: {}", e);
                        break;
                    }
                }
            }
            total_bytes
        };
        
        // 服务器→客户端方向
        let s2c_task = async {
            let mut buffer = vec![0u8; self.config.copy_pipe_buffer_size];
            let mut total_bytes = 0u64;
            
            loop {
                match server_read.read(&mut buffer).await {
                    Ok(0) => {
                        debug!("服务器连接关闭 (S→C): {} bytes", total_bytes);
                        break;
                    }
                    Ok(n) => {
                        if let Err(e) = client_write.write_all(&buffer[..n]).await {
                            warn!("写入客户端失败: {}", e);
                            break;
                        }
                        total_bytes += n as u64;
                    }
                    Err(e) => {
                        warn!("读取服务器数据失败: {}", e);
                        break;
                    }
                }
            }
            total_bytes
        };
        
        // 等待任一方向完成
        let (c2s_bytes, s2c_bytes) = tokio::join!(c2s_task, s2c_task);
        
        info!("Copy Pipe完成: {}→{} bytes, {}←{} bytes (连接: {})", 
              c2s_bytes, s2c_bytes, c2s_bytes, s2c_bytes, connection_id);
        
        Ok(())
    }
    
    /// 更新协议统计
    async fn update_protocol_stats(&self, protocol: &ProtocolType) {
        let mut stats = self.stats.write().await;
        stats.total_connections += 1;
        
        match protocol {
            ProtocolType::Tls => stats.tls_connections += 1,
            ProtocolType::HttpCleartext => stats.http_cleartext_connections += 1,
            ProtocolType::Ssh => stats.ssh_connections += 1,
            ProtocolType::Mysql | ProtocolType::Postgresql | ProtocolType::Redis => {
                stats.other_known_protocols += 1;
            }
            ProtocolType::Unknown => stats.unknown_protocol_connections += 1,
        }
    }
    
    /// 更新检测失败统计
    async fn update_failure_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.detection_failures += 1;
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> ProtocolDetectionStats {
        self.stats.read().await.clone()
    }
}

impl Default for ProtocolDetectionMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for ProtocolDetectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            peek_buffer_size: 1024,
            detection_timeout: std::time::Duration::from_secs(5),
            enable_copy_pipe: true,
            copy_pipe_buffer_size: 8192,
        }
    }
}

#[async_trait]
impl Middleware for ProtocolDetectionMiddleware {
    fn name(&self) -> &str {
        "protocol_detection"
    }
    
    #[instrument(skip(self, context, core))]
    async fn process(
        &self,
        context: &mut ConnectionContext,
        core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, TakeoverError> {
        if !self.config.enabled {
            debug!("协议检测中间件已禁用，跳过");
            return Ok(MiddlewareResult::Continue);
        }
        
        debug!("协议检测中间件开始处理连接: {}", context.id);
        
        // 更新处理状态
        context.set_processing_state(ProcessingState::ProtocolDetection);
        
        // 检查是否有TCP流可用
        let tcp_stream = match context.connection.stream.as_ref() {
            Some(stream) => stream,
            None => {
                warn!("没有可用的TCP流，跳过协议检测");
                return Ok(MiddlewareResult::Continue);
            }
        };
        
        // 检测协议类型
        let protocol = match self.detect_protocol(tcp_stream).await {
            Ok(protocol) => {
                debug!("协议检测成功: {:?}", protocol);
                protocol
            }
            Err(e) => {
                warn!("协议检测失败: {}", e);
                self.update_failure_stats().await;
                // 检测失败时假设为未知协议，继续透传
                ProtocolType::Unknown
            }
        };
        
        // 更新统计
        self.update_protocol_stats(&protocol).await;
        
        // 根据协议类型决定处理方式
        match protocol {
            ProtocolType::Tls | ProtocolType::HttpCleartext => {
                debug!("HTTP相关协议，继续后续中间件处理");
                
                // 存储协议信息到上下文
                context.custom_data.insert(
                    "detected_protocol".to_string(),
                    serde_json::json!(format!("{:?}", protocol).to_lowercase())
                );
                
                Ok(MiddlewareResult::Continue)
            }
            ProtocolType::Ssh | ProtocolType::Mysql | ProtocolType::Postgresql | 
            ProtocolType::Redis | ProtocolType::Unknown => {
                info!("检测到需要透传的协议: {:?}，启动Copy Pipe", protocol);
                
                if !self.config.enable_copy_pipe {
                    warn!("Copy Pipe已禁用，拒绝非HTTP连接");
                    return Err(TakeoverError::Config("Copy pipe disabled for non-HTTP protocols".to_string()));
                }
                
                // 存储协议信息
                context.custom_data.insert(
                    "detected_protocol".to_string(),
                    serde_json::json!(format!("{:?}", protocol).to_lowercase())
                );
                context.custom_data.insert(
                    "copy_pipe_mode".to_string(),
                    serde_json::json!(true)
                );
                
                // 取出TCP流，启动Copy Pipe
                let client_stream = context.connection.stream.take()
                    .ok_or_else(|| TakeoverError::Config("TCP stream not available".to_string()))?;
                
                self.start_copy_pipe(client_stream, context, core).await?;
                
                // Copy Pipe接管连接，停止后续中间件处理
                Ok(MiddlewareResult::Stop)
            }
        }
    }
    
    fn priority(&self) -> i32 {
        5 // 最高优先级，第一个执行
    }
    
    fn enabled(&self) -> bool {
        self.config.enabled
    }
}

// 临时的trait扩展，直到出站适配器支持TcpStream转换
trait NetworkStreamExt {
    fn try_into_tcp_stream(self) -> Result<TcpStream, Box<dyn std::error::Error>>;
}

impl NetworkStreamExt for Box<dyn crate::core::ports::NetworkStream> {
    fn try_into_tcp_stream(self) -> Result<TcpStream, Box<dyn std::error::Error>> {
        // 这是一个简化实现，实际需要适配器支持
        Err("TcpStream conversion not yet implemented".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_middleware_creation() {
        let middleware = ProtocolDetectionMiddleware::new();
        assert_eq!(middleware.name(), "protocol_detection");
        assert!(middleware.enabled());
        assert_eq!(middleware.priority(), 5);
        
        let stats = middleware.get_stats().await;
        assert_eq!(stats.total_connections, 0);
        
        println!("✅ 协议检测中间件创建成功");
    }
    
    #[test]
    fn test_tls_handshake_detection() {
        let middleware = ProtocolDetectionMiddleware::new();
        
        // TLS 1.2 Client Hello
        let tls12_data = [0x16, 0x03, 0x03, 0x00, 0x9b, 0x01];
        assert!(middleware.is_tls_handshake(&tls12_data));
        
        // TLS 1.3 Client Hello
        let tls13_data = [0x16, 0x03, 0x01, 0x00, 0x9b, 0x01];
        assert!(middleware.is_tls_handshake(&tls13_data));
        
        // 非TLS数据
        let non_tls_data = [0x47, 0x45, 0x54, 0x20, 0x2f, 0x20]; // "GET / "
        assert!(!middleware.is_tls_handshake(&non_tls_data));
        
        println!("✅ TLS握手检测测试通过");
    }
    
    #[test]
    fn test_http_cleartext_detection() {
        let middleware = ProtocolDetectionMiddleware::new();
        
        let http_data = b"GET / HTTP/1.1\r\nHost: example.com\r\n";
        assert!(middleware.is_http_cleartext(http_data));
        
        let post_data = b"POST /api HTTP/1.1\r\n";
        assert!(middleware.is_http_cleartext(post_data));
        
        let non_http_data = b"SSH-2.0-OpenSSH_7.4";
        assert!(!middleware.is_http_cleartext(non_http_data));
        
        println!("✅ HTTP明文检测测试通过");
    }
    
    #[test]
    fn test_ssh_protocol_detection() {
        let middleware = ProtocolDetectionMiddleware::new();
        
        let ssh_data = b"SSH-2.0-OpenSSH_7.4";
        assert!(middleware.is_ssh_protocol(ssh_data));
        
        let ssh_data2 = b"SSH-1.99-OpenSSH_3.4";
        assert!(middleware.is_ssh_protocol(ssh_data2));
        
        let non_ssh_data = b"GET / HTTP/1.1";
        assert!(!middleware.is_ssh_protocol(non_ssh_data));
        
        println!("✅ SSH协议检测测试通过");
    }
    
    #[test]
    fn test_database_protocol_detection() {
        let middleware = ProtocolDetectionMiddleware::new();
        
        // MySQL握手包（简化版）
        let mysql_data = [0x0a, 0x35, 0x2e, 0x37, 0x0a, 0x00, 0x00, 0x00];
        assert_eq!(middleware.detect_database_protocol(&mysql_data), Some(ProtocolType::Mysql));
        
        // PostgreSQL启动包
        let pg_data = [0x00, 0x00, 0x00, 0x08, 0x00, 0x03, 0x00, 0x00];
        assert_eq!(middleware.detect_database_protocol(&pg_data), Some(ProtocolType::Postgresql));
        
        // Redis RESP协议
        let redis_data = b"*2\r\n$4\r\nPING\r\n";
        assert_eq!(middleware.detect_database_protocol(redis_data), Some(ProtocolType::Redis));
        
        println!("✅ 数据库协议检测测试通过");
    }
    
    #[tokio::test]
    async fn test_stats_tracking() {
        let middleware = ProtocolDetectionMiddleware::new();
        
        let initial_stats = middleware.get_stats().await;
        assert_eq!(initial_stats.total_connections, 0);
        
        middleware.update_protocol_stats(&ProtocolType::Tls).await;
        middleware.update_protocol_stats(&ProtocolType::HttpCleartext).await;
        middleware.update_protocol_stats(&ProtocolType::Ssh).await;
        middleware.update_failure_stats().await;
        
        let updated_stats = middleware.get_stats().await;
        assert_eq!(updated_stats.total_connections, 3);
        assert_eq!(updated_stats.tls_connections, 1);
        assert_eq!(updated_stats.http_cleartext_connections, 1);
        assert_eq!(updated_stats.ssh_connections, 1);
        assert_eq!(updated_stats.detection_failures, 1);
        
        println!("✅ 统计跟踪测试通过");
    }
}
