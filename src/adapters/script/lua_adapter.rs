//! Lua脚本适配器
//! 
//! 实现ScriptPort接口，提供Lua脚本引擎集成：
//! - Lua运行时管理和连接池
//! - 脚本加载、验证和执行
//! - 沙箱安全机制
//! - API函数暴露给Lua脚本
//! - 异步执行支持

use std::sync::Arc;
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};
use std::path::{Path, PathBuf};
use tokio::sync::{RwLock, Mutex, Semaphore};
use tokio::fs;
use tracing::{info, warn, error, debug, instrument};
use async_trait::async_trait;

use mlua::{Lua, LuaSerdeExt, Result as LuaResult, Error as LuaError, Value as LuaValue, Table as LuaTable};

use crate::core::ports::{ScriptPort, ApiPort, AppError, ScriptInput, ScriptOutput, ScriptAction, ScriptInfo, TriggerPoint, TriggerCondition};
use crate::core::model::ConnectionContext;
use crate::core::model::{HttpRequest, HttpResponse, CompositeRequestConfig, MergeStrategy, LogLevel, CustomEvent};

/// Lua脚本适配器
/// 
/// 负责管理Lua脚本的执行环境和生命周期
/// 注意：不存储Lua实例以避免线程安全问题，每次执行时创建新实例
pub struct LuaScriptAdapter {
    /// 并发控制信号量
    execution_semaphore: Arc<Semaphore>,
    /// 已加载的脚本
    scripts: Arc<RwLock<HashMap<String, LoadedScript>>>,
    /// 脚本触发器
    triggers: Arc<RwLock<HashMap<String, ScriptTrigger>>>,
    /// API端口引用
    api_port: Arc<dyn ApiPort>,
    /// 配置
    config: LuaConfig,
    /// 执行统计
    stats: Arc<RwLock<LuaStats>>,
}

/// Lua配置
#[derive(Debug, Clone)]
pub struct LuaConfig {
    /// 脚本目录
    pub script_dir: PathBuf,
    /// 最大并发执行数
    pub max_concurrent_executions: usize,
    /// 单个脚本最大执行时间
    pub max_execution_time: Duration,
    /// 最大内存使用（字节）
    pub max_memory_usage: usize,
    /// Lua实例池大小
    pub pool_size: usize,
    /// 是否启用沙箱
    pub sandbox_enabled: bool,
    /// 允许的标准库模块
    pub allowed_stdlib_modules: Vec<String>,
    /// 自动重载脚本
    pub auto_reload: bool,
}

/// 已加载的脚本
#[derive(Debug, Clone)]
struct LoadedScript {
    /// 脚本名称
    name: String,
    /// 脚本内容
    content: String,
    /// 文件路径
    file_path: Option<PathBuf>,
    /// 加载时间
    loaded_at: SystemTime,
    /// 文件修改时间
    file_modified: Option<SystemTime>,
    /// 执行次数
    execution_count: usize,
    /// 成功次数
    success_count: usize,
    /// 平均执行时间
    avg_execution_time: Option<Duration>,
    /// 是否启用
    enabled: bool,
}

/// 脚本触发器
#[derive(Debug, Clone)]
pub struct ScriptTrigger {
    /// 脚本名称
    pub script_name: String,
    /// 触发点
    pub trigger_point: TriggerPoint,
    /// 触发条件
    pub conditions: Vec<TriggerCondition>,
    /// 是否启用
    pub enabled: bool,
    /// 优先级
    pub priority: i32,
}

/// Lua执行统计
#[derive(Debug, Default, Clone)]
struct LuaStats {
    /// 总执行次数
    total_executions: u64,
    /// 成功执行次数
    successful_executions: u64,
    /// 失败执行次数
    failed_executions: u64,
    /// 超时次数
    timeout_executions: u64,
    /// 当前活跃执行数
    active_executions: usize,
    /// 平均执行时间
    avg_execution_time: Option<Duration>,
    /// 脚本加载次数
    scripts_loaded: u64,
    /// 脚本重新加载次数
    scripts_reloaded: u64,
}

/// Lua执行上下文
struct LuaExecutionContext {
    /// 连接上下文
    connection_context: ConnectionContext,
    /// 脚本输入
    script_input: ScriptInput,
    /// 开始时间
    start_time: Instant,
    /// API端口引用
    api_port: Arc<dyn ApiPort>,
}

impl LuaScriptAdapter {
    /// 创建新的Lua脚本适配器
    pub async fn new(api_port: Arc<dyn ApiPort>, config: LuaConfig) -> Result<Self, AppError> {
        info!("🚀 Initializing Lua script adapter");
        
        // 确保脚本目录存在
        if !config.script_dir.exists() {
            fs::create_dir_all(&config.script_dir)
                .await
                .map_err(|e| AppError::Config(format!("Failed to create script directory: {}", e)))?;
        }
        
        let adapter = Self {
            execution_semaphore: Arc::new(Semaphore::new(config.max_concurrent_executions)),
            scripts: Arc::new(RwLock::new(HashMap::new())),
            triggers: Arc::new(RwLock::new(HashMap::new())),
            api_port,
            config,
            stats: Arc::new(RwLock::new(LuaStats::default())),
        };
        
        // 加载脚本目录中的所有脚本
        adapter.load_scripts_from_directory().await?;
        
        info!("✅ Lua script adapter initialized successfully");
        Ok(adapter)
    }
    
    /// 在阻塞线程中执行Lua脚本（静态方法，避免 self 引用）
    fn execute_lua_in_blocking_thread(
        script_content: String,
        input: ScriptInput,
        sandbox_enabled: bool,
        _timeout: Duration,
    ) -> Result<ScriptOutput, AppError> {
        // 创建新的Lua实例
        let lua = Lua::new();
        
        // 基本的沙箱设置
        if sandbox_enabled {
            lua.load(r#"
                os = nil
                io = nil
                debug = nil
                require = nil
                dofile = nil
                loadfile = nil
            "#).exec().map_err(|e| AppError::Script(format!("Failed to setup sandbox: {}", e)))?;
        }
        
        // 将输入数据注册到Lua环境
        let globals = lua.globals();
        
        // 创建一个简化的context表
        let context_table = lua.create_table()?;
        if let Some(ref request) = input.request {
            context_table.set("method", request.method.clone())?;
            context_table.set("url", request.url.clone())?;
        }
        globals.set("context", context_table)?;
        
        // 执行脚本
        let result: LuaValue = lua.load(&script_content).eval()
            .map_err(|e| AppError::Script(format!("Script execution failed: {}", e)))?;
        
        // 处理结果
        Ok(ScriptOutput {
            action: crate::core::ports::ScriptAction::Continue,
            modified_request: None,
            modified_response: None,
            additional_requests: vec![],
            custom_data: {
                let mut map = std::collections::HashMap::new();
                map.insert("result".to_string(), serde_json::json!(format!("{:?}", result)));
                map
            },
        })
    }
    

    
    /// 设置Lua沙箱
    async fn setup_sandbox(&self, lua: &Lua) -> Result<(), AppError> {
        // 移除危险的全局函数
        let dangerous_globals = vec![
            "dofile", "loadfile", "load", "loadstring",
            "require", "package", "io", "os", "debug"
        ];
        
        let globals = lua.globals();
        for global in dangerous_globals {
            globals.set(global, LuaValue::Nil)
                .map_err(|e| AppError::Config(format!("Failed to remove global '{}': {}", global, e)))?;
        }
        
        // 只保留允许的标准库模块
        for module in &self.config.allowed_stdlib_modules {
            match module.as_str() {
                "string" => {
                    // string模块通常是安全的，保持启用
                }
                "math" => {
                    // math模块通常是安全的，保持启用
                }
                "table" => {
                    // table模块通常是安全的，保持启用
                }
                "json" => {
                    // 我们将提供自定义的JSON模块
                    self.register_json_module(lua).await?;
                }
                _ => {
                    warn!("Unknown stdlib module requested: {}", module);
                }
            }
        }
        
        debug!("✅ Lua sandbox configured");
        Ok(())
    }
    
    /// 注册JSON模块
    async fn register_json_module(&self, lua: &Lua) -> Result<(), AppError> {
        let json_table = lua.create_table()
            .map_err(|e| AppError::Config(format!("Failed to create JSON table: {}", e)))?;
        
        // JSON encode函数
        let encode_fn = lua.create_function(|lua, value: LuaValue| {
            lua.to_value(&serde_json::to_string(&value).map_err(LuaError::external)?)
        }).map_err(|e| AppError::Config(format!("Failed to create JSON encode function: {}", e)))?;
        
        // JSON decode函数
        let decode_fn = lua.create_function(|lua, json_str: String| {
            let value: serde_json::Value = serde_json::from_str(&json_str).map_err(LuaError::external)?;
            lua.to_value(&value)
        }).map_err(|e| AppError::Config(format!("Failed to create JSON decode function: {}", e)))?;
        
        json_table.set("encode", encode_fn)
            .map_err(|e| AppError::Config(format!("Failed to set JSON encode: {}", e)))?;
        json_table.set("decode", decode_fn)
            .map_err(|e| AppError::Config(format!("Failed to set JSON decode: {}", e)))?;
        
        lua.globals().set("json", json_table)
            .map_err(|e| AppError::Config(format!("Failed to register JSON module: {}", e)))?;
        
        Ok(())
    }
    
    /// 注册API函数给Lua脚本
    fn register_api_functions(&self, lua: &Lua) -> Result<(), AppError> {
        let api_port = Arc::clone(&self.api_port);
        
        // 创建takeover API表
        let api_table = lua.create_table()
            .map_err(|e| AppError::Config(format!("Failed to create API table: {}", e)))?;
        
        // HTTP请求函数
        let http_request_fn = {
            let api_port = Arc::clone(&api_port);
            lua.create_async_function(move |_lua, request_table: LuaTable| {
                let api_port = Arc::clone(&api_port);
                async move {
                    // 将Lua表转换为HttpRequest
                    let method: String = request_table.get("method").unwrap_or_else(|_| "GET".to_string());
                    let url: String = request_table.get("url").map_err(|e| LuaError::external(format!("Missing URL: {}", e)))?;
                    let headers: Option<HashMap<String, String>> = request_table.get("headers").ok();
                    let body: Option<Vec<u8>> = request_table.get("body").ok();
                    
                    let mut http_request = HttpRequest::new(&method, &url);
                    if let Some(headers) = headers {
                        http_request.headers = headers;
                    }
                    if let Some(body) = body {
                        http_request.body = body;
                    }
                    
                    // 执行HTTP请求
                    match api_port.http_request(http_request).await {
                        Ok(response) => {
                            // 将响应转换为Lua值
                            Ok(LuaValue::Nil) // 简化实现，实际应该返回响应数据
                        }
                        Err(e) => Err(LuaError::external(format!("HTTP request failed: {}", e)))
                    }
                }
            }).map_err(|e| AppError::Config(format!("Failed to create HTTP request function: {}", e)))?
        };
        
        // 日志函数
        let log_fn = {
            let api_port = Arc::clone(&api_port);
            lua.create_async_function(move |_lua, (level, message): (String, String)| {
                let api_port = Arc::clone(&api_port);
                async move {
                    let log_level = match level.to_lowercase().as_str() {
                        "debug" => LogLevel::Debug,
                        "info" => LogLevel::Info,
                        "warn" => LogLevel::Warn,
                        "error" => LogLevel::Error,
                        _ => LogLevel::Info,
                    };
                    
                    let ports_log_level = match log_level {
                        LogLevel::Debug => crate::core::ports::LogLevel::Debug,
                        LogLevel::Info => crate::core::ports::LogLevel::Info,
                        LogLevel::Warn => crate::core::ports::LogLevel::Warn,
                        LogLevel::Error => crate::core::ports::LogLevel::Error,
                    };
                    match api_port.log(ports_log_level, &message).await {
                        Ok(_) => Ok(LuaValue::Nil),
                        Err(e) => Err(LuaError::external(format!("Log failed: {}", e)))
                    }
                }
            }).map_err(|e| AppError::Config(format!("Failed to create log function: {}", e)))?
        };
        
        // 获取配置函数
        let get_config_fn = {
            let api_port = Arc::clone(&api_port);
            lua.create_async_function(move |_lua, key: String| {
                let api_port = Arc::clone(&api_port);
                async move {
                    match api_port.get_config(&key).await {
                        Ok(Some(value)) => {
                            let lua_string = lua.create_string(&value)?;
                            Ok(LuaValue::String(lua_string))
                        },
                        Ok(None) => Ok(LuaValue::Nil),
                        Err(e) => Err(LuaError::external(format!("Get config failed: {}", e)))
                    }
                }
            }).map_err(|e| AppError::Config(format!("Failed to create get_config function: {}", e)))?
        };
        
        // 存储数据函数
        let store_data_fn = {
            let api_port = Arc::clone(&api_port);
            lua.create_async_function(move |lua, (key, value): (String, LuaValue)| {
                let api_port = Arc::clone(&api_port);
                async move {
                    let json_value = lua.from_value::<serde_json::Value>(value)
                        .map_err(|e| LuaError::external(format!("Value serialization failed: {}", e)))?;
                    
                    match api_port.store_data(&key, json_value).await {
                        Ok(_) => Ok(LuaValue::Nil),
                        Err(e) => Err(LuaError::external(format!("Store data failed: {}", e)))
                    }
                }
            }).map_err(|e| AppError::Config(format!("Failed to create store_data function: {}", e)))?
        };
        
        // 获取数据函数
        let get_data_fn = {
            let api_port = Arc::clone(&api_port);
            lua.create_async_function(move |lua, key: String| {
                let api_port = Arc::clone(&api_port);
                async move {
                    match api_port.get_data(&key).await {
                        Ok(Some(value)) => lua.to_value(&value).map_err(|e| LuaError::external(format!("Value deserialization failed: {}", e))),
                        Ok(None) => Ok(LuaValue::Nil),
                        Err(e) => Err(LuaError::external(format!("Get data failed: {}", e)))
                    }
                }
            }).map_err(|e| AppError::Config(format!("Failed to create get_data function: {}", e)))?
        };
        
        // 将所有函数添加到API表
        api_table.set("http_request", http_request_fn)
            .map_err(|e| AppError::Config(format!("Failed to set http_request: {}", e)))?;
        api_table.set("log", log_fn)
            .map_err(|e| AppError::Config(format!("Failed to set log: {}", e)))?;
        api_table.set("get_config", get_config_fn)
            .map_err(|e| AppError::Config(format!("Failed to set get_config: {}", e)))?;
        api_table.set("store_data", store_data_fn)
            .map_err(|e| AppError::Config(format!("Failed to set store_data: {}", e)))?;
        api_table.set("get_data", get_data_fn)
            .map_err(|e| AppError::Config(format!("Failed to set get_data: {}", e)))?;
        
        // 注册到全局
        lua.globals().set("takeover", api_table)
            .map_err(|e| AppError::Config(format!("Failed to register takeover API: {}", e)))?;
        
        debug!("✅ API functions registered successfully");
        Ok(())
    }
    
    /// 从目录加载所有脚本
    async fn load_scripts_from_directory(&self) -> Result<(), AppError> {
        let mut dir = fs::read_dir(&self.config.script_dir)
            .await
            .map_err(|e| AppError::Config(format!("Failed to read script directory: {}", e)))?;
        
        let mut loaded_count = 0;
        
        while let Some(entry) = dir.next_entry()
            .await
            .map_err(|e| AppError::Config(format!("Failed to read directory entry: {}", e)))? {
            
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("lua") {
                let script_name = path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                
                match self.load_script_from_file(&script_name, &path).await {
                    Ok(_) => {
                        loaded_count += 1;
                        debug!("✅ Loaded script: {}", script_name);
                    }
                    Err(e) => {
                        warn!("❌ Failed to load script {}: {}", script_name, e);
                    }
                }
            }
        }
        
        info!("✅ Loaded {} scripts from directory", loaded_count);
        Ok(())
    }
    
    /// 从文件加载脚本
    async fn load_script_from_file(&self, name: &str, path: &Path) -> Result<(), AppError> {
        let content = fs::read_to_string(path)
            .await
            .map_err(|e| AppError::Config(format!("Failed to read script file: {}", e)))?;
        
        let file_metadata = fs::metadata(path)
            .await
            .map_err(|e| AppError::Config(format!("Failed to get file metadata: {}", e)))?;
        
        let file_modified = file_metadata.modified().ok();
        
        // 验证脚本语法
        self.validate_script_syntax(&content).await?;
        
        let loaded_script = LoadedScript {
            name: name.to_string(),
            content,
            file_path: Some(path.to_path_buf()),
            loaded_at: SystemTime::now(),
            file_modified,
            execution_count: 0,
            success_count: 0,
            avg_execution_time: None,
            enabled: true,
        };
        
        let mut scripts = self.scripts.write().await;
        scripts.insert(name.to_string(), loaded_script);
        
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.scripts_loaded += 1;
        }
        
        Ok(())
    }
    
    /// 验证脚本语法
    async fn validate_script_syntax(&self, content: &str) -> Result<(), AppError> {
        let content = content.to_string();
        
        // 在阻塞线程中验证语法
        tokio::task::spawn_blocking(move || {
            let lua = mlua::Lua::new();
            match lua.load(&content).exec() {
                Ok(_) => Ok(()),
                Err(e) => Err(crate::error::TakeoverError::Script(format!("Script syntax error: {}", e)))
            }
        }).await
        .map_err(|e| crate::error::TakeoverError::Script(format!("Failed to validate script: {}", e)))?
    }
    

    
    /// 获取统计信息
    pub async fn get_stats(&self) -> LuaStats {
        self.stats.read().await.clone()
    }
}

impl Default for LuaConfig {
    fn default() -> Self {
        Self {
            script_dir: PathBuf::from("./scripts"),
            max_concurrent_executions: 10,
            max_execution_time: Duration::from_secs(30),
            max_memory_usage: 64 * 1024 * 1024, // 64MB
            pool_size: 5,
            sandbox_enabled: true,
            allowed_stdlib_modules: vec![
                "string".to_string(),
                "math".to_string(),
                "table".to_string(),
                "json".to_string(),
            ],
            auto_reload: false,
        }
    }
}

#[async_trait]
impl ScriptPort for LuaScriptAdapter {
    #[instrument(skip(self, context, input))]
    async fn execute_script(
        &self,
        script_name: &str,
        context: &mut ConnectionContext,
        input: ScriptInput,
    ) -> Result<ScriptOutput, AppError> {
        debug!("Executing Lua script: {}", script_name);
        
        // 获取执行许可
        let _permit = self.execution_semaphore
            .acquire()
            .await
            .map_err(|_| AppError::Config("Script execution semaphore closed".to_string()))?;
        
        // 更新活跃执行统计
        {
            let mut stats = self.stats.write().await;
            stats.total_executions += 1;
            stats.active_executions += 1;
        }
        
        let start_time = Instant::now();
        
        // 获取脚本
        let script_content = {
            let scripts = self.scripts.read().await;
            if let Some(script) = scripts.get(script_name) {
                if !script.enabled {
                    return Err(AppError::Config(format!("Script '{}' is disabled", script_name)));
                }
                script.content.clone()
            } else {
                return Err(AppError::Config(format!("Script '{}' not found", script_name)));
            }
        };
        
        // 复制配置以避免借用检查问题
        let sandbox_enabled = self.config.sandbox_enabled;
        let timeout = self.config.max_execution_time;
        
        // 在阻塞线程中执行Lua脚本，避免线程安全问题
        let result = tokio::task::spawn_blocking(move || {
            Self::execute_lua_in_blocking_thread(script_content, input, sandbox_enabled, timeout)
        }).await
        .map_err(|e| AppError::Script(format!("Failed to execute script in blocking thread: {}", e)))?;
        
        let execution_time = start_time.elapsed();
        
        // 更新统计信息
        {
            let mut stats = self.stats.write().await;
            stats.active_executions -= 1;
            
            match &result {
                Ok(_) => {
                    stats.successful_executions += 1;
                }
                Err(_) => {
                    stats.failed_executions += 1;
                }
            }
            
            // 更新平均执行时间
            stats.avg_execution_time = match stats.avg_execution_time {
                Some(avg) => Some((avg + execution_time) / 2),
                None => Some(execution_time),
            };
        }
        
        // 更新脚本统计
        {
            let mut scripts = self.scripts.write().await;
            if let Some(script) = scripts.get_mut(script_name) {
                script.execution_count += 1;
                if result.is_ok() {
                    script.success_count += 1;
                }
                script.avg_execution_time = match script.avg_execution_time {
                    Some(avg) => Some((avg + execution_time) / 2),
                    None => Some(execution_time),
                };
            }
        }
        
        match result {
            Ok(output) => {
                info!("✅ Script '{}' executed successfully in {:?}", script_name, execution_time);
                Ok(output)
            }
            Err(e) => {
                error!("❌ Script '{}' execution failed: {}", script_name, e);
                Err(e)
            }
            Err(_) => {
                error!("❌ Script '{}' execution timed out after {:?}", script_name, self.config.max_execution_time);
                Err(AppError::Config(format!("Script '{}' execution timed out", script_name)))
            }
        }
    }
    
    async fn load_script(&self, script_name: &str, script_content: &str) -> Result<(), AppError> {
        // 先验证脚本语法
        self.validate_script(script_content).await?;
        
        // 将脚本保存到存储中 (这里使用内存存储作为示例)
        let mut scripts = self.scripts.write().await;
        scripts.insert(script_name.to_string(), LoadedScript {
            name: script_name.to_string(),
            content: script_content.to_string(),
            loaded_at: SystemTime::now(),
            execution_count: 0,
            avg_execution_time: Some(Duration::from_millis(0)),
            enabled: true,
            file_modified: Some(SystemTime::now()),
            file_path: Some(std::path::PathBuf::from(script_name)),
            success_count: 0,
        });
        
        info!("Loaded script: {}", script_name);
        Ok(())
    }

    async fn list_scripts(&self) -> Result<Vec<ScriptInfo>, AppError> {
        let scripts = self.scripts.read().await;
        let mut script_infos = Vec::new();
        
        for (name, content) in scripts.iter() {
            script_infos.push(ScriptInfo {
                name: name.clone(),
                description: format!("Lua script: {}", name),
                version: "1.0.0".to_string(),
                size: content.content.len(),
                enabled: true,
                loaded_at: Some(content.loaded_at),
                last_executed: None,
                execution_count: content.execution_count,
            });
        }
        
        Ok(script_infos)
    }

    async fn validate_script(&self, script_content: &str) -> Result<(), AppError> {
        let content = script_content.to_string();
        tokio::task::spawn_blocking(move || {
            let lua = Lua::new();
            lua.load(&content).exec().map_err(|e| AppError::Script(format!("Script validation failed: {}", e)))
        }).await.map_err(|e| AppError::Config(format!("Task join error: {}", e)))?
    }

    async fn add_trigger(&self, trigger: crate::core::ports::ScriptTrigger) -> Result<(), AppError> {
        let mut triggers = self.triggers.write().await;
        let script_name = trigger.script_name.clone();
        triggers.insert(script_name, ScriptTrigger {
            script_name: trigger.script_name,
            trigger_point: trigger.trigger_point,
            conditions: trigger.conditions,
            priority: 100, // 默认优先级
            enabled: trigger.enabled,
        });
        Ok(())
    }

    async fn remove_trigger(&self, script_name: &str) -> Result<(), AppError> {
        let mut triggers = self.triggers.write().await;
        triggers.remove(script_name);
        Ok(())
    }
}

impl LuaScriptAdapter {
    async fn execute_script_with_timeout(
        &self,
        lua: &Lua,
        script_content: &str,
        context: &ConnectionContext,
        input: ScriptInput,
    ) -> Result<ScriptOutput, AppError> {
        // 设置上下文数据到Lua全局变量
        let globals = lua.globals();
        
        // 设置连接上下文
        globals.set("connection_id", context.id.to_string())
            .map_err(|e| AppError::Config(format!("Failed to set connection_id: {}", e)))?;
        globals.set("target_host", context.target_host.clone())
            .map_err(|e| AppError::Config(format!("Failed to set target_host: {}", e)))?;
        globals.set("target_port", context.target_port)
            .map_err(|e| AppError::Config(format!("Failed to set target_port: {}", e)))?;
        
        // 设置输入数据
        if let Some(request) = &input.request {
            let request_table = lua.create_table()
                .map_err(|e| AppError::Config(format!("Failed to create request table: {}", e)))?;
            
            request_table.set("method", request.method.clone())
                .map_err(|e| AppError::Config(format!("Failed to set request method: {}", e)))?;
            request_table.set("url", request.url.clone())
                .map_err(|e| AppError::Config(format!("Failed to set request URL: {}", e)))?;
            
            // 简化：只设置基本字段，实际实现可以更完整
            globals.set("request", request_table)
                .map_err(|e| AppError::Config(format!("Failed to set request: {}", e)))?;
        }
        
        // 执行脚本
        let result: LuaValue = lua.load(script_content)
            .call_async(())
            .await
            .map_err(|e| AppError::Config(format!("Script execution error: {}", e)))?;
        
        // 解析结果
        self.parse_script_result(lua, result).await
    }
    
    /// 解析脚本执行结果
    async fn parse_script_result(&self, _lua: &Lua, result: LuaValue<'_>) -> Result<ScriptOutput, AppError> {
        match result {
            LuaValue::Table(table) => {
                // 解析动作
                let action_str: String = table.get("action")
                    .unwrap_or_else(|_| "continue".to_string());
                
                let action = match action_str.to_string() {
                    "continue" => ScriptAction::Continue,
                    "pause" => {
                        let reason: String = table.get("reason").unwrap_or_else(|_| "Script paused".to_string());
                        ScriptAction::Pause(reason)
                    }
                    "reject" => {
                        let reason: String = table.get("reason").unwrap_or_else(|_| "Script rejected".to_string());
                        ScriptAction::Reject(reason)
                    }
                    "redirect" => {
                        let url: String = table.get("url")
                            .map_err(|_| AppError::Config("Redirect action requires 'url' field".to_string()))?;
                        ScriptAction::Redirect(url)
                    }
                    "composite_request" => {
                        // 解析复合请求配置
                        let requests_table: LuaTable = table.get("requests")
                            .map_err(|_| AppError::Config("Composite request action requires 'requests' field".to_string()))?;
                        
                        let requests = Vec::new(); // 简化实现
                        let merge_strategy = MergeStrategy::ReplaceBody; // 默认策略
                        
                        ScriptAction::CompositeRequest(crate::core::ports::CompositeRequestConfig {
                            name: "Lua Generated Request".to_string(),
                            requests,
                            merge_strategy: crate::core::ports::MergeStrategy::ReplaceBody,
                            timeout: Some(Duration::from_secs(30)),
                        })
                    }
                    _ => ScriptAction::Continue,
                };
                
                Ok(ScriptOutput {
                    action,
                    modified_request: None, // 简化实现
                    modified_response: None,
                    additional_requests: Vec::new(),
                    custom_data: HashMap::new(),
                })
            }
            LuaValue::String(action_str) => {
                // 简单的字符串返回值
                let action = match action_str.to_str() {
                    Ok("continue") => ScriptAction::Continue,
                    Ok("pause") => ScriptAction::Pause("Script returned pause".to_string()),
                    Ok("reject") => ScriptAction::Reject("Script returned reject".to_string()),
                    Ok(_) | Err(_) => ScriptAction::Continue,
                };
                
                Ok(ScriptOutput {
                    action,
                    modified_request: None,
                    modified_response: None,
                    additional_requests: Vec::new(),
                    custom_data: HashMap::new(),
                })
            }
            LuaValue::Nil => {
                // 没有返回值，默认继续
                Ok(ScriptOutput {
                    action: ScriptAction::Continue,
                    modified_request: None,
                    modified_response: None,
                    additional_requests: Vec::new(),
                    custom_data: HashMap::new(),
                })
            }
            _ => {
                Err(AppError::Config("Invalid script return value".to_string()))
            }
        }
    }
    
    async fn load_script(&self, script_name: &str, script_content: &str) -> Result<(), AppError> {
        debug!("Loading script: {}", script_name);
        
        // 验证脚本语法
        self.validate_script_syntax(script_content).await?;
        
        let loaded_script = LoadedScript {
            name: script_name.to_string(),
            content: script_content.to_string(),
            file_path: None,
            loaded_at: SystemTime::now(),
            file_modified: None,
            execution_count: 0,
            success_count: 0,
            avg_execution_time: None,
            enabled: true,
        };
        
        let mut scripts = self.scripts.write().await;
        let is_reload = scripts.contains_key(script_name);
        scripts.insert(script_name.to_string(), loaded_script);
        
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            if is_reload {
                stats.scripts_reloaded += 1;
            } else {
                stats.scripts_loaded += 1;
            }
        }
        
        info!("✅ Script '{}' loaded successfully", script_name);
        Ok(())
    }
    
    async fn list_scripts(&self) -> Result<Vec<ScriptInfo>, AppError> {
        let scripts = self.scripts.read().await;
        
        let script_infos: Vec<ScriptInfo> = scripts
            .values()
            .map(|script| ScriptInfo {
                name: script.name.clone(),
                description: "Lua script".to_string(),
                version: "1.0.0".to_string(),
                size: script.content.len(),
                enabled: script.enabled,
                loaded_at: Some(script.loaded_at),
                last_executed: None, // 可以扩展为实际的最后执行时间
                execution_count: script.execution_count,
            })
            .collect();
        
        Ok(script_infos)
    }
    
    async fn validate_script(&self, script_content: &str) -> Result<(), AppError> {
        self.validate_script_syntax(script_content).await
    }
    
    async fn add_trigger(&self, trigger: crate::core::ports::ScriptTrigger) -> Result<(), AppError> {
        let mut triggers = self.triggers.write().await;
        triggers.insert(trigger.script_name.clone(), ScriptTrigger {
            script_name: trigger.script_name,
            trigger_point: trigger.trigger_point,
            conditions: trigger.conditions,
            enabled: trigger.enabled,
            priority: 0, // 可以扩展
        });
        
        Ok(())
    }
    
    async fn remove_trigger(&self, script_name: &str) -> Result<(), AppError> {
        let mut triggers = self.triggers.write().await;
        triggers.remove(script_name);
        Ok(())
    }
}
