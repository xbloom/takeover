# macOS & Windows 跨平台代理方案重新设计

## 当前平台限制分析

### macOS 严重限制
```rust
// 当前代码显示的限制
#[cfg(target_os = "macos")]
// 使用 pfctl，需要 root 权限，且有诸多限制
```

**macOS 具体限制:**
- **System Integrity Protection (SIP)**: 保护系统应用不被劫持
- **pfctl 权限要求**: 需要管理员权限，用户体验差
- **粗粒度拦截**: 当前规则影响整个网络接口，不是进程特定
- **应用沙盒**: App Store 应用受沙盒保护
- **Gatekeeper**: 可能阻止未签名的网络拦截工具

### Windows 完全缺失
```rust
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
{
    Err(TakeoverError::NetworkInterception("Unsupported platform".to_string()).into())
}
```

**Windows 技术挑战:**
- **驱动签名要求**: WinDivert/WinPcap 需要签名驱动
- **UAC 提升**: 需要管理员权限
- **Windows Defender**: 可能误报网络拦截工具
- **版本兼容性**: Windows 7-11 API 差异

---

## 🎯 跨平台可行方案重排序

### 🏆 方案1: 用户态代理自动配置 (推荐)

#### 核心思路
**不拦截系统网络，而是让应用主动连接到我们的代理**

```rust
struct SmartProxyManager {
    // 智能代理服务器，支持多协议
    proxy_servers: HashMap<ProxyProtocol, ProxyServer>,
    // 应用配置注入器
    config_injectors: HashMap<String, ConfigInjector>,
    // 环境变量管理器
    env_manager: EnvironmentManager,
}

enum ProxyProtocol {
    Http,
    Socks5,
    Socks4,
}

impl SmartProxyManager {
    async fn setup_application_proxy(&mut self, app_info: &ApplicationInfo) -> Result<()> {
        match app_info.app_type {
            AppType::BrowserBased => {
                // 修改浏览器代理设置或启动参数
                self.setup_browser_proxy(app_info).await?
            }
            AppType::EnvironmentAware => {
                // 设置 HTTP_PROXY, HTTPS_PROXY 环境变量
                self.setup_environment_proxy(app_info).await?
            }
            AppType::ConfigFile => {
                // 修改应用的配置文件
                self.setup_config_file_proxy(app_info).await?
            }
            AppType::CommandLine => {
                // 通过启动参数传递代理设置
                self.setup_commandline_proxy(app_info).await?
            }
        }
    }
}
```

#### 平台实现策略

**macOS:**
```rust
impl MacOSProxyManager {
    // 通过 System Preferences 设置系统代理
    async fn set_system_proxy(&self, enabled: bool) -> Result<()> {
        Command::new("networksetup")
            .args(&["-setautoproxyurl", "Wi-Fi", "http://127.0.0.1:8080/proxy.pac"])
            .output()?;
    }
    
    // 为特定应用设置环境变量
    async fn launch_with_proxy_env(&self, app_path: &str) -> Result<()> {
        Command::new("open")
            .arg(app_path)
            .env("HTTP_PROXY", "127.0.0.1:8080")
            .env("HTTPS_PROXY", "127.0.0.1:8080")
            .spawn()?;
    }
}
```

**Windows:**
```rust
impl WindowsProxyManager {
    // 通过注册表设置系统代理
    async fn set_system_proxy(&self, enabled: bool) -> Result<()> {
        use winreg::RegKey;
        let hkcu = RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        let settings = hkcu.open_subkey_with_flags(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings",
            winreg::enums::KEY_WRITE
        )?;
        
        settings.set_value("ProxyEnable", &(enabled as u32))?;
        settings.set_value("ProxyServer", &"127.0.0.1:8080")?;
    }
    
    // 通过 CreateProcess 启动应用并设置环境
    async fn launch_with_proxy_env(&self, app_path: &str) -> Result<()> {
        // 使用 Windows API 设置进程环境变量
    }
}
```

#### 优势
- **零权限要求**: 不需要管理员权限
- **用户友好**: 应用感知代理存在，兼容性更好
- **跨平台一致**: 同一套逻辑适用所有平台
- **无时序问题**: 应用主动连接，无竞态条件

---

### 🎯 方案2: PAC (Proxy Auto-Configuration) 智能路由

#### 技术原理
```rust
struct PACProxyManager {
    pac_server: PACServer,
    rule_engine: ProxyRuleEngine,
    target_processes: HashSet<String>,
}

impl PACProxyManager {
    async fn generate_pac_script(&self) -> String {
        format!(r#"
function FindProxyForURL(url, host) {{
    // 检查是否是目标应用的请求
    if (isTargetApplication()) {{
        return "PROXY 127.0.0.1:{}";
    }}
    return "DIRECT";
}}

function isTargetApplication() {{
    // 基于 User-Agent 或其他特征识别目标应用
    return navigator.userAgent.indexOf("{}") !== -1;
}}
        "#, self.proxy_port, self.target_app_signature)
    }
    
    async fn setup_pac_proxy(&mut self) -> Result<()> {
        // 1. 启动 PAC 服务器
        self.pac_server.start().await?;
        
        // 2. 设置系统 PAC URL
        self.set_system_pac_url("http://127.0.0.1:8080/proxy.pac").await?;
        
        Ok(())
    }
}
```

#### 跨平台实现
```rust
trait PlatformPACManager {
    async fn set_pac_url(&self, url: &str) -> Result<()>;
    async fn get_current_pac_url(&self) -> Result<Option<String>>;
    async fn restore_original_settings(&self) -> Result<()>;
}

#[cfg(target_os = "macos")]
impl PlatformPACManager for MacOSPACManager {
    async fn set_pac_url(&self, url: &str) -> Result<()> {
        Command::new("networksetup")
            .args(&["-setautoproxyurl", "Wi-Fi", url])
            .output()?;
    }
}

#[cfg(target_os = "windows")]
impl PlatformPACManager for WindowsPACManager {
    async fn set_pac_url(&self, url: &str) -> Result<()> {
        // 修改注册表设置 PAC URL
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let settings = hkcu.open_subkey_with_flags(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings",
            KEY_WRITE
        )?;
        
        settings.set_value("AutoConfigURL", &url)?;
        settings.set_value("ProxyEnable", &0u32)?; // 禁用静态代理
    }
}
```

---

### 🔧 方案3: 进程启动拦截 + 配置注入

#### 实现架构
```rust
struct ProcessLaunchInterceptor {
    monitored_apps: HashMap<String, ProxyConfig>,
    launch_hooks: Vec<LaunchHook>,
}

enum LaunchHook {
    CommandLineModification,
    EnvironmentInjection,
    ConfigFileModification,
    RegistryModification, // Windows only
}

impl ProcessLaunchInterceptor {
    async fn monitor_process_launches(&mut self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            self.monitor_launch_services().await?;
        }
        
        #[cfg(target_os = "windows")]
        {
            self.monitor_process_creation().await?;
        }
    }
    
    async fn intercept_launch(&mut self, process_info: &ProcessInfo) -> Result<()> {
        if let Some(proxy_config) = self.monitored_apps.get(&process_info.name) {
            // 在进程实际启动前注入代理配置
            self.inject_proxy_config(process_info, proxy_config).await?;
        }
    }
}
```

**macOS 实现:**
```rust
#[cfg(target_os = "macos")]
impl ProcessLaunchInterceptor {
    async fn monitor_launch_services(&mut self) -> Result<()> {
        // 使用 Launch Services 监控应用启动
        // 或者监控 /Applications 目录的进程启动
    }
    
    async fn inject_proxy_config(&mut self, process_info: &ProcessInfo, config: &ProxyConfig) -> Result<()> {
        match process_info.app_type {
            AppType::Browser => {
                // 修改 Chrome/Firefox 启动参数
                let proxy_arg = format!("--proxy-server=socks5://127.0.0.1:{}", config.port);
                // 重新启动应用时添加参数
            }
            AppType::ElectronApp => {
                // 注入 Electron 代理配置
            }
        }
    }
}
```

**Windows 实现:**
```rust
#[cfg(target_os = "windows")]
impl ProcessLaunchInterceptor {
    async fn monitor_process_creation(&mut self) -> Result<()> {
        // 使用 WMI 监控进程创建事件
        // 或者使用 SetWindowsHookEx 拦截进程启动
    }
}
```

---

## 🎯 最优方案选择矩阵

| 方案 | macOS可行性 | Windows可行性 | 用户体验 | 技术复杂度 | 时序问题 |
|------|------------|-------------|----------|-----------|---------|
| **用户态代理配置** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐ | ⭐⭐⭐⭐⭐ |
| **PAC 智能路由** | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐⭐⭐⭐ |
| **进程启动拦截** | ⭐⭐⭐ | ⭐⭐ | ⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ |

## 🏆 推荐实施方案

### Phase 1: 用户态代理配置 (立即可行)
- 实现智能代理服务器
- 支持环境变量注入
- 添加常见应用的配置模板

### Phase 2: PAC 智能路由 (增强透明度)
- 实现 PAC 服务器
- 添加智能路由规则
- 支持自动应用检测

### Phase 3: 进程启动拦截 (高级功能)
- 监控目标应用启动
- 自动注入代理配置
- 实现无感知代理

## 💻 具体技术栈

```toml
[dependencies]
# 跨平台系统 API
sysinfo = "0.29"
directories = "5.0"

# Windows 特定
[target.'cfg(windows)'.dependencies]
winreg = "0.12"
windows = "0.48"

# macOS 特定
[target.'cfg(target_os = "macos")'.dependencies]
core-foundation = "0.9"
security-framework = "2.9"

# 网络代理
tokio = { version = "1.0", features = ["full"] }
hyper = "0.14"
socks = "0.3"
```

## 🎯 解决时序问题的关键

### 1. 预启动代理服务器
```rust
struct ProxyPool {
    available_proxies: VecDeque<ProxyServer>,
    min_pool_size: usize,
}

impl ProxyPool {
    async fn ensure_available(&mut self) -> Result<ProxyServer> {
        if let Some(proxy) = self.available_proxies.pop_front() {
            // 立即可用，无启动延迟
            tokio::spawn(async { self.replenish_pool() });
            Ok(proxy)
        } else {
            // 池空了，立即创建
            self.create_new_proxy().await
        }
    }
}
```

### 2. 应用主动连接模式
```rust
// 不存在网络拦截规则生效延迟
// 应用直接连接到已就绪的代理服务器
async fn setup_application(&mut self, app: &str) -> Result<()> {
    // 1. 获取预启动的代理服务器
    let proxy = self.proxy_pool.get_available().await?;
    
    // 2. 配置应用使用该代理
    self.configure_application(app, proxy.endpoint()).await?;
    
    // 3. 应用下次启动时直接连接到就绪的代理
    Ok(())
}
```

**结论**: 针对 macOS 和 Windows 的限制，**用户态代理配置 + PAC 智能路由** 是最可行的方案，完全避免了系统级网络拦截的时序问题和权限要求。
