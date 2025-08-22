use anyhow::Result;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

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
