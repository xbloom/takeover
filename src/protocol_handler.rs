//! 协议处理模块
//! 
//! 负责检测和解析各种网络协议，包括：
//! - HTTP/HTTPS协议检测
//! - TLS SNI解析
//! - URL提取和解析
//! - 为未来的TLS拦截预留接口

use anyhow::Result;
use tokio::net::TcpStream;
use std::time::Duration;
use tracing::debug;
use async_trait::async_trait;

/// 协议类型枚举
#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolType {
    Http,
    Https,
    Unknown,
}

/// TLS拦截模式（为未来功能预留）
#[derive(Debug, Clone, PartialEq)]
pub enum TlsInterceptionMode {
    /// 不拦截TLS，仅使用SNI
    None,
    /// 选择性拦截（基于域名配置）
    Selective,
    /// 完全拦截（SSL Bump模式）
    Full,
}

/// 协议解析结果
#[derive(Debug, Clone)]
pub struct ProtocolInfo {
    pub protocol_type: ProtocolType,
    pub target_host: String,
    pub target_port: u16,
    pub full_url: Option<String>,
    pub tls_interception_mode: TlsInterceptionMode,
}

/// 协议处理器接口
#[async_trait]
pub trait ProtocolHandler {
    /// 检测协议类型
    async fn detect_protocol(&self, data: &[u8]) -> ProtocolType;
    
    
    /// 解析目标地址
    async fn parse_target(&self, data: &[u8]) -> Result<(String, u16)>;
    
    /// 提取完整URL
    async fn extract_full_url(&self, data: &[u8]) -> Result<Option<String>>;
    
    /// 综合解析协议信息
    async fn parse_protocol_info(&self, data: &[u8]) -> Result<ProtocolInfo>;
}

/// 标准协议处理器实现
pub struct StandardProtocolHandler;

impl StandardProtocolHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ProtocolHandler for StandardProtocolHandler {
    async fn detect_protocol(&self, data: &[u8]) -> ProtocolType {
        if Self::is_tls_handshake(data) {
            ProtocolType::Https
        } else if Self::looks_like_http(data) {
            ProtocolType::Http
        } else {
            ProtocolType::Unknown
        }
    }

    async fn parse_target(&self, data: &[u8]) -> Result<(String, u16)> {
        if Self::is_tls_handshake(data) {
            Self::parse_tls_sni(data)
        } else {
            Self::parse_http_target(data)
        }
    }

    async fn extract_full_url(&self, data: &[u8]) -> Result<Option<String>> {
        if Self::is_tls_handshake(data) {
            // 从TLS SNI提取hostname并构造HTTPS URL
            let (hostname, _port) = Self::parse_tls_sni(data)?;
            Ok(Some(format!("https://{}/", hostname)))
        } else {
            // HTTP请求
            Self::extract_url_from_http_request(data).map(Some)
        }
    }

    async fn parse_protocol_info(&self, data: &[u8]) -> Result<ProtocolInfo> {
        let protocol_type = self.detect_protocol(data).await;
        let (target_host, target_port) = self.parse_target(data).await?;
        let full_url = self.extract_full_url(data).await?;
        
        // 目前默认不拦截TLS，未来可以根据配置决定
        let tls_interception_mode = TlsInterceptionMode::None;

        Ok(ProtocolInfo {
            protocol_type,
            target_host,
            target_port,
            full_url,
            tls_interception_mode,
        })
    }
}

impl StandardProtocolHandler {
    /// 检查是否是TLS握手数据包
    pub fn is_tls_handshake(data: &[u8]) -> bool {
        // TLS握手包的特征：
        // - 第一个字节是0x16 (TLS Handshake)
        // - 第二第三字节是版本号 (0x03, 0x01-0x04)
        // - 第五个字节是0x01 (Client Hello)
        if data.len() < 6 {
            return false;
        }
        
        data[0] == 0x16 && // TLS Handshake
        data[1] == 0x03 && // TLS Version Major
        (data[2] >= 0x01 && data[2] <= 0x04) && // TLS Version Minor (1.0-1.3)
        data[5] == 0x01 // Client Hello
    }

    /// 检查是否看起来像HTTP请求
    pub fn looks_like_http(data: &[u8]) -> bool {
        if data.len() < 16 {
            return false;
        }
        
        let data_str = String::from_utf8_lossy(data);
        let first_line = data_str.lines().next().unwrap_or("");
        
        // 检查HTTP方法
        first_line.starts_with("GET ") ||
        first_line.starts_with("POST ") ||
        first_line.starts_with("PUT ") ||
        first_line.starts_with("DELETE ") ||
        first_line.starts_with("HEAD ") ||
        first_line.starts_with("OPTIONS ") ||
        first_line.starts_with("PATCH ") ||
        first_line.starts_with("CONNECT ")
    }

    /// 从TLS Client Hello中提取SNI（Server Name Indication）
    pub fn parse_tls_sni(data: &[u8]) -> Result<(String, u16)> {
        if data.len() < 43 {
            return Err(anyhow::anyhow!("TLS packet too short"));
        }

        // 跳过TLS记录头（5字节）和握手消息头（4字节）
        let mut offset = 9;
        
        // 跳过版本号（2字节）
        offset += 2;
        
        // 跳过随机数（32字节）
        offset += 32;
        
        if offset >= data.len() {
            return Err(anyhow::anyhow!("Invalid TLS packet structure"));
        }
        
        // 跳过Session ID
        let session_id_len = data[offset] as usize;
        offset += 1 + session_id_len;
        
        if offset + 2 >= data.len() {
            return Err(anyhow::anyhow!("Invalid TLS packet - session ID"));
        }
        
        // 跳过Cipher Suites
        let cipher_suites_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2 + cipher_suites_len;
        
        if offset >= data.len() {
            return Err(anyhow::anyhow!("Invalid TLS packet - cipher suites"));
        }
        
        // 跳过Compression Methods
        let compression_len = data[offset] as usize;
        offset += 1 + compression_len;
        
        if offset + 2 >= data.len() {
            return Err(anyhow::anyhow!("Invalid TLS packet - compression"));
        }
        
        // 读取Extensions长度
        let extensions_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        
        let extensions_end = offset + extensions_len;
        if extensions_end > data.len() {
            return Err(anyhow::anyhow!("Invalid TLS packet - extensions length"));
        }
        
        // 解析Extensions寻找SNI
        while offset < extensions_end {
            if offset + 4 > data.len() {
                break;
            }
            
            let ext_type = u16::from_be_bytes([data[offset], data[offset + 1]]);
            let ext_len = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
            offset += 4;
            
            if ext_type == 0x0000 {
                // SNI Extension
                return Self::parse_sni_extension(&data[offset..offset + ext_len]);
            }
            
            offset += ext_len;
        }
        
        Err(anyhow::anyhow!("No SNI found in TLS handshake"))
    }

    /// 解析SNI扩展
    pub fn parse_sni_extension(data: &[u8]) -> Result<(String, u16)> {
        if data.len() < 5 {
            return Err(anyhow::anyhow!("SNI extension too short"));
        }
        
        // 跳过SNI列表长度（2字节）
        let mut offset = 2;
        
        // 读取第一个SNI条目
        if offset >= data.len() {
            return Err(anyhow::anyhow!("Invalid SNI structure"));
        }
        
        let name_type = data[offset];
        offset += 1;
        
        if name_type != 0x00 {
            // 只处理hostname类型
            return Err(anyhow::anyhow!("Unsupported SNI name type"));
        }
        
        if offset + 2 > data.len() {
            return Err(anyhow::anyhow!("Invalid SNI name length"));
        }
        
        let name_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        
        if offset + name_len > data.len() {
            return Err(anyhow::anyhow!("Invalid SNI name"));
        }
        
        let hostname = String::from_utf8_lossy(&data[offset..offset + name_len]).to_string();
        
        // 对于HTTPS，默认使用443端口
        Ok((hostname, 443))
    }

    /// 解析HTTP请求获取目标主机和端口
    pub fn parse_http_target(request_data: &[u8]) -> Result<(String, u16)> {
        let request_str = String::from_utf8_lossy(request_data);
        let lines: Vec<&str> = request_str.lines().collect();
        
        if lines.is_empty() {
            return Err(anyhow::anyhow!("Empty HTTP request"));
        }

        // 解析第一行获取方法和路径
        let first_line = lines[0];
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        
        if parts.len() < 2 {
            return Err(anyhow::anyhow!("Invalid HTTP request line"));
        }

        let method = parts[0];
        let url_or_path = parts[1];

        // CONNECT方法直接包含host:port
        if method == "CONNECT" {
            let host_port: Vec<&str> = url_or_path.split(':').collect();
            if host_port.len() == 2 {
                let host = host_port[0].to_string();
                let port = host_port[1].parse::<u16>()
                    .map_err(|_| anyhow::anyhow!("Invalid port in CONNECT: {}", host_port[1]))?;
                return Ok((host, port));
            }
        }

        // 对于其他方法，从Host头获取主机
        for line in &lines[1..] {
            if line.to_lowercase().starts_with("host:") {
                let host_part = line[5..].trim();
                
                // 检查是否包含端口
                if let Some(colon_pos) = host_part.rfind(':') {
                    let host = host_part[..colon_pos].to_string();
                    let port_str = &host_part[colon_pos + 1..];
                    
                    if let Ok(port) = port_str.parse::<u16>() {
                        return Ok((host, port));
                    }
                }
                
                // 没有端口，根据协议推断默认端口
                let default_port = if url_or_path.starts_with("https://") {
                    443
                } else {
                    80
                };
                
                return Ok((host_part.to_string(), default_port));
            }
        }

        // 如果找不到Host头，尝试从URL解析
        if url_or_path.starts_with("http://") || url_or_path.starts_with("https://") {
            if let Ok(url) = url::Url::parse(url_or_path) {
                let host = url.host_str().unwrap_or("").to_string();
                let port = url.port().unwrap_or(if url.scheme() == "https" { 443 } else { 80 });
                return Ok((host, port));
            }
        }

        Err(anyhow::anyhow!("Could not parse target from HTTP request"))
    }

    /// 从HTTP请求中提取完整URL
    pub fn extract_url_from_http_request(request_data: &[u8]) -> Result<String> {
        let request_str = String::from_utf8_lossy(request_data);
        let lines: Vec<&str> = request_str.lines().collect();
        
        if lines.is_empty() {
            return Err(anyhow::anyhow!("Empty HTTP request"));
        }

        // 解析第一行获取方法和路径
        let first_line = lines[0];
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        
        if parts.len() < 2 {
            return Err(anyhow::anyhow!("Invalid HTTP request line"));
        }

        let method = parts[0];
        let url_or_path = parts[1];

        // 如果已经是完整URL，直接返回
        if url_or_path.starts_with("http://") || url_or_path.starts_with("https://") {
            return Ok(url_or_path.to_string());
        }

        // CONNECT方法处理
        if method == "CONNECT" {
            return Ok(format!("https://{}", url_or_path));
        }

        // 从Host头构造完整URL
        for line in &lines[1..] {
            if line.to_lowercase().starts_with("host:") {
                let host = line[5..].trim();
                
                // 判断是否为HTTPS
                let scheme = if url_or_path.starts_with("/") && 
                               (request_str.contains(":443") || 
                                request_str.to_lowercase().contains("https")) {
                    "https"
                } else {
                    "http"
                };
                
                return Ok(format!("{}://{}{}", scheme, host, url_or_path));
            }
        }

        Err(anyhow::anyhow!("Could not extract URL from HTTP request"))
    }
}

/// 透明连接协议解析器 - 便捷封装
pub struct TransparentConnectionParser {
    handler: StandardProtocolHandler,
    peek_timeout_ms: u64,
    peek_buffer_size: usize,
}

impl TransparentConnectionParser {
    pub fn new() -> Self {
        Self {
            handler: StandardProtocolHandler::new(),
            peek_timeout_ms: 500,
            peek_buffer_size: 4096,
        }
    }

    pub fn with_timeouts(peek_timeout_ms: u64, peek_buffer_size: usize) -> Self {
        Self {
            handler: StandardProtocolHandler::new(),
            peek_timeout_ms,
            peek_buffer_size,
        }
    }

    /// 从TCP流中解析协议信息
    pub async fn parse_from_stream(&self, stream: &TcpStream) -> Result<ProtocolInfo> {
        // 使用peek来读取第一部分数据，判断协议类型
        let mut buffer = vec![0u8; self.peek_buffer_size];
        let n = match tokio::time::timeout(
            Duration::from_millis(self.peek_timeout_ms),
            stream.peek(&mut buffer)
        ).await {
            Ok(Ok(n)) if n > 0 => n,
            _ => {
                debug!("Failed to peek client data");
                return Err(anyhow::anyhow!("Failed to read client data"));
            }
        };

        let request_data = &buffer[..n];
        self.handler.parse_protocol_info(request_data).await
    }

    /// 从TCP流中解析目标地址（向后兼容）
    pub async fn parse_target_from_stream(&self, stream: &TcpStream) -> Result<(String, u16)> {
        let protocol_info = self.parse_from_stream(stream).await?;
        Ok((protocol_info.target_host, protocol_info.target_port))
    }

    /// 从TCP流中提取完整URL（向后兼容）
    pub async fn extract_full_url_from_stream(&self, stream: &TcpStream) -> Result<String> {
        let protocol_info = self.parse_from_stream(stream).await?;
        protocol_info.full_url.ok_or_else(|| anyhow::anyhow!("No URL available"))
    }
}

impl Default for TransparentConnectionParser {
    fn default() -> Self {
        Self::new()
    }
}

/// 未来TLS拦截能力的预留接口
pub trait TlsInterceptionCapable {
    /// 检查是否可以拦截指定域名的TLS
    async fn can_intercept_tls(&self, sni: &str) -> bool;
    
    /// 设置TLS拦截
    async fn setup_tls_interception(&self, sni: &str) -> Result<TlsInterceptionSetup>;
}

/// TLS拦截设置（预留结构）
#[derive(Debug)]
pub struct TlsInterceptionSetup {
    pub intercept_mode: TlsInterceptionMode,
    pub certificate_path: Option<String>,
    pub private_key_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_detection() {
        let http_request = b"GET / HTTP/1.1\r\nHost: www.google.com\r\n\r\n";
        assert!(StandardProtocolHandler::looks_like_http(http_request));
        assert!(!StandardProtocolHandler::is_tls_handshake(http_request));
    }

    #[test]
    fn test_tls_detection() {
        // 模拟TLS Client Hello开头
        let tls_handshake = [
            0x16, 0x03, 0x03, // TLS Handshake, version 3.3
            0x00, 0x20,       // Length
            0x01,             // Client Hello
        ];
        
        assert!(StandardProtocolHandler::is_tls_handshake(&tls_handshake));
        assert!(!StandardProtocolHandler::looks_like_http(&tls_handshake));
    }

    #[test]
    fn test_http_target_parsing() {
        let http_request = b"GET / HTTP/1.1\r\nHost: www.google.com\r\n\r\n";
        let (host, port) = StandardProtocolHandler::parse_http_target(http_request).unwrap();
        assert_eq!(host, "www.google.com");
        assert_eq!(port, 80);

        let https_request = b"CONNECT www.google.com:443 HTTP/1.1\r\n\r\n";
        let (host, port) = StandardProtocolHandler::parse_http_target(https_request).unwrap();
        assert_eq!(host, "www.google.com");
        assert_eq!(port, 443);
    }

    #[test]
    fn test_url_extraction() {
        let http_request = b"GET /api/v1/users HTTP/1.1\r\nHost: api.example.com\r\n\r\n";
        let url = StandardProtocolHandler::extract_url_from_http_request(http_request).unwrap();
        assert_eq!(url, "http://api.example.com/api/v1/users");
        
        let https_connect = b"CONNECT secure.example.com:443 HTTP/1.1\r\n\r\n";
        let url = StandardProtocolHandler::extract_url_from_http_request(https_connect).unwrap();
        assert_eq!(url, "https://secure.example.com:443");
    }

    #[tokio::test]
    async fn test_protocol_detection() {
        let handler = StandardProtocolHandler::new();
        
        let http_data = b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let protocol = handler.detect_protocol(http_data).await;
        assert_eq!(protocol, ProtocolType::Http);
        
        let tls_data = [0x16, 0x03, 0x03, 0x00, 0x20, 0x01];
        let protocol = handler.detect_protocol(&tls_data).await;
        assert_eq!(protocol, ProtocolType::Https);
    }
}
