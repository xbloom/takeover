use anyhow::Result;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use sqids::Sqids;
use std::sync::OnceLock;

/// Parse a proxy URL into components
pub fn parse_proxy_url(url: &str) -> Result<(String, String, u16, Option<(String, String)>)> {
    let url = url.trim();
    
    // Extract scheme
    let (scheme, remaining) = if let Some(pos) = url.find("://") {
        let scheme = &url[..pos];
        let remaining = &url[pos + 3..];
        (scheme, remaining)
    } else {
        return Err(anyhow::anyhow!("Invalid proxy URL format: missing scheme"));
    };
    
    // Extract auth info if present
    let (auth, host_port) = if let Some(at_pos) = remaining.find('@') {
        let auth_str = &remaining[..at_pos];
        let host_port = &remaining[at_pos + 1..];
        
        if let Some(colon_pos) = auth_str.find(':') {
            let username = auth_str[..colon_pos].to_string();
            let password = auth_str[colon_pos + 1..].to_string();
            (Some((username, password)), host_port)
        } else {
            (None, remaining)
        }
    } else {
        (None, remaining)
    };
    
    // Extract host and port
    let (host, port) = if let Some(colon_pos) = host_port.rfind(':') {
        let host = host_port[..colon_pos].to_string();
        let port_str = &host_port[colon_pos + 1..];
        let port = port_str.parse::<u16>()
            .map_err(|_| anyhow::anyhow!("Invalid port number: {}", port_str))?;
        (host, port)
    } else {
        return Err(anyhow::anyhow!("Invalid proxy URL format: missing port"));
    };
    
    Ok((scheme.to_string(), host, port, auth))
}

/// Check if the current process has sufficient privileges
pub fn check_privileges() -> Result<bool> {
    #[cfg(unix)]
    {
        // Check if effective user ID is 0 (root)
        unsafe {
            Ok(libc::geteuid() == 0)
        }
    }
    
    #[cfg(not(unix))]
    {
        warn!("Privilege checking not implemented for this platform");
        Ok(true)
    }
}

/// Request privilege escalation from the user using macOS Authorization Services
/// Returns true if escalation was successful, false if denied/failed
pub fn request_privilege_escalation() -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        println!("🔒 此程序需要管理员权限来修改系统网络规则");
        
        // Try to use osascript to show a system authentication dialog
        use std::process::Command;
        
        let script = r#"
        do shell script "sudo -v && echo 'Privileges verified'" with administrator privileges
        "#;
        
        println!("🔑 请在弹出的对话框中输入您的密码...");
        
        let output = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()?;
            
        if output.status.success() {
            // 确保 sudo 缓存真正建立 - 再次验证
            let sudo_check = Command::new("sudo")
                .arg("-n")  // 非交互模式
                .arg("true") // 简单的 true 命令
                .output()?;
                
            if sudo_check.status.success() {
                println!("✅ 权限验证成功！程序将继续以当前用户身份运行");
                println!("💡 需要管理员权限的操作将使用已验证的凭据");
                Ok(true)
            } else {
                println!("⚠️  AppleScript 验证成功，但 sudo 缓存未建立，将重试...");
                // 重新建立 sudo 缓存
                let sudo_refresh = Command::new("sudo")
                    .arg("-v")
                    .status()?;
                if sudo_refresh.success() {
                    println!("✅ sudo 缓存已建立");
                    Ok(true)
                } else {
                    println!("❌ 无法建立 sudo 缓存");
                    Ok(false)
                }
            }
        } else {
            println!("❌ 权限验证失败或被取消");
            Ok(false)
        }
    }
    
    #[cfg(not(target_os = "macos"))]
    {
        println!("⚠️  此权限提升方式仅支持 macOS");
        Ok(false)
    }
}

/// Execute a command with administrator privileges using sudo
pub fn execute_admin_command(command: &str, args: &[&str]) -> Result<std::process::Output> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        
        // Use sudo with cached credentials
        let mut cmd = Command::new("sudo");
        cmd.arg("-n");
        cmd.arg(command);
        for arg in args {
            cmd.arg(arg);
        }
        
        let output = cmd.output()?;
        Ok(output)
    }
    
    #[cfg(not(target_os = "macos"))]
    {
        Err(anyhow::anyhow!("Admin command execution not supported on this platform"))
    }
}

/// Get the executable path for a given process ID
pub fn get_process_executable_path(pid: u32) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        get_macos_process_path(pid)
    }
    
    #[cfg(target_os = "linux")]
    {
        get_linux_process_path(pid)
    }
    
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(anyhow::anyhow!("Process path lookup not implemented for this platform"))
    }
}

#[cfg(target_os = "macos")]
fn get_macos_process_path(pid: u32) -> Result<String> {
    use std::ffi::CStr;
    use std::mem;
    
    unsafe {
        let mut path_buf: [libc::c_char; libc::PATH_MAX as usize] = mem::zeroed();
        let ret = libc::proc_pidpath(
            pid as libc::c_int,
            path_buf.as_mut_ptr() as *mut libc::c_void,
            libc::PATH_MAX as u32,
        );
        
        if ret <= 0 {
            return Err(anyhow::anyhow!("Failed to get process path for PID {}", pid));
        }
        
        let path = CStr::from_ptr(path_buf.as_ptr())
            .to_string_lossy()
            .into_owned();
        
        Ok(path)
    }
}

#[cfg(target_os = "linux")]
fn get_linux_process_path(pid: u32) -> Result<String> {
    let exe_link = format!("/proc/{}/exe", pid);
    match std::fs::read_link(&exe_link) {
        Ok(path) => Ok(path.to_string_lossy().into_owned()),
        Err(e) => Err(anyhow::anyhow!("Failed to read process path for PID {}: {}", pid, e)),
    }
}

/// Validate if an IP address is valid
pub fn is_valid_ip(addr: &str) -> bool {
    IpAddr::from_str(addr).is_ok()
}

/// Validate if a socket address is valid
pub fn is_valid_socket_addr(addr: &str) -> bool {
    SocketAddr::from_str(addr).is_ok()
}

/// Get the current system's default network interface
pub fn get_default_interface() -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        // Use route command to get default interface
        let output = std::process::Command::new("route")
            .args(&["get", "default"])
            .output()?;
        
        let output_str = String::from_utf8_lossy(&output.stdout);
        for line in output_str.lines() {
            if line.trim().starts_with("interface:") {
                let interface = line.split(':').nth(1)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                return Ok(interface);
            }
        }
        
        Err(anyhow::anyhow!("Could not determine default interface"))
    }
    
    #[cfg(target_os = "linux")]
    {
        // Read from /proc/net/route
        let content = std::fs::read_to_string("/proc/net/route")?;
        for line in content.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 2 && fields[1] == "00000000" {
                return Ok(fields[0].to_string());
            }
        }
        
        Err(anyhow::anyhow!("Could not determine default interface"))
    }
    
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(anyhow::anyhow!("Interface detection not implemented for this platform"))
    }
}

// ========================================
// 短ID生成器 - 使用Sqids
// ========================================

// 全局Sqids实例，用于生成短连接ID
static SQIDS_INSTANCE: OnceLock<Sqids> = OnceLock::new();

/// 获取或初始化Sqids实例
fn get_sqids() -> &'static Sqids {
    SQIDS_INSTANCE.get_or_init(|| {
        // 配置Sqids：8-10字符长度，避免不良词汇
        Sqids::builder()
            .alphabet("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".chars().collect())
            .min_length(8)  // 最少8字符
            .build()
            .expect("Failed to create Sqids instance")
    })
}

/// 生成短连接ID
/// 
/// 使用Sqids从当前时间戳和随机数生成短而美观的ID
/// 生成的ID格式类似: `86Rf07xd4z`, `K5Mn3Pq8vR`
pub fn generate_short_connection_id() -> String {
    let sqids = get_sqids();
    
    // 使用当前时间的纳秒数和随机数确保唯一性
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    
    let random_part = fastrand::u64(1..1000000);
    
    // 编码生成短ID
    sqids.encode(&[now_ns % 1000000, random_part])
        .unwrap_or_else(|_| {
            // 降级方案：如果Sqids失败，使用简单的十六进制
            format!("{:08X}", fastrand::u32(..))
        })
}
