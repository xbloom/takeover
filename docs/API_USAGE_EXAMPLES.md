# Takeover Proxy - API 使用示例

## 快速开始

### API 基础信息

- **默认监听地址**: `http://127.0.0.1:8768`
- **响应格式**: JSON
- **认证**: 暂无（后续版本将支持）

### API 端点概览

| 方法 | 路径 | 描述 | Upsert |
|------|------|------|--------|
| `GET` | `/health` | 健康检查 | - |
| `GET` | `/api/rules` | 获取规则列表（支持查询参数） | - |
| `POST` | `/api/rules` | 批量应用规则 | ✅ |
| `GET` | `/api/rules/{id}` | 获取单个规则 | - |
| `PUT` | `/api/rules/{id}` | 创建或更新规则 | ✅ |
| `DELETE` | `/api/rules/{id}` | 删除规则 | - |
| `GET` | `/api/connections` | 获取连接列表 | - |
| `POST` | `/api/connections/{id}/intercept` | 拦截连接 | - |
| `POST` | `/api/requests/{id}/approve` | 批准请求 | - |
| `POST` | `/api/requests/{id}/reject` | 拒绝请求 | - |
| `POST` | `/api/requests/{id}/replay` | 重放请求 | - |

---

## 规则管理 API

### 1. 获取所有规则

```bash
curl -X GET http://127.0.0.1:8768/api/rules
```

响应：
```json
{
  "success": true,
  "data": [
    {
      "id": "rule-001",
      "name": "block-ads",
      "description": "阻止广告域名",
      "pattern": { "HostMatches": "ads.example.com" },
      "conditions": [],
      "action": "Reject",
      "enabled": true,
      "priority": 100,
      "metadata": {}
    }
  ],
  "error": null
}
```

### 2. 按名称模糊查询规则

支持部分匹配（不区分大小写），返回所有匹配的规则：

```bash
# 查询名称包含 "block" 的所有规则
curl -X GET "http://127.0.0.1:8768/api/rules?name=block"
```

响应：
```json
{
  "success": true,
  "data": [
    {
      "id": "rule-001",
      "name": "block-ads",
      "description": "阻止广告域名",
      ...
    },
    {
      "id": "rule-004",
      "name": "block-trackers",
      "description": "阻止追踪器",
      ...
    }
  ],
  "error": null
}
```

**模糊查询特性**：
- **不区分大小写**：`?name=BLOCK` 和 `?name=block` 效果相同
- **部分匹配**：`?name=ad` 可以匹配 `block-ads`、`ad-blocker`、`bad-site` 等
- **返回所有匹配的规则**：一次查询可能返回多个结果

**实际使用场景**：

```bash
# 场景1: 查找所有阻止规则
curl -X GET "http://127.0.0.1:8768/api/rules?name=block"

# 场景2: 查找所有代理规则（不区分大小写）
curl -X GET "http://127.0.0.1:8768/api/rules?name=PROXY"

# 场景3: 查找某个域名相关的规则
curl -X GET "http://127.0.0.1:8768/api/rules?name=github"

# 场景4: 查找测试相关的规则
curl -X GET "http://127.0.0.1:8768/api/rules?name=test"
```

如果没有匹配的规则：
```json
{
  "success": false,
  "data": null,
  "error": "No rules found matching name pattern 'xyz'"
}
```

### 3. 按ID查询规则（精确匹配）

使用查询参数：
```bash
curl -X GET "http://127.0.0.1:8768/api/rules?id=rule-001"
```

或使用路径参数（推荐）：
```bash
curl -X GET "http://127.0.0.1:8768/api/rules/rule-001"
```

响应：
```json
{
  "success": true,
  "data": {
    "id": "rule-001",
    "name": "block-ads",
    "description": "阻止广告域名",
    "pattern": { "HostMatches": "ads.example.com" },
    "conditions": [],
    "action": "Reject",
    "enabled": true,
    "priority": 100,
    "metadata": {}
  },
  "error": null
}
```

**注意**：ID查询是精确匹配，只返回单个规则（使用 `ApiResponse<Rule>`），而name查询是模糊匹配，返回规则列表（使用 `ApiResponse<Vec<Rule>>`）。

### 4. 批量应用规则（Upsert语义）

`POST /api/rules` 使用 upsert 语义：如果规则ID已存在则更新，否则创建新规则。

```bash
curl -X POST http://127.0.0.1:8768/api/rules \
  -H "Content-Type: application/json" \
  -d '{
    "rules": [
      {
        "id": "rule-001",
        "name": "block-ads",
        "description": "阻止广告域名",
        "pattern": { "HostMatches": "ads.example.com" },
        "conditions": [],
        "action": "Reject",
        "enabled": true,
        "priority": 100,
        "metadata": {}
      },
      {
        "id": "rule-002",
        "name": "allow-github",
        "description": "允许访问 GitHub",
        "pattern": { "HostMatches": "github.com" },
        "conditions": [],
        "action": "Allow",
        "enabled": true,
        "priority": 90,
        "metadata": {}
      }
    ]
  }'
```

响应：
```json
{
  "success": true,
  "data": "Rules applied (upsert)",
  "error": null
}
```

### 5. 创建或更新单个规则（Upsert）

```bash
curl -X PUT http://127.0.0.1:8768/api/rules/rule-003 \
  -H "Content-Type: application/json" \
  -d '{
    "id": "rule-003",
    "name": "proxy-corporate",
    "description": "企业流量走代理",
    "pattern": { "HostMatches": "corporate.example.com" },
    "conditions": [
      {
        "name": "office_hours",
        "condition_type": "TimeRange",
        "value": {
          "start": "09:00",
          "end": "18:00"
        },
        "negated": false
      }
    ],
    "action": {
      "Proxy": {
        "proxy_type": "Socks5",
        "address": "127.0.0.1:1080",
        "auth": null
      }
    },
    "enabled": true,
    "priority": 80,
    "metadata": {
      "department": "IT",
      "created_by": "admin"
    }
  }'
```

响应：
```json
{
  "success": true,
  "data": {
    "id": "rule-003",
    "name": "proxy-corporate",
    ...
  },
  "error": null
}
```

**注意**：路径中的 ID 必须与请求体中的 ID 一致，否则返回 400 错误。

### 6. 删除规则

```bash
curl -X DELETE http://127.0.0.1:8768/api/rules/rule-003
```

响应：
```json
{
  "success": true,
  "data": "Rule rule-003 deleted",
  "error": null
}
```

如果规则不存在：
```json
{
  "success": false,
  "data": null,
  "error": "Rule rule-999 not found"
}
```

## 规则模式类型

### HostMatches - 主机名匹配
```json
{ "HostMatches": "example.com" }
```

### HostSuffix - 主机名后缀匹配
```json
{ "HostSuffix": ".example.com" }
```

### HostContains - 主机名包含
```json
{ "HostContains": "ads" }
```

### PathMatches - 路径匹配
```json
{ "PathMatches": "/api/users" }
```

### PathPrefix - 路径前缀
```json
{ "PathPrefix": "/api/" }
```

### Regex - 正则表达式匹配
```json
{ "Regex": ".*\\.googleapis\\.com" }
```

### All - 匹配所有
```json
"All"
```

## 规则动作类型

### Allow - 允许直连
```json
"Allow"
```

### Reject - 拒绝访问
```json
"Reject"
```

### Proxy - 通过代理
```json
{
  "Proxy": {
    "proxy_type": "Socks5",
    "address": "127.0.0.1:1080",
    "auth": {
      "username": "user",
      "password": "pass"
    }
  }
}
```

### Intercept - 拦截并等待审批
```json
"Intercept"
```

### Modify - 修改请求/响应
```json
{
  "Modify": {
    "add_headers": {
      "X-Custom-Header": "value"
    },
    "remove_headers": ["User-Agent"],
    "add_query_params": {
      "token": "abc123"
    }
  }
}
```

### Script - 执行脚本处理
```json
{
  "Script": {
    "script_path": "/path/to/script.lua",
    "trigger_point": "BeforeRequest"
  }
}
```

## 规则条件类型

### TimeRange - 时间范围
```json
{
  "name": "office_hours",
  "condition_type": "TimeRange",
  "value": {
    "start": "09:00",
    "end": "18:00"
  },
  "negated": false
}
```

### SourceIP - 源IP匹配
```json
{
  "name": "internal_network",
  "condition_type": "SourceIP",
  "value": "192.168.1.0/24",
  "negated": false
}
```

### ProcessName - 进程名称匹配
```json
{
  "name": "browser_only",
  "condition_type": "ProcessName",
  "value": "chrome",
  "negated": false
}
```

### Header - HTTP头部匹配
```json
{
  "name": "has_auth",
  "condition_type": "Header",
  "value": {
    "name": "Authorization",
    "pattern": "Bearer .*"
  },
  "negated": false
}
```

## API 最佳实践

### 查询优化建议

1. **使用路径参数而非查询参数查询单个规则**
   ```bash
   # 推荐
   curl -X GET http://127.0.0.1:8768/api/rules/rule-001
   
   # 不推荐（但也支持）
   curl -X GET "http://127.0.0.1:8768/api/rules?id=rule-001"
   ```

2. **模糊查询优化**
   - 使用更具体的关键词减少返回结果
   - 考虑在客户端进行二次过滤
   ```bash
   # 模糊：可能返回很多结果
   curl -X GET "http://127.0.0.1:8768/api/rules?name=rule"
   
   # 精确：返回结果更少
   curl -X GET "http://127.0.0.1:8768/api/rules?name=block-ads"
   ```

3. **批量操作vs单个操作**
   - 创建多个规则时使用 `POST /api/rules`（批量）
   - 更新单个规则时使用 `PUT /api/rules/{id}`（单个）

### Upsert 语义说明

**什么是 Upsert？**
- **Update** + **Insert** 的组合操作
- 如果资源存在则更新，不存在则创建
- 避免了"检查-创建"的竞态条件

**应用场景**：
```bash
# 第一次调用：创建规则
curl -X PUT http://127.0.0.1:8768/api/rules/my-rule \
  -H "Content-Type: application/json" \
  -d '{"id": "my-rule", "name": "test", ...}'
# 返回: 新创建的规则

# 第二次调用：更新相同ID的规则
curl -X PUT http://127.0.0.1:8768/api/rules/my-rule \
  -H "Content-Type: application/json" \
  -d '{"id": "my-rule", "name": "test-updated", ...}'
# 返回: 更新后的规则
```

**批量 Upsert**：
```bash
curl -X POST http://127.0.0.1:8768/api/rules \
  -H "Content-Type: application/json" \
  -d '{
    "rules": [
      {"id": "rule-1", ...},  # 如果存在则更新
      {"id": "rule-2", ...},  # 如果存在则更新
      {"id": "rule-3", ...}   # 如果不存在则创建
    ]
  }'
```

---

## 完整工作流示例

### 场景1：设置企业代理规则

1. **创建基础规则** - 所有流量默认直连
```bash
curl -X PUT http://127.0.0.1:8768/api/rules/default \
  -H "Content-Type: application/json" \
  -d '{
    "id": "default",
    "name": "default-direct",
    "description": "默认直连",
    "pattern": "All",
    "conditions": [],
    "action": "Allow",
    "enabled": true,
    "priority": 0,
    "metadata": {}
  }'
```

2. **添加代理规则** - 企业域名走代理
```bash
curl -X PUT http://127.0.0.1:8768/api/rules/corporate \
  -H "Content-Type: application/json" \
  -d '{
    "id": "corporate",
    "name": "corporate-proxy",
    "description": "企业流量走SOCKS5代理",
    "pattern": { "HostSuffix": ".corp.example.com" },
    "conditions": [],
    "action": {
      "Proxy": {
        "proxy_type": "Socks5",
        "address": "proxy.corp.example.com:1080",
        "auth": null
      }
    },
    "enabled": true,
    "priority": 100,
    "metadata": {}
  }'
```

3. **添加拦截规则** - 敏感API需要审批
```bash
curl -X PUT http://127.0.0.1:8768/api/rules/sensitive \
  -H "Content-Type: application/json" \
  -d '{
    "id": "sensitive",
    "name": "sensitive-api",
    "description": "敏感API需要审批",
    "pattern": { "PathPrefix": "/api/admin/" },
    "conditions": [],
    "action": "Intercept",
    "enabled": true,
    "priority": 200,
    "metadata": {}
  }'
```

4. **查看所有规则**
```bash
curl -X GET http://127.0.0.1:8768/api/rules
```

5. **更新规则优先级**
```bash
curl -X PUT http://127.0.0.1:8768/api/rules/corporate \
  -H "Content-Type: application/json" \
  -d '{
    "id": "corporate",
    "name": "corporate-proxy",
    "description": "企业流量走SOCKS5代理",
    "pattern": { "HostSuffix": ".corp.example.com" },
    "conditions": [],
    "action": {
      "Proxy": {
        "proxy_type": "Socks5",
        "address": "proxy.corp.example.com:1080",
        "auth": null
      }
    },
    "enabled": true,
    "priority": 150,
    "metadata": {}
  }'
```

6. **删除不需要的规则**
```bash
curl -X DELETE http://127.0.0.1:8768/api/rules/sensitive
```

### 场景2：动态规则管理

**需求**：开发环境下临时添加测试规则，测试完成后删除

```bash
# 1. 添加测试规则
curl -X PUT http://127.0.0.1:8768/api/rules/dev-test \
  -H "Content-Type: application/json" \
  -d '{
    "id": "dev-test",
    "name": "dev-api-test",
    "description": "开发测试规则",
    "pattern": { "PathPrefix": "/dev/api/" },
    "conditions": [],
    "action": "Allow",
    "enabled": true,
    "priority": 999,
    "metadata": {
      "env": "development",
      "temporary": true
    }
  }'

# 2. 验证规则是否生效
curl -X GET "http://127.0.0.1:8768/api/rules?name=dev"

# 3. 测试完成后，查找所有临时规则
curl -X GET "http://127.0.0.1:8768/api/rules?name=test"

# 4. 删除测试规则
curl -X DELETE http://127.0.0.1:8768/api/rules/dev-test
```

### 场景3：规则优先级管理

**需求**：调整规则执行顺序，高优先级规则先匹配

```bash
# 1. 查看所有规则
curl -X GET http://127.0.0.1:8768/api/rules

# 2. 查找特定规则（例如：所有阻止规则）
curl -X GET "http://127.0.0.1:8768/api/rules?name=block"

# 3. 提高某个规则的优先级（通过更新）
curl -X PUT http://127.0.0.1:8768/api/rules/block-malware \
  -H "Content-Type: application/json" \
  -d '{
    "id": "block-malware",
    "name": "block-malware-sites",
    "description": "阻止恶意软件站点",
    "pattern": { "HostSuffix": ".malware.com" },
    "conditions": [],
    "action": "Reject",
    "enabled": true,
    "priority": 1000,
    "metadata": {}
  }'

# 4. 验证更新
curl -X GET http://127.0.0.1:8768/api/rules/block-malware
```

### 场景4：批量规则导入导出

**导出规则到文件**：
```bash
curl -X GET http://127.0.0.1:8768/api/rules | jq '.data' > rules-backup.json
```

**从文件导入规则**：
```bash
# 假设 rules-backup.json 包含规则数组
curl -X POST http://127.0.0.1:8768/api/rules \
  -H "Content-Type: application/json" \
  -d @rules-backup.json
```

**筛选并导出特定规则**：
```bash
# 导出所有代理规则
curl -X GET "http://127.0.0.1:8768/api/rules?name=proxy" | jq '.data' > proxy-rules.json

# 导出所有阻止规则
curl -X GET "http://127.0.0.1:8768/api/rules?name=block" | jq '.data' > block-rules.json
```

## 错误响应示例

### 404 - 规则未找到
```json
{
  "success": false,
  "data": null,
  "error": "Rule rule-999 not found"
}
```

### 400 - 请求参数错误
```json
{
  "success": false,
  "data": null,
  "error": "Rule ID in path and body must match"
}
```

### 500 - 服务器内部错误
```json
{
  "success": false,
  "data": null,
  "error": "Failed to sync rules: connection timeout"
}
```

## Shell 脚本示例

### 规则管理脚本

```bash
#!/bin/bash
# rule-manager.sh - 规则管理辅助脚本

API_BASE="http://127.0.0.1:8768/api"

# 列出所有规则
list_rules() {
    echo "📋 所有规则:"
    curl -s "${API_BASE}/rules" | jq '.data[] | {id, name, enabled, priority}'
}

# 查找规则
search_rules() {
    local pattern=$1
    echo "🔍 搜索规则: $pattern"
    curl -s "${API_BASE}/rules?name=${pattern}" | jq '.data[] | {id, name, description}'
}

# 启用/禁用规则
toggle_rule() {
    local rule_id=$1
    local enabled=$2
    echo "🔄 更新规则 $rule_id (enabled=$enabled)"
    
    # 获取当前规则
    rule=$(curl -s "${API_BASE}/rules/${rule_id}" | jq '.data')
    
    # 更新 enabled 字段
    updated=$(echo "$rule" | jq ".enabled = $enabled")
    
    # 提交更新
    curl -X PUT "${API_BASE}/rules/${rule_id}" \
        -H "Content-Type: application/json" \
        -d "$updated"
}

# 删除所有测试规则
cleanup_test_rules() {
    echo "🧹 清理测试规则..."
    curl -s "${API_BASE}/rules?name=test" | jq -r '.data[]?.id' | while read rule_id; do
        if [ -n "$rule_id" ]; then
            echo "  删除: $rule_id"
            curl -X DELETE "${API_BASE}/rules/${rule_id}"
        fi
    done
}

# 导出规则
export_rules() {
    local filename=${1:-"rules-backup-$(date +%Y%m%d-%H%M%S).json"}
    echo "💾 导出规则到: $filename"
    curl -s "${API_BASE}/rules" | jq '.data' > "$filename"
    echo "✅ 导出完成 ($(jq length $filename) 条规则)"
}

# 导入规则
import_rules() {
    local filename=$1
    if [ ! -f "$filename" ]; then
        echo "❌ 文件不存在: $filename"
        return 1
    fi
    
    echo "📥 从 $filename 导入规则..."
    rules=$(cat "$filename")
    curl -X POST "${API_BASE}/rules" \
        -H "Content-Type: application/json" \
        -d "{\"rules\": $rules}"
}

# 主菜单
case "${1:-help}" in
    list)
        list_rules
        ;;
    search)
        search_rules "$2"
        ;;
    enable)
        toggle_rule "$2" true
        ;;
    disable)
        toggle_rule "$2" false
        ;;
    cleanup)
        cleanup_test_rules
        ;;
    export)
        export_rules "$2"
        ;;
    import)
        import_rules "$2"
        ;;
    *)
        echo "Usage: $0 {list|search|enable|disable|cleanup|export|import}"
        echo ""
        echo "Commands:"
        echo "  list              - 列出所有规则"
        echo "  search <pattern>  - 搜索规则"
        echo "  enable <id>       - 启用规则"
        echo "  disable <id>      - 禁用规则"
        echo "  cleanup           - 清理测试规则"
        echo "  export [file]     - 导出规则"
        echo "  import <file>     - 导入规则"
        ;;
esac
```

**使用示例**：
```bash
# 列出所有规则
./rule-manager.sh list

# 搜索包含 "block" 的规则
./rule-manager.sh search block

# 禁用某个规则
./rule-manager.sh disable rule-001

# 导出规则备份
./rule-manager.sh export my-rules.json

# 导入规则
./rule-manager.sh import my-rules.json

# 清理所有测试规则
./rule-manager.sh cleanup
```

## 常见问题

### Q1: 如何批量更新规则优先级？

```bash
# 获取所有规则
rules=$(curl -s http://127.0.0.1:8768/api/rules | jq '.data')

# 更新优先级（示例：将所有阻止规则优先级提高）
echo "$rules" | jq -c '.[] | select(.name | contains("block"))' | while read rule; do
    rule_id=$(echo "$rule" | jq -r '.id')
    updated=$(echo "$rule" | jq '.priority = 1000')
    curl -X PUT "http://127.0.0.1:8768/api/rules/${rule_id}" \
        -H "Content-Type: application/json" \
        -d "$updated"
done
```

### Q2: 如何验证规则是否正确添加？

```bash
# 添加规则
curl -X PUT http://127.0.0.1:8768/api/rules/test-rule \
  -H "Content-Type: application/json" \
  -d '{"id": "test-rule", "name": "test", ...}' \
  -w "\nHTTP Status: %{http_code}\n"

# 立即验证
curl -s http://127.0.0.1:8768/api/rules/test-rule | jq '.data.name'
```

### Q3: 如何查找未启用的规则？

```bash
# 方法1: 使用 jq 过滤
curl -s http://127.0.0.1:8768/api/rules | jq '.data[] | select(.enabled == false)'

# 方法2: 只显示ID和名称
curl -s http://127.0.0.1:8768/api/rules | \
    jq '.data[] | select(.enabled == false) | {id, name}'
```

### Q4: 模糊查询性能如何？

当前实现使用 `O(n)` 线性查询，对于规则数量不大的场景（<1000条）性能良好。如果规则数量非常大，建议：
- 使用更精确的查询关键词
- 在客户端进行二次过滤
- 考虑使用ID精确查询

## 健康检查

```bash
curl -X GET http://127.0.0.1:8768/health
```

响应：
```json
{
  "success": true,
  "data": "OK",
  "error": null
}
```

## OpenAPI 文档

如果启用了 `openapi` feature，可以访问 Swagger UI：

```
http://127.0.0.1:8768/swagger-ui
```

OpenAPI JSON 规范：
```
http://127.0.0.1:8768/api-docs/openapi.json
```

---

## 总结

### 核心特性

✅ **模糊查询**: 按名称部分匹配，不区分大小写  
✅ **Upsert语义**: 智能创建/更新，避免重复  
✅ **RESTful设计**: 符合HTTP语义，易于理解  
✅ **灵活查询**: 支持查询参数和路径参数两种方式  

### API设计亮点

- **幂等性**: `PUT` 操作可安全重试
- **一致性**: 所有操作自动同步到 RuleService
- **错误清晰**: 404/400/500 语义明确
- **扩展性**: 预留 OpenAPI 文档支持

### 下一步

查看更多文档：
- `docs/RULE_MATCHING_MIDDLEWARE_DESIGN.md` - 规则匹配设计
- `docs/HIGH_LEVEL_SOLUTIONS.md` - 架构设计
- `README.md` - 项目概览

