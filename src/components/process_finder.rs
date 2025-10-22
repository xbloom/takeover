//! 进程查询组件
//! 
//! 负责查询网络连接对应的进程信息的核心业务逻辑
//! 
//! ## macOS实现说明
//! 
//! **参考mihomo项目的macOS实现**:
//! - GitHub: https://github.com/MetaCubeX/mihomo/blob/dev-vision/component/process/process_darwin.go
//! - 使用`net.inet.tcp.pcblist_n` sysctl获取PCB列表
//! - Darwin >= 22 (macOS 13.0+): PCB结构体大小 408 字节
//! - Darwin < 22 (macOS 12.x-): PCB结构体大小 384 字节
//! - 关键偏移量：
//!   - xinpcb_n: +0
//!   - xsocket_n: +104
//!   - 源端口: inp+18:inp+20 (big endian)
//!   - IP标志: inp+44 (inp_vflag)
//!   - IPv4地址: inp+76:inp+80
//!   - IPv6地址: inp+64:inp+80
//!   - 进程PID: so+68:so+72 (little endian)

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::net::{SocketAddr, IpAddr, Ipv4Addr, Ipv6Addr};
use tracing::{warn, debug, instrument};
use crate::error::TakeoverError;

/// 进程查询组件配置
#[derive(Debug, Clone)]
pub struct ProcessFinderConfig {
    /// 是否启用缓存
    pub enable_cache: bool,
    /// 缓存过期时间（秒）
    pub cache_ttl: u64,
    /// 查询超时时间
    pub query_timeout: std::time::Duration,
    /// 是否查询完整路径
    pub full_path: bool,
}

/// 进程信息
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProcessInfo {
    /// 进程ID
    pub pid: Option<u32>,
    /// 进程名称
    pub process_name: Option<String>,
    /// 可执行文件路径
    pub executable_path: Option<String>,
    /// 用户ID
    pub user_id: Option<u32>,
    /// 查询时间戳
    pub timestamp: std::time::SystemTime,
}

/// 进程查询统计信息
#[derive(Debug, Default, Clone)]
pub struct ProcessFinderStats {
    /// 总查询次数
    pub total_queries: u64,
    /// 成功查询次数
    pub successful_queries: u64,
    /// 缓存命中次数
    pub cache_hits: u64,
    /// 查询失败次数
    pub query_failures: u64,
    /// 系统调用失败次数
    pub syscall_failures: u64,
}

/// 网络协议类型
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum NetworkProtocol {
    Tcp,
    Udp,
}

/// 进程查询组件
/// 
/// 提供网络连接到进程信息的查询功能
pub struct ProcessFinder {
    /// 配置
    config: ProcessFinderConfig,
    /// 进程信息缓存
    cache: Arc<RwLock<HashMap<String, ProcessInfo>>>,
    /// 统计信息
    stats: Arc<RwLock<ProcessFinderStats>>,
}

impl ProcessFinder {
    /// 创建新的进程查询组件
    pub fn new() -> Self {
        Self {
            config: ProcessFinderConfig::default(),
            cache: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(RwLock::new(ProcessFinderStats::default())),
        }
    }
    
    /// 使用配置创建组件
    pub fn with_config(config: ProcessFinderConfig) -> Self {
        Self {
            config,
            cache: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(RwLock::new(ProcessFinderStats::default())),
        }
    }
    
    /// 查询连接对应的进程信息
    /// 
    /// # 参数
    /// - `source_addr`: 连接的源地址
    /// 
    /// # 返回
    /// 返回进程信息，如果查询失败则返回错误
    #[instrument(skip(self))]
    pub async fn find_process(&self, source_addr: SocketAddr) -> Result<ProcessInfo, TakeoverError> {
        debug!("开始查询进程信息: {}", source_addr);
        
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.total_queries += 1;
        }
        
        // 构建缓存键
        let cache_key = format!("{}:{}", source_addr.ip(), source_addr.port());
        
        // 检查缓存
        if self.config.enable_cache {
            if let Some(cached_info) = self.check_cache(&cache_key).await {
                debug!("命中进程信息缓存: {}", cache_key);
                let mut stats = self.stats.write().await;
                stats.cache_hits += 1;
                return Ok(cached_info);
            }
        }
        
        // 执行系统查询
        let process_info = match tokio::time::timeout(
            self.config.query_timeout,
            self.system_query_process(source_addr)
        ).await {
            Ok(Ok(info)) => {
                debug!("系统查询成功: {:?}", info);
                let mut stats = self.stats.write().await;
                stats.successful_queries += 1;
                info
            }
            Ok(Err(e)) => {
                warn!("系统查询失败: {}", e);
                let mut stats = self.stats.write().await;
                stats.query_failures += 1;
                return Err(e);
            }
            Err(_) => {
                warn!("系统查询超时");
                let mut stats = self.stats.write().await;
                stats.query_failures += 1;
                return Err(TakeoverError::System("Process query timeout".to_string()));
            }
        };
        
        // 存入缓存
        if self.config.enable_cache {
            self.update_cache(cache_key, process_info.clone()).await;
        }
        
        Ok(process_info)
    }
    
    /// 系统级进程查询
    #[cfg(target_os = "macos")]
    async fn system_query_process(&self, source_addr: SocketAddr) -> Result<ProcessInfo, TakeoverError> {
        debug!("macOS系统查询进程信息: {}", source_addr);
        
        // 确定协议类型
        let protocol = NetworkProtocol::Tcp; // 透明代理主要处理TCP
        
        // 调用系统查询（在线程池中执行，避免阻塞异步运行时）
        let result = tokio::task::spawn_blocking(move || {
            Self::find_process_by_socket(protocol, source_addr.ip(), source_addr.port())
        }).await
        .map_err(|e| TakeoverError::System(format!("Spawn blocking failed: {}", e)))?;
        
        match result {
            Ok((pid, path)) => {
                let process_name = path.split('/').last().map(|s| s.to_string());
                
                // 查询用户ID
                let user_id = Self::get_process_user_id(pid).unwrap_or_else(|e| {
                    debug!("获取进程{}用户ID失败: {}", pid, e);
                    None
                });
                
                debug!("进程查询完成: PID={}, 名称={:?}, 用户ID={:?}", 
                       pid, process_name, user_id);
                
                Ok(ProcessInfo {
                    pid: Some(pid),
                    process_name,
                    executable_path: if self.config.full_path { Some(path) } else { None },
                    user_id,
                    timestamp: std::time::SystemTime::now(),
                })
            }
            Err(e) => {
                let mut stats_guard = self.stats.try_write()
                    .map_err(|_| TakeoverError::System("Stats lock failed".to_string()))?;
                stats_guard.syscall_failures += 1;
                Err(e)
            }
        }
    }
    
    /// 非macOS系统的进程查询
    #[cfg(not(target_os = "macos"))]
    async fn system_query_process(&self, _source_addr: SocketAddr) -> Result<ProcessInfo, TakeoverError> {
        warn!("当前平台不支持进程查询，返回空信息");
        Ok(ProcessInfo {
            pid: None,
            process_name: None,
            executable_path: None,
            user_id: None,
            timestamp: std::time::SystemTime::now(),
        })
    }
    
    /// macOS特定的系统调用实现
    #[cfg(target_os = "macos")]
    fn find_process_by_socket(protocol: NetworkProtocol, ip: IpAddr, port: u16) -> Result<(u32, String), TakeoverError> {
        debug!("查找socket对应进程: {:?} {}:{}", protocol, ip, port);
        
        // 构建sysctl路径
        let sysctl_path = match protocol {
            NetworkProtocol::Tcp => "net.inet.tcp.pcblist_n",
            NetworkProtocol::Udp => "net.inet.udp.pcblist_n",
        };
        
        // 执行sysctl调用
        let pcb_data = Self::sysctl_by_name(sysctl_path)?;
        debug!("获取PCB数据: {} bytes", pcb_data.len());
        
        // 解析PCB数据
        let (pid, path) = Self::parse_pcb_list(&pcb_data, protocol, ip, port)?;
        
        debug!("找到进程: PID={}, Path={}", pid, path);
        Ok((pid, path))
    }
    
    /// 执行sysctl系统调用
    #[cfg(target_os = "macos")]
    fn sysctl_by_name(name: &str) -> Result<Vec<u8>, TakeoverError> {
        use libc::{sysctlbyname, size_t};
        use std::ffi::CString;
        use std::ptr;
        
        let c_name = CString::new(name)
            .map_err(|e| TakeoverError::System(format!("Invalid sysctl name: {}", e)))?;
        
        // 第一次调用获取大小
        let mut size: size_t = 0;
        let ret = unsafe {
            sysctlbyname(
                c_name.as_ptr(),
                ptr::null_mut(),
                &mut size,
                ptr::null_mut(),
                0,
            )
        };
        
        if ret != 0 {
            return Err(TakeoverError::System(format!("sysctl size query failed: {}", ret)));
        }
        
        // 第二次调用获取数据
        let mut buffer = vec![0u8; size];
        let ret = unsafe {
            sysctlbyname(
                c_name.as_ptr(),
                buffer.as_mut_ptr() as *mut libc::c_void,
                &mut size,
                ptr::null_mut(),
                0,
            )
        };
        
        if ret != 0 {
            return Err(TakeoverError::System(format!("sysctl data query failed: {}", ret)));
        }
        
        buffer.truncate(size);
        Ok(buffer)
    }
    
    /// 解析PCB列表数据
    #[cfg(target_os = "macos")]
    fn parse_pcb_list(
        data: &[u8],
        protocol: NetworkProtocol,
        target_ip: IpAddr,
        target_port: u16,
    ) -> Result<(u32, String), TakeoverError> {
        if data.len() < 24 {
            return Err(TakeoverError::System("PCB data too small".to_string()));
        }
        
        // 根据系统版本确定结构体大小
        let struct_size = Self::get_pcb_struct_size();
        let item_size = struct_size + match protocol {
            NetworkProtocol::Tcp => 208, // rup8(sizeof(xtcpcb_n))
            NetworkProtocol::Udp => 0,
        };
        
        debug!("PCB解析开始: 协议={:?}, 目标={}:{}, struct_size={}, item_size={}, data_len={}", 
               protocol, target_ip, target_port, struct_size, item_size, data.len());
        
        // 跳过xinpgen头部（24字节）
        let mut offset = 24;
        let mut checked_count = 0;
        let mut matched_port_count = 0;
        
        while offset + item_size <= data.len() {
            checked_count += 1;
            
            // offset of xinpcb_n and xsocket_n 
            let inp_offset = offset;
            let so_offset = offset + 104;
            
            // 边界检查
            if inp_offset + 80 > data.len() || so_offset + 72 > data.len() {
                debug!("边界检查失败: inp_offset={}, so_offset={}, data_len={}", 
                       inp_offset, so_offset, data.len());
                break;
            }
            
            // 读取源端口（big endian: inp+18:inp+20）
            let src_port = u16::from_be_bytes([
                data[inp_offset + 18],
                data[inp_offset + 19],
            ]);
            
            if src_port != target_port {
                offset += item_size;
                continue;
            }
            
            matched_port_count += 1;
            debug!("找到匹配端口{}: 第{}个连接", target_port, matched_port_count);
            
            // xinpcb_n.inp_vflag: inp+44
            let flag = data[inp_offset + 44];
            debug!("IP版本标志: 0x{:02x}", flag);
            
            // 根据IP版本读取源IP
            let src_ip = match target_ip {
                IpAddr::V4(_) if flag & 0x1 > 0 => {
                    // IPv4: inp+76:inp+80
                    IpAddr::V4(Ipv4Addr::from([
                        data[inp_offset + 76],
                        data[inp_offset + 77],
                        data[inp_offset + 78],
                        data[inp_offset + 79],
                    ]))
                }
                IpAddr::V6(_) if flag & 0x2 > 0 => {
                    // IPv6: inp+64:inp+80
                    let mut ipv6_bytes = [0u8; 16];
                    ipv6_bytes.copy_from_slice(&data[inp_offset + 64..inp_offset + 80]);
                    IpAddr::V6(Ipv6Addr::from(ipv6_bytes))
                }
                _ => {
                    debug!("IP版本不匹配: 目标={}, 标志=0x{:02x}", target_ip, flag);
                    offset += item_size;
                    continue;
                }
            };
            
            debug!("解析出的源IP: {}", src_ip);
            
            if src_ip == target_ip {
                // xsocket_n.so_last_pid: so+68:so+72
                let pid = u32::from_le_bytes([
                    data[so_offset + 68],
                    data[so_offset + 69],
                    data[so_offset + 70],
                    data[so_offset + 71],
                ]);
                
                debug!("找到匹配连接: {}:{} -> PID {}", src_ip, src_port, pid);
                
                if pid == 0 {
                    debug!("PID为0，跳过此连接");
                    offset += item_size;
                    continue;
                }
                
                // 获取进程路径
                match Self::get_process_path(pid) {
                    Ok(path) => {
                        debug!("成功解析进程: PID={}, 路径={}", pid, path);
                        return Ok((pid, path));
                    }
                    Err(e) => {
                        debug!("获取进程{}路径失败: {}, 继续查找", pid, e);
                        // 继续查找其他匹配的连接
                    }
                }
            }
            
            offset += item_size;
        }
        
        debug!("PCB解析完成: 检查了{}个连接, {}个端口匹配", checked_count, matched_port_count);
        Err(TakeoverError::System(format!(
            "Process not found for {}:{} (checked {} connections, {} port matches)", 
            target_ip, target_port, checked_count, matched_port_count
        )))
    }
    
    /// 获取PCB结构体大小（参考mihomo实现）
    /// 
    /// 参考: https://github.com/MetaCubeX/mihomo/blob/dev-vision/component/process/process_darwin.go
    /// 
    /// from darwin-xnu/bsd/netinet/in_pcblist.c:get_pcblist_n
    /// size/offset are round up (aligned) to 8 bytes in darwin
    /// rup8(sizeof(xinpcb_n)) + rup8(sizeof(xsocket_n)) +
    /// 2 * rup8(sizeof(xsockbuf_n)) + rup8(sizeof(xsockstat_n))
    #[cfg(target_os = "macos")]
    fn get_pcb_struct_size() -> usize {
        use std::ffi::CString;
        use libc::{sysctlbyname, size_t};
        use std::ptr;
        
        // 获取内核版本信息
        let name = CString::new("kern.osrelease").unwrap();
        let mut size: size_t = 0;
        
        // 先获取版本字符串长度
        unsafe {
            sysctlbyname(name.as_ptr(), ptr::null_mut(), &mut size, ptr::null_mut(), 0);
        }
        
        if size == 0 {
            warn!("无法获取内核版本信息，使用默认PCB大小");
            return 384; // 安全的默认值
        }
        
        let mut buffer = vec![0u8; size];
        let ret = unsafe {
            sysctlbyname(
                name.as_ptr(),
                buffer.as_mut_ptr() as *mut libc::c_void,
                &mut size,
                ptr::null_mut(),
                0,
            )
        };
        
        if ret != 0 {
            warn!("sysctl kern.osrelease调用失败，使用默认PCB大小");
            return 384;
        }
        
        // 解析版本字符串
        let version_str = String::from_utf8_lossy(&buffer[..size.saturating_sub(1)]);
        let version_parts: Vec<&str> = version_str.split('.').collect();
        
        debug!("检测到macOS内核版本: {}", version_str);
        
        if let Ok(major_version) = version_parts[0].parse::<u32>() {
            // 严格按照mihomo实现: Darwin >= 22 使用408，否则使用384
            let pcb_size = if major_version >= 22 {
                408 // Darwin 22+ (macOS 13.0+ Ventura)
            } else {
                384 // Darwin < 22 (macOS 12.x及更老版本)
            };
            
            debug!("Darwin {} -> PCB结构体大小: {} bytes (参考mihomo实现)", major_version, pcb_size);
            pcb_size
        } else {
            warn!("无法解析内核版本: {}, 使用默认PCB大小", version_str);
            384 // 无法解析时的安全默认值
        }
    }
    
    /// 通过PID获取进程路径
    #[cfg(target_os = "macos")]
    fn get_process_path(pid: u32) -> Result<String, TakeoverError> {
        const PROC_PIDPATHINFO_SIZE: usize = 1024;
        
        let mut buffer = vec![0u8; PROC_PIDPATHINFO_SIZE];
        
        // 使用proc_pidpath系统调用
        let ret = unsafe {
            libc::proc_pidpath(
                pid as libc::pid_t,
                buffer.as_mut_ptr() as *mut libc::c_void,
                PROC_PIDPATHINFO_SIZE as u32,
            )
        };
        
        if ret <= 0 {
            let errno = unsafe { *libc::__error() };
            return Err(TakeoverError::System(format!(
                "proc_pidpath failed for PID {}: ret={}, errno={}", 
                pid, ret, errno
            )));
        }
        
        // 查找字符串结尾
        let end = buffer.iter().position(|&x| x == 0).unwrap_or(buffer.len());
        let path = String::from_utf8_lossy(&buffer[..end]).to_string();
        
        if path.is_empty() {
            return Err(TakeoverError::System(format!("Empty path returned for PID {}", pid)));
        }
        
        debug!("获取进程{}路径: {}", pid, path);
        Ok(path)
    }
    
    /// 通过PID获取进程用户ID
    #[cfg(target_os = "macos")]
    fn get_process_user_id(pid: u32) -> Result<Option<u32>, TakeoverError> {
        use std::process::Command;
        
        debug!("查询进程{}的用户ID", pid);
        
        // 使用ps命令获取进程用户ID，这是一个更简单可靠的方法
        let output = Command::new("ps")
            .args(&["-o", "uid=", "-p", &pid.to_string()])
            .output()
            .map_err(|e| TakeoverError::System(format!("Failed to run ps command: {}", e)))?;
        
        if !output.status.success() {
            debug!("进程{}不存在或ps命令失败", pid);
            return Ok(None);
        }
        
        let output_str = String::from_utf8_lossy(&output.stdout);
        let uid_str = output_str.trim();
        
        if uid_str.is_empty() {
            debug!("进程{}的用户ID为空", pid);
            return Ok(None);
        }
        
        match uid_str.parse::<u32>() {
            Ok(uid) => {
                debug!("进程{}的用户ID: {}", pid, uid);
                Ok(Some(uid))
            }
            Err(e) => {
                debug!("解析进程{}用户ID失败: {}, 原始输出: '{}'", pid, e, uid_str);
                Ok(None)
            }
        }
    }
    
    /// 检查缓存
    async fn check_cache(&self, key: &str) -> Option<ProcessInfo> {
        let cache = self.cache.read().await;
        
        if let Some(info) = cache.get(key) {
            // 检查是否过期
            if let Ok(elapsed) = info.timestamp.elapsed() {
                if elapsed.as_secs() < self.config.cache_ttl {
                    return Some(info.clone());
                }
            }
        }
        
        None
    }
    
    /// 更新缓存
    async fn update_cache(&self, key: String, info: ProcessInfo) {
        let mut cache = self.cache.write().await;
        cache.insert(key, info);
        
        // 简单的缓存清理（避免无限增长）
        if cache.len() > 1000 {
            let keys: Vec<String> = cache.keys().take(100).cloned().collect();
            for key in keys {
                cache.remove(&key);
            }
        }
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> ProcessFinderStats {
        self.stats.read().await.clone()
    }
}

impl Default for ProcessFinder {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for ProcessFinderConfig {
    fn default() -> Self {
        Self {
            enable_cache: true,
            cache_ttl: 300, // 5分钟 - 进程信息相对稳定
            query_timeout: std::time::Duration::from_secs(5), // 增加超时时间，系统调用可能较慢
            full_path: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_process_finder_creation() {
        let finder = ProcessFinder::new();
        
        let stats = finder.get_stats().await;
        assert_eq!(stats.total_queries, 0);
        
        println!("✅ 进程查询组件创建成功");
    }
    
    #[test]
    fn test_config_defaults() {
        let config = ProcessFinderConfig::default();
        assert!(config.enable_cache);
        assert_eq!(config.cache_ttl, 300); // 5分钟
        assert_eq!(config.query_timeout, std::time::Duration::from_secs(5));
        assert!(config.full_path);
        
        println!("✅ 配置默认值测试通过");
    }
}
