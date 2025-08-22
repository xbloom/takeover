# macOS curl 拦截实现方案

目标：在macOS下让curl访问能被代理拦截处理，解决当前架构的时序问题。

---

## 🎯 curl 拦截的特殊考虑

curl作为命令行工具有以下特点：
- **无Bundle ID**: 不适用于Network Extension的应用规则
- **短生命周期**: 执行完成后立即退出
- **系统工具**: 位于`/usr/bin/curl`，受SIP保护
- **多种协议**: 支持HTTP/HTTPS/FTP等多种协议
- **代理感知**: 原生支持多种代理配置方式

## 🏆 三层方案设计

### 方案1: 环境变量代理 (最简单) ⭐⭐⭐⭐⭐

```rust
// 环境变量代理实现
struct MacOSEnvProxyManager {
    proxy_config: ProxyConfig,
    proxy_server: Option<LocalProxyServer>,
    intercepted_commands: HashSet<String>,
}

impl MacOSEnvProxyManager {
    pub async fn new(proxy_config: ProxyConfig) -> Result<Self> {
        Ok(Self {
            proxy_config,
            proxy_server: None,
            intercepted_commands: HashSet::from([
                "curl".to_string(),
                "wget".to_string(), 
                "git".to_string(),
                "npm".to_string(),
            ]),
        })
    }
    
    // 启动本地代理服务器
    pub async fn start_local_proxy(&mut self) -> Result<u16> {
        let proxy_server = LocalProxyServer::new()
            .bind("127.0.0.1:0")?  // 自动分配端口
            .target_proxy(&self.proxy_config.url)?
            .start().await?;
            
        let local_port = proxy_server.local_port();
        self.proxy_server = Some(proxy_server);
        
        info!("Local proxy server started on port: {}", local_port);
        Ok(local_port)
    }
    
    // 拦截curl命令
    pub async fn intercept_curl(&self, original_args: Vec<String>) -> Result<Vec<String>> {
        // 检查是否已经设置了代理
        if original_args.iter().any(|arg| arg.starts_with("--proxy")) {
            return Ok(original_args); // 已有代理设置，不修改
        }
        
        let local_proxy_port = self.proxy_server.as_ref()
            .ok_or_else(|| anyhow!("Proxy server not started"))?
            .local_port();
            
        // 添加代理参数
        let mut new_args = original_args;
        new_args.push("--proxy".to_string());
        
        match self.proxy_config.proxy_type {
            ProxyType::Socks5 => {
                new_args.push(format!("socks5://127.0.0.1:{}", local_proxy_port));
            }
            ProxyType::Http => {
                new_args.push(format!("http://127.0.0.1:{}", local_proxy_port));
            }
            _ => return Err(anyhow!("Unsupported proxy type for curl")),
        }
        
        info!("Modified curl command with proxy: --proxy {}", new_args.last().unwrap());
        Ok(new_args)
    }
    
    // 设置环境变量 (适用于所有支持代理的命令)
    pub fn get_proxy_env_vars(&self) -> HashMap<String, String> {
        let local_proxy_port = match &self.proxy_server {
            Some(server) => server.local_port(),
            None => return HashMap::new(),
        };
        
        let proxy_url = match self.proxy_config.proxy_type {
            ProxyType::Http => format!("http://127.0.0.1:{}", local_proxy_port),
            ProxyType::Socks5 => format!("socks5://127.0.0.1:{}", local_proxy_port),
            _ => return HashMap::new(),
        };
        
        HashMap::from([
            ("HTTP_PROXY".to_string(), proxy_url.clone()),
            ("HTTPS_PROXY".to_string(), proxy_url.clone()),
            ("http_proxy".to_string(), proxy_url.clone()),
            ("https_proxy".to_string(), proxy_url.clone()),
            ("ALL_PROXY".to_string(), proxy_url),
        ])
    }
}
```

### 使用示例1: 环境变量方式

```rust
#[tokio::main]
async fn main() -> Result<()> {
    let proxy_config = ProxyConfig {
        url: "socks5://127.0.0.1:1080".to_string(),
        proxy_type: ProxyType::Socks5,
    };
    
    let mut env_proxy = MacOSEnvProxyManager::new(proxy_config).await?;
    let local_port = env_proxy.start_local_proxy().await?;
    
    println!("✅ Proxy server ready on port: {}", local_port);
    
    // 方式1: 直接修改curl命令
    let original_curl = vec!["curl".to_string(), "-v".to_string(), "https://httpbin.org/ip".to_string()];
    let proxied_curl = env_proxy.intercept_curl(original_curl).await?;
    
    println!("Original: curl -v https://httpbin.org/ip");
    println!("Proxied:  {}", proxied_curl.join(" "));
    
    // 方式2: 环境变量设置
    let env_vars = env_proxy.get_proxy_env_vars();
    for (key, value) in &env_vars {
        println!("export {}={}", key, value);
    }
    
    // 执行代理后的curl
    let output = Command::new(&proxied_curl[0])
        .args(&proxied_curl[1..])
        .output()?;
        
    println!("Response: {}", String::from_utf8_lossy(&output.stdout));
    
    Ok(())
}
```

---

### 方案2: DYLD_INSERT_LIBRARIES Hook (需要包装执行) ⭐⭐

```rust
// macOS动态库注入实现
struct MacOSDylibInterceptor {
    hook_dylib_path: PathBuf,
    proxy_config: ProxyConfig,
    proxy_server: Option<LocalProxyServer>,
}

impl MacOSDylibInterceptor {
    pub async fn new(proxy_config: ProxyConfig) -> Result<Self> {
        let hook_dylib_path = Self::build_hook_dylib().await?;
        
        Ok(Self {
            hook_dylib_path,
            proxy_config,
            proxy_server: None,
        })
    }
    
    // 构建Hook动态库
    async fn build_hook_dylib() -> Result<PathBuf> {
        let hook_source = r#"
// libcurl_hook.c - 拦截curl的网络调用
#include <dlfcn.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <stdio.h>
#include <string.h>

// 保存原始函数
static int (*original_connect)(int, const struct sockaddr *, socklen_t) = NULL;

// Hook初始化
__attribute__((constructor))
void init_hook() {
    original_connect = dlsym(RTLD_NEXT, "connect");
    if (!original_connect) {
        fprintf(stderr, "Failed to find original connect function\n");
    }
    printf("🔗 Curl hook library loaded\n");
}

// 拦截connect系统调用
int connect(int socket, const struct sockaddr *address, socklen_t address_len) {
    struct sockaddr_in *addr = (struct sockaddr_in *)address;
    
    // 检查是否是HTTP/HTTPS端口
    int port = ntohs(addr->sin_port);
    if (port == 80 || port == 443 || port == 8080) {
        printf("🎯 Intercepting connection to port %d\n", port);
        
        // 重定向到本地代理服务器
        addr->sin_addr.s_addr = inet_addr("127.0.0.1");
        addr->sin_port = htons(10080); // 我们的代理端口
        
        printf("➡️  Redirected to 127.0.0.1:10080\n");
    }
    
    return original_connect(socket, address, address_len);
}
        "#;
        
        // 写入临时文件
        let source_path = "/tmp/libcurl_hook.c";
        std::fs::write(source_path, hook_source)?;
        
        // 编译动态库
        let dylib_path = "/tmp/libcurl_hook.dylib";
        let output = Command::new("clang")
            .args(&[
                "-shared",
                "-fPIC",
                "-o", dylib_path,
                source_path
            ])
            .output()?;
            
        if !output.status.success() {
            return Err(anyhow!("Failed to compile hook dylib: {}", 
                String::from_utf8_lossy(&output.stderr)));
        }
        
        println!("✅ Compiled hook dylib: {}", dylib_path);
        Ok(PathBuf::from(dylib_path))
    }
    
    // 启动代理服务器
    pub async fn start_proxy_server(&mut self) -> Result<u16> {
        let proxy_server = LocalProxyServer::new()
            .bind("127.0.0.1:10080")?  // 固定端口，与Hook中的端口一致
            .target_proxy(&self.proxy_config.url)?
            .start().await?;
            
        let local_port = proxy_server.local_port();
        self.proxy_server = Some(proxy_server);
        
        info!("Hook proxy server started on port: {}", local_port);
        Ok(local_port)
    }
    
    // 使用Hook执行curl
    pub async fn execute_hooked_curl(&self, args: Vec<String>) -> Result<std::process::Output> {
        // 启动curl并注入我们的动态库
        let output = Command::new("curl")
            .args(&args[1..]) // 跳过第一个"curl"
            .env("DYLD_INSERT_LIBRARIES", &self.hook_dylib_path)
            .env("DYLD_FORCE_FLAT_NAMESPACE", "1")  // 确保Hook生效
            .output()?;
            
        Ok(output)
    }
}
```

### 使用示例2: 动态库Hook方式

```rust
#[tokio::main]
async fn main() -> Result<()> {
    let proxy_config = ProxyConfig {
        url: "socks5://127.0.0.1:1080".to_string(),
        proxy_type: ProxyType::Socks5,
    };
    
    let mut dylib_interceptor = MacOSDylibInterceptor::new(proxy_config).await?;
    
    // 启动代理服务器
    let proxy_port = dylib_interceptor.start_proxy_server().await?;
    println!("✅ Hook proxy server ready on port: {}", proxy_port);
    
    // 通过我们的程序执行curl - 用户需要改变使用方式
    let curl_args = vec![
        "curl".to_string(), 
        "-v".to_string(),
        "https://httpbin.org/ip".to_string()
    ];
    
    println!("🚀 Executing via our wrapper: {}", curl_args.join(" "));
    let output = dylib_interceptor.execute_hooked_curl(curl_args).await?;
    
    println!("Hook output:");
    println!("{}", String::from_utf8_lossy(&output.stderr)); // curl的-v输出在stderr
    println!("Response:");
    println!("{}", String::from_utf8_lossy(&output.stdout));
    
    Ok(())
}
```

### ⚠️ 方案2的问题分析

**为什么说"用户无感知"是错误的：**

1. **用户不能直接使用curl**: 用户无法在终端直接执行 `curl https://example.com`
2. **需要通过我们的程序**: 必须运行我们的包装程序来执行curl
3. **需要预编译动态库**: 用户需要安装开发工具并编译Hook库
4. **环境设置复杂**: 需要正确设置 `DYLD_INSERT_LIBRARIES` 和 `DYLD_FORCE_FLAT_NAMESPACE`
5. **兼容性问题**: 在新版macOS上，SIP可能阻止动态库注入

**真正的用户体验：**
```bash
# 用户期望的（但做不到）：
$ curl https://httpbin.org/ip  # 自动通过代理

# 实际需要的：
$ our_program execute-curl "curl https://httpbin.org/ip"  # 通过包装程序
```

**结论**: 方案2并不透明，用户感知度很高！

---

### 方案3: 改进的pfctl (解决时序问题) ⭐⭐⭐

```rust
// 改进的pfctl实现，解决时序问题
struct ImprovedPfctlInterceptor {
    proxy_server: Option<LocalProxyServer>,
    pfctl_anchor: String,
    ready_checker: ProxyReadinessChecker,
}

impl ImprovedPfctlInterceptor {
    pub async fn new(proxy_config: ProxyConfig) -> Result<Self> {
        Ok(Self {
            proxy_server: None,
            pfctl_anchor: "takeover_improved".to_string(),
            ready_checker: ProxyReadinessChecker::new(),
        })
    }
    
    // 解决时序问题的关键：确保代理服务器就绪
    pub async fn setup_interception(&mut self, proxy_config: ProxyConfig) -> Result<()> {
        // 1. 预启动代理服务器
        let proxy_server = LocalProxyServer::new()
            .bind("127.0.0.1:0")?
            .target_proxy(&proxy_config.url)?
            .start().await?;
            
        let local_port = proxy_server.local_port();
        self.proxy_server = Some(proxy_server);
        
        // 2. 等待代理服务器完全就绪
        self.ready_checker.wait_for_proxy_ready(local_port).await?;
        
        // 3. 现在设置pfctl规则 (代理已确认就绪)
        self.setup_pfctl_rules(local_port).await?;
        
        println!("✅ Improved pfctl setup complete - no timing issues!");
        Ok(())
    }
    
    async fn setup_pfctl_rules(&self, proxy_port: u16) -> Result<()> {
        // 创建pfctl规则文件
        let rules = format!(
            "# Improved takeover rules\n\
             rdr pass on lo0 inet proto tcp from any to any port 80 -> 127.0.0.1 port {}\n\
             rdr pass on lo0 inet proto tcp from any to any port 443 -> 127.0.0.1 port {}\n",
            proxy_port, proxy_port
        );
        
        let rule_file = "/tmp/takeover_improved.conf";
        std::fs::write(rule_file, &rules)?;
        
        // 加载pfctl规则
        let output = Command::new("pfctl")
            .args(&["-a", &self.pfctl_anchor, "-f", rule_file])
            .output()?;
            
        if !output.status.success() {
            return Err(anyhow!("Failed to load pfctl rules: {}", 
                String::from_utf8_lossy(&output.stderr)));
        }
        
        // 验证规则已加载
        self.verify_pfctl_rules().await?;
        
        Ok(())
    }
    
    async fn verify_pfctl_rules(&self) -> Result<()> {
        let output = Command::new("pfctl")
            .args(&["-a", &self.pfctl_anchor, "-s", "nat"])
            .output()?;
            
        let rules_output = String::from_utf8_lossy(&output.stdout);
        if rules_output.trim().is_empty() {
            return Err(anyhow!("pfctl rules not loaded properly"));
        }
        
        println!("✅ pfctl rules verified: {}", rules_output.lines().count());
        Ok(())
    }
}

// 代理就绪检查器
struct ProxyReadinessChecker;

impl ProxyReadinessChecker {
    pub fn new() -> Self {
        Self
    }
    
    // 等待代理服务器就绪
    pub async fn wait_for_proxy_ready(&self, port: u16) -> Result<()> {
        let max_attempts = 10;
        let delay = Duration::from_millis(100);
        
        for attempt in 1..=max_attempts {
            match self.check_proxy_connectivity(port).await {
                Ok(()) => {
                    println!("✅ Proxy server ready after {} attempts", attempt);
                    return Ok(());
                }
                Err(_) if attempt < max_attempts => {
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Err(e) => {
                    return Err(anyhow!("Proxy server not ready after {} attempts: {}", max_attempts, e));
                }
            }
        }
        
        Ok(())
    }
    
    async fn check_proxy_connectivity(&self, port: u16) -> Result<()> {
        let addr = format!("127.0.0.1:{}", port);
        let _stream = tokio::net::TcpStream::connect(&addr).await?;
        Ok(())
    }
}
```

---

## 🧪 测试验证

### 创建测试脚本

```bash
#!/bin/bash
# test_macos_curl.sh - 测试macOS curl拦截

echo "🧪 Testing macOS curl interception..."

# 测试1: 环境变量代理
echo "📝 Test 1: Environment variable proxy"
export HTTP_PROXY=socks5://127.0.0.1:10080
export HTTPS_PROXY=socks5://127.0.0.1:10080
curl -v https://httpbin.org/ip

# 测试2: 命令行代理参数
echo "📝 Test 2: Command line proxy"
curl --proxy socks5://127.0.0.1:10080 -v https://httpbin.org/ip

# 测试3: Hook透明代理
echo "📝 Test 3: Dynamic library hook"
DYLD_INSERT_LIBRARIES=./libcurl_hook.dylib curl -v https://httpbin.org/ip

echo "✅ All tests completed"
```

## 🎯 方案重新评估

### 真实的用户体验对比：

| 方案 | 用户感知度 | 使用方式 | 真正透明度 | 推荐度 |
|------|-----------|---------|-----------|--------|
| **方案1: 环境变量** | 🔴 很高 | 用户必须`export HTTP_PROXY=...`才能使用 | ⭐⭐ | ⭐⭐⭐ |
| **方案2: DYLD_HOOK** | 🔴 很高 | 必须通过我们的程序包装执行 | ⭐ | ⭐⭐ |
| **方案3: 改进pfctl** | 🟢 很低 | 直接使用curl，完全透明 | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ |

### 🔍 透明度重新分析

**什么是真正的"用户无感知"？**
```bash
# 用户期望的理想体验：
$ curl https://httpbin.org/ip    # 直接运行，自动通过代理，用户完全不知道
```

**各方案的真实用户体验：**

**方案1: 环境变量**
```bash
# 用户必须做的事：
$ export HTTP_PROXY=socks5://127.0.0.1:10080  # 用户感知度很高！
$ export HTTPS_PROXY=socks5://127.0.0.1:10080
$ curl https://httpbin.org/ip  # 然后才能正常使用
```

**方案2: DYLD_HOOK**  
```bash
# 用户必须做的事：
$ our_program execute-curl "curl https://httpbin.org/ip"  # 完全无法直接使用curl！
```

**方案3: 改进的pfctl**
```bash
# 用户直接使用：
$ curl https://httpbin.org/ip  # 就这样，什么都不用知道！系统自动拦截
```

### 🏆 最终结论

**只有改进的pfctl方案才是真正透明的！**

1. **唯一推荐: 改进的pfctl** ⭐⭐⭐⭐⭐
   - ✅ **真正无感知**: 用户直接使用curl，完全透明
   - ✅ **解决时序问题**: 预启动代理，等待就绪后设置规则  
   - ✅ **支持所有工具**: curl、wget、git、npm等全部生效
   - ✅ **一次设置**: 管理员权限设置后，所有用户受益
   - ⚠️ **需要root权限**: 但这是实现真正透明的必要代价

2. **其他方案都不够透明**:
   - 环境变量：用户必须手动设置，感知度很高
   - DYLD_HOOK：用户无法直接使用curl，感知度极高

## 🎯 重新聚焦：只实施 pfctl 方案

**您的分析完全正确！** 既然目标是真正的透明拦截，那么：

### 💡 新的实施策略

**直接实施改进的pfctl方案** - 这是唯一真正透明的解决方案

### 🚀 实施步骤 (一步到位)

**Phase 1: 核心pfctl拦截器 (2-3天)**
```rust
struct ImprovedPfctlInterceptor {
    proxy_server: LocalProxyServer,     // 预启动的代理服务器
    pfctl_rules: PfctlRuleManager,      // pfctl规则管理
    readiness_checker: ReadinessChecker, // 解决时序问题的关键
}

// 实施关键：
// 1. 先启动并等待代理服务器完全就绪
// 2. 再设置pfctl规则重定向流量
// 3. 用户直接运行 curl，完全透明
```

**Phase 2: 测试验证 (1天)**
```bash
# 测试场景：
sudo our_program start-transparent-proxy --target-proxy socks5://127.0.0.1:1080

# 然后所有程序都能直接使用，包括：
curl https://httpbin.org/ip        # ✅ 自动通过代理
wget https://example.com           # ✅ 自动通过代理  
git clone https://github.com/...   # ✅ 自动通过代理

# 🚀 Node.js 程序也完全透明！
node -e "require('https').get('https://httpbin.org/ip', res => res.pipe(process.stdout))"
npm install some-package            # ✅ npm下载也通过代理
yarn add express                    # ✅ yarn也生效

# 🚀 Python 程序也生效
python -c "import requests; print(requests.get('https://httpbin.org/ip').text)"
pip install requests                # ✅ pip也通过代理

# 🚀 浏览器也生效（如果拦截浏览器端口）
```

**Phase 3: 优化完善 (1天)**
- 错误处理和恢复机制
- 优雅的启动和关闭流程
- 自动清理规则

### ✅ 为什么这是正确的决定

1. **真正解决问题**: 实现了用户期望的透明体验
2. **一步到位**: 不浪费时间在"感知度高"的中间方案上
3. **解决时序问题**: 从根本上设计了正确的启动顺序

### 🎯 核心技术要点

**时序问题的彻底解决：**
```rust
async fn setup_transparent_proxy(&mut self) -> Result<()> {
    // 1. 启动本地代理服务器
    let proxy_server = self.start_proxy_server().await?;
    
    // 2. 等待服务器完全就绪 - 关键步骤！
    self.wait_for_proxy_ready(proxy_server.port()).await?;
    
    // 3. 现在设置pfctl规则（代理已确认就绪）
    self.setup_pfctl_rules(proxy_server.port()).await?;
    
    // ✅ 无竞态条件，用户可以立即使用curl
}
```

---

## 🚀 Node.js 程序透明代理详解

### ✅ 为什么 pfctl 对 Node.js 完全有效？

**pfctl 在系统网络层拦截，比应用层更底层：**

```bash
应用层(L7)     Node.js App -----> HTTP请求
传输层(L4)     TCP Socket  -----> 创建连接  
网络层(L3)     IP包        -----> 🎯 pfctl 在这里拦截！
```

**Node.js 的网络请求最终都会经过系统 socket API：**
- `http.get()` → `net.Socket` → 系统 `connect()` 调用
- `https.request()` → TLS over TCP → 系统 `connect()` 调用  
- `fetch()` → 底层还是 HTTP → 系统 `connect()` 调用

### 🎯 Node.js 测试用例

```javascript
// test-nodejs-proxy.js - 验证 Node.js 透明代理

// 测试1: 原生 http 模块
const http = require('http');
http.get('http://httpbin.org/ip', (res) => {
    let data = '';
    res.on('data', chunk => data += chunk);
    res.on('end', () => console.log('HTTP:', JSON.parse(data)));
});

// 测试2: 原生 https 模块  
const https = require('https');
https.get('https://httpbin.org/ip', (res) => {
    let data = '';
    res.on('data', chunk => data += chunk);
    res.on('end', () => console.log('HTTPS:', JSON.parse(data)));
});

// 测试3: axios (如果安装了)
const axios = require('axios');
axios.get('https://httpbin.org/ip')
    .then(res => console.log('Axios:', res.data))
    .catch(err => console.log('Axios error:', err.message));

// 测试4: fetch (Node.js 18+)
if (globalThis.fetch) {
    fetch('https://httpbin.org/ip')
        .then(res => res.json())
        .then(data => console.log('Fetch:', data))
        .catch(err => console.log('Fetch error:', err.message));
}
```

### 🛠️ pfctl 规则配置示例

```rust
// Node.js 常用端口的 pfctl 规则
let pfctl_rules = format!(
    "# Transparent proxy rules for all applications\n\
     rdr pass on lo0 inet proto tcp from any to any port 80 -> 127.0.0.1 port {}\n\
     rdr pass on lo0 inet proto tcp from any to any port 443 -> 127.0.0.1 port {}\n\
     rdr pass on lo0 inet proto tcp from any to any port 8080 -> 127.0.0.1 port {}\n\
     rdr pass on lo0 inet proto tcp from any to any port 3000 -> 127.0.0.1 port {}\n\
     rdr pass on lo0 inet proto tcp from any to any port 5000 -> 127.0.0.1 port {}\n",
    local_proxy_port, local_proxy_port, local_proxy_port, local_proxy_port, local_proxy_port
);
```

### ⚡ 特殊场景处理

**1. npm/yarn 包管理器**
```bash
# 完全透明，无需任何配置
npm install express     # ✅ 自动通过代理
yarn add react         # ✅ 自动通过代理
npm publish            # ✅ 发布也通过代理
```

**2. WebSocket 连接**
```javascript
// WebSocket 也会被拦截（如果设置了相应端口）
const WebSocket = require('ws');
const ws = new WebSocket('wss://echo.websocket.org');  // 透明代理
```

**3. 数据库连接**
```javascript
// 数据库连接也可以被代理（如果配置了相应端口）
const mysql = require('mysql2');
const connection = mysql.createConnection({
    host: 'remote-db.example.com',
    port: 3306,  // 需要在pfctl规则中添加此端口
    user: 'user',
    password: 'pass'
});
```

### 🎯 与其他方案对比

| 特性 | pfctl 透明代理 | Node.js 环境变量 | Node.js 代理配置 |
|------|---------------|-----------------|----------------|
| **透明度** | ⭐⭐⭐⭐⭐ 完全透明 | ⭐⭐ 需要设置 | ⭐ 需要修改代码 |
| **适用范围** | 所有 Node.js 库 | 支持环境变量的库 | 特定库 |
| **npm/yarn** | ✅ 自动生效 | ✅ 需要设置 | ❌ 需要单独配置 |
| **第三方库** | ✅ 全部支持 | ⭐ 部分支持 | ❌ 需要逐个配置 |

### 💡 最佳实践建议

1. **开发环境**: 使用 pfctl 透明代理，所有工具自动生效
2. **生产部署**: 仍然建议显式配置代理，便于调试和监控
3. **混合使用**: 透明代理作为默认，关键服务显式配置

**现在就开始实施改进的pfctl方案？** Node.js支持让这个方案更有价值！
