
use std::net::SocketAddr;
use crate::config::Config;
use tracing::debug;

/// 路由决策引擎 - 核心的智能过滤逻辑
#[derive(Debug, Clone)]
pub struct RoutingEngine {
    rules: RoutingRules,
}

#[derive(Debug, Clone)]
pub struct RoutingRules {
    /// 需要代理的进程名列表（支持通配符）
    pub target_processes: Vec<String>,
    /// 需要代理的进程路径模式（支持通配符）
    pub target_process_paths: Vec<String>,
    /// 需要代理的域名模式（支持通配符）
    pub target_domains: Vec<String>,
    /// 需要代理的URL模式（支持通配符）
    pub target_url_patterns: Vec<String>,
    /// 需要代理的端口列表  
    pub target_ports: Vec<u16>,
    /// 需要代理的IP范围
    pub target_ip_ranges: Vec<String>,
    /// URL重写规则
    pub url_rewrites: Vec<UrlRewriteRule>,
}

/// URL重写规则
#[derive(Debug, Clone)]
pub struct UrlRewriteRule {
    /// 匹配模式（支持通配符）
    pub pattern: String,
    /// 替换模板（支持$1, $2等占位符）
    pub replacement: String,
    /// 是否只替换第一个匹配
    pub replace_first_only: bool,
}

#[derive(Debug)]
pub enum RoutingDecision {
    /// 通过SOCKS5代理
    Proxy {
        /// 可能重写后的URL
        rewritten_url: Option<String>,
    },
    /// 直接连接
    Direct {
        /// 可能重写后的URL
        rewritten_url: Option<String>,
    },
}

impl RoutingEngine {
    pub fn new(config: &Config) -> Self {
        let rules = RoutingRules::from_config(config);
        Self { rules }
    }

    /// 核心决策方法：是否需要代理这个连接
    pub fn should_proxy(
        &self,
        target_host: &str,
        target_port: u16,
        request_url: Option<&str>,
        process_path: Option<&str>,
        _client_addr: &SocketAddr,
    ) -> RoutingDecision {
        debug!("Evaluating routing for {}:{}", target_host, target_port);
        
        let full_url = request_url.map(|s| s.to_string())
            .unwrap_or_else(|| format!("http://{}:{}", target_host, target_port));
        let mut should_proxy = false;
        let mut rewritten_url = None;

        // 1. 检查URL重写规则（先执行重写）
        for rewrite_rule in &self.rules.url_rewrites {
            if let Some(new_url) = self.apply_url_rewrite(&full_url, rewrite_rule) {
                debug!("URL rewritten: {} -> {}", full_url, new_url);
                rewritten_url = Some(new_url);
                break; // 应用第一个匹配的重写规则
            }
        }

        // 2. 检查端口匹配
        if self.rules.target_ports.contains(&target_port) {
            debug!("Port {} matches proxy rules", target_port);
            should_proxy = true;
        }

        // 3. 检查域名匹配（支持通配符）
        if !should_proxy {
            for domain_pattern in &self.rules.target_domains {
                if self.wildcard_match(target_host, domain_pattern) {
                    debug!("Host {} matches domain pattern {}", target_host, domain_pattern);
                    should_proxy = true;
                    break;
                }
            }
        }

        // 4. 检查URL模式匹配（支持通配符）
        if !should_proxy {
            for url_pattern in &self.rules.target_url_patterns {
                if self.wildcard_match(&full_url, url_pattern) {
                    debug!("URL {} matches pattern {}", full_url, url_pattern);
                    should_proxy = true;
                    break;
                }
            }
        }

        // 5. 检查进程路径匹配（如果提供了进程路径）
        if !should_proxy {
            if let Some(proc_path) = process_path {
                for path_pattern in &self.rules.target_process_paths {
                    if self.wildcard_match(proc_path, path_pattern) {
                        debug!("Process path {} matches pattern {}", proc_path, path_pattern);
                        should_proxy = true;
                        break;
                    }
                }
            }
        }

        // 6. 检查IP范围匹配
        if !should_proxy {
            for ip_range in &self.rules.target_ip_ranges {
                if target_host.starts_with(ip_range) {
                    debug!("Host {} matches IP range {}", target_host, ip_range);
                    should_proxy = true;
                    break;
                }
            }
        }

        // 返回决策结果
        if should_proxy {
            RoutingDecision::Proxy { rewritten_url }
        } else {
            debug!("No proxy rules matched for {}:{}, using direct connection", target_host, target_port);
            RoutingDecision::Direct { rewritten_url }
        }
    }

    /// 通配符匹配逻辑（支持 * 和 ? 通配符）
    fn wildcard_match(&self, text: &str, pattern: &str) -> bool {
        self.wildcard_match_recursive(text, pattern)
    }

    /// 递归实现通配符匹配
    fn wildcard_match_recursive(&self, text: &str, pattern: &str) -> bool {
        if pattern.is_empty() {
            return text.is_empty();
        }

        if pattern.starts_with('*') {
            // * 匹配0个或多个任意字符
            let rest_pattern = &pattern[1..];
            
            // 尝试匹配0个字符
            if self.wildcard_match_recursive(text, rest_pattern) {
                return true;
            }
            
            // 尝试匹配1个或多个字符
            for i in 1..=text.len() {
                if self.wildcard_match_recursive(&text[i..], rest_pattern) {
                    return true;
                }
            }
            
            false
        } else if pattern.starts_with('?') {
            // ? 匹配1个任意字符
            if text.is_empty() {
                false
            } else {
                self.wildcard_match_recursive(&text[1..], &pattern[1..])
            }
        } else {
            // 普通字符匹配
            if text.is_empty() || text.chars().next() != pattern.chars().next() {
                false
            } else {
                let text_rest = &text[text.chars().next().unwrap().len_utf8()..];
                let pattern_rest = &pattern[pattern.chars().next().unwrap().len_utf8()..];
                self.wildcard_match_recursive(text_rest, pattern_rest)
            }
        }
    }

    /// 应用URL重写规则
    fn apply_url_rewrite(&self, url: &str, rule: &UrlRewriteRule) -> Option<String> {
        if self.wildcard_match(url, &rule.pattern) {
            // 简化版重写：支持基本的字符串替换
            // 未来可以扩展支持正则表达式和捕获组
            Some(self.simple_url_replace(url, &rule.pattern, &rule.replacement))
        } else {
            None
        }
    }

    /// 简单的URL替换（支持基本通配符到文本的转换）
    fn simple_url_replace(&self, url: &str, pattern: &str, replacement: &str) -> String {
        // 这是一个简化版本，支持 * 通配符的捕获和替换
        if pattern.contains('*') {
            // 找到 * 的位置
            if let Some(star_pos) = pattern.find('*') {
                let prefix = &pattern[..star_pos];
                let suffix = &pattern[star_pos + 1..];
                
                if url.starts_with(prefix) && url.ends_with(suffix) {
                    let captured = &url[prefix.len()..url.len() - suffix.len()];
                    // 替换 $1 为捕获的内容
                    return replacement.replace("$1", captured);
                }
            }
        }
        
        // 如果没有通配符，直接返回replacement
        replacement.to_string()
    }

    /// 更新路由规则
    pub fn update_rules(&mut self, rules: RoutingRules) {
        debug!("Updating routing rules: {:?}", rules);
        self.rules = rules;
    }

    /// 获取当前规则统计
    pub fn get_stats(&self) -> RoutingStats {
        RoutingStats {
            total_processes: self.rules.target_processes.len(),
            total_process_paths: self.rules.target_process_paths.len(),
            total_domains: self.rules.target_domains.len(),
            total_url_patterns: self.rules.target_url_patterns.len(),
            total_ports: self.rules.target_ports.len(),
            total_ip_ranges: self.rules.target_ip_ranges.len(),
            total_url_rewrites: self.rules.url_rewrites.len(),
        }
    }
}

impl RoutingRules {
    pub fn from_config(config: &Config) -> Self {
        let mut rules = Self {
            target_processes: Vec::new(),
            target_process_paths: Vec::new(),
            target_domains: Vec::new(),
            target_url_patterns: Vec::new(),
            target_ports: Vec::new(),
            target_ip_ranges: Vec::new(),
            url_rewrites: Vec::new(),
        };

        // 从配置中提取规则
        for (process_name, _proxy_config) in &config.targets {
            rules.target_processes.push(process_name.clone());
            
            // 可以从proxy_config中提取更多规则
            // 例如特定的域名或端口
        }

        // 添加一些默认规则
        rules.target_domains.extend(vec![
            "*.google.com".to_string(),
            "*.github.com".to_string(),
            "*.openai.com".to_string(),
            "httpbin.org".to_string(), // 走代理（应显示代理IP）
            // ipinfo.io 不在列表中，直连（应显示真实IP）
        ]);

        // 添加一些默认的进程路径模式
        rules.target_process_paths.extend(vec![
            "/usr/bin/curl".to_string(),
            "/usr/local/bin/node".to_string(),
            "*/node_modules/.bin/*".to_string(),
        ]);

        // 添加一些默认的URL模式（支持HTTPS）
        rules.target_url_patterns.extend(vec![
            "https://api.openai.com/*".to_string(),
            "https://github.com/*/releases/*".to_string(),
            "https://registry.npmjs.org/*".to_string(),
            "https://pypi.org/*".to_string(),
            "https://*.googleapis.com/*".to_string(),
        ]);

        // 添加一些示例URL重写规则
        rules.url_rewrites.extend(vec![
            UrlRewriteRule {
                pattern: "https://github.com/*".to_string(),
                replacement: "https://mirror.github.com/$1".to_string(),
                replace_first_only: true,
            },
            // UrlRewriteRule {
            //     pattern: "http://*".to_string(),
            //     replacement: "https://$1".to_string(),
            //     replace_first_only: false,
            // },
        ]);

        rules
    }

    /// 添加进程规则
    pub fn add_process(&mut self, process_name: String) {
        if !self.target_processes.contains(&process_name) {
            self.target_processes.push(process_name);
        }
    }

    /// 添加域名规则
    pub fn add_domain(&mut self, domain: String) {
        if !self.target_domains.contains(&domain) {
            self.target_domains.push(domain);
        }
    }

    /// 添加端口规则
    pub fn add_port(&mut self, port: u16) {
        if !self.target_ports.contains(&port) {
            self.target_ports.push(port);
        }
    }

    /// 添加进程路径规则
    pub fn add_process_path(&mut self, path: String) {
        if !self.target_process_paths.contains(&path) {
            self.target_process_paths.push(path);
        }
    }

    /// 添加URL模式规则
    pub fn add_url_pattern(&mut self, pattern: String) {
        if !self.target_url_patterns.contains(&pattern) {
            self.target_url_patterns.push(pattern);
        }
    }

    /// 添加URL重写规则
    pub fn add_url_rewrite(&mut self, rule: UrlRewriteRule) {
        self.url_rewrites.push(rule);
    }
}

#[derive(Debug)]
pub struct RoutingStats {
    pub total_processes: usize,
    pub total_process_paths: usize,
    pub total_domains: usize,
    pub total_url_patterns: usize,
    pub total_ports: usize,
    pub total_ip_ranges: usize,
    pub total_url_rewrites: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_wildcard_matching() {
        let engine = RoutingEngine {
            rules: RoutingRules {
                target_processes: vec![],
                target_process_paths: vec![],
                target_domains: vec!["*.google.com".to_string(), "github.com".to_string()],
                target_url_patterns: vec![],
                target_ports: vec![],
                target_ip_ranges: vec![],
                url_rewrites: vec![],
            },
        };

        let client_addr = SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), 12345);

        // 测试通配符匹配
        assert!(matches!(
            engine.should_proxy("www.google.com", 80, None, None, &client_addr),
            RoutingDecision::Proxy { .. }
        ));
        
        assert!(matches!(
            engine.should_proxy("api.google.com", 80, None, None, &client_addr),
            RoutingDecision::Proxy { .. }
        ));

        // 测试精确匹配
        assert!(matches!(
            engine.should_proxy("github.com", 443, None, None, &client_addr),
            RoutingDecision::Proxy { .. }
        ));

        // 测试不匹配
        assert!(matches!(
            engine.should_proxy("example.com", 80, None, None, &client_addr),
            RoutingDecision::Direct { .. }
        ));
    }

    #[test]
    fn test_url_pattern_matching() {
        let engine = RoutingEngine {
            rules: RoutingRules {
                target_processes: vec![],
                target_process_paths: vec![],
                target_domains: vec![],
                target_url_patterns: vec!["https://api.openai.com/*".to_string()],
                target_ports: vec![],
                target_ip_ranges: vec![],
                url_rewrites: vec![],
            },
        };

        let client_addr = SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), 12345);

        // 测试URL模式匹配
        assert!(matches!(
            engine.should_proxy("api.openai.com", 443, Some("https://api.openai.com/v1/chat/completions"), None, &client_addr),
            RoutingDecision::Proxy { .. }
        ));

        // 测试不匹配
        assert!(matches!(
            engine.should_proxy("example.com", 80, Some("https://example.com/api"), None, &client_addr),
            RoutingDecision::Direct { .. }
        ));
    }

    #[test]
    fn test_url_rewrite() {
        let engine = RoutingEngine {
            rules: RoutingRules {
                target_processes: vec![],
                target_process_paths: vec![],
                target_domains: vec![],
                target_url_patterns: vec![],
                target_ports: vec![],
                target_ip_ranges: vec![],
                url_rewrites: vec![
                    UrlRewriteRule {
                        pattern: "https://github.com/*".to_string(),
                        replacement: "https://mirror.github.com/$1".to_string(),
                        replace_first_only: true,
                    }
                ],
            },
        };

        let client_addr = SocketAddr::new(Ipv4Addr::new(127, 0, 0, 1).into(), 12345);

        // 测试URL重写
        match engine.should_proxy("github.com", 443, Some("https://github.com/user/repo"), None, &client_addr) {
            RoutingDecision::Direct { rewritten_url: Some(rewritten) } => {
                assert_eq!(rewritten, "https://mirror.github.com/user/repo");
            }
            _ => panic!("Expected Direct with rewritten URL"),
        }
    }

    #[test]
    fn test_wildcard_basic() {
        let engine = RoutingEngine {
            rules: RoutingRules {
                target_processes: vec![],
                target_process_paths: vec![],
                target_domains: vec![],
                target_url_patterns: vec![],
                target_ports: vec![],
                target_ip_ranges: vec![],
                url_rewrites: vec![],
            },
        };

        // 测试基本通配符
        assert!(engine.wildcard_match("hello world", "hello*"));
        assert!(engine.wildcard_match("hello", "hello*"));
        assert!(engine.wildcard_match("hello.txt", "*.txt"));
        assert!(engine.wildcard_match("test", "t?st"));
        assert!(!engine.wildcard_match("test", "t?s"));
        assert!(engine.wildcard_match("abc", "*"));
        assert!(engine.wildcard_match("", "*"));
    }
}
