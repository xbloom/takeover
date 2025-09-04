-- 请求修改脚本
-- 展示如何修改HTTP请求的各种属性
-- 
-- 场景：动态修改请求头、URL重写、添加认证等

-- 日志函数
local function log(level, message)
    takeover.log(level, "[RequestModifier] " .. message)
end

-- URL重写函数
local function rewrite_url(original_url)
    -- 将所有对 old-api.example.com 的请求重写到 new-api.example.com
    local new_url = string.gsub(original_url, "old%-api%.example%.com", "new-api.example.com")
    
    -- API版本升级：v1 -> v2
    new_url = string.gsub(new_url, "/api/v1/", "/api/v2/")
    
    -- 添加统一的API密钥参数
    local api_key = takeover.get_config("api_key")
    if api_key then
        local separator = string.find(new_url, "?") and "&" or "?"
        new_url = new_url .. separator .. "api_key=" .. api_key
    end
    
    return new_url
end

-- 添加认证头
local function add_auth_headers(headers)
    -- 添加JWT token
    local jwt_token = takeover.get_config("jwt_token")
    if jwt_token then
        headers["Authorization"] = "Bearer " .. jwt_token
    end
    
    -- 添加API密钥头
    local api_secret = takeover.get_config("api_secret")
    if api_secret then
        headers["X-API-Secret"] = api_secret
    end
    
    -- 添加请求ID用于追踪
    headers["X-Request-ID"] = "takeover-" .. os.time() .. "-" .. math.random(1000, 9999)
    
    -- 添加用户代理
    headers["User-Agent"] = "Takeover-Proxy/1.0 (Enhanced)"
    
    return headers
end

-- 添加监控和调试头
local function add_monitoring_headers(headers)
    headers["X-Proxy-Timestamp"] = tostring(os.time())
    headers["X-Proxy-Source"] = "takeover-lua-script"
    
    -- 添加连接信息
    if connection_id then
        headers["X-Connection-ID"] = connection_id
    end
    
    if target_host then
        headers["X-Original-Host"] = target_host
    end
    
    return headers
end

-- 主处理函数
function main()
    log("info", "Processing request modification for " .. (request and request.url or "unknown"))
    
    if not request then
        log("debug", "No request found, skipping modification")
        return "continue"
    end
    
    -- 记录原始请求信息
    log("info", "Original: " .. request.method .. " " .. request.url)
    
    -- 克隆请求对象进行修改
    local modified_request = {
        method = request.method,
        url = request.url,
        headers = {},
        body = request.body or {}
    }
    
    -- 复制原有头部
    if request.headers then
        for key, value in pairs(request.headers) do
            modified_request.headers[key] = value
        end
    end
    
    -- URL重写
    local original_url = modified_request.url
    modified_request.url = rewrite_url(original_url)
    
    if modified_request.url ~= original_url then
        log("info", "URL rewritten: " .. original_url .. " -> " .. modified_request.url)
    end
    
    -- 添加认证头
    modified_request.headers = add_auth_headers(modified_request.headers)
    
    -- 添加监控头
    modified_request.headers = add_monitoring_headers(modified_request.headers)
    
    -- 特殊处理：如果是POST/PUT请求，可能需要修改请求体
    if modified_request.method == "POST" or modified_request.method == "PUT" then
        -- 这里可以添加请求体修改逻辑
        log("debug", "Processing " .. modified_request.method .. " request body")
        
        -- 示例：为所有POST请求添加时间戳
        if type(modified_request.body) == "string" then
            local body_json = json.decode(modified_request.body)
            if body_json then
                body_json.modified_at = os.time()
                body_json.proxy_version = "takeover-1.0"
                modified_request.body = json.encode(body_json)
                modified_request.headers["Content-Length"] = tostring(#modified_request.body)
            end
        end
    end
    
    -- 记录修改后的请求信息
    log("info", "Modified: " .. modified_request.method .. " " .. modified_request.url)
    log("debug", "Added " .. 
        (modified_request.headers and 
         (function() 
            local count = 0
            for _ in pairs(modified_request.headers) do count = count + 1 end
            return count
         end)() or 0) .. " headers")
    
    -- 存储修改历史
    takeover.store_data("request_modification_" .. (connection_id or "unknown"), {
        original_url = original_url,
        modified_url = modified_request.url,
        timestamp = os.time(),
        headers_added = true,
        auth_applied = true
    })
    
    -- 发射修改事件
    takeover.emit_event({
        name = "request_modified",
        data = {
            connection_id = connection_id or "unknown",
            original_url = original_url,
            modified_url = modified_request.url,
            modification_count = 1
        }
    })
    
    return {
        action = "continue",
        modified_request = modified_request
    }
end

-- 错误处理
local status, result = pcall(main)
if not status then
    takeover.log("error", "[RequestModifier] Script error: " .. tostring(result))
    return "continue"
end

return result
