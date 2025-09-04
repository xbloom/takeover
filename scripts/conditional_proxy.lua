-- 条件代理脚本
-- 基于各种条件决定如何处理请求
-- 
-- 场景：智能路由、负载均衡、A/B测试、地理位置路由等

-- 日志函数
local function log(level, message)
    takeover.log(level, "[ConditionalProxy] " .. message)
end

-- 检查是否在维护时间
local function is_maintenance_window()
    local current_hour = tonumber(os.date("%H"))
    local maintenance_start = tonumber(takeover.get_config("maintenance_start_hour") or "2")
    local maintenance_end = tonumber(takeover.get_config("maintenance_end_hour") or "4")
    
    return current_hour >= maintenance_start and current_hour <= maintenance_end
end

-- 检查用户是否在A/B测试组
local function is_ab_test_user(user_agent)
    if not user_agent then return false end
    
    -- 简单的哈希算法来分配A/B测试组
    local hash = 0
    for i = 1, #user_agent do
        hash = hash + string.byte(user_agent, i)
    end
    
    -- 50% 的用户进入A组
    return (hash % 100) < 50
end

-- 获取地理位置（简化版本）
local function get_geo_region(ip)
    -- 在实际实现中，这里会查询GeoIP数据库
    -- 这里用简单的IP段来模拟
    if string.match(ip, "^192%.168%.") then
        return "internal"
    elseif string.match(ip, "^10%.") then
        return "staging"
    else
        return "external"
    end
end

-- 负载均衡算法
local function select_backend_server(service_name)
    local backends = {
        api = {"api1.internal.com", "api2.internal.com", "api3.internal.com"},
        auth = {"auth1.internal.com", "auth2.internal.com"},
        static = {"cdn1.example.com", "cdn2.example.com"}
    }
    
    local service_backends = backends[service_name]
    if not service_backends then
        return nil
    end
    
    -- 简单的轮询算法
    local count_key = "backend_count_" .. service_name
    local count = tonumber(takeover.get_data(count_key) or "0")
    count = (count + 1) % #service_backends
    takeover.store_data(count_key, tostring(count))
    
    return service_backends[count + 1]
end

-- 主处理函数
function main()
    log("info", "Evaluating routing conditions for " .. (request and request.url or "unknown"))
    
    if not request then
        log("debug", "No request found, using default routing")
        return "continue"
    end
    
    local url = request.url
    local user_agent = request.headers and request.headers["User-Agent"] or ""
    local host = target_host or "unknown"
    
    log("info", "Processing request to " .. host .. " from user-agent: " .. 
        (string.sub(user_agent, 1, 50) .. (string.len(user_agent) > 50 and "..." or "")))
    
    -- 检查维护窗口
    if is_maintenance_window() then
        log("warn", "Request during maintenance window, redirecting to maintenance page")
        
        takeover.emit_event({
            name = "maintenance_redirect",
            data = {
                original_url = url,
                user_agent = user_agent,
                timestamp = os.time()
            }
        })
        
        return {
            action = "redirect",
            url = "https://maintenance.example.com/maintenance.html"
        }
    end
    
    -- 特殊URL处理
    if string.match(url, "/api/admin/") then
        log("warn", "Admin API access attempted")
        
        -- 检查是否有管理员权限
        local admin_token = request.headers and request.headers["X-Admin-Token"]
        local valid_admin_token = takeover.get_config("admin_token")
        
        if not admin_token or admin_token ~= valid_admin_token then
            log("warn", "Unauthorized admin API access rejected")
            return {
                action = "reject",
                reason = "Unauthorized admin access"
            }
        end
        
        log("info", "Admin API access authorized")
    end
    
    -- A/B测试路由
    if string.match(url, "/api/v1/") and is_ab_test_user(user_agent) then
        log("info", "User in A/B test group, routing to experimental API")
        
        local new_url = string.gsub(url, "/api/v1/", "/api/v2-experimental/")
        
        takeover.store_data("ab_test_" .. (connection_id or "unknown"), {
            original_url = url,
            experimental_url = new_url,
            user_agent = user_agent,
            timestamp = os.time()
        })
        
        return {
            action = "continue",
            modified_request = {
                method = request.method,
                url = new_url,
                headers = request.headers,
                body = request.body
            }
        }
    end
    
    -- 负载均衡
    local service_name = nil
    if string.match(url, "/api/") then
        service_name = "api"
    elseif string.match(url, "/auth/") then
        service_name = "auth"
    elseif string.match(url, "/static/") then
        service_name = "static"
    end
    
    if service_name then
        local backend = select_backend_server(service_name)
        if backend then
            log("info", "Load balancing " .. service_name .. " service to " .. backend)
            
            -- 替换主机名
            local new_url = string.gsub(url, "://[^/]+", "://" .. backend)
            
            takeover.store_data("load_balance_" .. (connection_id or "unknown"), {
                service = service_name,
                backend = backend,
                original_url = url,
                balanced_url = new_url,
                timestamp = os.time()
            })
            
            return {
                action = "continue",
                modified_request = {
                    method = request.method,
                    url = new_url,
                    headers = request.headers,
                    body = request.body
                }
            }
        end
    end
    
    -- 地理位置路由
    local source_ip = "192.168.1.100"  -- 在实际实现中从连接信息获取
    local region = get_geo_region(source_ip)
    
    log("debug", "Request from region: " .. region)
    
    if region == "internal" then
        log("info", "Internal request, using fast internal routing")
        -- 内部请求可以使用更快的路由
    elseif region == "external" then
        log("info", "External request, applying rate limiting checks")
        
        -- 外部请求需要检查速率限制
        local rate_limit_key = "rate_limit_" .. source_ip
        local request_count = tonumber(takeover.get_data(rate_limit_key) or "0")
        local rate_limit = tonumber(takeover.get_config("external_rate_limit") or "100")
        
        if request_count >= rate_limit then
            log("warn", "Rate limit exceeded for " .. source_ip)
            return {
                action = "reject",
                reason = "Rate limit exceeded"
            }
        end
        
        takeover.store_data(rate_limit_key, tostring(request_count + 1))
    end
    
    -- 记录路由决策
    takeover.store_data("routing_decision_" .. (connection_id or "unknown"), {
        url = url,
        host = host,
        region = region,
        service = service_name,
        timestamp = os.time(),
        user_agent_hash = tostring(string.len(user_agent))
    })
    
    log("info", "Standard routing applied for " .. host)
    return "continue"
end

-- 错误处理
local status, result = pcall(main)
if not status then
    takeover.log("error", "[ConditionalProxy] Script error: " .. tostring(result))
    return "continue"
end

return result
