# Changelog

## 2024-12-XX - ipinfo.io Integration

### Changed
- **测试服务更换**: 从 `httpbin.org` 更换为 `ipinfo.io`
  - 更快的响应时间
  - 更简洁的API格式  
  - 更稳定的服务可用性

### Modified Files

#### Scripts Updated
- `scripts/quick_test.sh` - 快速测试脚本
- `scripts/test_curl_proxy.sh` - 完整curl代理测试
- `scripts/start_test_proxy.sh` - 测试代理启动脚本

#### Documentation Updated  
- `TESTING.md` - 测试指南
- `TROUBLESHOOTING.md` - 故障排除指南
- `examples/basic_usage.md` - 基本使用示例

#### New Files
- `scripts/verify_ipinfo.sh` - ipinfo.io集成验证脚本

### Technical Changes

#### Response Format Changes
- **httpbin.org/ip**: 返回 JSON `{"origin": "1.2.3.4"}`
- **ipinfo.io/ip**: 返回纯文本 `1.2.3.4`

#### Script Parsing Updates
```bash
# Old (httpbin.org)
IP=$(curl -s URL | jq -r .origin)

# New (ipinfo.io)  
IP=$(curl -s URL | tr -d '\n%')
```

#### URL Mappings
| Old URL | New URL | Purpose |
|---------|---------|---------|
| `httpbin.org/ip` | `ipinfo.io/ip` | 获取IP地址 |
| `httpbin.org/headers` | `ipinfo.io/json` | 获取详细信息 |
| `httpbin.org/user-agent` | `ipinfo.io/org` | 获取组织信息 |

### Benefits

1. **更简单的解析**: 不需要JSON解析，直接使用纯文本
2. **更快的响应**: ipinfo.io通常比httpbin.org更快
3. **更好的可用性**: 更稳定的服务
4. **统一的API**: 所有端点都在同一个域名下

### Verification

运行验证脚本确保更改正确：
```bash
./scripts/verify_ipinfo.sh
```

### Migration Notes

如果你有基于旧版本的自定义脚本：

1. 将 `httpbin.org/ip` 替换为 `ipinfo.io/ip`
2. 移除 `jq -r .origin` JSON解析
3. 使用 `tr -d '\n%'` 清理响应
4. 测试新的端点是否正常工作

### Testing

所有测试脚本已更新并验证：
- ✅ `quick_test.sh` - 快速测试正常
- ✅ `test_curl_proxy.sh` - 完整测试正常  
- ✅ `verify_ipinfo.sh` - 验证通过
- ✅ 文档示例全部更新
