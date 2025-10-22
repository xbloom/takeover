//! 进程信息查询中间件
//! 
//! 轻量级中间件，负责调用进程查询组件并将结果存储到连接上下文中
//! 
//! 实际的业务逻辑位于 `crate::component::process_finder` 模块中

use tracing::{warn, debug};
use async_trait::async_trait;

use crate::core::engine::{Middleware, MiddlewareResult};
use crate::core::model::{ConnectionContext, ProcessingState};
use crate::components::{ProcessFinder, ProcessFinderConfig};
use crate::error::TakeoverError;

/// 进程信息查询中间件配置
#[derive(Debug, Clone)]
pub struct ProcessInfoMiddlewareConfig {
    /// 是否启用进程查询
    pub enabled: bool,
    /// 失败时是否继续处理
    pub continue_on_failure: bool,
    /// 进程查询组件配置
    pub finder_config: ProcessFinderConfig,
}

/// 进程信息查询中间件
/// 
/// 轻量级中间件，调用 ProcessFinder 组件查询进程信息
pub struct ProcessInfoMiddleware {
    /// 是否启用
    enabled: bool,
    /// 失败时是否继续处理
    continue_on_failure: bool,
    /// 进程查询组件
    finder: ProcessFinder,
}

impl ProcessInfoMiddleware {
    /// 创建新的进程信息查询中间件
    pub fn new() -> Self {
        Self {
            enabled: true,
            continue_on_failure: true,
            finder: ProcessFinder::new(),
        }
    }
    
    /// 使用配置创建中间件
    pub fn with_config(config: ProcessInfoMiddlewareConfig) -> Self {
        Self {
            enabled: config.enabled,
            continue_on_failure: config.continue_on_failure,
            finder: ProcessFinder::with_config(config.finder_config),
        }
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> crate::components::ProcessFinderStats {
        self.finder.get_stats().await
    }
}

impl Default for ProcessInfoMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for ProcessInfoMiddlewareConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            continue_on_failure: true,
            finder_config: ProcessFinderConfig::default(),
        }
    }
}

#[async_trait]
impl Middleware for ProcessInfoMiddleware {
    fn name(&self) -> &str {
        "process_info"
    }
    
    async fn process(
        &self,
        context: &mut ConnectionContext,
        _core: &crate::core::AppCore,
    ) -> Result<MiddlewareResult, TakeoverError> {
        if !self.enabled {
            debug!("进程信息查询中间件已禁用，跳过");
            return Ok(MiddlewareResult::Continue);
        }
        
        debug!("进程信息查询中间件开始处理连接: {}", context.id);
        
        // 更新处理状态
        context.set_processing_state(ProcessingState::ProcessInfoQuery);
        
        // 调用进程查询组件
        match self.finder.find_process(context.connection.source_addr).await {
            Ok(process_info) => {
                debug!("成功查询进程信息: {:?}", process_info);
                
                // 存储进程信息到上下文
                context.custom_data.insert(
                    "process_info".to_string(),
                    serde_json::to_value(&process_info)
                        .map_err(|e| TakeoverError::System(format!("Serialize process info failed: {}", e)))?
                );
                
                // 存储具体字段便于后续使用
                if let Some(pid) = process_info.pid {
                    context.custom_data.insert("process_pid".to_string(), serde_json::json!(pid));
                }
                if let Some(name) = &process_info.process_name {
                    context.custom_data.insert("process_name".to_string(), serde_json::json!(name));
                }
                if let Some(path) = &process_info.executable_path {
                    context.custom_data.insert("process_path".to_string(), serde_json::json!(path));
                }
                if let Some(uid) = process_info.user_id {
                    context.custom_data.insert("process_uid".to_string(), serde_json::json!(uid));
                }
                
                Ok(MiddlewareResult::Continue)
            }
            Err(e) => {
                warn!("进程信息查询失败: {}", e);
                
                if self.continue_on_failure {
                    debug!("配置为失败时继续处理，跳过进程信息查询");
                    Ok(MiddlewareResult::Continue)
                } else {
                    Err(e)
                }
            }
        }
    }
    
    fn priority(&self) -> i32 {
        8 // 在协议检测之后，TLS处理之前
    }
    
    fn enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_middleware_creation() {
        let middleware = ProcessInfoMiddleware::new();
        assert_eq!(middleware.name(), "process_info");
        assert!(middleware.enabled());
        assert_eq!(middleware.priority(), 8);
        
        let stats = middleware.get_stats().await;
        assert_eq!(stats.total_queries, 0);
        
        println!("✅ 进程信息查询中间件创建成功");
    }
    
    #[tokio::test]
    async fn test_middleware_config() {
        let config = ProcessInfoMiddlewareConfig {
            enabled: true,
            continue_on_failure: true,
            finder_config: ProcessFinderConfig::default(),
        };
        
        let middleware = ProcessInfoMiddleware::with_config(config);
        assert!(middleware.enabled());
        assert_eq!(middleware.priority(), 8);
        
        println!("✅ 进程信息查询中间件配置测试通过");
    }
    
    #[test]
    fn test_config_defaults() {
        let config = ProcessInfoMiddlewareConfig::default();
        assert!(config.enabled);
        assert!(config.continue_on_failure);
        assert!(config.finder_config.enable_cache);
        assert_eq!(config.finder_config.cache_ttl, 300); // 5分钟
        assert_eq!(config.finder_config.query_timeout, std::time::Duration::from_secs(5));
        assert!(config.finder_config.full_path);
        
        println!("✅ 配置默认值测试通过");
    }
    
    #[tokio::test]
    async fn test_stats_tracking() {
        let middleware = ProcessInfoMiddleware::new();
        
        let initial_stats = middleware.get_stats().await;
        assert_eq!(initial_stats.total_queries, 0);
        assert_eq!(initial_stats.successful_queries, 0);
        assert_eq!(initial_stats.cache_hits, 0);
        assert_eq!(initial_stats.query_failures, 0);
        assert_eq!(initial_stats.syscall_failures, 0);
        
        println!("✅ 统计跟踪测试通过");
    }
}