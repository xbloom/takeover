-- 响应处理脚本
-- 处理和修改HTTP响应
-- 
-- 场景：响应内容过滤、数据脱敏、格式转换、缓存控制

-- 日志函数
local function log(level, message)
    takeover.log(level, "[ResponseProcessor] " .. message)
end

-- 内容过滤配置
local content_filters = {
    -- 敏感信息脱敏
    sensitive_patterns = {
        {pattern = "%d%d%d%d%-%d%d%d%d%-%d%d%d%d%-%d%d%d%d", replacement = "****-****-****-****"},  -- 信用卡号
        {pattern = "%d%d%d%-%d%d%-%d%d%d%d", replacement = "***-**-****"},  -- SSN
        {pattern = "[%w%.%-]+@[%w%.%-]+%.%w+", replacement = "***@***.***"},  -- 邮箱
        {pattern = "%+?%d?%d?%d?[%-%s]?%(?%d%d%d%)?[%-%s]?%d%d%d[%-%s]?%d%d%d%d", replacement = "***-***-****"}  -- 电话号码
    },
    
    -- 广告和追踪代码过滤
    ad_patterns = {
        "google%-analytics%.com",
        "googletagmanager%.com",
        "facebook%.com/tr",
        "doubleclick%.net",
        "googlesyndication%.com"
    },
    
    -- 恶意脚本模式
    malicious_patterns = {
        "<script[^>]*>.*eval%(",
        "<script[^>]*>.*document%.write%(",
        "javascript:.*eval%(",
        "on%w+%s*=%s*[\"'].*eval%("
    }
}

-- 数据脱敏处理
local function sanitize_sensitive_data(content)
    if type(content) ~= "string" then
        return content
    end
    
    local sanitized = content
    local replacements = 0
    
    for _, filter in ipairs(content_filters.sensitive_patterns) do
        local new_content, count = string.gsub(sanitized, filter.pattern, filter.replacement)
        sanitized = new_content
        replacements = replacements + count
    end
    
    if replacements > 0 then
        log("info", "Sanitized " .. replacements .. " sensitive data patterns")
    end
    
    return sanitized
end

-- 广告和追踪代码过滤
local function filter_tracking_content(content)
    if type(content) ~= "string" then
        return content
    end
    
    local filtered = content
    local removals = 0
    
    for _, pattern in ipairs(content_filters.ad_patterns) do
        local new_content, count = string.gsub(filtered, pattern, "blocked-tracking-domain")
        filtered = new_content
        removals = removals + count
    end
    
    if removals > 0 then
        log("info", "Filtered " .. removals .. " tracking/ad patterns")
    end
    
    return filtered
end

-- 恶意脚本检测和移除
local function remove_malicious_scripts(content)
    if type(content) ~= "string" then
        return content
    end
    
    local cleaned = content
    local threats_found = 0
    
    for _, pattern in ipairs(content_filters.malicious_patterns) do
        if string.match(cleaned:lower(), pattern:lower()) then
            threats_found = threats_found + 1
            cleaned = string.gsub(cleaned, pattern, "<!-- MALICIOUS_SCRIPT_REMOVED -->")
        end
    end
    
    if threats_found > 0 then
        log("warn", "Removed " .. threats_found .. " potentially malicious scripts")
        
        -- 发送安全事件
        takeover.emit_event({
            name = "malicious_script_detected",
            data = {
                connection_id = connection_id,
                threats_count = threats_found,
                timestamp = os.time()
            }
        })
    end
    
    return cleaned
end

-- JSON响应处理
local function process_json_response(json_str)
    local success, json_data = pcall(json.decode, json_str)
    if not success then
        log("warn", "Failed to parse JSON response")
        return json_str
    end
    
    local modified = false
    
    -- 添加代理信息
    if type(json_data) == "table" then
        json_data._proxy_info = {
            processed_by = "takeover-proxy",
            timestamp = os.time(),
            version = "1.0"
        }
        modified = true
        
        -- 移除调试信息
        if json_data.debug then
            json_data.debug = nil
            modified = true
            log("debug", "Removed debug information from JSON response")
        end
        
        -- 脱敏处理
        if json_data.user and json_data.user.email then
            local original_email = json_data.user.email
            json_data.user.email = sanitize_sensitive_data(original_email)
            if json_data.user.email ~= original_email then
                modified = true
            end
        end
    end
    
    if modified then
        local success_encode, result = pcall(json.encode, json_data)
        if success_encode then
            return result
        else
            log("warn", "Failed to encode modified JSON")
            return json_str
        end
    end
    
    return json_str
end

-- HTML响应处理
local function process_html_response(html_content)
    local processed = html_content
    
    -- 移除恶意脚本
    processed = remove_malicious_scripts(processed)
    
    -- 过滤追踪代码
    processed = filter_tracking_content(processed)
    
    -- 脱敏敏感数据
    processed = sanitize_sensitive_data(processed)
    
    -- 添加安全头部（在HTML中添加meta标签）
    if not string.match(processed, "<meta.*Content%-Security%-Policy") then
        local csp_meta = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'self'; script-src 'self' 'unsafe-inline'\">"
        processed = string.gsub(processed, "(<head[^>]*>)", "%1\n" .. csp_meta)
        log("debug", "Added CSP meta tag to HTML response")
    end
    
    -- 添加代理标识注释
    local proxy_comment = "<!-- Processed by Takeover Proxy at " .. os.date("%Y-%m-%d %H:%M:%S") .. " -->"
    processed = string.gsub(processed, "(</body>)", proxy_comment .. "\n%1")
    
    return processed
end

-- 缓存控制
local function apply_cache_control(headers, content_type)
    -- 为不同类型的内容应用不同的缓存策略
    if string.match(content_type or "", "application/json") then
        -- API响应：短时间缓存
        headers["Cache-Control"] = "private, max-age=300"
        headers["Vary"] = "Authorization"
    elseif string.match(content_type or "", "text/html") then
        -- HTML页面：更短的缓存时间
        headers["Cache-Control"] = "private, max-age=60"
    elseif string.match(content_type or "", "image/") then
        -- 图片：长时间缓存
        headers["Cache-Control"] = "public, max-age=86400"
    elseif string.match(content_type or "", "text/css") or string.match(content_type or "", "application/javascript") then
        -- CSS/JS：中等缓存时间
        headers["Cache-Control"] = "public, max-age=3600"
    end
    
    -- 添加代理标识头
    headers["X-Processed-By"] = "Takeover-Proxy"
    headers["X-Processing-Time"] = tostring(os.time())
    
    return headers
end

-- 主处理函数
function main()
    log("info", "Processing response for " .. (target_host or "unknown"))
    
    if not response then
        log("debug", "No response found, skipping processing")
        return "continue"
    end
    
    local content_type = ""
    local headers = response.headers or {}
    
    -- 获取Content-Type
    for key, value in pairs(headers) do
        if key:lower() == "content-type" then
            content_type = value:lower()
            break
        end
    end
    
    log("info", "Processing response: " .. (response.status_code or "unknown") .. 
               " (" .. content_type .. "), " .. 
               (response.body and #response.body or 0) .. " bytes")
    
    -- 复制响应以进行修改
    local modified_response = {
        status_code = response.status_code,
        status_text = response.status_text,
        headers = {},
        body = response.body,
        id = response.id,
        sent_at = response.sent_at
    }
    
    -- 复制头部
    for key, value in pairs(headers) do
        modified_response.headers[key] = value
    end
    
    -- 处理响应体
    local original_body = response.body
    local processed_body = original_body
    
    if original_body and #original_body > 0 then
        local body_str = type(original_body) == "string" and original_body or string.char(table.unpack(original_body))
        
        if string.match(content_type, "application/json") then
            log("debug", "Processing JSON response")
            processed_body = process_json_response(body_str)
        elseif string.match(content_type, "text/html") then
            log("debug", "Processing HTML response")
            processed_body = process_html_response(body_str)
        elseif string.match(content_type, "text/plain") then
            log("debug", "Processing plain text response")
            processed_body = sanitize_sensitive_data(body_str)
        else
            log("debug", "Content type not supported for processing: " .. content_type)
        end
        
        -- 转换回字节数组格式（如果需要）
        if type(original_body) ~= "string" and type(processed_body) == "string" then
            local byte_array = {}
            for i = 1, #processed_body do
                table.insert(byte_array, string.byte(processed_body, i))
            end
            modified_response.body = byte_array
        else
            modified_response.body = processed_body
        end
        
        -- 更新Content-Length
        local new_length = type(processed_body) == "string" and #processed_body or #modified_response.body
        modified_response.headers["Content-Length"] = tostring(new_length)
        
        if processed_body ~= body_str then
            log("info", "Response content modified, size changed: " .. 
                       #body_str .. " -> " .. new_length)
        end
    end
    
    -- 应用缓存控制
    modified_response.headers = apply_cache_control(modified_response.headers, content_type)
    
    -- 添加安全头部
    modified_response.headers["X-Content-Type-Options"] = "nosniff"
    modified_response.headers["X-Frame-Options"] = "SAMEORIGIN"
    modified_response.headers["Referrer-Policy"] = "strict-origin-when-cross-origin"
    
    -- 记录处理统计
    takeover.store_data("response_processing_" .. (connection_id or "unknown"), {
        original_size = original_body and #original_body or 0,
        processed_size = modified_response.body and 
                        (type(modified_response.body) == "string" and #modified_response.body or #modified_response.body) or 0,
        content_type = content_type,
        status_code = response.status_code,
        timestamp = os.time(),
        headers_added = 4  -- 添加了4个安全头部
    })
    
    -- 发送处理完成事件
    takeover.emit_event({
        name = "response_processed",
        data = {
            connection_id = connection_id,
            content_type = content_type,
            status_code = response.status_code,
            size_change = (modified_response.body and 
                          (type(modified_response.body) == "string" and #modified_response.body or #modified_response.body) or 0) - 
                         (original_body and #original_body or 0),
            timestamp = os.time()
        }
    })
    
    log("info", "Response processing completed successfully")
    
    return {
        action = "continue",
        modified_response = modified_response
    }
end

-- 错误处理
local status, result = pcall(main)
if not status then
    takeover.log("error", "[ResponseProcessor] Script error: " .. tostring(result))
    return "continue"
end

return result
