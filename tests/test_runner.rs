//! 测试运行器
//! 
//! 提供运行各种测试套件的便利函数

use anyhow::Result;

// 导入测试模块
pub mod level_1_config;
pub mod level_2_core_model;
pub mod level_3_inbound_adapter;
pub mod level_3_5_port_interception_test;
pub mod level_4_outbound_adapter;
pub mod level_5_integration_pipeline;

/// 运行所有可用的测试
pub async fn run_all_tests() -> Result<()> {
    println!("🚀 Starting Takeover Architecture Test Suite\n");

    // 1. 运行配置层测试 (Level 1)
    println!("⚙️  Level 1: Running configuration tests...");
    level_1_config::run_all_level_1_tests().await?;

    // 2. 运行核心模型测试 (Level 2)  
    println!("\n🧬 Level 2: Running core model tests...");
    level_2_core_model::run_all_level_2_tests().await?;
    
    // 3. 运行入站适配器测试 (Level 3)
    println!("\n🔌 Level 3: Running inbound adapter tests...");
    level_3_inbound_adapter::run_all_level_3_tests().await?;
    
    // 3.5. 运行端口拦截功能验证测试 (Level 3.5)
    println!("\n🔍 Level 3.5: Running port interception verification tests...");
    level_3_5_port_interception_test::run_all_level_3_5_tests().await?;
    
    // 4. 运行出站适配器测试 (Level 4)
    println!("\n🌐 Level 4: Running outbound adapter tests...");
    level_4_outbound_adapter::run_all_level_4_tests().await?;
    
    // 5. 运行集成Pipeline测试 (Level 5)
    println!("\n🏗️ Level 5: Running integration pipeline tests...");
    level_5_integration_pipeline::run_all_level_5_tests().await?;
    
    println!("\n✨ All test suites completed successfully!");
    println!("🎯 New architecture validation: PASSED");
    println!("🚀 Complex Pipeline integration: VERIFIED");
    
    Ok(())
}

/// 运行特定的测试套件
pub async fn run_test_suite(suite_name: &str) -> Result<()> {
    match suite_name {
        "config" | "level1" => {
            level_1_config::run_all_level_1_tests().await?;
        }
        "core" | "model" | "level2" => {
            level_2_core_model::run_all_level_2_tests().await?;
        }
        "inbound" | "adapter" | "level3" => {
            level_3_inbound_adapter::run_all_level_3_tests().await?;
        }
        "port" | "interception" | "level3.5" => {
            level_3_5_port_interception_test::run_all_level_3_5_tests().await?;
        }
        "outbound" | "level4" => {
            level_4_outbound_adapter::run_all_level_4_tests().await?;
        }
        "integration" | "pipeline" | "level5" => {
            level_5_integration_pipeline::run_all_level_5_tests().await?;
        }

        _ => {
            println!("❌ Unknown test suite: {}", suite_name);
            println!("Available suites:");
            println!("  - config, level1: Configuration layer tests");
            println!("  - core, model, level2: Core model tests");
            println!("  - inbound, adapter, level3: Inbound adapter tests");
            println!("  - port, interception, level3.5: Port interception verification tests");
            println!("  - outbound, level4: Outbound adapter tests");
            println!("  - integration, pipeline, level5: Integration pipeline tests");
            return Ok(());
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // 设置日志
    tracing_subscriber::fmt::init();
    
    // 检查命令行参数
    let args: Vec<String> = std::env::args().collect();
    
    if args.len() > 1 {
        let suite_name = &args[1];
        run_test_suite(suite_name).await?;
    } else {
        run_all_tests().await?;
    }
    
    Ok(())
}
