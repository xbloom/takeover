# 高层透明代理方案 - 不破坏进程的优雅拦截

既然不希望破坏进程，又要比TUN/TAP网络层更高层，让我提供一些在传输层和应用层的优雅解决方案。

---

## 🎯 方案1: LD_PRELOAD / DLL替换 (推荐)

### 核心思路
**不修改进程，而是替换系统库函数**

```rust
// 我们的代理库 - 替换系统网络库
// libproxy_intercept.so (Linux) / proxy_intercept.dll (Windows)

struct LibraryInterceptor {
    original_functions: OriginalNetworkFunctions,
    proxy_config: ProxyConfig,
    target_processes: HashSet<String>,
}

// 在我们的共享库中实现网络函数
#[no_mangle]
pub extern "C" fn connect(socket: c_int, address: *const sockaddr, len: socklen_t) -> c_int {
    // 1. 检查是否是目标进程
    if is_target_process() {
        // 2. 修改连接地址到我们的代理
        let proxy_addr = create_proxy_address();
        return original_connect(socket, &proxy_addr, len);
    }
    
    // 3. 非目标进程，正常连接
    original_connect(socket, address, len)
}

#[no_mangle]  
pub extern "C" fn send(socket: c_int, buffer: *const c_void, length: size_t, flags: c_int) -> ssize_t {
    // 传输层拦截发送数据
    if is_target_process() {
        return proxy_send(socket, buffer, length, flags);
    }
    original_send(socket, buffer, length, flags)
}
```

### 跨平台实现

**Linux (LD_PRELOAD):**
```rust
impl LibraryInterceptor {
    pub fn setup_for_process(&self, process_name: &str) -> Result<()> {
        // 1. 编译我们的拦截库
        self.build_intercept_library()?;
        
        // 2. 启动目标进程时设置环境变量
        Command::new(process_name)
            .env("LD_PRELOAD", "./libproxy_intercept.so")
            .spawn()?;
        
        Ok(())
    }
}
```

**Windows (DLL替换):**
```rust
#[cfg(windows)]
impl LibraryInterceptor {
    pub fn setup_for_process(&self, process_name: &str) -> Result<()> {
        // 1. 将我们的DLL命名为系统DLL名称
        // 比如 ws2_32.dll -> ws2_32_original.dll
        // 我们的DLL -> ws2_32.dll
        
        // 2. 在应用目录放置我们的代理DLL
        self.deploy_proxy_dll(process_name)?;
        
        Ok(())
    }
}
```

**macOS (DYLD_INSERT_LIBRARIES):**
```rust
#[cfg(target_os = "macos")]
impl LibraryInterceptor {
    pub fn setup_for_process(&self, process_name: &str) -> Result<()> {
        Command::new(process_name)
            .env("DYLD_INSERT_LIBRARIES", "./libproxy_intercept.dylib")
            .spawn()?;
        
        Ok(())
    }
}
```

**优势:**
- **不破坏进程**: 只是替换库函数
- **传输层拦截**: 比TUN/TAP高一层
- **完全透明**: 应用无感知
- **相对安全**: 不直接修改进程内存

---

## 🌐 方案2: DNS劫持 + 智能重定向

### 技术架构
```rust
struct DNSProxy {
    dns_server: DNSServer,
    domain_rules: HashMap<String, ProxyRule>,
    reverse_proxy: ReverseProxyServer,
}

impl DNSProxy {
    async fn setup_dns_interception(&mut self) -> Result<()> {
        // 1. 启动我们的DNS服务器
        self.dns_server.bind("127.0.0.1:53").await?;
        
        // 2. 修改系统DNS设置指向我们
        self.set_system_dns("127.0.0.1").await?;
        
        // 3. 启动反向代理服务器
        self.reverse_proxy.start().await?;
    }
    
    async fn handle_dns_query(&mut self, query: DNSQuery) -> Result<DNSResponse> {
        let domain = &query.domain;
        
        // 1. 检查是否是目标进程的查询
        if self.is_target_process_query(&query) {
            // 2. 返回我们反向代理的地址
            return Ok(DNSResponse {
                domain: domain.clone(),
                ip: "127.0.0.1".parse()?,
                ttl: 60,
            });
        }
        
        // 3. 非目标进程，返回真实DNS解析
        self.resolve_real_dns(domain).await
    }
    
    async fn handle_reverse_proxy(&self, request: HttpRequest) -> Result<()> {
        // 1. 从DNS查询记录中找到真实目标地址
        let real_target = self.get_real_target(&request.host)?;
        
        // 2. 通过指定的代理转发请求
        let proxy_client = ProxyClient::new(&self.proxy_config);
        let response = proxy_client.forward(real_target, request).await?;
        
        // 3. 返回响应给应用
        Ok(response)
    }
}
```

### DNS + HTTP/HTTPS 完整拦截
```rust
struct ApplicationLayerProxy {
    dns_interceptor: DNSProxy,
    http_proxy: HttpProxy,
    https_proxy: HttpsProxy,
    socks_proxy: SocksProxy,
}

impl ApplicationLayerProxy {
    async fn setup_complete_interception(&mut self, process_name: &str) -> Result<()> {
        // 1. DNS层：劫持域名解析
        self.dns_interceptor.add_target_process(process_name)?;
        
        // 2. HTTP层：反向代理HTTP请求
        self.http_proxy.listen_on("127.0.0.1:80").await?;
        
        // 3. HTTPS层：透明HTTPS代理
        self.https_proxy.listen_on("127.0.0.1:443").await?;
        
        // 4. 其他协议：通用SOCKS代理
        self.socks_proxy.listen_on("127.0.0.1:1080").await?;
    }
}
```

**优势:**
- **应用层拦截**: 最高层，最容易理解
- **协议感知**: 可以解析HTTP/HTTPS内容
- **不破坏进程**: 只是DNS重定向
- **调试友好**: 可以看到完整的HTTP请求

---

## 🔌 方案3: Socket层过滤驱动 (Windows WFP)

### Windows Filtering Platform
```rust
#[cfg(windows)]
struct WFPInterceptor {
    filter_engine: FilterEngine,
    callout_driver: CalloutDriver,
    proxy_redirector: ProxyRedirector,
}

#[cfg(windows)]
impl WFPInterceptor {
    async fn setup_wfp_filters(&mut self, pid: u32) -> Result<()> {
        // 1. 创建WFP过滤器
        let filter = FilterCondition::new()
            .layer(FWPM_LAYER_ALE_CONNECT_REDIRECT_V4)
            .condition(FWPM_CONDITION_ALE_APP_ID, pid)?;
        
        // 2. 注册Callout函数
        let callout = Callout::new()
            .key(GUID_PROXY_CALLOUT)
            .layer(FWPM_LAYER_ALE_CONNECT_REDIRECT_V4)
            .callback(Self::redirect_callback);
        
        self.filter_engine.add_callout(callout)?;
        self.filter_engine.add_filter(filter)?;
    }
    
    // Callout回调函数
    extern "system" fn redirect_callback(
        context: *const c_void,
        fixed_values: *const FWPS_INCOMING_VALUES0,
        metadata: *const FWPS_INCOMING_METADATA_VALUES0,
        layer_data: *mut c_void,
        classify_out: *mut FWPS_CLASSIFY_OUT0,
    ) {
        // 1. 检查连接信息
        let original_dest = extract_destination_from_metadata(metadata);
        
        // 2. 重定向到我们的代理
        let proxy_dest = SocketAddress::new("127.0.0.1", 10080);
        redirect_connection(layer_data, proxy_dest);
        
        // 3. 允许连接继续
        classify_out.actionType = FWP_ACTION_PERMIT;
    }
}
```

**macOS 类似方案 (Network Extension):**
```rust
#[cfg(target_os = "macos")]
struct NetworkExtensionProxy {
    tunnel_provider: NEPacketTunnelProvider,
    flow_divert: NEFlowDivert,
}

#[cfg(target_os = "macos")]
impl NetworkExtensionProxy {
    async fn setup_network_extension(&mut self) -> Result<()> {
        // 1. 创建Network Extension
        let config = NETunnelProviderManager::new();
        config.set_protocol_configuration(tunnel_config);
        
        // 2. 注册流量重定向规则
        let rule = NEOnDemandRule::new();
        rule.add_process_rule(target_process_name);
        
        config.set_on_demand_rules(&[rule]);
        config.save_to_preferences().await?;
    }
}
```

**优势:**
- **系统级支持**: 操作系统原生API
- **高性能**: 内核级过滤
- **不破坏进程**: 在系统层面重定向
- **精确控制**: 可以基于进程ID过滤

---

## 🕷️ 方案4: 应用协议网关

### HTTP/HTTPS 透明网关
```rust
struct ApplicationGateway {
    http_gateway: HttpGateway,
    https_gateway: HttpsGateway,
    tcp_gateway: TcpGateway,
}

impl ApplicationGateway {
    async fn setup_transparent_gateways(&mut self) -> Result<()> {
        // 1. HTTP透明网关 (端口80)
        self.http_gateway.listen_transparent("0.0.0.0:80").await?;
        
        // 2. HTTPS透明网关 (端口443) 
        self.https_gateway.listen_transparent("0.0.0.0:443").await?;
        
        // 3. 通用TCP网关 (其他端口)
        self.tcp_gateway.listen_on_all_ports().await?;
        
        // 4. 设置iptables重定向规则(仅针对目标进程)
        self.setup_process_specific_redirect().await?;
    }
    
    async fn handle_http_request(&self, req: HttpRequest) -> Result<()> {
        // 1. 解析HTTP请求
        let target_host = req.headers().get("Host").unwrap();
        let method = req.method();
        let path = req.uri().path();
        
        // 2. 通过代理转发
        let proxy_client = HttpProxyClient::new(&self.proxy_config);
        let response = proxy_client.forward(target_host, req).await?;
        
        // 3. 返回响应
        Ok(response)
    }
    
    async fn handle_https_request(&self, stream: TcpStream) -> Result<()> {
        // 1. TLS握手透传
        let mut tls_stream = self.establish_tls_tunnel(stream).await?;
        
        // 2. 通过代理建立隧道连接
        let mut proxy_stream = self.connect_via_proxy().await?;
        
        // 3. 数据双向转发
        tokio::io::copy_bidirectional(&mut tls_stream, &mut proxy_stream).await?;
        
        Ok(())
    }
}
```

### 智能协议检测
```rust
struct ProtocolDetector {
    detectors: Vec<Box<dyn ProtocolDetector>>,
}

impl ProtocolDetector {
    fn detect_protocol(&self, initial_data: &[u8]) -> Protocol {
        // HTTP检测
        if initial_data.starts_with(b"GET ") || 
           initial_data.starts_with(b"POST ") ||
           initial_data.starts_with(b"PUT ") {
            return Protocol::Http;
        }
        
        // HTTPS/TLS检测
        if initial_data[0] == 0x16 && initial_data[1] == 0x03 {
            return Protocol::Tls;
        }
        
        // SOCKS检测
        if initial_data[0] == 0x05 {
            return Protocol::Socks5;
        }
        
        // 默认TCP
        Protocol::Tcp
    }
    
    async fn handle_by_protocol(&self, protocol: Protocol, stream: TcpStream) -> Result<()> {
        match protocol {
            Protocol::Http => self.handle_http(stream).await,
            Protocol::Tls => self.handle_tls_tunnel(stream).await,
            Protocol::Socks5 => self.handle_socks_proxy(stream).await,
            Protocol::Tcp => self.handle_raw_tcp(stream).await,
        }
    }
}
```

---

## 📊 方案对比表

| 方案 | 层级 | 不破坏进程 | 透明度 | 实现复杂度 | 跨平台 |
|------|------|-----------|--------|------------|--------|
| **LD_PRELOAD库替换** | 传输层 | ✅ | ⭐⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐⭐⭐ |
| **DNS劫持+重定向** | 应用层 | ✅ | ⭐⭐⭐⭐ | ⭐⭐ | ⭐⭐⭐⭐⭐ |
| **WFP过滤驱动** | 传输层 | ✅ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐ |
| **应用协议网关** | 应用层 | ✅ | ⭐⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐⭐⭐ |

## 🏆 推荐方案

### 首选: DNS劫持 + 应用层网关
- **最高层**: 应用层，比TUN/TAP高很多
- **不破坏进程**: 完全外部拦截
- **用户友好**: 可以看到完整的HTTP请求/响应
- **易调试**: 所有流量都可见

### 备选: LD_PRELOAD 库替换  
- **传输层**: 比网络层高一层
- **相对安全**: 不直接修改进程
- **性能好**: 在socket层拦截

### 高级: WFP/Network Extension
- **系统原生**: 操作系统官方支持
- **最稳定**: 不会导致崩溃
- **精确控制**: 基于进程ID过滤

这些方案都完全避免了：
- ❌ 进程注入/Hook
- ❌ 内存修改  
- ❌ 时序竞态问题
- ✅ 优雅的高层拦截

您觉得哪种方案比较合适？
