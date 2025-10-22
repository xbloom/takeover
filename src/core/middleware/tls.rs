//! TLS中间件 - 重构版本
//! 
//! 使用统一的TLS Service进行客户端拦截，负责：
//! - TLS连接检测和拦截
//! - SSL Bump实现（通过TLS Service）
//! - ALPN协议协商
//! - TLS流的透明代理

use std::sync::Arc;
use tracing::{info, warn, debug};
use async_trait::async_trait;

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{ConnectionContext, ProcessingState, TlsConfig};
use crate::core::ports::AppError;
use crate::core::services::TlsService;
use crate::error::TakeoverError;

/// TLS中间件（重构版本）
/// 
/// 使用统一的TLS Service进行客户端拦截
pub struct TlsMiddleware {
    /// TLS服务
    tls_service: Option<Arc<TlsService>>,
    /// 是否启用
    enabled: bool,
}

impl TlsMiddleware {
    /// 创建新的TLS中间件
    pub fn new() -> Self {
        Self {
            tls_service: None,
            enabled: false,
        }
    }

    /// 使用TLS配置创建中间件
    pub fn from_config(tls_config: &TlsConfig) -> Result<Self, AppError> {
        if !tls_config.enabled {
            debug!("TLS middleware disabled in config");
            return Ok(Self::new());
        }

        let tls_service = TlsService::from_config(tls_config)?;
        
        Ok(Self {
            tls_service: Some(Arc::new(tls_service)),
            enabled: true,
        })
    }

    /// 检测是否是TLS连接
    async fn detect_tls_handshake(&self, context: &ConnectionContext) -> bool {
        // 检查端口是否是HTTPS端口
        if matches!(context.target_port, 443 | 8443 | 9443) {
            return true;
        }
        
        // 检查自定义数据中的协议信息
        if let Some(protocol) = context.custom_data.get("detected_protocol") {
            if let Some(protocol_str) = protocol.as_str() {
                return protocol_str == "tls" || protocol_str == "https";
            }
        }

        // 检查原始目标协议
        if let Some(protocol) = context.custom_data.get("original_target_protocol") {
            if let Some(protocol_str) = protocol.as_str() {
                return protocol_str == "https";
            }
        }

        false
    }
    
    /// 检查是否应该拦截此连接
    async fn should_intercept_connection(&self, context: &ConnectionContext) -> bool {
        if let Some(ref tls_service) = self.tls_service {
            tls_service.should_intercept_domain(&context.target_host).await
        } else {
            false
        }
    }
    
    /// 执行TLS拦截
    async fn perform_tls_interception(&self, context: &mut ConnectionContext) -> Result<(), AppError> {
        let tls_service = self.tls_service.as_ref()
            .ok_or_else(|| AppError::Config("TLS service not available".to_string()))?;

        let domain = &context.target_host;
        let port = context.target_port;
        
        debug!("🔐 开始TLS拦截: {}:{}", domain, port);
        
        // 获取TCP流
        let tcp_stream = context.connection.stream.take()
            .ok_or_else(|| AppError::Config("No TCP stream available for TLS interception".to_string()))?;
        
        // 使用TLS Service执行客户端拦截
        let (client_tls_stream, alpn_protocol) = tls_service
            .intercept_client_tls(tcp_stream, domain)
            .await?;

        // 存储客户端TLS流
                context.connection.set_client_tls_stream(client_tls_stream);
                
                debug!("TLS拦截成功: {}:{}", domain, port);
                
        // 存储ALPN协商结果到上下文
                if let Some(protocol) = &alpn_protocol {
                    context.custom_data.insert(
                        "alpn_protocol".to_string(),
                        serde_json::json!(protocol)
                    );
                    debug!("ALPN协商结果: {}", protocol);
                }
                
                // 存储拦截信息到上下文
                context.custom_data.insert(
                    "tls_intercepted".to_string(),
                    serde_json::json!({
                        "success": true,
                        "domain": domain,
                        "port": port,
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                        "tls_version": "rustls-tokio",
                        "client_side_only": true,
                "alpn_protocol": alpn_protocol,
                "intercepted_by": "tls_service"
            })
        );
                
                Ok(())
    }
}

impl Default for TlsMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for TlsMiddleware {
    fn name(&self) -> &str {
        "tls"
    }
    
    async fn process(
        &self,
        context: &mut ConnectionContext,
        _core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, TakeoverError> {
        if !self.enabled {
            debug!("TLS middleware disabled, skipping");
            return Ok(MiddlewareResult::Continue);
        }

        debug!("TLS中间件开始处理");
        
        // 更新处理状态
        context.set_processing_state(ProcessingState::TlsHandshake);
        
        // 检查是否是TLS连接
        let is_tls = self.detect_tls_handshake(context).await;
        
        if !is_tls {
            debug!("Not a TLS connection, skipping TLS middleware");
            return Ok(MiddlewareResult::Continue);
        }
        
        debug!("Detected TLS connection to {}:{}", context.target_host, context.target_port);
        
        // 检查是否应该拦截
        if !self.should_intercept_connection(context).await {
            debug!("TLS connection not intercepted based on configuration");
            
            // 标记为TLS但不拦截
            context.custom_data.insert(
                "tls_processed".to_string(),
                serde_json::json!({"intercepted": false, "reason": "not_in_intercept_list"})
            );
            
            return Ok(MiddlewareResult::Continue);
        }
        
        // 执行TLS拦截
        match self.perform_tls_interception(context).await {
            Ok(()) => {
                debug!("✅ TLS interception successful for {}:{}", context.target_host, context.target_port);
                
                // 标记为已拦截
                context.custom_data.insert(
                    "tls_processed".to_string(),
                    serde_json::json!({"intercepted": true, "success": true})
                );
                
                Ok(MiddlewareResult::Continue)
            }
            Err(e) => {
                debug!("❌ TLS interception failed for {}:{}: {}", context.target_host, context.target_port, e);
                
                // 标记为拦截失败
                context.custom_data.insert(
                    "tls_processed".to_string(),
                    serde_json::json!({"intercepted": false, "success": false, "error": e.to_string()})
                );
                
                // 拦截失败则拒绝连接
                Err(TakeoverError::Config(format!("TLS interception failed: {}", e)))
            }
        }
    }
    
    fn priority(&self) -> i32 {
        10 // 高优先级，在HTTP解析之前执行
    }
    
    fn enabled(&self) -> bool {
        self.enabled
    }
    
    /// 初始化中间件 - 同步初始化TLS Service
    async fn initialize(&self, _app_core: &crate::core::AppCore) -> Result<(), TakeoverError> {
        if !self.enabled {
            debug!("TLS拦截未启用，跳过初始化");
            return Ok(());
        }
        
        if let Some(ref tls_service) = self.tls_service {
            info!("🔐 初始化TLS中间件...");
            
            // 初始化TLS Service（安装根CA证书等）
            tls_service.initialize().await
                .map_err(|e| TakeoverError::Config(format!("TLS service initialization failed: {}", e)))?;
            
            info!("✅ TLS中间件初始化完成");
        } else {
            warn!("⚠️  TLS拦截已启用但TLS服务未初始化");
        }
        
        Ok(())
    }

    /// 注册中间件服务到AppCore
    async fn register_services(&self, app_core: &crate::core::AppCore) -> Result<(), TakeoverError> {
        if let Some(ref tls_service) = self.tls_service {
            info!("📋 注册TLS Service到AppCore...");
            
            // 注册TLS Service到AppCore状态（使用register_arc避免类型不匹配）
            // tls_service 已经是 Arc<TlsService>，直接注册
            app_core.services.register_arc(tls_service.clone()).await;
            info!("✅ TLS Service已注册到AppCore");
        } else {
            warn!("⚠️ TLS中间件没有TLS Service实例 (可能TLS未启用)");
        }
        
        Ok(())
    }
}

/// 从TlsConfig创建TLS中间件的便捷函数
pub fn create_tls_middleware_from_config(tls_config: &TlsConfig) -> Result<TlsMiddleware, AppError> {
    TlsMiddleware::from_config(tls_config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::{Connection, TlsConfig};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    
    #[tokio::test]
    async fn test_middleware_creation() {
        let middleware = TlsMiddleware::new();
        assert_eq!(middleware.name(), "tls");
        assert!(!middleware.enabled());
    }
    
    #[tokio::test]
    async fn test_middleware_from_config_disabled() {
        let mut config = TlsConfig::default();
        config.enabled = false;
        
        let middleware = TlsMiddleware::from_config(&config).unwrap();
        assert!(!middleware.enabled);
        assert!(middleware.tls_service.is_none());
    }
    
    #[tokio::test]
    async fn test_tls_detection() {
        let middleware = TlsMiddleware::new();
        
        // 创建测试上下文
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443)
        );
        let context = ConnectionContext::new(connection, "example.com".to_string(), 443);
        
        // HTTPS端口应该被检测为TLS
        assert!(middleware.detect_tls_handshake(&context).await);
    }
    
    #[tokio::test]
    async fn test_non_tls_detection() {
        let middleware = TlsMiddleware::new();
        
        // 创建测试上下文
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        let context = ConnectionContext::new(connection, "example.com".to_string(), 80);
        
        // HTTP端口不应该被检测为TLS
        assert!(!middleware.detect_tls_handshake(&context).await);
    }
}