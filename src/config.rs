use crate::error::TakeoverError;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub url: String,
    pub proxy_type: ProxyType,
    pub auth: Option<ProxyAuth>,
}

impl ProxyConfig {
    /// 解析代理URL获取主机和端口
    pub fn parse_host_port(&self) -> Result<(String, u16), crate::error::TakeoverError> {
        crate::utils::parse_proxy_url(&self.url)
            .map(|(_, host, port, _)| (host, port))
            .map_err(|e| crate::error::TakeoverError::Config(format!("Failed to parse proxy URL: {}", e)))
    }
    
    /// 获取主机地址
    pub fn host(&self) -> Result<String, crate::error::TakeoverError> {
        self.parse_host_port().map(|(host, _)| host)
    }
    
    /// 获取端口
    pub fn port(&self) -> Result<u16, crate::error::TakeoverError> {
        self.parse_host_port().map(|(_, port)| port)
    }
    
    /// 获取认证信息作为元组
    pub fn auth_tuple(&self) -> Option<(String, String)> {
        self.auth.as_ref().map(|auth| (auth.username.clone(), auth.password.clone()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProxyType {
    Socks5,
    Socks4,
    Http,
    Https,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyAuth {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetProgram {
    pub name: String,
    pub path_patterns: Vec<String>,
    pub proxy_config: ProxyConfig,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub targets: HashMap<String, TargetProgram>,
    pub global_settings: GlobalSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalSettings {
    pub log_level: String,
    pub bind_interface: Option<String>,
    pub dns_servers: Vec<String>,
    pub enable_ipv6: bool,
    pub connection_timeout: u64,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            log_level: "info".to_string(),
            bind_interface: None,
            dns_servers: vec!["8.8.8.8".to_string(), "8.8.4.4".to_string()],
            enable_ipv6: false,
            connection_timeout: 30,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            targets: HashMap::new(),
            global_settings: GlobalSettings::default(),
        }
    }
}

impl Config {
    pub fn load_or_create<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        
        if path.exists() {
            info!("Loading configuration from: {:?}", path);
            let content = fs::read_to_string(path)?;
            let config: Config = serde_json::from_str(&content)?;
            Ok(config)
        } else {
            warn!("Configuration file not found, creating default config at: {:?}", path);
            let config = Config::default();
            config.save(path)?;
            Ok(config)
        }
    }
    
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, content)?;
        Ok(())
    }
    
    pub fn add_target_program(&mut self, name: String, proxy_url: String) -> Result<()> {
        let proxy_type = self.parse_proxy_type(&proxy_url)?;
        
        let proxy_config = ProxyConfig {
            url: proxy_url,
            proxy_type,
            auth: None,
        };
        
        let target = TargetProgram {
            name: name.clone(),
            path_patterns: vec![
                format!("**/{}*", name),
                format!("**/{}", name),
                format!("{}", name),
            ],
            proxy_config,
            enabled: true,
        };
        
        self.targets.insert(name, target);
        Ok(())
    }
    
    fn parse_proxy_type(&self, proxy_url: &str) -> Result<ProxyType> {
        if proxy_url.starts_with("socks5://") {
            Ok(ProxyType::Socks5)
        } else if proxy_url.starts_with("socks4://") {
            Ok(ProxyType::Socks4)
        } else if proxy_url.starts_with("https://") {
            Ok(ProxyType::Https)
        } else if proxy_url.starts_with("http://") {
            Ok(ProxyType::Http)
        } else {
            Err(TakeoverError::Config(format!("Unsupported proxy URL format: {}", proxy_url)).into())
        }
    }
    
    pub fn get_target_for_path(&self, executable_path: &str) -> Option<&TargetProgram> {
        for target in self.targets.values() {
            if !target.enabled {
                continue;
            }
            
            for pattern in &target.path_patterns {
                if self.matches_pattern(executable_path, pattern) {
                    return Some(target);
                }
            }
        }
        None
    }
    
    fn matches_pattern(&self, path: &str, pattern: &str) -> bool {
        // Simple pattern matching - can be enhanced with regex
        if pattern.contains("**") {
            // Recursive wildcard matching
            let pattern = pattern.replace("**", "");
            let pattern = pattern.trim_matches('/');
            path.contains(pattern)
        } else if pattern.contains('*') {
            // Simple wildcard matching
            let regex_pattern = pattern.replace('*', ".*");
            regex::Regex::new(&regex_pattern)
                .map(|re| re.is_match(path))
                .unwrap_or(false)
        } else {
            path.ends_with(pattern)
        }
    }
}
