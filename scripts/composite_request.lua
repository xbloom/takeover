-- 复合请求示例脚本
-- 展示如何发起多个HTTP请求并合并响应
-- 
-- 场景：当用户访问某个API时，同时从多个数据源获取信息并合并

-- 日志函数封装
local function log(level, message)
    takeover.log(level, "[CompositeRequest] " .. message)
end

-- 主处理函数
function main()
    log("info", "Starting composite request processing for " .. (request and request.url or "unknown URL"))
    
    -- 检查是否是我们关心的请求
    if not request or not request.url then
        log("debug", "No request or URL found, skipping")
        return "continue"
    end
    
    -- 只处理特定的API路径
    if not string.match(request.url, "/api/user/profile") then
        log("debug", "URL doesn't match profile API pattern, skipping")
        return "continue"
    end
    
    log("info", "Processing user profile request, will fetch from multiple sources")
    
    -- 从URL中提取用户ID
    local user_id = string.match(request.url, "/api/user/profile/(%d+)")
    if not user_id then
        log("warn", "Could not extract user ID from URL")
        return "continue"
    end
    
    log("info", "Extracted user ID: " .. user_id)
    
    -- 准备多个请求
    local requests = {}
    
    -- 1. 基本用户信息
    table.insert(requests, {
        method = "GET",
        url = "https://api.internal.com/users/" .. user_id,
        headers = {
            ["Authorization"] = "Bearer " .. (takeover.get_config("internal_api_token") or ""),
            ["Content-Type"] = "application/json"
        }
    })
    
    -- 2. 用户权限信息
    table.insert(requests, {
        method = "GET",
        url = "https://permissions.internal.com/users/" .. user_id .. "/roles",
        headers = {
            ["Authorization"] = "Bearer " .. (takeover.get_config("permissions_api_token") or ""),
            ["Content-Type"] = "application/json"
        }
    })
    
    -- 3. 用户活动统计
    table.insert(requests, {
        method = "GET",
        url = "https://analytics.internal.com/users/" .. user_id .. "/stats",
        headers = {
            ["Authorization"] = "Bearer " .. (takeover.get_config("analytics_api_token") or ""),
            ["Content-Type"] = "application/json"
        }
    })
    
    -- 存储原始请求信息以便追踪
    takeover.store_data("composite_request_" .. user_id, {
        original_url = request.url,
        timestamp = os.time(),
        user_id = user_id,
        source_requests = #requests
    })
    
    log("info", "Initiating composite request with " .. #requests .. " parallel requests")
    
    -- 返回复合请求配置
    return {
        action = "composite_request",
        requests = requests,
        merge_strategy = "merge_json",  -- 合并JSON响应
        timeout = 30  -- 30秒超时
    }
end

-- 错误处理
local status, result = pcall(main)
if not status then
    takeover.log("error", "[CompositeRequest] Script error: " .. tostring(result))
    return "continue"  -- 出错时继续正常流程
end

return result
