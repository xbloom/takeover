# Takeover Proxy Lua Scripts

这个目录包含了Takeover透明代理的示例Lua脚本，展示了如何使用脚本引擎来扩展代理功能。

## 脚本概览

### 1. composite_request.lua - 复合请求处理
**功能：** 同时发起多个HTTP请求并合并响应
**适用场景：**
- API聚合服务
- 微服务数据合并
- 后端服务调用优化

**触发条件：** 访问`/api/user/profile/*`路径
**特性：**
- 并行发起多个HTTP请求
- JSON响应合并
- 超时控制
- 错误处理

### 2. request_modifier.lua - 请求修改
**功能：** 动态修改HTTP请求的各种属性
**适用场景：**
- API版本升级
- 认证头添加
- URL重写和重定向
- 请求体修改

**特性：**
- URL重写（old-api -> new-api，v1 -> v2）
- 自动添加认证头（JWT、API密钥）
- 监控头部添加
- 请求体JSON修改

### 3. conditional_proxy.lua - 条件代理
**功能：** 基于各种条件智能路由请求
**适用场景：**
- 智能负载均衡
- A/B测试
- 维护模式切换
- 地理位置路由
- 速率限制

**特性：**
- 维护窗口检测
- A/B测试用户分组
- 负载均衡算法
- 管理员权限验证
- 地理位置路由

### 4. monitoring_logger.lua - 监控日志
**功能：** 全面的监控、日志记录和安全审计
**适用场景：**
- 性能监控
- 安全审计
- 错误追踪
- 统计分析

**特性：**
- 性能指标收集
- 安全威胁检测
- 实时统计
- 慢请求检测
- 大请求监控

### 5. response_processor.lua - 响应处理
**功能：** 处理和修改HTTP响应内容
**适用场景：**
- 内容过滤
- 数据脱敏
- 安全增强
- 缓存控制

**特性：**
- 敏感数据脱敏
- 恶意脚本检测
- 广告追踪过滤
- 安全头部添加
- 智能缓存控制

## 使用方法

### 1. 脚本部署
将Lua脚本文件放置在`scripts/`目录中，系统会自动加载。

### 2. 触发器配置
```rust
use takeover::core::middleware::script::{ScriptTrigger, ScriptMiddleware};
use takeover::core::ports::{TriggerPoint, TriggerCondition};

// 添加脚本触发器
let trigger = ScriptTrigger {
    script_name: "composite_request".to_string(),
    trigger_point: TriggerPoint::AfterRequest,
    conditions: vec![
        TriggerCondition::PathMatches("/api/user/profile/*".to_string()),
    ],
    enabled: true,
    priority: 1,
};

script_middleware.add_script_trigger(trigger).await;
```

### 3. 配置参数
脚本可以通过`takeover.get_config(key)`访问配置：

```toml
# 配置文件示例
[lua_config]
internal_api_token = "your-internal-api-token"
permissions_api_token = "your-permissions-token"
analytics_api_token = "your-analytics-token"
api_key = "your-api-key"
jwt_token = "your-jwt-token"
maintenance_start_hour = "2"
maintenance_end_hour = "4"
external_rate_limit = "100"
admin_token = "secure-admin-token"
```

## API 参考

### 可用的全局变量
- `connection_id`: 当前连接ID
- `target_host`: 目标主机名
- `target_port`: 目标端口
- `request`: HTTP请求对象
- `response`: HTTP响应对象（如果可用）

### takeover API函数

#### HTTP请求
```lua
-- 发起HTTP请求
local response = takeover.http_request({
    method = "GET",
    url = "https://api.example.com/data",
    headers = {
        ["Authorization"] = "Bearer token",
        ["Content-Type"] = "application/json"
    },
    body = ""
})
```

#### 日志记录
```lua
-- 记录日志
takeover.log("info", "Processing request")
takeover.log("warn", "Rate limit approaching")
takeover.log("error", "Request failed")
```

#### 配置访问
```lua
-- 获取配置值
local api_key = takeover.get_config("api_key")
local timeout = takeover.get_config("request_timeout") or "30"
```

#### 数据存储
```lua
-- 存储数据
takeover.store_data("user_session", {
    user_id = "12345",
    timestamp = os.time()
})

-- 获取数据
local session = takeover.get_data("user_session")
```

#### 事件发射
```lua
-- 发射自定义事件
takeover.emit_event({
    name = "custom_event",
    data = {
        message = "Something happened",
        timestamp = os.time()
    }
})
```

### 脚本返回值

脚本可以返回以下动作：

#### 1. 继续处理
```lua
return "continue"
-- 或
return {
    action = "continue",
    modified_request = modified_request,  -- 可选
    modified_response = modified_response  -- 可选
}
```

#### 2. 暂停处理
```lua
return {
    action = "pause",
    reason = "Waiting for manual approval"
}
```

#### 3. 拒绝请求
```lua
return {
    action = "reject",
    reason = "Security policy violation"
}
```

#### 4. 重定向
```lua
return {
    action = "redirect",
    url = "https://maintenance.example.com"
}
```

#### 5. 复合请求
```lua
return {
    action = "composite_request",
    requests = {
        {method = "GET", url = "https://api1.com/data"},
        {method = "GET", url = "https://api2.com/data"}
    },
    merge_strategy = "merge_json"
}
```

## 最佳实践

### 1. 错误处理
始终使用`pcall`来处理可能的错误：

```lua
local function main()
    -- 主要逻辑
    return "continue"
end

local status, result = pcall(main)
if not status then
    takeover.log("error", "Script error: " .. tostring(result))
    return "continue"
end
return result
```

### 2. 性能考虑
- 避免在脚本中进行昂贵的操作
- 使用适当的超时设置
- 缓存重复计算的结果

### 3. 安全注意事项
- 验证所有输入数据
- 使用沙箱模式
- 限制外部网络访问
- 定期审查脚本内容

### 4. 调试技巧
- 使用详细的日志记录
- 利用`takeover.store_data`保存调试信息
- 监控脚本执行统计

## 扩展开发

### 创建新脚本
1. 在`scripts/`目录创建`.lua`文件
2. 实现主要处理逻辑
3. 添加错误处理
4. 配置触发器
5. 测试和验证

### 脚本模板
```lua
-- 脚本描述
-- 功能：...
-- 场景：...

local function log(level, message)
    takeover.log(level, "[ScriptName] " .. message)
end

function main()
    log("info", "Script started")
    
    -- 主要逻辑
    
    return "continue"
end

-- 错误处理
local status, result = pcall(main)
if not status then
    takeover.log("error", "[ScriptName] Error: " .. tostring(result))
    return "continue"
end

return result
```

## 故障排除

### 常见问题
1. **脚本不执行**: 检查触发器配置和条件
2. **语法错误**: 验证Lua语法
3. **权限错误**: 检查沙箱限制
4. **性能问题**: 检查脚本复杂度和执行时间

### 调试方法
1. 查看日志输出
2. 检查脚本统计
3. 使用数据存储功能保存调试信息
4. 利用事件系统监控执行

## 版本历史

- v1.0: 初始版本，包含5个示例脚本
- 支持的功能：HTTP请求、日志记录、数据存储、事件发射
- 安全特性：沙箱执行、输入验证、权限控制
