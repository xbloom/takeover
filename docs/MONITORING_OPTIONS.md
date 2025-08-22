# 进程监控方案选项

## 🚨 当前进程扫描方式的问题

### 从代码分析发现的限制：

```rust
// 当前实现的问题
pub async fn start_monitoring(&mut self) -> Result<()> {
    // 问题1: 2秒轮询间隔，反应太慢
    let mut interval = interval(Duration::from_secs(2));
    
    loop {
        interval.tick().await;
        self.scan_processes().await?; // 问题2: 依赖进程名匹配
    }
}
```

### 具体问题分析：

1. **⏰ 轮询延迟**: 2秒间隔，快速启动的进程可能错过
2. **📛 进程名匹配不准**: 依赖 `process.name()` 可能不准确
3. **🔒 权限限制**: 无法访问某些受保护的进程
4. **🏃 快速进程**: curl、wget等执行完就退出，难以捕获
5. **💻 系统负载**: 高负载时轮询性能差
6. **🔍 路径获取失败**: `get_process_executable_path()` 可能失败

---

## 🎯 更好的方案选项

### 选项1: 全局透明代理 (推荐) ⭐⭐⭐⭐⭐

**无需进程监控，直接拦截所有网络流量**

```rust
pub enum ProxyMode {
    /// 全局透明代理 - 拦截所有HTTP/HTTPS流量
    GlobalTransparent {
        ports: Vec<u16>, // 要拦截的端口列表
    },
}

impl TransparentProxy {
    pub async fn start_global_mode(&mut self, ports: Vec<u16>) -> Result<()> {
        // 1. 启动代理服务器
        let local_port = self.start_local_proxy_server().await?;
        
        // 2. 等待就绪
        self.readiness_checker.wait_for_proxy_ready(local_port).await?;
        
        // 3. 为所有指定端口设置重定向规则
        self.pfctl_rules.setup_global_rules(local_port, ports).await?;
        
        info!("✅ Global transparent proxy active for ports: {:?}", ports);
        Ok(())
    }
}
```

**使用示例：**
```bash
# 启动全局透明代理
sudo ./takeover --mode global --ports 80,443,8080,3000,5000

# 然后所有程序自动生效：
curl https://httpbin.org/ip      # ✅ 自动代理
node app.js                      # ✅ 自动代理
python requests_app.py           # ✅ 自动代理
```

**优势：**
- ✅ **零延迟**: 无需等待进程检测
- ✅ **100%覆盖**: 所有程序都被拦截
- ✅ **简单可靠**: 无复杂的进程匹配逻辑
- ✅ **适合开发**: 开发环境理想选择

---

### 选项2: 手动指定模式 ⭐⭐⭐⭐

**用户明确指定要代理的进程**

```rust
pub enum ProxyMode {
    /// 手动指定要代理的进程
    Manual {
        target_pids: Vec<u32>,
        target_commands: Vec<String>,
    },
}

impl TransparentProxy {
    pub async fn start_manual_mode(&mut self, target_pids: Vec<u32>) -> Result<()> {
        // 为指定的PID设置精确的拦截规则
        for pid in target_pids {
            self.setup_pid_specific_interception(pid).await?;
        }
    }
    
    pub async fn add_target_pid(&mut self, pid: u32) -> Result<()> {
        // 动态添加目标进程
        info!("Adding PID {} to proxy targets", pid);
        self.setup_pid_specific_interception(pid).await?;
    }
}
```

**使用示例：**
```bash
# 方式1: 指定PID
ps aux | grep firefox  # 查找firefox的PID
sudo ./takeover --mode manual --pid 12345

# 方式2: 启动程序并自动代理
sudo ./takeover --mode launch --command "curl https://httpbin.org/ip"
```

**优势：**
- ✅ **精确控制**: 只代理指定的进程
- ✅ **无误伤**: 不影响其他程序
- ✅ **灵活性**: 可以随时添加/移除目标
- ✅ **适合生产**: 生产环境的精确控制

---

### 选项3: 改进的实时监控 ⭐⭐⭐

**使用系统事件而非轮询**

```rust
pub enum ProxyMode {
    /// 改进的实时进程监控
    SmartMonitoring {
        target_patterns: Vec<String>,
        monitoring_method: MonitoringMethod,
    },
}

pub enum MonitoringMethod {
    #[cfg(target_os = "macos")]
    KQueue,           // macOS kqueue事件
    #[cfg(target_os = "linux")]  
    ProcessEvents,    // Linux process events
    ImprovedPolling,  // 改进的轮询 (500ms间隔)
}

#[cfg(target_os = "macos")]
impl SmartProcessMonitor {
    async fn setup_kqueue_monitoring(&mut self) -> Result<()> {
        // 使用kqueue监控进程创建/退出事件
        let kq = unsafe { libc::kqueue() };
        
        // 注册EVFILT_PROC事件
        let mut event = libc::kevent {
            ident: 0,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE,
            fflags: libc::NOTE_FORK | libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        
        // 实时检测进程事件
        self.process_kqueue_events(kq).await?;
    }
}
```

**优势：**
- ✅ **实时响应**: 进程启动立即检测
- ✅ **低开销**: 事件驱动，无需轮询
- ✅ **精确匹配**: 可以配置复杂的匹配规则

---

### 选项4: 混合智能模式 ⭐⭐⭐⭐

**结合多种方式的智能选择**

```rust
pub enum ProxyMode {
    /// 智能混合模式
    Smart {
        primary_method: PrimaryMethod,
        fallback_method: FallbackMethod,
        auto_switch: bool,
    },
}

pub enum PrimaryMethod {
    GlobalTransparent,  // 优先使用全局透明
    ProcessMonitoring,  // 或优先使用进程监控
}

impl SmartProxy {
    pub async fn start_smart_mode(&mut self) -> Result<()> {
        match self.detect_best_method().await? {
            BestMethod::GlobalTransparent => {
                info!("🌐 Using global transparent proxy (recommended)");
                self.start_global_transparent().await?;
            }
            BestMethod::ProcessSpecific => {
                info!("🎯 Using process-specific monitoring");
                self.start_improved_monitoring().await?;
            }
        }
    }
    
    async fn detect_best_method(&self) -> Result<BestMethod> {
        // 检测系统环境，自动选择最佳方案
        if self.can_use_global_transparent().await? {
            Ok(BestMethod::GlobalTransparent)
        } else {
            Ok(BestMethod::ProcessSpecific)
        }
    }
}
```

---

## 🎯 推荐方案选择

### 🏆 对于 curl 和 Node.js：选项1 (全局透明代理)

**为什么推荐全局透明？**

```bash
# 当前问题：进程监控对短生命周期程序失效
curl https://httpbin.org/ip  # 执行时间：200ms，进程监控来不及

# 全局透明解决方案：
sudo ./takeover --mode global --ports 80,443
curl https://httpbin.org/ip  # ✅ 立即生效，无需等待进程检测
```

### 🎯 使用场景对比

| 使用场景 | 推荐方案 | 原因 |
|---------|---------|------|
| **开发环境** | 全局透明代理 | 所有工具自动生效，最简单 |
| **测试curl/Node.js** | 全局透明代理 | 短生命周期程序的最佳选择 |
| **生产环境** | 手动指定模式 | 精确控制，安全性高 |
| **特定应用** | 改进监控 | 只代理特定的长运行程序 |
| **不确定场景** | 智能混合模式 | 自动选择最佳方案 |

## 🚀 立即实施建议

**为您的需求 (curl + Node.js)**，建议立即实施：

```rust
// 在 src/transparent_proxy.rs 中添加
impl TransparentProxy {
    /// 启动全局透明代理模式 - 最适合curl和Node.js
    pub async fn start_global(&mut self, ports: Vec<u16>) -> Result<()> {
        info!("🌐 Starting global transparent proxy for ports: {:?}", ports);
        
        // 使用我们已实现的透明代理基础
        self.start().await?;
        
        // 添加额外端口支持
        if !ports.is_empty() {
            self.pfctl_rules.add_additional_ports(ports).await?;
        }
        
        Ok(())
    }
}
```

这样就完全避免了进程监控的所有问题！您觉得这个方向如何？
