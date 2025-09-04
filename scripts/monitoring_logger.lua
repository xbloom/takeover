-- 监控和日志脚本
-- 收集性能指标、记录详细日志、发送告警事件
-- 
-- 场景：性能监控、安全审计、错误追踪、统计分析

-- 配置
local config = {
    log_level = "info",
    enable_performance_tracking = true,
    enable_security_audit = true,
    enable_statistics = true,
    slow_request_threshold = 5000,  -- 5秒
    large_response_threshold = 1048576,  -- 1MB
}

-- 获取配置值
local function get_config_value(key, default)
    return takeover.get_config(key) or default
end

-- 更新配置
for key, default_value in pairs(config) do
    local config_value = get_config_value("monitor_" .. key, tostring(default_value))
    if type(default_value) == "boolean" then
        config[key] = config_value:lower() == "true"
    elseif type(default_value) == "number" then
        config[key] = tonumber(config_value) or default_value
    else
        config[key] = config_value
    end
end

-- 日志函数
local function log(level, message, extra_data)
    local log_entry = "[Monitor] " .. message
    if extra_data then
        log_entry = log_entry .. " | " .. json.encode(extra_data)
    end
    takeover.log(level, log_entry)
end

-- 性能追踪
local function track_performance()
    if not config.enable_performance_tracking then
        return {}
    end
    
    local perf_data = {
        timestamp = os.time(),
        connection_id = connection_id,
        target_host = target_host,
        target_port = target_port
    }
    
    -- 记录请求开始时间
    local start_key = "request_start_" .. (connection_id or "unknown")
    takeover.store_data(start_key, json.encode(perf_data))
    
    return perf_data
end

-- 安全审计
local function security_audit()
    if not config.enable_security_audit or not request then
        return {}
    end
    
    local security_info = {
        suspicious_patterns = {},
        risk_level = "low"
    }
    
    local url = request.url or ""
    local user_agent = request.headers and request.headers["User-Agent"] or ""
    local headers = request.headers or {}
    
    -- 检查可疑的URL模式
    local suspicious_patterns = {
        "%.%.%/",           -- 路径遍历
        "script>",          -- XSS尝试
        "union.*select",    -- SQL注入
        "etc/passwd",       -- 系统文件访问
        "cmd%.exe",         -- 命令执行
        "eval%(",           -- 代码执行
    }
    
    for _, pattern in ipairs(suspicious_patterns) do
        if string.match(url:lower(), pattern) then
            table.insert(security_info.suspicious_patterns, pattern)
            security_info.risk_level = "high"
        end
    end
    
    -- 检查可疑的User-Agent
    local suspicious_ua_patterns = {
        "sqlmap",
        "nmap",
        "nikto",
        "burp",
        "zgrab"
    }
    
    for _, pattern in ipairs(suspicious_ua_patterns) do
        if string.match(user_agent:lower(), pattern) then
            table.insert(security_info.suspicious_patterns, "suspicious_ua:" .. pattern)
            security_info.risk_level = "high"
        end
    end
    
    -- 检查异常头部
    if headers["X-Forwarded-For"] and string.match(headers["X-Forwarded-For"], "127%.0%.0%.1") then
        table.insert(security_info.suspicious_patterns, "localhost_xff")
        security_info.risk_level = "medium"
    end
    
    -- 记录安全事件
    if #security_info.suspicious_patterns > 0 then
        log("warn", "Security alert detected", {
            url = url,
            patterns = security_info.suspicious_patterns,
            risk_level = security_info.risk_level,
            user_agent = user_agent,
            source_ip = "unknown"  -- 在实际实现中从连接信息获取
        })
        
        -- 发送安全事件
        takeover.emit_event({
            name = "security_alert",
            data = {
                connection_id = connection_id,
                url = url,
                patterns = security_info.suspicious_patterns,
                risk_level = security_info.risk_level,
                timestamp = os.time()
            }
        })
    end
    
    return security_info
end

-- 统计收集
local function collect_statistics()
    if not config.enable_statistics then
        return
    end
    
    local stats_data = {
        timestamp = os.time(),
        hour = os.date("%H"),
        day = os.date("%Y-%m-%d"),
        host = target_host,
        method = request and request.method or "unknown",
        path = request and request.url and string.match(request.url, "://[^/]+(/[^?]*)") or "/"
    }
    
    -- 更新各种计数器
    local counters = {
        "total_requests",
        "requests_by_hour_" .. stats_data.hour,
        "requests_by_day_" .. stats_data.day,
        "requests_by_host_" .. (stats_data.host or "unknown"),
        "requests_by_method_" .. stats_data.method
    }
    
    for _, counter in ipairs(counters) do
        local current_count = tonumber(takeover.get_data(counter) or "0")
        takeover.store_data(counter, tostring(current_count + 1))
    end
    
    -- 发送统计事件
    takeover.emit_event({
        name = "request_statistics",
        data = stats_data
    })
end

-- 详细请求日志
local function log_request_details()
    if not request then
        return
    end
    
    local request_details = {
        method = request.method,
        url = request.url,
        headers_count = 0,
        body_size = 0,
        content_type = "unknown"
    }
    
    if request.headers then
        for _ in pairs(request.headers) do
            request_details.headers_count = request_details.headers_count + 1
        end
        request_details.content_type = request.headers["Content-Type"] or "unknown"
    end
    
    if request.body then
        request_details.body_size = type(request.body) == "string" and #request.body or 0
    end
    
    log("info", "Request processed", {
        connection_id = connection_id,
        target = (target_host or "unknown") .. ":" .. (target_port or "unknown"),
        details = request_details
    })
    
    -- 记录大请求
    if request_details.body_size > config.large_response_threshold then
        log("warn", "Large request body detected", {
            size = request_details.body_size,
            threshold = config.large_response_threshold,
            url = request.url
        })
        
        takeover.emit_event({
            name = "large_request",
            data = {
                connection_id = connection_id,
                size = request_details.body_size,
                url = request.url,
                timestamp = os.time()
            }
        })
    end
end

-- 主处理函数
function main()
    log("debug", "Starting monitoring and logging", {
        connection_id = connection_id,
        target = (target_host or "unknown") .. ":" .. (target_port or "unknown")
    })
    
    -- 性能追踪
    local perf_data = track_performance()
    
    -- 安全审计
    local security_info = security_audit()
    
    -- 统计收集
    collect_statistics()
    
    -- 详细请求日志
    log_request_details()
    
    -- 合并监控数据
    local monitoring_data = {
        performance = perf_data,
        security = security_info,
        timestamp = os.time(),
        config = config
    }
    
    -- 存储完整的监控数据
    takeover.store_data("monitoring_" .. (connection_id or "unknown"), monitoring_data)
    
    -- 如果发现高风险安全问题，暂停处理
    if security_info.risk_level == "high" then
        log("error", "High security risk detected, pausing request", {
            patterns = security_info.suspicious_patterns,
            url = request and request.url
        })
        
        return {
            action = "pause",
            reason = "High security risk detected: " .. table.concat(security_info.suspicious_patterns, ", ")
        }
    end
    
    log("debug", "Monitoring completed successfully")
    
    -- 返回监控数据作为自定义数据
    return {
        action = "continue",
        custom_data = {
            monitoring = monitoring_data
        }
    }
end

-- 清理函数（在请求完成后调用）
function cleanup()
    if not config.enable_performance_tracking then
        return
    end
    
    local start_key = "request_start_" .. (connection_id or "unknown")
    local start_data = takeover.get_data(start_key)
    
    if start_data then
        local start_info = json.decode(start_data)
        if start_info then
            local duration = os.time() - start_info.timestamp
            
            -- 记录慢请求
            if duration > config.slow_request_threshold / 1000 then
                log("warn", "Slow request detected", {
                    duration_seconds = duration,
                    threshold_seconds = config.slow_request_threshold / 1000,
                    url = request and request.url
                })
                
                takeover.emit_event({
                    name = "slow_request",
                    data = {
                        connection_id = connection_id,
                        duration = duration,
                        url = request and request.url,
                        timestamp = os.time()
                    }
                })
            end
            
            -- 更新性能统计
            local perf_key = "performance_stats"
            local perf_stats = takeover.get_data(perf_key)
            if perf_stats then
                perf_stats = json.decode(perf_stats) or {}
            else
                perf_stats = {total_requests = 0, total_duration = 0, avg_duration = 0}
            end
            
            perf_stats.total_requests = perf_stats.total_requests + 1
            perf_stats.total_duration = perf_stats.total_duration + duration
            perf_stats.avg_duration = perf_stats.total_duration / perf_stats.total_requests
            
            takeover.store_data(perf_key, json.encode(perf_stats))
        end
        
        -- 清理临时数据
        takeover.store_data(start_key, nil)
    end
end

-- 错误处理
local status, result = pcall(main)
if not status then
    takeover.log("error", "[Monitor] Script error: " .. tostring(result))
    return "continue"
end

return result
