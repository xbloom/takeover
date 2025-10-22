//! 复合出站适配器
//! 
//! 支持根据规则匹配结果选择不同的出站适配器：
//! - Direct连接：直接连接目标服务器
//! - SOCKS5代理：通过SOCKS5代理连接
//! - 智能选择：根据RuleMatchingMiddleware的决策自动选择

use std::sync::Arc;
use std::collections::HashMap;
use tracing::{debug, warn, error};
use async_trait::async_trait;

use crate::core::ports::{OutboundPort, NetworkStream, AppError};
use crate::core::model::ConnectionContext;
use super::{DirectOutboundAdapter, Socks5OutboundAdapter};
use crate::core::ports::ProxyConfig;

/// 复合出站适配器
/// 
/// 根据规则匹配中间件的决策结果选择合适的出站适配器
pub struct CompositeOutboundAdapter {
    /// Direct出站适配器
    direct_adapter: DirectOutboundAdapter,
    /// 代理配置映射 (代理名称 -> 配置)
    proxy_configs: HashMap<String, ProxyConfig>,
    /// 代理适配器缓存 (代理名称 -> 适配器)
    proxy_adapters: Arc<tokio::sync::RwLock<HashMap<String, Arc<Socks5OutboundAdapter>>>>,
    /// 默认代理名称
    default_proxy_name: Option<String>,
    /// 统计信息
    stats: Arc<tokio::sync::RwLock<CompositeStats>>,
}

/// 复合适配器统计信息
#[derive(Debug, Default, Clone)]
pub struct CompositeStats {
    /// 总连接数
    pub total_connections: u64,
    /// Direct连接数
    pub direct_connections: u64,
    /// SOCKS5连接数
    pub socks5_connections: u64,
    /// 规则匹配成功数
    pub rule_matches: u64,
    /// 使用默认策略数
    pub default_decisions: u64,
    /// 适配器选择错误数
    pub adapter_selection_errors: u64,
}

impl CompositeOutboundAdapter {
    /// 创建新的复合出站适配器
    pub fn new() -> Self {
        Self {
            direct_adapter: DirectOutboundAdapter::new(),
            proxy_configs: HashMap::new(),
            proxy_adapters: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            default_proxy_name: None,
            stats: Arc::new(tokio::sync::RwLock::new(CompositeStats::default())),
        }
    }
    
    /// 使用代理配置映射创建复合适配器
    pub fn with_proxy_configs(proxy_configs: HashMap<String, ProxyConfig>) -> Self {
        Self {
            direct_adapter: DirectOutboundAdapter::new(),
            proxy_configs,
            proxy_adapters: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            default_proxy_name: None,
            stats: Arc::new(tokio::sync::RwLock::new(CompositeStats::default())),
        }
    }
    
    /// 使用默认SOCKS5配置创建复合适配器（保持向后兼容）
    pub fn with_socks5_config(socks5_config: ProxyConfig) -> Self {
        let mut proxy_configs = HashMap::new();
        proxy_configs.insert("default".to_string(), socks5_config);
        
        Self {
            direct_adapter: DirectOutboundAdapter::new(),
            proxy_configs,
            proxy_adapters: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            default_proxy_name: Some("default".to_string()),
            stats: Arc::new(tokio::sync::RwLock::new(CompositeStats::default())),
        }
    }
    
    /// 添加代理配置
    pub fn add_proxy_config(&mut self, name: String, config: ProxyConfig) {
        self.proxy_configs.insert(name, config);
    }
    
    /// 设置默认代理
    pub fn set_default_proxy(&mut self, name: String) {
        if self.proxy_configs.contains_key(&name) {
            self.default_proxy_name = Some(name);
        }
    }
    
    /// 从连接上下文中读取路由决策
    fn read_routing_decision(&self, context: &ConnectionContext) -> RoutingDecision {
        // 读取规则匹配中间件存储的决策结果
        if let Some(routing_data) = context.custom_data.get("routing_decision") {
            if let Some(decision_str) = routing_data.get("decision").and_then(|v| v.as_str()) {
                debug!("🎯 读取到路由决策: {}", decision_str);
                
                match decision_str {
                    "proxy_by_name" => {
                        // 读取代理名称
                        if let Some(proxy_name) = routing_data.get("proxy_name").and_then(|v| v.as_str()) {
                            debug!("🔀 规则匹配结果：使用指定代理: {}", proxy_name);
                            self.update_rule_match_stats();
                            return RoutingDecision::ProxyByName(proxy_name.to_string());
                        } else {
                            warn!("⚠️ 代理决策缺少代理名称，使用默认代理");
                            self.update_rule_match_stats();
                            return RoutingDecision::Proxy;
                        }
                    }
                    "proxy" => {
                        debug!("🔀 规则匹配结果：使用代理 (兼容旧格式，使用默认SOCKS5)");
                        self.update_rule_match_stats();
                        return RoutingDecision::Proxy;
                    }
                    "direct" => {
                        debug!("📡 规则匹配结果：直接连接");
                        self.update_rule_match_stats();
                        return RoutingDecision::Direct;
                    }
                    "deny" => {
                        debug!("🚫 规则匹配结果：拒绝连接");
                        self.update_rule_match_stats();
                        return RoutingDecision::Deny;
                    }
                    _ => {
                        warn!("⚠️ 未知的路由决策: {}", decision_str);
                    }
                }
            }
        }
        
        // 没有找到路由决策，使用默认策略
        debug!("📋 未找到路由决策，使用默认策略: Direct");
        self.update_default_decision_stats();
        RoutingDecision::Direct
    }
    
    /// 选择适配器并建立连接
    async fn select_adapter_and_connect(
        &self, 
        context: &ConnectionContext
    ) -> Result<Box<dyn NetworkStream>, AppError> {
        // 更新总连接统计
        self.update_total_connection_stats().await;
        
        let decision = self.read_routing_decision(context);
        debug!("🚀 路由决策: {:?}", decision);
        
        match decision {
            RoutingDecision::Direct => {
                debug!("📡 使用Direct适配器连接到: {}:{}", context.target_host, context.target_port);
                self.update_direct_connection_stats().await;
                
                self.direct_adapter.connect(context).await
                    .map_err(|e| {
                        error!("❌ Direct连接失败: {}", e);
                        e
                    })
            }
            RoutingDecision::Proxy => {
                debug!("🔀 使用代理适配器连接到: {}:{}", context.target_host, context.target_port);
                self.update_socks5_connection_stats().await;
                
                // 获取或创建代理适配器
                let proxy_adapter = self.get_or_create_proxy_adapter_by_name(None).await?;
                
                proxy_adapter.connect(context).await
                    .map_err(|e| {
                        error!("❌ 代理连接失败: {}", e);
                        e
                    })
            }
            RoutingDecision::ProxyByName(proxy_name) => {
                debug!("🔀 使用指定代理适配器 {} 连接到: {}:{}", proxy_name, context.target_host, context.target_port);
                self.update_socks5_connection_stats().await;
                
                // 克隆代理名称以避免移动问题
                let proxy_name_clone = proxy_name.clone();
                
                // 获取或创建指定名称的代理适配器
                let proxy_adapter = self.get_or_create_proxy_adapter_by_name(Some(proxy_name)).await?;
                
                proxy_adapter.connect(context).await
                    .map_err(|e| {
                        error!("❌ 代理 {} 连接失败: {}", proxy_name_clone, e);
                        e
                    })
            }
            RoutingDecision::Deny => {
                warn!("🚫 连接被规则拒绝: {}:{}", context.target_host, context.target_port);
                Err(AppError::Config("Connection denied by rules".to_string()))
            }
        }
    }
    
    /// 获取或创建代理适配器
    #[allow(dead_code)]
    async fn get_or_create_proxy_adapter(&self) -> Result<Arc<Socks5OutboundAdapter>, AppError> {
        // 确定使用哪个代理
        let proxy_name = self.default_proxy_name.as_ref()
            .unwrap_or(&"default".to_string())
            .clone();
        
        debug!("🔍 查找代理适配器: {}", proxy_name);
        
        // 检查缓存中是否已有适配器
        {
            let adapters = self.proxy_adapters.read().await;
            if let Some(adapter) = adapters.get(&proxy_name) {
                debug!("✅ 使用缓存的代理适配器: {}", proxy_name);
                return Ok(Arc::clone(adapter));
            }
        }
        
        // 获取代理配置
        let proxy_config = self.proxy_configs.get(&proxy_name)
            .ok_or_else(|| AppError::Config(format!("代理配置未找到: {}", proxy_name)))?;
        
        debug!("🔧 创建新的代理适配器: {} ({})", proxy_name, proxy_config.url);
        
        // 创建新的适配器
        let adapter = Arc::new(Socks5OutboundAdapter::with_proxy(proxy_config.clone()));
        
        // 缓存适配器
        {
            let mut adapters = self.proxy_adapters.write().await;
            adapters.insert(proxy_name.clone(), Arc::clone(&adapter));
        }
        
        Ok(adapter)
    }
    
    /// 根据代理名称获取或创建代理适配器
    async fn get_or_create_proxy_adapter_by_name(&self, proxy_name: Option<String>) -> Result<Arc<Socks5OutboundAdapter>, AppError> {
        // 确定使用哪个代理
        let proxy_name = proxy_name
            .or_else(|| self.default_proxy_name.clone())
            .unwrap_or_else(|| "default".to_string());
        
        debug!("🔍 查找指定代理适配器: {}", proxy_name);
        
        // 检查缓存中是否已有适配器
        {
            let adapters = self.proxy_adapters.read().await;
            if let Some(adapter) = adapters.get(&proxy_name) {
                debug!("✅ 使用缓存的指定代理适配器: {}", proxy_name);
                return Ok(Arc::clone(adapter));
            }
        }
        
        // 获取代理配置
        let proxy_config = self.proxy_configs.get(&proxy_name)
            .ok_or_else(|| AppError::Config(format!("代理配置未找到: {}", proxy_name)))?;
        
        debug!("🔧 创建新的指定代理适配器: {} ({})", proxy_name, proxy_config.url);
        
        // 创建新的适配器
        let adapter = Arc::new(Socks5OutboundAdapter::with_proxy(proxy_config.clone()));
        
        // 缓存适配器
        {
            let mut adapters = self.proxy_adapters.write().await;
            adapters.insert(proxy_name.clone(), Arc::clone(&adapter));
        }
        
        Ok(adapter)
    }
    
    /// 更新总连接统计
    async fn update_total_connection_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.total_connections += 1;
    }
    
    /// 更新Direct连接统计
    async fn update_direct_connection_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.direct_connections += 1;
    }
    
    /// 更新SOCKS5连接统计
    async fn update_socks5_connection_stats(&self) {
        let mut stats = self.stats.write().await;
        stats.socks5_connections += 1;
    }
    
    /// 更新规则匹配统计
    fn update_rule_match_stats(&self) {
        tokio::spawn({
            let stats = Arc::clone(&self.stats);
            async move {
                let mut stats = stats.write().await;
                stats.rule_matches += 1;
            }
        });
    }
    
    /// 更新默认决策统计
    fn update_default_decision_stats(&self) {
        tokio::spawn({
            let stats = Arc::clone(&self.stats);
            async move {
                let mut stats = stats.write().await;
                stats.default_decisions += 1;
            }
        });
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> CompositeStats {
        self.stats.read().await.clone()
    }
}

/// 路由决策类型
#[derive(Debug, Clone)]
enum RoutingDecision {
    /// 直接连接
    Direct,
    /// 使用代理
    Proxy,
    /// 使用指定名称的代理
    ProxyByName(String),
    /// 拒绝连接
    Deny,
}

#[async_trait]
impl OutboundPort for CompositeOutboundAdapter {
    // #[instrument(skip(self, context))]
    async fn connect(&self, context: &ConnectionContext) -> Result<Box<dyn NetworkStream>, AppError> {
        debug!("🔗 复合适配器开始连接: {}:{}", context.target_host, context.target_port);
        
        self.select_adapter_and_connect(context).await
    }
    
    async fn supports_target(&self, context: &ConnectionContext) -> bool {
        // 复合适配器支持所有目标，具体支持性由子适配器决定
        let decision = self.read_routing_decision(context);
        
        match decision {
            RoutingDecision::Direct => self.direct_adapter.supports_target(context).await,
            RoutingDecision::Proxy => {
                // 检查是否有可用的代理配置
                if let Some(proxy_name) = &self.default_proxy_name {
                    self.proxy_configs.contains_key(proxy_name)
                } else {
                    !self.proxy_configs.is_empty()
                }
            },
            RoutingDecision::ProxyByName(proxy_name) => {
                // 检查指定的代理配置是否存在
                self.proxy_configs.contains_key(&proxy_name)
            },
            RoutingDecision::Deny => false,
        }
    }
    
    fn name(&self) -> &str {
        "composite_outbound"
    }
}

impl Default for CompositeOutboundAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use crate::core::model::Connection;
    
    #[tokio::test]
    async fn test_composite_adapter_creation() {
        let adapter = CompositeOutboundAdapter::new();
        assert_eq!(adapter.name(), "composite_outbound");
        
        let stats = adapter.get_stats().await;
        assert_eq!(stats.total_connections, 0);
    }
    
    #[tokio::test]
    async fn test_routing_decision_reading() {
        let adapter = CompositeOutboundAdapter::new();
        
        let connection = Connection::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80)
        );
        
        let mut context = ConnectionContext::new(connection, "example.com".to_string(), 80);
        
        // 测试默认决策（没有路由信息）
        let decision = adapter.read_routing_decision(&context);
        assert!(matches!(decision, RoutingDecision::Direct));
        
        // 测试Direct决策
        context.custom_data.insert(
            "routing_decision".to_string(),
            serde_json::json!({
                "decision": "direct"
            })
        );
        let decision = adapter.read_routing_decision(&context);
        assert!(matches!(decision, RoutingDecision::Direct));
        
        // 测试Proxy决策
        context.custom_data.insert(
            "routing_decision".to_string(),
            serde_json::json!({
                "decision": "proxy"
            })
        );
        let decision = adapter.read_routing_decision(&context);
        assert!(matches!(decision, RoutingDecision::Proxy));
    }
    
    #[tokio::test]
    async fn test_stats_tracking() {
        let adapter = CompositeOutboundAdapter::new();
        
        adapter.update_total_connection_stats().await;
        adapter.update_direct_connection_stats().await;
        
        let stats = adapter.get_stats().await;
        assert_eq!(stats.total_connections, 1);
        assert_eq!(stats.direct_connections, 1);
        assert_eq!(stats.socks5_connections, 0);
    }
}
