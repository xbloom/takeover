//! 透明代理管理器 - 业务逻辑层
//! 
//! 这个模块负责管理透明代理的业务逻辑，包括：
//! - 生成适当的 pfctl 规则
//! - 协调底层 pfctl 操作

use anyhow::Result;
use tracing::{info, warn, error, debug};
use crate::pfctl::Pfctl;



const DEFAULT_ANCHOR_NAME: &str = "takeover_transparent";

/// 透明代理管理器
/// 
/// 负责设置和管理 macOS 上的透明代理功能。
/// 使用 pfctl 的 rdr 和 pass 规则实现流量拦截和重定向。
pub struct TransparentProxyManager {
    /// 底层 pfctl 实例
    pfctl: Pfctl,
    /// 被拦截的端口列表
    intercepted_ports: Vec<u16>,
    /// 本地代理服务器端口
    local_proxy_port: u16,
    /// 豁免用户名（该用户的连接不会被拦截）
    bypass_user: Option<String>,
    /// 是否已激活
    is_active: bool,
}

impl TransparentProxyManager {
    /// 创建新的透明代理管理器
    pub fn new() -> Self {
        Self::with_anchor_name(DEFAULT_ANCHOR_NAME)
    }

    /// 使用指定的锚点名称创建管理器
    pub fn with_anchor_name(anchor_name: &str) -> Self {
        Self {
            pfctl: Pfctl::new(anchor_name),
            intercepted_ports: Vec::new(),
            local_proxy_port: 0,
            bypass_user: None,
            is_active: false,
        }
    }

    // === 主要操作 ===

    /// 启动透明代理
    /// 
    /// # 参数
    /// - `local_proxy_port`: 本地代理服务器监听的端口
    /// - `intercepted_ports`: 需要拦截的目标端口列表
    /// - `bypass_user`: 豁免用户名（该用户的连接不会被拦截）
    pub async fn start(&mut self, local_proxy_port: u16, intercepted_ports: &[u16], bypass_user: Option<&str>) -> Result<()> {
        if self.is_active {
            warn!("Transparent proxy is already active");
            return Ok(());
        }

        // 设置豁免用户
        self.bypass_user = bypass_user.map(|s| s.to_string());

        info!("Starting transparent proxy: local_port={}, intercept_ports={:?}, bypass_user={:?}", 
              local_proxy_port, intercepted_ports, self.bypass_user);

        // 1. 确保 pf 已启用
        if !self.pfctl.is_enabled().await? {
            info!("Enabling pf firewall");
            self.pfctl.enable().await?;
        }

        // 2. 生成规则并设置 pfctl（首次启动需要修改主配置文件）
        self.local_proxy_port = local_proxy_port;
        self.intercepted_ports = intercepted_ports.to_vec();
        
        let rules = self.generate_rules();
        
        // 使用 setup_anchor_and_reload_main：写入 anchor 文件 + 设置主配置 + 重新加载主配置
        self.pfctl.setup_anchor_and_reload_main(&rules).await?;

        self.is_active = true;
        info!("✅ Transparent proxy started successfully");
        
        Ok(())
    }

    /// 停止透明代理
    pub async fn stop(&mut self) -> Result<()> {
        if !self.is_active {
            debug!("Transparent proxy is not active");
            return Ok(());
        }

        info!("Stopping transparent proxy");

        // 1. 清空锚点规则
        if let Err(e) = self.pfctl.flush_anchor().await {
            warn!("Failed to flush anchor rules: {}", e);
        }

        // 2. 从主配置中移除锚点并重新加载（主配置文件被修改，需要重新加载主配置）
        if let Err(e) = self.pfctl.remove_anchor_from_main_conf().await {
            warn!("Failed to remove anchor from main conf: {}", e);
        }
        
        // 使用 smart_reload：主配置文件被修改，需要重新加载主配置
        if let Err(e) = self.pfctl.smart_reload(true).await {
            warn!("Failed to reload main conf: {}", e);
        }

        self.is_active = false;
        self.intercepted_ports.clear();
        self.bypass_user = None;
        self.local_proxy_port = 0;

        info!("✅ Transparent proxy stopped successfully");
        Ok(())
    }

    /// 更新拦截的端口列表
    pub async fn update_ports(&mut self, new_ports: &[u16]) -> Result<()> {
        if !self.is_active {
            return Err(anyhow::anyhow!("Transparent proxy is not active"));
        }

        info!("Updating intercepted ports: {:?} -> {:?}", self.intercepted_ports, new_ports);

        // 更新端口列表
        self.intercepted_ports = new_ports.to_vec();

        // 重新生成并加载规则（只需要重新加载 anchor 文件）
        let rules = self.generate_rules();
        
        // 使用 write_and_reload_anchor：只写入和重新加载 anchor 文件（不需要修改主配置）
        self.pfctl.write_and_reload_anchor(&rules).await?;

        info!("✅ Successfully updated intercepted ports");
        Ok(())
    }

    // === 规则生成 ===

    /// 生成完整的 pfctl 规则内容
    fn generate_rules(&self) -> String {
        let mut rules = String::new();

        // 添加文件头注释
        rules.push_str(&format!(
            "# Transparent Proxy Rules\n\
             # Generated at: {}\n\
             # Anchor: {}\n\
             # Local proxy port: {}\n\
             # Intercepted ports: {:?}\n\
             \n",
            chrono::Utc::now(),
            self.pfctl.anchor_name(),
            self.local_proxy_port,
            self.intercepted_ports
        ));

        // 添加 RDR 规则部分
        rules.push_str("# === RDR Rules (Translation) ===\n");
        rules.push_str("# Redirect incoming traffic on lo0 to the proxy port\n");
        for rule in self.generate_rdr_rules() {
            rules.push_str(&format!("{}\n", rule));
        }
        rules.push('\n');

        // 添加 Filter 规则部分
        rules.push_str("# === Filter Rules ===\n");
        
        // 添加路由规则（使用用户过滤器防止循环）
        rules.push_str("# Route outbound traffic to lo0 for interception (bypass specific user)\n");
        for rule in self.generate_filter_rules() {
            rules.push_str(&format!("{}\n", rule));
        }

        rules
    }

    /// 生成 RDR（重定向）规则
    fn generate_rdr_rules(&self) -> Vec<String> {
        let mut rules = Vec::new();

        // 为每个拦截的端口生成 rdr 规则
        // 这些规则将 lo0 接口上的入站流量重定向到本地代理端口
        for port in &self.intercepted_ports {
            rules.push(format!(
                "rdr pass on lo0 inet proto tcp from any to any port {} -> 127.0.0.1 port {}",
                port, self.local_proxy_port
            ));
        }

        rules
    }

    /// 生成 Filter（过滤）规则
    fn generate_filter_rules(&self) -> Vec<String> {
        let mut rules = Vec::new();

        // 为每个拦截的端口生成 route-to 规则
        // 使用用户过滤器避免拦截指定用户的连接（防止循环）
        for port in &self.intercepted_ports {
            let user_filter = if let Some(ref bypass_user) = self.bypass_user {
                format!(" user {{ !={} }}", bypass_user)
            } else {
                String::new()
            };
            
            rules.push(format!(
                "pass out quick route-to (lo0 127.0.0.1) inet proto tcp from any to any port {}{} keep state",
                port, user_filter
            ));
        }

        rules
    }



    // === 状态查询 ===

    /// 检查透明代理是否已激活
    pub fn is_active(&self) -> bool {
        self.is_active
    }

    /// 获取被拦截的端口列表
    pub fn get_intercepted_ports(&self) -> &[u16] {
        &self.intercepted_ports
    }

    /// 获取本地代理端口
    pub fn get_proxy_port(&self) -> u16 {
        self.local_proxy_port
    }

    /// 获取当前的 pfctl 规则（用于调试）
    pub async fn get_current_rules(&self) -> Result<String> {
        self.pfctl.get_anchor_rules().await
    }
}

impl Default for TransparentProxyManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TransparentProxyManager {
    fn drop(&mut self) {
        if self.is_active {
            error!(
                "TransparentProxyManager dropped while still active! \
                 This may leave pfctl rules in place. \
                 Always call stop() before dropping."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manager_creation() {
        let manager = TransparentProxyManager::new();
        assert!(!manager.is_active());
        assert_eq!(manager.get_intercepted_ports().len(), 0);
        assert_eq!(manager.get_proxy_port(), 0);
    }

        #[test]
    fn test_rule_generation() {
        let mut manager = TransparentProxyManager::new();
        manager.local_proxy_port = 8080;
        manager.intercepted_ports = vec![80, 443];

        let rules = manager.generate_rules();

        // 验证规则包含必要的部分
        assert!(rules.contains("# Transparent Proxy Rules"));
        assert!(rules.contains("# === RDR Rules"));
        assert!(rules.contains("# === Filter Rules"));

        // 验证 RDR 规则
        assert!(rules.contains("rdr pass on lo0 inet proto tcp from any to any port 80 -> 127.0.0.1 port 8080"));
        assert!(rules.contains("rdr pass on lo0 inet proto tcp from any to any port 443 -> 127.0.0.1 port 8080"));
        
        // 验证路由规则（无用户过滤器）
        assert!(rules.contains("pass out quick route-to (lo0 127.0.0.1) inet proto tcp from any to any port 80 keep state"));
        assert!(rules.contains("pass out quick route-to (lo0 127.0.0.1) inet proto tcp from any to any port 443 keep state"));
    }

    #[test]
    fn test_rule_generation_with_user() {
        let mut manager = TransparentProxyManager::new();
        manager.local_proxy_port = 8080;
        manager.intercepted_ports = vec![80, 443];
        manager.bypass_user = Some("nobody".to_string());

        let rules = manager.generate_rules();

        // 验证用户过滤器被正确应用
        assert!(rules.contains("pass out quick route-to (lo0 127.0.0.1) inet proto tcp from any to any port 80 user { !=nobody } keep state"));
        assert!(rules.contains("pass out quick route-to (lo0 127.0.0.1) inet proto tcp from any to any port 443 user { !=nobody } keep state"));
    }

    #[test]
    fn test_rdr_rules() {
        let mut manager = TransparentProxyManager::new();
        manager.local_proxy_port = 10080;
        manager.intercepted_ports = vec![22, 80, 443, 8080];

        let rdr_rules = manager.generate_rdr_rules();
        
        assert_eq!(rdr_rules.len(), 4);
        assert!(rdr_rules.iter().all(|r| r.contains("rdr pass on lo0")));
        assert!(rdr_rules.iter().all(|r| r.contains("-> 127.0.0.1 port 10080")));
    }

    #[test]
    fn test_filter_rules() {
        let mut manager = TransparentProxyManager::new();
        manager.intercepted_ports = vec![80, 443];

        let filter_rules = manager.generate_filter_rules();
        
        assert_eq!(filter_rules.len(), 2);
        assert!(filter_rules.iter().all(|r| r.contains("pass out route-to (lo0 127.0.0.1)")));
        assert!(filter_rules.iter().all(|r| r.contains("keep state")));
    }

    #[test]
    fn test_custom_anchor_name() {
        let manager = TransparentProxyManager::with_anchor_name("custom_anchor");
        assert_eq!(manager.pfctl.anchor_name(), "custom_anchor");
    }
}
