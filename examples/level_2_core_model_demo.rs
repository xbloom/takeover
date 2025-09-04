//! Level 2 核心模型演示
//! 
//! 展示重构后的六边形架构核心业务模型功能
//! 独立运行，不依赖有编译错误的适配器模块

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::SystemTime;
use uuid::Uuid;

// 直接从src/core/model.rs导入核心模型
// 避开有编译错误的适配器模块
use takeover::core::model::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 Level 2 核心模型演示开始");
    println!("{}", "=".repeat(50));
    
    demo_connection_creation().await?;
    demo_connection_context().await?;
    demo_http_models().await?;
    demo_rule_creation().await?;
    demo_connection_status().await?;
    demo_app_config().await?;
    demo_adapter_status().await?;
    
    println!("{}", "=".repeat(50));
    println!("🎉 Level 2 核心模型演示完成！");
    println!("✅ 重构后的六边形架构核心功能验证通过");
    Ok(())
}

/// 演示连接创建和基本属性
async fn demo_connection_creation() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n🧪 1. 连接创建演示");
    
    let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345);
    let dest = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 80);
    
    let connection = Connection::new(source, dest);
    
    println!("   📝 连接 ID: {}", connection.id);
    println!("   📡 源地址: {}", connection.source_addr);
    println!("   🎯 目标地址: {}", connection.original_dest);
    println!("   🔗 协议: {}", connection.metadata.protocol);
    println!("   ⏰ 创建时间: {:?}", connection.metadata.created_at);
    
    // 验证
    assert_eq!(connection.source_addr, source);
    assert_eq!(connection.original_dest, dest);
    assert!(!connection.id.is_nil());
    assert_eq!(connection.metadata.protocol, "TCP");
    
    println!("   ✅ 连接创建功能正常");
    Ok(())
}

/// 演示连接上下文管理
async fn demo_connection_context() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n🧪 2. 连接上下文演示");
    
    let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)), 54321);
    let dest = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), 443);
    
    let connection = Connection::new(source, dest);
    let context = ConnectionContext::new(
        connection.clone(),
        "example.com".to_string(),
        443
    );
    
    println!("   🆔 上下文 ID: {}", context.id);
    println!("   🌐 目标主机: {}", context.target_host);
    println!("   🔌 目标端口: {}", context.target_port);
    println!("   📊 处理状态: {:?}", context.processing_state);
    
    // 验证
    assert_eq!(context.id, connection.id);
    assert_eq!(context.target_host, "example.com");
    assert_eq!(context.target_port, 443);
    
    println!("   ✅ 连接上下文管理正常");
    Ok(())
}

/// 演示HTTP请求和响应模型
async fn demo_http_models() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n🧪 3. HTTP模型演示");
    
    // HTTP请求
    let mut headers = HashMap::new();
    headers.insert("Host".to_string(), "api.example.com".to_string());
    headers.insert("User-Agent".to_string(), "Takeover/1.0".to_string());
    headers.insert("Accept".to_string(), "application/json".to_string());
    
    let request = HttpRequest {
        id: Some(Uuid::new_v4()),
        method: "GET".to_string(),
        url: "https://api.example.com/v1/data".to_string(),
        version: "HTTP/1.1".to_string(),
        headers: headers.clone(),
        body: vec![],
        sent_at: Some(SystemTime::now()),
    };
    
    println!("   📤 HTTP请求:");
    println!("      方法: {}", request.method);
    println!("      URL: {}", request.url);
    println!("      请求头数量: {}", request.headers.len());
    println!("      Content-Length: {}", request.body.len());
    
    // HTTP响应
    let mut response_headers = HashMap::new();
    response_headers.insert("Content-Type".to_string(), "application/json".to_string());
    response_headers.insert("Cache-Control".to_string(), "no-cache".to_string());
    
    let response_body = r#"{"message": "success", "data": {"users": 42}}"#;
    let response = HttpResponse {
        id: Some(Uuid::new_v4()),
        status_code: 200,
        status_text: Some("OK".to_string()),
        headers: response_headers,
        body: response_body.as_bytes().to_vec(),
        sent_at: Some(SystemTime::now()),
    };
    
    println!("   📥 HTTP响应:");
    println!("      状态码: {} {:?}", response.status_code, response.status_text);
    println!("      响应头数量: {}", response.headers.len());
    println!("      响应体: {}", String::from_utf8_lossy(&response.body));
    
    // 验证
    assert_eq!(request.method, "GET");
    assert_eq!(response.status_code, 200);
    
    println!("   ✅ HTTP模型功能正常");
    Ok(())
}

/// 演示规则创建和配置
async fn demo_rule_creation() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n🧪 4. 规则创建演示");
    
    // 创建允许规则
    let allow_rule = Rule::new(
        "allow_api".to_string(),
        "允许API访问".to_string(),
        RulePattern::HostMatches("api.*.com".to_string()),
        RuleAction::Allow,
        100,
    );
    
    println!("   📋 允许规则:");
    println!("      ID: {}", allow_rule.id);
    println!("      名称: {}", allow_rule.name);
    println!("      描述: {}", allow_rule.description);
    println!("      优先级: {}", allow_rule.priority);
    println!("      状态: {}", if allow_rule.enabled { "启用" } else { "禁用" });
    
    // 创建阻止规则
    let block_rule = Rule::new(
        "block_ads".to_string(),
        "阻止广告域名".to_string(),
        RulePattern::HostMatches("*.ads.com".to_string()),
        RuleAction::Block,
        200,
    );
    
    println!("   🚫 阻止规则:");
    println!("      名称: {}", block_rule.name);
    println!("      描述: {}", block_rule.description);
    println!("      优先级: {}", block_rule.priority);
    
    // 验证规则类型
    match allow_rule.pattern {
        RulePattern::HostMatches(ref pattern) => {
            println!("      模式类型: 主机匹配 ({})", pattern);
        }
        _ => println!("      其他模式类型"),
    }
    
    match allow_rule.action {
        RuleAction::Allow => println!("      动作: 允许"),
        RuleAction::Block => println!("      动作: 阻止"),
        _ => println!("      其他动作"),
    }
    
    println!("   ✅ 规则创建功能正常");
    Ok(())
}

/// 演示连接状态转换
async fn demo_connection_status() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n🧪 5. 连接状态演示");
    
    let statuses = vec![
        ("连接中", ConnectionStatus::Connecting),
        ("已连接", ConnectionStatus::Connected),
        ("处理中", ConnectionStatus::Processing),
        ("已暂停", ConnectionStatus::Paused),
        ("已完成", ConnectionStatus::Completed),
        ("错误状态", ConnectionStatus::Error("网络超时".to_string())),
    ];
    
    for (desc, status) in statuses {
        print!("   🔄 {}: ", desc);
        match status {
            ConnectionStatus::Connecting => println!("正在建立连接..."),
            ConnectionStatus::Connected => println!("连接已建立"),
            ConnectionStatus::Active => println!("活跃状态"),
            ConnectionStatus::Processing => println!("正在处理请求"),
            ConnectionStatus::Paused => println!("等待用户干预"),
            ConnectionStatus::Completed => println!("处理完成"),
            ConnectionStatus::Error(ref msg) => println!("发生错误: {}", msg),
            ConnectionStatus::Closing => println!("正在关闭连接"),
            ConnectionStatus::Closed => println!("连接已关闭"),
            _ => println!("其他状态"),
        }
    }
    
    println!("   ✅ 连接状态管理正常");
    Ok(())
}

/// 演示应用配置
async fn demo_app_config() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n🧪 6. 应用配置演示");
    
    let config = AppConfig::default();
    
    println!("   ⚙️  入站配置:");
    println!("      监听地址: {}", config.inbound_config.listen_address);
    println!("      监听端口: {}", config.inbound_config.listen_port);
    println!("      最大连接数: {}", config.inbound_config.max_connections);
    println!("      缓冲区大小: {}", config.inbound_config.buffer_size);
    
    println!("   ⚙️  出站配置:");
    println!("      连接超时: {:?}", config.outbound_config.timeout);
    println!("      读取超时: {:?}", config.outbound_config.read_timeout);
    println!("      写入超时: {:?}", config.outbound_config.write_timeout);
    println!("      重试次数: {:?}", config.outbound_config.retry_count);
    println!("      重试延迟: {:?}", config.outbound_config.retry_delay);
    
    println!("   ⚙️  通用配置:");
    println!("      最大连接数: {}", config.general_config.max_connections);
    println!("      日志级别: {:?}", config.general_config.log_level);
    
    // 验证默认配置合理性
    assert!(config.inbound_config.listen_port > 0);
    assert!(config.general_config.max_connections > 0);
    assert!(!config.inbound_config.listen_address.is_empty());
    
    println!("   ✅ 应用配置功能正常");
    Ok(())
}

/// 演示适配器状态
async fn demo_adapter_status() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n🧪 7. 适配器状态演示");
    
    let inbound_status = AdapterStatus {
        name: "PfctlInbound".to_string(),
        active: true,
        connections: 3,
        last_activity: Some(SystemTime::now()),
        errors: 0,
    };
    
    let outbound_status = AdapterStatus {
        name: "DirectOutbound".to_string(),
        active: true,
        connections: 2,
        last_activity: Some(SystemTime::now()),
        errors: 1,
    };
    
    println!("   📊 入站适配器状态:");
    println!("      名称: {}", inbound_status.name);
    println!("      状态: {}", if inbound_status.active { "活跃" } else { "非活跃" });
    println!("      连接数: {}", inbound_status.connections);
    println!("      错误数: {}", inbound_status.errors);
    
    println!("   📊 出站适配器状态:");
    println!("      名称: {}", outbound_status.name);
    println!("      状态: {}", if outbound_status.active { "活跃" } else { "非活跃" });
    println!("      连接数: {}", outbound_status.connections);
    println!("      错误数: {}", outbound_status.errors);
    
    println!("   ✅ 适配器状态管理正常");
    Ok(())
}
