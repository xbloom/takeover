# macOS 透明代理快速开始

## 🎯 功能说明

这是一个解决了时序问题的透明代理系统，让 curl、Node.js、wget 等所有网络工具自动通过代理访问，用户完全无感知。

## 🚀 快速开始

### 1. 准备SOCKS5代理

首先确保有一个可用的SOCKS5代理：

```bash
# 方式1: SSH隧道（推荐用于测试）
ssh -D 1080 user@remote-server

# 方式2: 使用其他SOCKS5代理工具
# 例如 shadowsocks、v2ray 等
```

### 2. 构建项目

```bash
# 构建项目
cargo build --release

# 或者直接运行示例
cargo build --example transparent_proxy_demo
```

### 3. 启动透明代理

```bash
# 需要管理员权限
sudo ./target/debug/examples/transparent_proxy_demo

# 或者运行发布版本
sudo ./target/release/examples/transparent_proxy_demo
```

### 4. 测试透明代理

一旦透明代理启动成功，您可以在**任何其他终端窗口**直接使用：

```bash
# 测试curl - 完全透明！
curl https://httpbin.org/ip
curl http://httpbin.org/user-agent

# 测试wget
wget -O- https://httpbin.org/ip

# 测试Node.js
node -e "
const https = require('https');
https.get('https://httpbin.org/ip', res => {
    let data = '';
    res.on('data', d => data += d);
    res.on('end', () => console.log(JSON.parse(data)));
});
"

# 测试npm（如果需要下载包）
npm info express

# 测试git
git ls-remote https://github.com/torvalds/linux.git
```

## ✨ 核心特性

### 🎯 真正透明
- ✅ 用户直接运行 `curl`，无需任何修改
- ✅ 支持所有使用HTTP/HTTPS的程序
- ✅ Node.js、Python、Go等语言的网络库都自动生效

### ⚡ 零时序问题
- ✅ 预启动代理服务器，确保就绪后再设置网络规则
- ✅ 内置就绪检查机制，消除竞态条件
- ✅ 优雅的启动和关闭流程

### 🔧 智能管理
- ✅ 自动清理pfctl规则
- ✅ 错误恢复机制
- ✅ 详细的日志输出

## 🛠️ 技术原理

### 工作流程

1. **预启动代理服务器** → 绑定本地端口
2. **等待就绪确认** → 确保代理服务器可接受连接
3. **设置pfctl规则** → 重定向HTTP/HTTPS流量到本地代理
4. **透明转发** → 本地代理将流量转发到真实的SOCKS5代理

### pfctl规则示例

```bash
# 查看当前的pfctl规则
sudo pfctl -a takeover_transparent -s nat

# 手动清理（如果需要）
sudo pfctl -a takeover_transparent -F all
```

### 支持的端口

默认拦截以下端口：
- **80** (HTTP)
- **443** (HTTPS)  
- **8080** (开发用HTTP)
- **3000** (Node.js开发常用)
- **5000** (Flask等开发常用)

## 🐛 故障排除

### 问题1: 权限不足
```
错误: Failed to load pfctl rules: pfctl: Permission denied
解决: 使用 sudo 运行程序
```

### 问题2: SOCKS5代理不可达
```
错误: Failed to connect to target proxy: Connection refused
解决: 确保127.0.0.1:1080有SOCKS5代理在运行
```

### 问题3: pfctl规则残留
```bash
# 手动清理
sudo pfctl -a takeover_transparent -F all
```

## 📈 性能说明

- **延迟**: 增加约1-2ms（透明代理转发开销）
- **吞吐量**: 支持高并发连接
- **内存**: 每个连接约16KB缓冲区
- **CPU**: 最小开销，主要是数据复制

## 🔒 安全注意事项

- **需要root权限**: 修改pfctl规则需要管理员权限
- **流量可见**: 本地代理可以看到所有通过的流量  
- **规则清理**: 程序退出时会自动清理pfctl规则
- **紧急清理**: 异常退出时有紧急清理机制

---

**享受真正透明的代理体验！** 🚀
