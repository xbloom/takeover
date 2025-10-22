# Takeover 透明代理 - AI 编码助手指南

## 项目概述

Takeover 是一个基于 Rust 的透明代理工具（类似 Proxifier），采用**六边形架构**（Hexagonal Architecture）设计，支持 TLS 拦截、HTTP/1.1 和 HTTP/2 协议处理。

**核心技术栈**: tokio, hyper, rustls, mlua, serde_json, tracing

## 架构核心原则

### 1. 六边形架构分层

```
src/core/          # 核心六边形（业务逻辑）
├── model.rs       # 数据模型（Connection, HttpRequest, Rule等）
├── ports.rs       # 端口接口定义（InboundPort, OutboundPort等）
├── engine.rs      # 中间件管道引擎
├── middleware/    # 中间件实现（按优先级执行）
└── services/      # 核心服务（StateManager, ReplayManager）

src/adapters/      # 适配器层（外部世界接口）
├── inbound/       # 入站适配器（pfctl透明代理）
├── outbound/      # 出站适配器（direct, socks5）
├── control/       # 控制适配器（REST API）
└── script/        # 脚本适配器（Lua引擎）
```

**关键规则**: 
- 核心业务逻辑只依赖 `ports.rs` 中的接口，不直接依赖适配器实现
- 适配器实现端口接口，可以访问核心服务
- 数据结构统一定义在 `model.rs`，通过 `ports.rs` 重新导出

### 2. 中间件管道架构（四分支协议处理）

中间件按**优先级**顺序执行，形成处理管道：

```rust
// 优先级定义（数字越小越早执行）
5   - ProtocolDetectionMiddleware  // 协议检测，非HTTP透传
10  - TlsMiddleware                 // TLS拦截（可选）
15  - Http2StreamMiddleware         // HTTP/2完整生命周期（Stop）
16  - Http1StreamMiddleware         // HTTP/1.1解析（Continue）
30  - RuleMatchingMiddleware        // 规则匹配
35  - ScriptMiddleware              // Lua脚本执行
40  - UserInterventionMiddleware    // 用户干预
50  - HttpModificationMiddleware    // HTTP修改
60  - OutboundRequestMiddleware     // 转发请求
70  - ResponseMergeMiddleware       // 响应合并
75  - InboundResponseMiddleware     // 响应写回
```

**四分支处理流程**:
1. **非HTTP协议** → Copy Pipe透传 → Stop
2. **HTTP/2** → hyper接管完整生命周期 → Stop
3. **HTTPS (HTTP/1.1)** → TLS拦截 → 标准Pipeline → 响应写回
4. **HTTP明文** → 标准Pipeline → 响应写回

### 3. 中间件实现模板

```rust
use async_trait::async_trait;
use crate::core::{Middleware, MiddlewareResult, ConnectionContext, AppCore};

pub struct MyMiddleware {
    // 配置字段
}

#[async_trait]
impl Middleware for MyMiddleware {
    fn name(&self) -> &str { "MyMiddleware" }
    fn priority(&self) -> i32 { 50 }  // 设置优先级
    
    async fn process(
        &self,
        context: &mut ConnectionContext,
        core: &AppCore,
    ) -> Result<MiddlewareResult, TakeoverError> {
        // 1. 检查前置条件
        // 2. 执行业务逻辑
        // 3. 修改context
        // 4. 返回Continue/Stop/Pause/Reject
        Ok(MiddlewareResult::Continue)
    }
    
    // 可选：注册服务到AppCore
    async fn register_services(&self, app_core: &AppCore) -> Result<(), TakeoverError> {
        let service = MyService::new();
        app_core.manage(service).await;
        Ok(())
    }
}
```

### 4. 服务容器模式（Tauri风格依赖注入）

```rust
// 注册服务（在中间件的register_services中）
app_core.manage(TlsService::new()).await;

// 查询服务（在任何地方）
let app_core = AppCore::global();  // 全局单例访问
if let Some(tls_service) = app_core.state::<TlsService>().await {
    tls_service.do_something().await?;
}
```

**关键点**: 
- 服务通过 `TypeId` 实现类型安全的依赖注入
- 使用 `Arc<T>` 支持多线程共享
- 全局 `AppCore::global()` 访问，无需显式传递引用

## 开发工作流

### 构建和运行

```bash
# 快速开发运行（debug模式，日志输出）
make dev

# 生产构建
make build

# 运行主程序（需要sudo）
sudo ./target/release/takeover

# 运行示例
make lv5    # HTTP/2 direct demo
make tls    # Complete TLS proxy demo
```

### 测试策略

测试文件位于 `tests/` 目录，采用**渐进式集成测试**：
- `level_1_config.rs` - 配置加载
- `level_2_core_model.rs` - 核心模型
- `level_3_inbound_adapter.rs` - 入站适配器
- `level_4_outbound_adapter.rs` - 出站适配器
- `level_5_integration_pipeline.rs` - 完整管道

```bash
cargo test                    # 运行所有测试
cargo test level_2            # 运行特定级别测试
```

### 调试技巧

```bash
# 启用详细日志
RUST_LOG=takeover=debug,main=debug cargo run

# 检查pfctl状态（macOS）
make status

# 清理pfctl规则
make cpf
```

## 项目特定约定

### 1. 错误处理

使用 `TakeoverError` 统一错误类型，定义在 `src/error.rs`：

```rust
use crate::error::TakeoverError;

// 返回Result
fn my_function() -> Result<(), TakeoverError> {
    // 使用 ? 操作符传播错误
    some_operation()?;
    Ok(())
}
```

### 2. 日志规范

使用 `tracing` 库，遵循以下约定：

```rust
use tracing::{debug, info, warn, error};

debug!("🔍 详细调试信息");
info!("✅ 重要状态变化");
warn!("⚠️  警告信息");
error!("❌ 错误信息");
```

**Emoji约定**: 🔍调试 ✅成功 ⚠️警告 ❌错误 🚀启动 🔧配置 🌐网络 🔐安全

### 3. 连接上下文传递

`ConnectionContext` 是核心数据结构，贯穿整个处理管道：

```rust
pub struct ConnectionContext {
    pub connection: Connection,        // 连接信息（含TCP/TLS流）
    pub request: Option<HttpRequest>,  // HTTP请求
    pub response: Option<HttpResponse>,// HTTP响应
    pub custom_data: HashMap<String, serde_json::Value>, // 自定义数据
    // ...
}
```

**使用模式**:
- 中间件通过 `context.custom_data` 传递状态
- 使用 `context.connection.stream` 访问TCP流
- 使用 `context.connection.inbound_tls_stream` 访问TLS流

### 4. 协议检测集成

HTTP/1.1中间件应优先检查协议检测结果：

```rust
// 方法1：检查协议检测中间件的结果（最优先）
if let Some(protocol) = context.custom_data.get("detected_protocol") {
    if protocol == "httpcleartext" { /* HTTP明文 */ }
    if protocol == "tls" { /* TLS协议，继续检查ALPN */ }
}

// 方法2：检查ALPN结果（仅HTTPS）
if let Some(alpn) = context.custom_data.get("alpn_protocol") {
    if alpn == "http/1.1" { /* HTTP/1.1 over TLS */ }
}
```

### 5. 配置管理

配置定义在 `src/core/model.rs` 的 `AppConfig`：

```rust
pub struct AppConfig {
    pub inbound: InboundConfig,   // 入站配置
    pub outbound: OutboundConfig, // 出站配置
    pub tls: TlsConfig,           // TLS配置
    pub script: ScriptConfig,     // 脚本配置
    pub general: GeneralConfig,   // 通用配置
}
```

**配置传递原则**:
- Engine接收完整 `AppConfig`
- 每个中间件从 `AppConfig` 提取自己需要的部分
- 使用 `from_config()` 构造方法

### 6. macOS pfctl 透明代理机制

**核心工作流程**:
```
应用程序发起连接 (curl https://google.com:443)
    ↓
pfctl rdr规则拦截 (端口80/443等)
    ↓
重定向到本地监听端口 (127.0.0.1:随机端口)
    ↓
PfInboundAdapter接收连接
    ↓
解析原始目标地址和协议 (TransparentConnectionParser)
    ↓
创建Connection对象 → 进入中间件管道
```

**关键组件**:
- `TransparentProxyManager`: 管理pfctl规则生命周期
- `TransparentConnectionParser`: 从TCP流解析协议和目标信息
- `ProcessFinder`: 查询连接所属进程（PID、进程名、用户）
- `UserManager`: 管理代理用户，防止回环

**pfctl规则示例**:
```bash
# 重定向规则
rdr pass on lo0 inet proto tcp from any to any port {80, 443} -> 127.0.0.1 port <随机端口>

# 绕过代理用户（防止回环）
pass out quick proto tcp from any to any user _takeover_proxy
```

**防回环机制**:
1. 创建专用代理用户 `_takeover_proxy`
2. pfctl规则绕过该用户的出站流量
3. 代理程序切换到该用户身份发起出站连接

**协议检测**:
- 从TCP流中peek数据，识别HTTP/HTTPS/其他协议
- 解析HTTP请求头获取目标主机名和完整URL
- 将解析结果存入 `Connection.metadata.custom_data`

## 关键文件速查

| 文件 | 用途 |
|------|------|
| `src/core/mod.rs` | AppCore核心编排器，全局单例 |
| `src/core/engine.rs` | 中间件管道引擎 |
| `src/core/model.rs` | 所有数据结构定义 |
| `src/core/ports.rs` | 端口接口定义 |
| `src/adapters/inbound/pf.rs` | macOS pfctl透明代理实现 |
| `src/adapters/outbound/direct.rs` | 直连出站适配器 |
| `src/core/middleware/tls.rs` | TLS拦截中间件 |
| `src/core/middleware/http2_stream.rs` | HTTP/2处理 |
| `src/core/middleware/forwarding.rs` | 请求转发中间件 |
| `.cursor/rules/ARCHITECTURE.mdc` | 详细架构文档（中文） |

## 常见任务

### 添加新中间件

1. 在 `src/core/middleware/` 创建新文件
2. 实现 `Middleware` trait
3. 在 `src/core/middleware/mod.rs` 导出
4. 在 `ProcessingEngine::create_default_middlewares_with_config()` 注册

### 添加新适配器

1. 在 `src/adapters/` 对应目录创建文件
2. 实现对应的 Port trait（如 `OutboundPort`）
3. 在 `main.rs` 或测试中实例化并注册到 `AppCore`

### 修改协议处理逻辑

- **HTTP/2**: 修改 `src/core/middleware/http2_stream.rs`
- **HTTP/1.1**: 修改 `src/core/middleware/http1_stream.rs`
- **TLS拦截**: 修改 `src/core/middleware/tls.rs`

## 注意事项

1. **需要sudo权限**: pfctl操作和TLS证书安装需要root权限
2. **用户切换**: 使用 `UserManager` 防止代理回环（创建 `_takeover_proxy` 用户）
3. **流所有权**: `TcpStream` 和 `TlsStream` 不能clone，使用 `Option::take()` 转移所有权
4. **HTTP/2特殊性**: HTTP/2中间件会 `Stop` 管道，由hyper接管完整生命周期
5. **响应写回**: 仅HTTP/1.1需要显式写回，HTTP/2由hyper自动处理

## 参考文档

- 架构设计: `.cursor/rules/ARCHITECTURE.mdc`
- 编码规范: `.cursor/rules/coding.mdc`
- 工具使用: `.cursor/rules/tools.mdc`
- 测试规范: `.cursor/rules/TEST.mdc`
- README: `README.md`
