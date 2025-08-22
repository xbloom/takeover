# Testing Guide

这个文档描述如何测试Takeover的功能，特别是使用curl和本地HTTP代理进行测试。

## 测试脚本概览

我们提供了几个测试脚本来验证Takeover的功能：

| 脚本 | 用途 | 执行时间 |
|------|------|----------|
| `quick_test.sh` | 快速功能测试 | ~30秒 |
| `test_curl_proxy.sh` | 完整的curl代理测试 | ~2分钟 |
| `test_cleanup.sh` | 测试清理机制 | ~1分钟 |
| `start_test_proxy.sh` | 启动测试用HTTP代理 | 立即 |
| `stop_test_proxy.sh` | 停止测试代理 | 立即 |
| `manual_cleanup.sh` | 手动清理网络规则 | 立即 |

## 快速开始

### 1. 构建项目

```bash
cargo build --release
```

### 2. 快速测试（推荐）

```bash
# 快速验证基本功能
sudo ./scripts/quick_test.sh
```

这个脚本会：
- 检查基础环境
- 获取基线IP地址
- 启动takeover监控curl
- 测试curl请求
- 清理并验证

### 3. 完整测试（如果需要详细验证）

如果你有本地HTTP代理：

```bash
# 使用现有代理测试
sudo ./scripts/test_curl_proxy.sh
```

如果没有代理，先启动测试代理：

```bash
# 终端1：启动测试代理
./scripts/start_test_proxy.sh

# 终端2：运行完整测试
sudo ./scripts/test_curl_proxy.sh

# 测试完成后停止代理
./scripts/stop_test_proxy.sh
```

## 详细测试流程

### 步骤1：准备环境

确保你有一个可用的HTTP代理。有几种选择：

**选项A：使用我们的测试代理**
```bash
./scripts/start_test_proxy.sh
```

**选项B：使用mitmproxy**
```bash
pip install mitmproxy
mitmproxy --listen-port 8080
```

**选项C：使用其他HTTP代理**
- Squid
- Privoxy
- 任何监听8080端口的HTTP代理

### 步骤2：验证代理工作

```bash
# 测试代理是否可用
curl --proxy http://127.0.0.1:8080 https://ipinfo.io/ip
```

应该返回一个不同于直接连接的IP地址。

### 步骤3：运行takeover测试

```bash
# 运行完整测试套件
sudo ./scripts/test_curl_proxy.sh
```

或者运行快速测试：

```bash
# 快速验证
sudo ./scripts/quick_test.sh
```

### 步骤4：解释结果

#### 测试服务说明

我们使用 **ipinfo.io** 作为测试的IP检测服务：

- **https://ipinfo.io/ip** - 返回纯文本IP地址
- **https://ipinfo.io/json** - 返回JSON格式的详细信息  
- **https://ipinfo.io/org** - 返回组织信息

#### 测试结果解读

测试会显示三个IP地址：

1. **Baseline IP**: 直接连接的IP地址
2. **Test IP**: 通过takeover的IP地址  
3. **Final IP**: 清理后的IP地址

**成功的测试结果**：
- Test IP ≠ Baseline IP（说明流量被拦截）
- Final IP = Baseline IP（说明清理成功）

**如果代理可用**：
- Test IP = Manual Proxy IP（说明流量确实通过代理）

## 测试场景

### 场景1：基本拦截测试

```bash
# 启动takeover
sudo ./target/release/takeover -t curl -p http://127.0.0.1:8080 &

# 测试curl
curl https://ipinfo.io/ip

# 停止takeover
sudo pkill takeover
```

### 场景2：多次请求测试

```bash
# 测试多次请求的一致性
for i in {1..5}; do
    curl https://ipinfo.io/ip
    sleep 1
done
```

### 场景3：不同URL测试

```bash
# 测试不同的目标URL
curl https://ipinfo.io/json
curl https://ipinfo.io/org
curl http://ipinfo.io/ip  # HTTP vs HTTPS
```

### 场景4：清理测试

```bash
# 测试各种退出方式的清理效果
sudo ./scripts/test_cleanup.sh
```

## 故障排除

### 问题：curl没有被拦截

**可能原因**：
1. takeover没有正确启动
2. 网络规则没有应用
3. 进程匹配失败

**调试步骤**：
```bash
# 检查takeover日志
RUST_LOG=debug sudo ./target/release/takeover -t curl -p http://127.0.0.1:8080

# 检查网络规则
# macOS:
sudo pfctl -a takeover -s all

# Linux:
sudo iptables -t nat -L OUTPUT | grep takeover
```

### 问题：代理连接失败

**检查代理可用性**：
```bash
# 测试代理直接连接
curl --proxy http://127.0.0.1:8080 https://ipinfo.io/ip

# 检查代理是否在运行
lsof -i :8080
```

### 问题：权限错误

**确保使用sudo**：
```bash
sudo ./scripts/quick_test.sh
```

**检查pfctl/iptables权限**：
```bash
# macOS:
sudo pfctl -s all

# Linux:
sudo iptables -L
```

### 问题：规则没有清理

**手动清理**：
```bash
sudo ./scripts/manual_cleanup.sh
```

**验证清理**：
```bash
# macOS:
sudo pfctl -a takeover -s all

# Linux:
sudo iptables -t nat -L OUTPUT | grep takeover
```

## 高级测试

### 性能测试

```bash
# 测试大量请求的性能影响
time for i in {1..100}; do curl -s https://ipinfo.io/ip > /dev/null; done
```

### 并发测试

```bash
# 测试并发请求
for i in {1..10}; do
    curl https://ipinfo.io/ip &
done
wait
```

### 长时间运行测试

```bash
# 测试长时间运行的稳定性
while true; do
    curl -s https://ipinfo.io/ip
    sleep 60
done
```

## 自动化测试

你可以将这些测试集成到CI/CD管道中：

```bash
#!/bin/bash
# CI测试脚本

set -e

# 构建
cargo build --release

# 运行测试
sudo ./scripts/quick_test.sh

# 检查退出码
if [ $? -eq 0 ]; then
    echo "✅ All tests passed"
else
    echo "❌ Tests failed"
    exit 1
fi
```

## 测试报告

测试脚本会生成详细的输出，包括：

- 时间戳和执行时间
- 网络规则状态
- IP地址变化
- 错误信息和调试信息
- 清理验证结果

保存测试结果：

```bash
# 保存完整测试结果
sudo ./scripts/test_curl_proxy.sh 2>&1 | tee test_results.log

# 保存快速测试结果
sudo ./scripts/quick_test.sh 2>&1 | tee quick_test.log
```

## 贡献测试

如果你发现了新的测试场景或改进建议：

1. 在现有脚本基础上添加新的测试用例
2. 创建特定场景的测试脚本
3. 更新这个文档
4. 提交Pull Request

测试脚本的设计原则：
- 自包含（不依赖外部状态）
- 可重复执行
- 清晰的输出和错误信息
- 自动清理
- 兼容不同平台
