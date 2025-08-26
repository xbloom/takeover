//! 用户管理模块
//! 
//! 负责管理透明代理系统的用户权限，包括：
//! - 创建和管理专用用户
//! - 用户身份切换
//! - 管理员权限检查和执行
//! - pfctl操作的权限管理

use anyhow::Result;
use std::process::Command;
use tracing::{info, warn, error, debug};
use std::ffi::{CStr, CString};

/// 默认的透明代理专用用户名
pub const DEFAULT_PROXY_USER: &str = "_takeover_proxy";

/// 用户管理器
#[derive(Clone)]
pub struct UserManager {
    proxy_user: String,
    is_admin: bool,
}

/// 临时切换用户身份的RAII包装器
pub struct UserSwitcher {
    original_euid: libc::uid_t,
    switched: bool,
}

impl UserManager {
    /// 创建新的用户管理器
    pub fn new() -> Self {
        Self {
            proxy_user: DEFAULT_PROXY_USER.to_string(),
            is_admin: Self::check_admin_privileges(),
        }
    }

    /// 使用自定义代理用户名创建用户管理器
    pub fn with_proxy_user(proxy_user: &str) -> Self {
        Self {
            proxy_user: proxy_user.to_string(),
            is_admin: Self::check_admin_privileges(),
        }
    }

    /// 检查是否具有管理员权限
    pub fn check_admin_privileges() -> bool {
        unsafe { libc::geteuid() == 0 }
    }

    /// 使用AppleScript临时申请管理员权限执行命令
    /// 这会弹出macOS的权限确认对话框
    #[allow(dead_code)]
    async fn execute_with_admin_privileges(&self, command: &str, args: &[&str]) -> Result<std::process::Output> {
        // 构建完整的命令字符串
        let full_command = if args.is_empty() {
            command.to_string()
        } else {
            format!("{} {}", command, args.join(" "))
        };

        info!("🔐 Requesting admin privileges for: {}", full_command);

        // 使用osascript执行带管理员权限的命令
        // 这会弹出macOS的权限确认对话框
        let script = format!(
            r#"do shell script "{}" with administrator privileges"#,
            full_command.replace("\"", "\\\"") // 转义引号
        );

        debug!("🍎 AppleScript: {}", script);

        let output = Command::new("osascript")
            .args(&["-e", &script])
            .output()?;

        if output.status.success() {
            info!("✅ Admin command executed successfully");
            debug!("📤 Command output: {}", String::from_utf8_lossy(&output.stdout));
        } else {
            error!("❌ Admin command failed: {}", String::from_utf8_lossy(&output.stderr));
        }

        Ok(output)
    }

    /// 使用传统sudo方式执行命令（备用方案）
    #[allow(dead_code)]
    async fn execute_with_sudo(&self, command: &str, args: &[&str]) -> Result<std::process::Output> {
        info!("🔐 Using sudo for: {} {:?}", command, args);
        
        let mut cmd = Command::new("sudo");
        cmd.arg(command);
        cmd.args(args);
        
        let output = cmd.output()?;
        
        if output.status.success() {
            info!("✅ Sudo command executed successfully");
        } else {
            error!("❌ Sudo command failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        
        Ok(output)
    }

    /// 获取当前进程的用户名
    pub fn get_current_user() -> Option<String> {
        unsafe {
            let uid = libc::getuid();
            let pwd = libc::getpwuid(uid);
            if pwd.is_null() {
                return None;
            }
            let name_ptr = (*pwd).pw_name;
            if name_ptr.is_null() {
                return None;
            }
            CStr::from_ptr(name_ptr).to_str().ok().map(|s| s.to_string())
        }
    }

    /// 根据用户名获取UID
    pub fn get_uid_by_name(username: &str) -> Option<libc::uid_t> {
        let username_c = CString::new(username).ok()?;
        unsafe {
            let pwd = libc::getpwnam(username_c.as_ptr());
            if pwd.is_null() {
                None
            } else {
                Some((*pwd).pw_uid)
            }
        }
    }

    /// 检查用户是否存在
    pub fn user_exists(&self, username: &str) -> bool {
        Self::get_uid_by_name(username).is_some()
    }

    /// 确保代理用户存在，如果不存在则创建
    pub async fn ensure_proxy_user(&self) -> Result<()> {
        if !self.is_admin {
            return Err(anyhow::anyhow!("需要管理员权限才能创建用户"));
        }

        if self.user_exists(&self.proxy_user) {
            info!("✅ 代理用户 '{}' 已存在", self.proxy_user);
            return Ok(());
        }

        info!("🔨 创建代理用户: {}", self.proxy_user);
        
        // 在macOS上创建用户，设置为系统用户，不能登录
        let mut cmd = Command::new("dscl");
        cmd.args(&[
            ".",
            "create",
            &format!("/Users/{}", self.proxy_user),
        ]);

        let output = cmd.output()?;
        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "创建用户失败: {}", 
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        // 设置用户ID (使用系统用户范围)
        let uid = self.find_available_system_uid().await?;
        let mut cmd = Command::new("dscl");
        cmd.args(&[
            ".",
            "create",
            &format!("/Users/{}", self.proxy_user),
            "UniqueID",
            &uid.to_string(),
        ]);

        let output = cmd.output()?;
        if !output.status.success() {
            warn!("设置用户ID失败，但用户可能已创建");
        }

        // 设置用户组ID
        let mut cmd = Command::new("dscl");
        cmd.args(&[
            ".",
            "create",
            &format!("/Users/{}", self.proxy_user),
            "PrimaryGroupID",
            "99", // nobody组
        ]);

        let _ = cmd.output(); // 忽略错误

        // 设置用户shell为不可登录
        let mut cmd = Command::new("dscl");
        cmd.args(&[
            ".",
            "create",
            &format!("/Users/{}", self.proxy_user),
            "UserShell",
            "/usr/bin/false",
        ]);

        let _ = cmd.output(); // 忽略错误

        // 设置实际名称
        let mut cmd = Command::new("dscl");
        cmd.args(&[
            ".",
            "create",
            &format!("/Users/{}", self.proxy_user),
            "RealName",
            "Takeover Transparent Proxy User",
        ]);

        let _ = cmd.output(); // 忽略错误

        // 验证用户是否创建成功
        if self.user_exists(&self.proxy_user) {
            info!("✅ 成功创建代理用户: {}", self.proxy_user);
        } else {
            return Err(anyhow::anyhow!("用户创建后验证失败"));
        }

        Ok(())
    }

    /// 查找可用的系统用户ID (200-399范围)
    async fn find_available_system_uid(&self) -> Result<u32> {
        for uid in 200..400 {
            let output = Command::new("dscl")
                .args(&[".", "search", "/Users", "UniqueID", &uid.to_string()])
                .output()?;
            
            // 如果没有找到使用此UID的用户，则可以使用
            if output.stdout.is_empty() {
                return Ok(uid);
            }
        }
        
        Err(anyhow::anyhow!("未找到可用的系统用户ID"))
    }

    /// 删除代理用户
    pub async fn remove_proxy_user(&self) -> Result<()> {
        if !self.is_admin {
            return Err(anyhow::anyhow!("需要管理员权限才能删除用户"));
        }

        if !self.user_exists(&self.proxy_user) {
            info!("代理用户 '{}' 不存在，无需删除", self.proxy_user);
            return Ok(());
        }

        info!("🗑️  删除代理用户: {}", self.proxy_user);
        
        let mut cmd = Command::new("dscl");
        cmd.args(&[
            ".",
            "delete",
            &format!("/Users/{}", self.proxy_user),
        ]);

        let output = cmd.output()?;
        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "删除用户失败: {}", 
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        info!("✅ 成功删除代理用户: {}", self.proxy_user);
        Ok(())
    }

    /// 获取代理用户名
    pub fn get_proxy_user(&self) -> &str {
        &self.proxy_user
    }

    /// 检查当前是否有管理员权限
    pub fn is_admin(&self) -> bool {
        Self::check_admin_privileges()
    }

    /// 使用管理员权限执行命令
    pub async fn execute_as_admin(&self, command: &str, args: &[&str]) -> Result<std::process::Output> {
        if !self.is_admin {
            return Err(anyhow::anyhow!("需要管理员权限才能执行此命令"));
        }

        debug!("🔧 执行管理员命令: {} {:?}", command, args);
        
        let output = Command::new(command)
            .args(args)
            .output()?;

        Ok(output)
    }

    /// 创建用户切换器
    pub fn create_user_switcher(&self) -> Result<UserSwitcher> {
        UserSwitcher::switch_to_user(&self.proxy_user)
    }

    /// 永久切换到代理用户身份
    /// 
    /// 这个方法会永久改变进程的有效用户ID，不可恢复。
    /// 主要用于启动时切换，避免每个连接都临时切换造成的并发问题。
    pub fn switch_to_proxy_user_permanently(&self) -> Result<()> {
        let current_euid = unsafe { libc::geteuid() };
        
        if let Some(target_uid) = Self::get_uid_by_name(&self.proxy_user) {
            // 检查是否已经是目标用户
            if current_euid == target_uid {
                debug!("✅ Already running as proxy user {} (UID {})", self.proxy_user, target_uid);
                return Ok(());
            }
            
            info!("🔄 Permanently switching from UID {} to UID {} (user: {})", 
                  current_euid, target_uid, self.proxy_user);
            
            let result = unsafe { libc::seteuid(target_uid) };
            if result == 0 {
                info!("✅ Successfully switched permanently to user: {}", self.proxy_user);
                Ok(())
            } else {
                let error = std::io::Error::last_os_error();
                Err(anyhow::anyhow!("Failed to switch permanently to user {}: {}", self.proxy_user, error))
            }
        } else {
            Err(anyhow::anyhow!("Proxy user {} not found", self.proxy_user))
        }
    }

    /// 检查当前进程是否已经是代理用户身份
    pub fn is_running_as_proxy_user(&self) -> bool {
        if let Some(current_user) = Self::get_current_user() {
            current_user == self.proxy_user
        } else {
            false
        }
    }
}

impl UserSwitcher {
    /// 切换到指定用户身份
    pub fn switch_to_user(username: &str) -> Result<Self> {
        let original_euid = unsafe { libc::geteuid() };
        
        if let Some(target_uid) = UserManager::get_uid_by_name(username) {
            // 检查是否已经是目标用户 - 避免不必要的切换
            if original_euid == target_uid {
                debug!("✅ Already running as user {} (UID {}), no switch needed", username, target_uid);
                return Ok(UserSwitcher {
                    original_euid,
                    switched: false, // 标记为未切换，因为不需要恢复
                });
            }
            
            debug!("🔄 Switching from UID {} to UID {} (user: {})", original_euid, target_uid, username);
            
            let result = unsafe { libc::seteuid(target_uid) };
            if result == 0 {
                debug!("✅ Successfully switched to user: {}", username);
                Ok(UserSwitcher {
                    original_euid,
                    switched: true,
                })
            } else {
                let error = std::io::Error::last_os_error();
                warn!("❌ Failed to switch to user {}: {}", username, error);
                Err(anyhow::anyhow!("Failed to switch to user {}: {}", username, error))
            }
        } else {
            Err(anyhow::anyhow!("User {} not found", username))
        }
    }
}

impl Drop for UserSwitcher {
    fn drop(&mut self) {
        if self.switched {
            debug!("🔄 Restoring original UID: {}", self.original_euid);
            let result = unsafe { libc::seteuid(self.original_euid) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                error!("❌ Failed to restore original UID {}: {}", self.original_euid, error);
            } else {
                debug!("✅ Successfully restored original UID: {}", self.original_euid);
            }
        }
    }
}

impl Default for UserManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_manager_creation() {
        let manager = UserManager::new();
        assert_eq!(manager.get_proxy_user(), DEFAULT_PROXY_USER);
    }

    #[test]
    fn test_custom_proxy_user() {
        let manager = UserManager::with_proxy_user("custom_user");
        assert_eq!(manager.get_proxy_user(), "custom_user");
    }

    #[test]
    fn test_admin_check() {
        // 这个测试的结果取决于运行环境
        let is_admin = UserManager::check_admin_privileges();
        println!("Admin privileges: {}", is_admin);
    }

    #[test]
    fn test_get_current_user() {
        if let Some(user) = UserManager::get_current_user() {
            println!("Current user: {}", user);
            assert!(!user.is_empty());
        }
    }

    #[test]
    fn test_user_switcher_same_user() {
        // 测试当目标用户与当前用户相同时的行为
        if let Some(current_user) = UserManager::get_current_user() {
            let result = UserSwitcher::switch_to_user(&current_user);
            
            match result {
                Ok(_switcher) => {
                    // 应该成功但不需要实际切换
                    println!("UserSwitcher created successfully for same user");
                    // switcher 的 switched 字段应该是 false
                    // 但我们无法直接访问私有字段，这个测试主要验证不会出错
                }
                Err(e) => {
                    // 如果当前用户是系统用户（如 root），可能会有权限问题
                    println!("Note: UserSwitcher failed for same user (may be expected for system users): {}", e);
                }
            }
        }
    }
}
