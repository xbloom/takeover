use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::debug;
use crate::config::ProxyConfig;
use crate::status::TrafficStats;

// SOCKS5 协议常量  
pub const SOCKS5_VERSION: u8 = 0x05;
pub const SOCKS5_CMD_CONNECT: u8 = 0x01;
pub const SOCKS5_RSV: u8 = 0x00;
pub const SOCKS5_ATYP_IPV4: u8 = 0x01;
pub const SOCKS5_ATYP_DOMAIN: u8 = 0x03;
pub const SOCKS5_ATYP_IPV6: u8 = 0x04;
pub const SOCKS5_NO_AUTH: u8 = 0x00;
pub const SOCKS5_USERNAME_PASSWORD: u8 = 0x02;
pub const SOCKS5_AUTH_VERSION: u8 = 0x01;
pub const SOCKS5_AUTH_SUCCESS: u8 = 0x00;
pub const SOCKS5_CONNECT_SUCCESS: u8 = 0x00;
pub const SOCKS5_NO_ACCEPTABLE_METHODS: u8 = 0xFF;

/// SOCKS5客户端 - 统一的SOCKS5握手实现
pub struct Socks5Client;

impl Socks5Client {
    /// 执行完整的SOCKS5连接（认证 + CONNECT）
    pub async fn connect(
        stream: &mut TcpStream,
        proxy_config: &ProxyConfig,
        target_host: &str,
        target_port: u16,
    ) -> Result<()> {
        debug!("Performing full SOCKS5 handshake to {}:{}", target_host, target_port);

        // 第一步：认证握手
        Self::perform_authentication(stream, proxy_config).await?;

        // 第二步：发送CONNECT请求
        Self::perform_connect(stream, target_host, target_port).await?;

        debug!("✅ SOCKS5 connection to {}:{} completed successfully", target_host, target_port);
        Ok(())
    }

    /// 执行SOCKS5认证握手
    async fn perform_authentication(
        stream: &mut TcpStream,
        proxy_config: &ProxyConfig,
    ) -> Result<()> {
        debug!("Performing SOCKS5 authentication handshake");

        // 发送认证方法选择
        let greeting = if proxy_config.auth_tuple().is_some() {
            vec![SOCKS5_VERSION, 0x02, SOCKS5_NO_AUTH, SOCKS5_USERNAME_PASSWORD]
        } else {
            vec![SOCKS5_VERSION, 0x01, SOCKS5_NO_AUTH]
        };

        stream.write_all(&greeting).await?;

        let mut response = vec![0; 2];
        stream.read_exact(&mut response).await?;

        if response[0] != SOCKS5_VERSION {
            return Err(anyhow::anyhow!("Invalid SOCKS5 response version: {}", response[0]));
        }

        match response[1] {
            SOCKS5_NO_AUTH => {
                debug!("SOCKS5: No authentication required");
            }
            SOCKS5_USERNAME_PASSWORD => {
                debug!("SOCKS5: Username/password authentication required");
                if let Some((username, password)) = proxy_config.auth_tuple() {
                    Self::perform_username_password_auth(stream, &username, &password).await?;
                } else {
                    return Err(anyhow::anyhow!("SOCKS5 requires authentication but none provided"));
                }
            }
            SOCKS5_NO_ACCEPTABLE_METHODS => {
                return Err(anyhow::anyhow!("SOCKS5: No acceptable authentication methods"));
            }
            _ => {
                return Err(anyhow::anyhow!("SOCKS5: Unsupported authentication method: {}", response[1]));
            }
        }

        debug!("✅ SOCKS5 authentication completed successfully");
        Ok(())
    }

    /// 执行用户名密码认证
    async fn perform_username_password_auth(
        stream: &mut TcpStream,
        username: &str,
        password: &str,
    ) -> Result<()> {
        debug!("Performing SOCKS5 username/password authentication");

        let mut auth_request = vec![SOCKS5_AUTH_VERSION];
        auth_request.push(username.len() as u8);
        auth_request.extend_from_slice(username.as_bytes());
        auth_request.push(password.len() as u8);
        auth_request.extend_from_slice(password.as_bytes());

        stream.write_all(&auth_request).await?;

        let mut auth_response = vec![0; 2];
        stream.read_exact(&mut auth_response).await?;

        if auth_response[1] != SOCKS5_AUTH_SUCCESS {
            return Err(anyhow::anyhow!("SOCKS5 authentication failed"));
        }

        debug!("✅ SOCKS5 authentication successful");
        Ok(())
    }

    /// 执行CONNECT请求
    async fn perform_connect(
        stream: &mut TcpStream,
        target_host: &str,
        target_port: u16,
    ) -> Result<()> {
        let mut connect_request = vec![SOCKS5_VERSION, SOCKS5_CMD_CONNECT, SOCKS5_RSV];

        // 地址类型和地址
        if target_host.parse::<std::net::Ipv4Addr>().is_ok() {
            // IPv4地址
            connect_request.push(SOCKS5_ATYP_IPV4);
            let ip: std::net::Ipv4Addr = target_host.parse()?;
            connect_request.extend_from_slice(&ip.octets());
        } else if target_host.parse::<std::net::Ipv6Addr>().is_ok() {
            // IPv6地址
            connect_request.push(SOCKS5_ATYP_IPV6);
            let ip: std::net::Ipv6Addr = target_host.parse()?;
            connect_request.extend_from_slice(&ip.octets());
        } else {
            // 域名
            connect_request.push(SOCKS5_ATYP_DOMAIN);
            connect_request.push(target_host.len() as u8);
            connect_request.extend_from_slice(target_host.as_bytes());
        }

        // 端口
        connect_request.extend_from_slice(&target_port.to_be_bytes());

        stream.write_all(&connect_request).await?;

        // 读取CONNECT响应
        let mut connect_response = vec![0; 4];
        stream.read_exact(&mut connect_response).await?;

        if connect_response[0] != SOCKS5_VERSION {
            return Err(anyhow::anyhow!("Invalid SOCKS5 CONNECT response version"));
        }

        if connect_response[1] != SOCKS5_CONNECT_SUCCESS {
            return Err(anyhow::anyhow!("SOCKS5 CONNECT failed with code: {}", connect_response[1]));
        }

        // 读取剩余的地址信息（我们不需要使用，但必须读取完整）
        Self::read_address_info(stream, connect_response[3]).await?;

        Ok(())
    }

    /// 读取地址信息
    async fn read_address_info(stream: &mut TcpStream, addr_type: u8) -> Result<()> {
        match addr_type {
            SOCKS5_ATYP_IPV4 => {
                // IPv4: 4字节地址 + 2字节端口
                let mut addr_info = vec![0; 6];
                stream.read_exact(&mut addr_info).await?;
            }
            SOCKS5_ATYP_DOMAIN => {
                // 域名: 1字节长度 + 域名 + 2字节端口
                let mut len_buf = vec![0; 1];
                stream.read_exact(&mut len_buf).await?;
                let len = len_buf[0] as usize;
                let mut addr_info = vec![0; len + 2];
                stream.read_exact(&mut addr_info).await?;
            }
            SOCKS5_ATYP_IPV6 => {
                // IPv6: 16字节地址 + 2字节端口
                let mut addr_info = vec![0; 18];
                stream.read_exact(&mut addr_info).await?;
            }
            _ => {
                return Err(anyhow::anyhow!("Unsupported address type in SOCKS5 response"));
            }
        }
        Ok(())
    }
}

/// 双向数据转发工具
pub struct DataForwarder;

impl DataForwarder {
    /// 在两个TCP流之间进行双向数据转发
    pub async fn bidirectional_copy(
        client_stream: TcpStream,
        proxy_stream: TcpStream,
        buffer_size: usize,
    ) -> Result<()> {
        // 直接调用统计版本，忽略统计结果
        Self::bidirectional_copy_with_stats(client_stream, proxy_stream, buffer_size)
            .await
            .map(|_stats| ())
    }

    /// 在两个TCP流之间进行双向数据转发并统计传输量
    pub async fn bidirectional_copy_with_stats(
        client_stream: TcpStream,
        proxy_stream: TcpStream,
        buffer_size: usize,
    ) -> Result<TrafficStats> {
        let (mut client_read, mut client_write) = client_stream.into_split();
        let (mut proxy_read, mut proxy_write) = proxy_stream.into_split();

        let client_to_proxy = tokio::spawn(async move {
            let mut buffer = vec![0; buffer_size];
            let mut total_bytes = 0u64;
            loop {
                match client_read.read(&mut buffer).await {
                    Ok(0) => break, // 连接关闭
                    Ok(n) => {
                        if proxy_write.write_all(&buffer[..n]).await.is_err() {
                            break;
                        }
                        total_bytes += n as u64;
                    }
                    Err(_) => break,
                }
            }
            total_bytes
        });

        let proxy_to_client = tokio::spawn(async move {
            let mut buffer = vec![0; buffer_size];
            let mut total_bytes = 0u64;
            loop {
                match proxy_read.read(&mut buffer).await {
                    Ok(0) => break, // 连接关闭
                    Ok(n) => {
                        if client_write.write_all(&buffer[..n]).await.is_err() {
                            break;
                        }
                        total_bytes += n as u64;
                    }
                    Err(_) => break,
                }
            }
            total_bytes
        });

        // 等待两个方向的传输都完成并收集统计数据
        let client_to_server_bytes = client_to_proxy.await.unwrap_or(0);
        let server_to_client_bytes = proxy_to_client.await.unwrap_or(0);

        debug!("Transfer completed - client to server: {} bytes, server to client: {} bytes", 
               client_to_server_bytes, server_to_client_bytes);

        let mut stats = TrafficStats::new();
        stats.add_client_to_server(client_to_server_bytes);
        stats.add_server_to_client(server_to_client_bytes);
        stats.finish();
        
        Ok(stats)
    }
}
