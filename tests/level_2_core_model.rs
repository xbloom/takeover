//! Level 2: 核心模型集成测试
//! 
//! 测试核心数据结构：Connection、ConnectionContext、HttpRequest/Response 等
//! 这一层验证六边形架构的核心业务模型是否正确工作

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime};

use anyhow::Result;

use takeover::{
    Connection, ConnectionContext, ConnectionMetadata, 
    HttpRequest, HttpResponse, ProcessingState, RequestModifications,
    ConnectionStatus, ConnectionSummary, DomainEvent, ErrorCategory
};

/// 创建测试用的连接
fn create_test_connection() -> Connection {
    let source_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    let dest_addr: SocketAddr = "93.184.216.34:80".parse().unwrap(); // example.com
    Connection::new(source_addr, dest_addr)
}

/// 创建测试用的HTTP请求
fn create_test_http_request() -> HttpRequest {
    let mut request = HttpRequest::new("GET", "http://example.com/test");
    request.headers.insert("Host".to_string(), "example.com".to_string());
    request.headers.insert("User-Agent".to_string(), "takeover-test/1.0".to_string());
    request.body = b"test body".to_vec();
    request
}

/// 创建测试用的HTTP响应
fn create_test_http_response() -> HttpResponse {
    let mut response = HttpResponse::new(200);
    response.status_text = Some("OK".to_string());
    response.headers.insert("Content-Type".to_string(), "application/json".to_string());
    response.body = br#"{"message": "Hello, World!"}"#.to_vec();
    response
}

#[tokio::test]
async fn test_connection_creation_and_metadata() -> Result<()> {
    // 测试连接的创建和元数据
    let connection = create_test_connection();
    
    // 验证连接基本属性
    assert_eq!(connection.source_addr.port(), 12345);
    assert_eq!(connection.original_dest.port(), 80);
    assert!(connection.id.is_empty());
    
    // 验证元数据
    assert_eq!(connection.metadata.protocol, "TCP");
    assert!(connection.metadata.process_name.is_none());
    assert!(connection.metadata.created_at <= SystemTime::now());
    assert!(connection.metadata.custom_data.is_empty());
    
    println!("✅ Connection creation and metadata test passed");
    Ok(())
}

#[tokio::test]
async fn test_connection_context_lifecycle() -> Result<()> {
    // 测试连接上下文的生命周期管理
    let connection = create_test_connection();
    let connection_id = connection.id.clone();
    let mut context = ConnectionContext::new(
        connection,
        "example.com".to_string(),
        80
    );
    
    // 验证初始状态
    assert_eq!(context.id, connection_id);
    assert_eq!(context.target_host, "example.com");
    assert_eq!(context.target_port, 80);
    assert_eq!(context.processing_state, ProcessingState::Initialized);
    assert!(context.request.is_none());
    assert!(context.response.is_none());
    assert!(context.applied_modifications.is_empty());
    
    // 测试状态转换
    context.set_processing_state(ProcessingState::HttpParsing);
    assert_eq!(context.processing_state, ProcessingState::HttpParsing);
    
    context.set_processing_state(ProcessingState::RuleMatching);
    assert_eq!(context.processing_state, ProcessingState::RuleMatching);
    
    // 测试添加匹配的规则
    context.add_matched_rule("rule_001".to_string());
    context.add_matched_rule("rule_002".to_string());
    
    let matched_rules = context.custom_data.get("matched_rules")
        .and_then(|v| v.as_array())
        .unwrap();
    assert_eq!(matched_rules.len(), 2);
    
    println!("✅ Connection context lifecycle test passed");
    Ok(())
}

#[tokio::test]
async fn test_http_request_response_handling() -> Result<()> {
    // 测试HTTP请求和响应的处理
    let mut context = ConnectionContext::new(
        create_test_connection(),
        "example.com".to_string(),
        80
    );
    
    // 添加HTTP请求
    let request = create_test_http_request();
    let request_id = request.id.unwrap();
    context.request = Some(request);
    
    // 验证请求
    let stored_request = context.request.as_ref().unwrap();
    assert_eq!(stored_request.method, "GET");
    assert_eq!(stored_request.url, "http://example.com/test");
    assert_eq!(stored_request.version, "HTTP/1.1");
    assert_eq!(stored_request.headers.get("Host").unwrap(), "example.com");
    assert_eq!(stored_request.body, b"test body");
    assert_eq!(stored_request.id.unwrap(), request_id);
    
    // 添加HTTP响应
    let response = create_test_http_response();
    let response_id = response.id.unwrap();
    context.response = Some(response);
    
    // 验证响应
    let stored_response = context.response.as_ref().unwrap();
    assert_eq!(stored_response.status_code, 200);
    assert_eq!(stored_response.status_text.as_ref().unwrap(), "OK");
    assert_eq!(stored_response.headers.get("Content-Type").unwrap(), "application/json");
    assert_eq!(stored_response.id.unwrap(), response_id);
    
    println!("✅ HTTP request/response handling test passed");
    Ok(())
}

#[tokio::test]
async fn test_request_modifications() -> Result<()> {
    // 测试请求修改功能
    let mut context = ConnectionContext::new(
        create_test_connection(),
        "example.com".to_string(),
        80
    );
    
    // 创建请求修改配置
    let modifications = RequestModifications {
        url: Some("http://modified-example.com/test".to_string()),
        method: Some("POST".to_string()),
        headers: Some({
            let mut headers = HashMap::new();
            headers.insert("X-Modified".to_string(), "true".to_string());
            headers
        }),
        body: Some(b"modified body".to_vec()),
        remove_headers: Some(vec!["User-Agent".to_string()]),
        add_query_params: Some({
            let mut params = HashMap::new();
            params.insert("modified".to_string(), "1".to_string());
            params
        }),
        remove_query_params: Some(vec!["old_param".to_string()]),
    };
    
    // 应用修改
    context.apply_modification(modifications);
    
    // 验证修改已被记录
    assert_eq!(context.applied_modifications.len(), 1);
    let applied = &context.applied_modifications[0];
    assert_eq!(applied.url.as_ref().unwrap(), "http://modified-example.com/test");
    assert_eq!(applied.method.as_ref().unwrap(), "POST");
    assert!(applied.headers.as_ref().unwrap().contains_key("X-Modified"));
    
    println!("✅ Request modifications test passed");
    Ok(())
}

#[tokio::test]
async fn test_connection_status_transitions() -> Result<()> {
    // 测试连接状态转换
    let connection = create_test_connection();
    
    // 创建连接摘要
    let mut summary = ConnectionSummary {
        id: connection.id,
        source_addr: connection.source_addr,
        target_host: "example.com".to_string(),
        target_port: 80,
        protocol: "HTTP".to_string(),
        status: ConnectionStatus::Connecting,
        bytes_sent: 0,
        bytes_received: 0,
        created_at: SystemTime::now(),
        last_activity: None,
    };
    
    // 测试状态转换序列
    assert!(matches!(summary.status, ConnectionStatus::Connecting));
    
    summary.status = ConnectionStatus::Connected;
    assert!(matches!(summary.status, ConnectionStatus::Connected));
    
    summary.status = ConnectionStatus::Processing;
    assert!(matches!(summary.status, ConnectionStatus::Processing));
    
    summary.status = ConnectionStatus::Completed;
    assert!(matches!(summary.status, ConnectionStatus::Completed));
    
    // 测试错误状态
    summary.status = ConnectionStatus::Error("Connection timeout".to_string());
    match summary.status {
        ConnectionStatus::Error(ref msg) => assert_eq!(msg, "Connection timeout"),
        _ => panic!("Expected error status"),
    }
    
    println!("✅ Connection status transitions test passed");
    Ok(())
}

#[tokio::test]
async fn test_domain_events() -> Result<()> {
    // 测试领域事件的创建和属性
    let connection_id = takeover::utils::generate_short_connection_id();
    let source_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
    
    // 测试新连接事件
    let new_conn_event = DomainEvent::ConnectionNew {
        id: connection_id.clone(),
        addr: source_addr,
        metadata: ConnectionMetadata::default(),
    };
    
    match new_conn_event {
        DomainEvent::ConnectionNew { id, addr, .. } => {
            assert_eq!(id, connection_id);
            assert_eq!(addr, source_addr);
        }
        _ => panic!("Expected ConnectionNew event"),
    }
    
    // 测试连接关闭事件
    let close_event = DomainEvent::ConnectionClosed {
        id: connection_id.clone(),
        duration: Duration::from_secs(30),
        bytes_transferred: 1024,
    };
    
    match close_event {
        DomainEvent::ConnectionClosed { duration, bytes_transferred, .. } => {
            assert_eq!(duration, Duration::from_secs(30));
            assert_eq!(bytes_transferred, 1024);
        }
        _ => panic!("Expected ConnectionClosed event"),
    }
    
    // 测试错误事件
    let error_event = DomainEvent::Error {
        conn_id: Some(connection_id.clone()),
        error: "Network timeout".to_string(),
        category: ErrorCategory::Network,
    };
    
    match error_event {
        DomainEvent::Error { error, category, .. } => {
            assert_eq!(error, "Network timeout");
            assert!(matches!(category, ErrorCategory::Network));
        }
        _ => panic!("Expected Error event"),
    }
    
    println!("✅ Domain events test passed");
    Ok(())
}

#[tokio::test]
async fn test_concurrent_context_operations() -> Result<()> {
    // 测试并发的上下文操作
    use std::sync::Arc;
    use tokio::sync::Mutex;
    
    let context: Arc<Mutex<ConnectionContext>> = Arc::new(Mutex::new(ConnectionContext::new(
        create_test_connection(),
        "example.com".to_string(),
        80
    )));
    
    let mut handles = Vec::new();
    
    // 启动多个任务同时操作上下文
    for i in 0..5 {
        let context_clone: Arc<Mutex<ConnectionContext>> = Arc::clone(&context);
        let handle = tokio::spawn(async move {
            let mut ctx = context_clone.lock().await;
            
            // 添加规则匹配
            ctx.add_matched_rule(format!("rule_{}", i));
            
            // 设置状态
            match i % 3 {
                0 => ctx.set_processing_state(ProcessingState::HttpParsing),
                1 => ctx.set_processing_state(ProcessingState::RuleMatching),
                _ => ctx.set_processing_state(ProcessingState::ScriptExecution),
            }
            
            // 添加自定义数据
            ctx.custom_data.insert(
                format!("worker_{}", i),
                serde_json::Value::String(format!("data_{}", i))
            );
            
            format!("Worker {} completed", i)
        });
        
        handles.push(handle);
    }
    
    // 等待所有任务完成
    let results = futures::future::join_all(handles).await;
    
    for result in results {
        assert!(result.is_ok());
        let message = result.unwrap();
        assert!(message.contains("Worker"));
        assert!(message.contains("completed"));
    }
    
    // 验证最终状态
    let final_context = context.lock().await;
    let matched_rules = final_context.custom_data.get("matched_rules")
        .and_then(|v| v.as_array())
        .unwrap();
    assert_eq!(matched_rules.len(), 5);
    
    // 验证自定义数据
    for i in 0..5 {
        let key = format!("worker_{}", i);
        assert!(final_context.custom_data.contains_key(&key));
    }
    
    println!("✅ Concurrent context operations test passed");
    Ok(())
}

/// 运行所有Level 2测试的便利函数
pub async fn run_all_level_2_tests() -> Result<()> {
    println!("🧪 Running Level 2: Core Model Integration Tests...\n");
    
    test_connection_creation_and_metadata()?; 
    test_connection_context_lifecycle()?;
    test_http_request_response_handling()?;
    test_request_modifications()?;
    test_connection_status_transitions()?;
    test_domain_events()?;
    test_concurrent_context_operations()?;
    
    println!("\n🎉 All Level 2 tests passed successfully!");
    println!("✅ Core business models are working correctly");
    Ok(())
}   