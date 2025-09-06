//! Level 1: 配置层集成测试
//! 
//! 测试应用配置系统的各个方面：
//! - AppConfig及其子配置的创建和验证
//! - 配置的序列化和反序列化
//! - 默认配置的合理性
//! - 配置验证和错误处理
//! - 配置的动态更新

use std::time::Duration;
use std::path::PathBuf;
use anyhow::Result;
use tempfile::TempDir;

use takeover::{
    AppConfig, InboundConfig, OutboundConfig, ScriptConfig, GeneralConfig,
    LogLevel
};

/// 测试默认应用配置的创建和合理性
#[tokio::test]
async fn test_default_app_config() -> Result<()> {
    println!("🧪 测试默认应用配置创建");
    
    let config = AppConfig::default();
    
    // 验证基本属性
    assert_eq!(config.app_name, "takeover");
    assert_eq!(config.version, "0.1.0");
    
    // 验证入站配置
    assert_eq!(config.inbound.listen_address, "127.0.0.1");
    assert!(config.inbound.listen_port > 0);
    assert!(config.inbound.max_connections > 0);
    assert!(config.inbound.buffer_size > 0);
    assert!(config.inbound.timeout > Duration::from_secs(0));
    
    // 验证出站配置
    assert!(config.outbound.timeout > Duration::from_secs(0));
    assert!(config.outbound.read_timeout > Duration::from_secs(0));
    assert!(config.outbound.write_timeout > Duration::from_secs(0));
    assert!(config.outbound.retry_count > 0);
    assert!(config.outbound.retry_delay > Duration::from_secs(0));
    
    // 验证脚本配置
    assert!(config.script.enabled);
    assert!(!config.script.script_dir.as_os_str().is_empty());
    assert!(config.script.timeout > Duration::from_secs(0));
    assert!(config.script.max_concurrent > 0);
    
    // 验证通用配置
    assert!(config.general.max_connections > 0);
    assert!(config.general.connection_timeout > Duration::from_secs(0));
    assert_eq!(config.general.log_level, "info");
    
    println!("✅ 默认配置验证通过");
    Ok(())
}

/// 测试入站配置的各种设置
#[tokio::test]
async fn test_inbound_variations() -> Result<()> {
    println!("🧪 测试入站配置变化");
    
    // 测试自定义入站配置
    let custom_inbound = InboundConfig {
        listen_address: "0.0.0.0".to_string(),
        listen_port: 8080,
        max_connections: 1000,
        buffer_size: 8192,
        timeout: Duration::from_secs(60),
        intercepted_ports: vec![80, 443, 8080, 3000],
        bypass_user: Some("nobody".to_string()),
    };
    
    assert_eq!(custom_inbound.listen_address, "0.0.0.0");
    assert_eq!(custom_inbound.listen_port, 8080);
    assert_eq!(custom_inbound.max_connections, 1000);
    assert_eq!(custom_inbound.intercepted_ports.len(), 4);
    assert_eq!(custom_inbound.bypass_user.as_ref().unwrap(), "nobody");
    
    // 测试无绑定用户
    let no_user_inbound = InboundConfig {
        bypass_user: None,
        ..custom_inbound.clone()
    };
    
    assert!(no_user_inbound.bypass_user.is_none());
    
    println!("✅ 入站配置变化测试通过");
    Ok(())
}

/// 测试出站配置的超时设置
#[tokio::test]
async fn test_outbound_timeouts() -> Result<()> {
    println!("🧪 测试出站配置超时设置");
    
    let config = OutboundConfig {
        timeout: Duration::from_secs(30),
        read_timeout: Duration::from_secs(10),
        write_timeout: Duration::from_secs(10),
        retry_count: 3,
        retry_delay: Duration::from_millis(500),
    };
    
    // 验证超时配置
    assert_eq!(config.timeout, Duration::from_secs(30));
    assert_eq!(config.read_timeout, Duration::from_secs(10));
    assert_eq!(config.write_timeout, Duration::from_secs(10));
    
    // 验证重试配置
    assert_eq!(config.retry_count, 3);
    assert_eq!(config.retry_delay, Duration::from_millis(500));
    
    println!("✅ 出站配置超时测试通过");
    Ok(())
}

/// 测试脚本配置的各种选项
#[tokio::test]
async fn test_script_options() -> Result<()> {
    println!("🧪 测试脚本配置选项");
    
    // 启用脚本的配置
    let enabled_config = ScriptConfig {
        enabled: true,
        script_dir: PathBuf::from("./scripts"),
        timeout: Duration::from_secs(5),
        max_concurrent: 10,
    };
    
    assert!(enabled_config.enabled);
    assert_eq!(enabled_config.script_dir, PathBuf::from("./scripts"));
    assert_eq!(enabled_config.timeout, Duration::from_secs(5));
    assert_eq!(enabled_config.max_concurrent, 10);
    
    // 禁用脚本的配置
    let disabled_config = ScriptConfig {
        enabled: false,
        script_dir: PathBuf::from("/dev/null"),
        timeout: Duration::from_secs(1),
        max_concurrent: 1,
    };
    
    assert!(!disabled_config.enabled);
    assert_eq!(disabled_config.max_concurrent, 1);
    
    println!("✅ 脚本配置选项测试通过");
    Ok(())
}

/// 测试日志级别和通用配置
#[tokio::test]
async fn test_general_log_levels() -> Result<()> {
    println!("🧪 测试通用配置和日志级别");
    
    // 测试不同的日志级别（使用字符串）
    let log_levels = vec!["debug", "info", "warn", "error"];
    
    for log_level in log_levels {
        let config = GeneralConfig {
            max_connections: 500,
            connection_timeout: Duration::from_secs(30),
            log_level: log_level.to_string(),
        };
        
        assert_eq!(config.log_level, log_level);
        assert_eq!(config.max_connections, 500);
        assert_eq!(config.connection_timeout, Duration::from_secs(30));
    }
    
    // 测试LogLevel枚举
    let log_levels_enum = vec![
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
    ];
    
    for log_level in log_levels_enum {
        // 验证枚举可以正常创建
        assert!(matches!(log_level, LogLevel::Debug | LogLevel::Info | LogLevel::Warn | LogLevel::Error));
    }
    
    println!("✅ 通用配置和日志级别测试通过");
    Ok(())
}

/// 测试配置的序列化和反序列化
#[tokio::test]
async fn test_config_serialization() -> Result<()> {
    println!("🧪 测试配置序列化和反序列化");
    
    let original_config = AppConfig {
        app_name: "test-app".to_string(),
        version: "1.0.0".to_string(),
        inbound: InboundConfig {
            listen_address: "0.0.0.0".to_string(),
            listen_port: 9999,
            max_connections: 2000,
            buffer_size: 16384,
            timeout: Duration::from_secs(45),
            intercepted_ports: vec![80, 443],
            bypass_user: Some("test".to_string()),
        },
        outbound: OutboundConfig {
            timeout: Duration::from_secs(20),
            read_timeout: Duration::from_secs(15),
            write_timeout: Duration::from_secs(15),
            retry_count: 5,
            retry_delay: Duration::from_millis(1000),
        },
        script: ScriptConfig {
            enabled: false,
            script_dir: PathBuf::from("/custom/scripts"),
            timeout: Duration::from_secs(10),
            max_concurrent: 20,
        },
        tls: takeover::TlsConfig { 
            enabled: true,
            interception_mode: "selective".to_string(),
            intercept_domains: vec!["*.org".to_string(), "*.com".to_string()],
            cert_store_path: None,
            cert_cache_duration: 86400, // 24小时
            max_concurrent_handshakes: 50,
            auto_install_ca: true,
        },
        general: GeneralConfig {
            max_connections: 1500,
            connection_timeout: Duration::from_secs(60),
            log_level: "debug".to_string(),
        }
    };
    
    // 序列化为JSON
    let json_str = serde_json::to_string_pretty(&original_config)?;
    assert!(!json_str.is_empty());
    assert!(json_str.contains("test-app"));
    assert!(json_str.contains("1.0.0"));
    
    // 从JSON反序列化
    let deserialized_config: AppConfig = serde_json::from_str(&json_str)?;
    
    // 验证反序列化的正确性
    assert_eq!(deserialized_config.app_name, original_config.app_name);
    assert_eq!(deserialized_config.version, original_config.version);
    assert_eq!(deserialized_config.inbound.listen_port, original_config.inbound.listen_port);
    assert_eq!(deserialized_config.outbound.retry_count, original_config.outbound.retry_count);
    assert_eq!(deserialized_config.script.enabled, original_config.script.enabled);
    assert_eq!(deserialized_config.general.max_connections, original_config.general.max_connections);
    
    println!("✅ 配置序列化测试通过");
    Ok(())
}

/// 测试配置文件的加载和保存
#[tokio::test]
async fn test_config_file_operations() -> Result<()> {
    println!("🧪 测试配置文件操作");
    
    // 创建临时目录
    let temp_dir = TempDir::new()?;
    let config_path = temp_dir.path().join("test_config.json");
    
    // 创建测试配置
    let test_config = AppConfig {
        app_name: "file-test".to_string(),
        version: "2.0.0".to_string(),
        ..AppConfig::default()
    };
    
    // 序列化配置到文件
    let json_content = serde_json::to_string_pretty(&test_config)?;
    std::fs::write(&config_path, json_content)?;
    
    // 验证文件存在
    assert!(config_path.exists());
    
    // 从文件加载配置
    let loaded_content = std::fs::read_to_string(&config_path)?;
    let loaded_config: AppConfig = serde_json::from_str(&loaded_content)?;
    
    // 验证加载的配置
    assert_eq!(loaded_config.app_name, "file-test");
    assert_eq!(loaded_config.version, "2.0.0");
    
    // 测试配置更新
    let mut updated_config = loaded_config;
    updated_config.app_name = "updated-file-test".to_string();
    updated_config.inbound.listen_port = 7777;
    
    // 保存更新的配置
    let updated_json = serde_json::to_string_pretty(&updated_config)?;
    std::fs::write(&config_path, updated_json)?;
    
    // 重新加载验证
    let reloaded_content = std::fs::read_to_string(&config_path)?;
    let reloaded_config: AppConfig = serde_json::from_str(&reloaded_content)?;
    
    assert_eq!(reloaded_config.app_name, "updated-file-test");
    assert_eq!(reloaded_config.inbound.listen_port, 7777);
    
    println!("✅ 配置文件操作测试通过");
    Ok(())
}

/// 测试配置验证逻辑
#[tokio::test]
async fn test_config_validation() -> Result<()> {
    println!("🧪 测试配置验证逻辑");
    
    // 测试有效的配置
    let valid_config = AppConfig {
        app_name: "valid-app".to_string(),
        inbound: InboundConfig {
            listen_port: 8080,
            max_connections: 1000,
            ..AppConfig::default().inbound
        },
        ..AppConfig::default()
    };
    
    // 基本验证 - 端口号应该在有效范围内
    assert!(valid_config.inbound.listen_port > 0);
    assert!(valid_config.inbound.listen_port <= 65535);
    
    // 最大连接数应该合理
    assert!(valid_config.inbound.max_connections > 0);
    assert!(valid_config.inbound.max_connections <= 100000);
    
    // 超时时间应该合理
    assert!(valid_config.outbound.timeout >= Duration::from_secs(1));
    assert!(valid_config.outbound.timeout <= Duration::from_secs(300));
    
    // 缓冲区大小应该合理
    assert!(valid_config.inbound.buffer_size >= 1024);
    assert!(valid_config.inbound.buffer_size <= 1024 * 1024);
    
    // 脚本并发数应该合理
    assert!(valid_config.script.max_concurrent >= 1);
    assert!(valid_config.script.max_concurrent <= 1000);
    
    println!("✅ 配置验证逻辑测试通过");
    Ok(())
}

/// 测试配置合并和覆盖
#[tokio::test]
async fn test_config_merging() -> Result<()> {
    println!("🧪 测试配置合并和覆盖");
    
    let base_config = AppConfig::default();
    
    // 创建部分覆盖配置
    let override_inbound = InboundConfig {
        listen_port: 9090,
        max_connections: 2000,
        ..base_config.inbound.clone()
    };
    
    let override_script = ScriptConfig {
        enabled: false,
        timeout: Duration::from_secs(1),
        ..base_config.script.clone()
    };
    
    // 创建合并后的配置
    let merged_config = AppConfig {
        app_name: "merged-app".to_string(),
        inbound: override_inbound,
        script: override_script,
        ..base_config.clone()
    };
    
    // 验证合并结果
    assert_eq!(merged_config.app_name, "merged-app");
    assert_eq!(merged_config.inbound.listen_port, 9090);
    assert_eq!(merged_config.inbound.max_connections, 2000);
    assert!(!merged_config.script.enabled);
    assert_eq!(merged_config.script.timeout, Duration::from_secs(1));
    
    // 验证未覆盖的部分保持原值
    assert_eq!(merged_config.version, "0.1.0"); // 来自默认值
    assert_eq!(merged_config.outbound.retry_count, base_config.outbound.retry_count);
    
    println!("✅ 配置合并测试通过");
    Ok(())
}

/// 运行所有Level 1配置测试的便利函数
pub async fn run_all_level_1_tests() -> Result<()> {
    println!("🧪 Running Level 1: Configuration Layer Tests...\n");
    
    // 内联执行所有测试，避免调用 #[tokio::test] 函数
    
    // 1. 测试默认应用配置的创建和合理性
    println!("🧪 测试默认应用配置创建");
    {
        let config = AppConfig::default();
        
        // 验证基本属性
        assert_eq!(config.app_name, "takeover");
        assert_eq!(config.version, "0.1.0");
        
        // 验证入站配置
        assert_eq!(config.inbound.listen_address, "127.0.0.1");
        assert!(config.inbound.listen_port > 0);
        assert!(config.inbound.max_connections > 0);
        assert!(config.inbound.buffer_size > 0);
        assert!(config.inbound.timeout > Duration::from_secs(0));
        
        // 验证出站配置
        assert!(config.outbound.timeout > Duration::from_secs(0));
        assert!(config.outbound.read_timeout > Duration::from_secs(0));
        assert!(config.outbound.write_timeout > Duration::from_secs(0));
        assert!(config.outbound.retry_count > 0);
        assert!(config.outbound.retry_delay > Duration::from_secs(0));
        
        // 验证脚本配置
        assert!(config.script.enabled);
        assert!(!config.script.script_dir.as_os_str().is_empty());
        assert!(config.script.timeout > Duration::from_secs(0));
        assert!(config.script.max_concurrent > 0);
        
        // 验证通用配置
        assert!(config.general.max_connections > 0);
        assert!(config.general.connection_timeout > Duration::from_secs(0));
        assert_eq!(config.general.log_level, "info");
        
        println!("✅ 默认配置验证通过");
    }
    
    // 2. 测试入站配置变化
    println!("🧪 测试入站配置变化");
    {
        let custom_inbound = InboundConfig {
            listen_address: "0.0.0.0".to_string(),
            listen_port: 8080,
            max_connections: 1000,
            buffer_size: 8192,
            timeout: Duration::from_secs(60),
            intercepted_ports: vec![80, 443, 8080, 3000],
            bypass_user: Some("nobody".to_string()),
        };
        
        assert_eq!(custom_inbound.listen_address, "0.0.0.0");
        assert_eq!(custom_inbound.listen_port, 8080);
        assert_eq!(custom_inbound.max_connections, 1000);
        assert_eq!(custom_inbound.intercepted_ports.len(), 4);
        assert_eq!(custom_inbound.bypass_user.as_ref().unwrap(), "nobody");
        
        println!("✅ 入站配置变化测试通过");
    }
    
    // 3. 测试配置序列化
    println!("🧪 测试配置序列化和反序列化");
    {
        let test_config = AppConfig {
            app_name: "test-app".to_string(),
            version: "1.0.0".to_string(),
            ..AppConfig::default()
        };
        
        // 序列化为JSON
        let json_str = serde_json::to_string_pretty(&test_config)?;
        assert!(!json_str.is_empty());
        assert!(json_str.contains("test-app"));
        assert!(json_str.contains("1.0.0"));
        
        // 从JSON反序列化
        let deserialized_config: AppConfig = serde_json::from_str(&json_str)?;
        
        // 验证反序列化的正确性
        assert_eq!(deserialized_config.app_name, test_config.app_name);
        assert_eq!(deserialized_config.version, test_config.version);
        assert_eq!(deserialized_config.inbound.listen_port, test_config.inbound.listen_port);
        
        println!("✅ 配置序列化测试通过");
    }
    
    // 4. 测试配置文件操作
    println!("🧪 测试配置文件操作");
    {
        // 创建临时目录
        let temp_dir = TempDir::new()?;
        let config_path = temp_dir.path().join("test_config.json");
        
        // 创建测试配置
        let test_config = AppConfig {
            app_name: "file-test".to_string(),
            version: "2.0.0".to_string(),
            ..AppConfig::default()
        };
        
        // 序列化配置到文件
        let json_content = serde_json::to_string_pretty(&test_config)?;
        std::fs::write(&config_path, json_content)?;
        
        // 验证文件存在
        assert!(config_path.exists());
        
        // 从文件加载配置
        let loaded_content = std::fs::read_to_string(&config_path)?;
        let loaded_config: AppConfig = serde_json::from_str(&loaded_content)?;
        
        // 验证加载的配置
        assert_eq!(loaded_config.app_name, "file-test");
        assert_eq!(loaded_config.version, "2.0.0");
        
        println!("✅ 配置文件操作测试通过");
    }
    
    // 5. 测试配置验证逻辑
    println!("🧪 测试配置验证逻辑");
    {
        let valid_config = AppConfig::default();
        
        // 基本验证 - 端口号应该在有效范围内
        assert!(valid_config.inbound.listen_port > 0);
        assert!(valid_config.inbound.listen_port <= 65535);
        
        // 最大连接数应该合理
        assert!(valid_config.inbound.max_connections > 0);
        assert!(valid_config.inbound.max_connections <= 100000);
        
        // 超时时间应该合理
        assert!(valid_config.outbound.timeout >= Duration::from_secs(1));
        assert!(valid_config.outbound.timeout <= Duration::from_secs(300));
        
        // 缓冲区大小应该合理
        assert!(valid_config.inbound.buffer_size >= 1024);
        assert!(valid_config.inbound.buffer_size <= 1024 * 1024);
        
        // 脚本并发数应该合理
        assert!(valid_config.script.max_concurrent >= 1);
        assert!(valid_config.script.max_concurrent <= 1000);
        
        println!("✅ 配置验证逻辑测试通过");
    }
    
    println!("\n🎉 All Level 1 tests passed successfully!");
    println!("✅ Configuration system is working correctly");
    Ok(())
}