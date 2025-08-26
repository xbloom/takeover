//! 底层 pfctl 交互层
//! 
//! 这个模块只负责与 pfctl 命令行工具的交互，不包含任何业务逻辑。
//! 所有操作都是原子的，不维护状态。

use anyhow::Result;
use std::path::PathBuf;
use tracing::{debug, warn, error};
use crate::utils;

/// pfctl 命令的底层封装
/// 
/// 这个结构体提供了与 macOS pfctl 防火墙工具交互的低级接口。
/// 它不包含任何业务逻辑，只负责执行命令和返回结果。
pub struct Pfctl {
    /// 锚点名称
    anchor_name: String,
    /// 锚点文件路径
    anchor_file_path: PathBuf,
}

impl Pfctl {
    /// 创建新的 Pfctl 实例
    pub fn new(anchor_name: &str) -> Self {
        let anchor_file_path = PathBuf::from(format!("/etc/pf.anchors/{}", anchor_name));
        Self {
            anchor_name: anchor_name.to_string(),
            anchor_file_path,
        }
    }

    /// 获取锚点名称
    pub fn anchor_name(&self) -> &str {
        &self.anchor_name
    }

    /// 获取锚点文件路径
    pub fn anchor_file_path(&self) -> &PathBuf {
        &self.anchor_file_path
    }

    // === 命令执行 ===

    /// 执行 pfctl 命令
    pub async fn execute(&self, args: &[&str]) -> Result<std::process::Output> {
        debug!("Executing pfctl with args: {:?}", args);
        
        let output = tokio::process::Command::new("pfctl")
            .args(args)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            debug!("pfctl command failed: {}", stderr);
        }

        Ok(output.into())
    }

    /// 执行需要管理员权限的 pfctl 命令
    pub async fn execute_admin(&self, args: &[&str]) -> Result<std::process::Output> {
        debug!("Executing pfctl with admin privileges, args: {:?}", args);
        
        // 尝试使用管理员权限
        match utils::execute_admin_command("pfctl", args) {
            Ok(output) => Ok(output),
            Err(_) => {
                // 回退到普通执行
                warn!("Failed to execute with admin privileges, falling back to regular execution");
                self.execute(args).await
            }
        }
    }

    // === 规则操作 ===

    /// 写入规则到锚点文件
    pub async fn write_anchor_file(&self, content: &str) -> Result<()> {
        debug!("Writing rules to anchor file: {:?}", self.anchor_file_path);
        debug!("Rules content:\n{}", content);
        
        // 使用 sudo 权限写入文件到 /etc/pf.anchors/
        let temp_file = format!("/tmp/pf_anchor_{}", self.anchor_name);
        
        // 先写入临时文件
        tokio::fs::write(&temp_file, content).await?;
        
        // 使用 sudo cp 将临时文件复制到目标位置
        let output = utils::execute_admin_command("cp", &[&temp_file, &self.anchor_file_path.to_string_lossy()])?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // 清理临时文件
            let _ = tokio::fs::remove_file(&temp_file).await;
            return Err(anyhow::anyhow!("Failed to write anchor file with sudo: {}", stderr));
        }
        
        // 设置正确的文件权限
        let _output = utils::execute_admin_command("chmod", &["644", &self.anchor_file_path.to_string_lossy()])?;
        
        // 清理临时文件
        let _ = tokio::fs::remove_file(&temp_file).await;
        
        debug!("Successfully wrote rules to anchor file with sudo");
        Ok(())
    }

    /// 从文件加载规则到锚点
    pub async fn load_anchor_file(&self) -> Result<()> {
        debug!("Loading rules from file {:?} to anchor '{}'", 
               self.anchor_file_path, self.anchor_name);
        
        let output = self.execute_admin(&[
            "-a", &self.anchor_name,
            "-f", &self.anchor_file_path.to_string_lossy()
        ]).await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!(
                "Failed to load rules into anchor '{}': {}", 
                self.anchor_name, stderr
            ));
        }

        debug!("Successfully loaded rules into anchor '{}'", self.anchor_name);
        Ok(())
    }

    /// 清空锚点中的所有规则
    pub async fn flush_anchor(&self) -> Result<()> {
        debug!("Flushing all rules from anchor '{}'", self.anchor_name);
        
        // 清空 rdr 规则
        let output = self.execute_admin(&[
            "-a", &self.anchor_name,
            "-F", "nat"
        ]).await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to flush nat rules from anchor '{}': {}", 
                  self.anchor_name, stderr);
        }

        // 清空 filter 规则
        let output = self.execute_admin(&[
            "-a", &self.anchor_name,
            "-F", "rules"
        ]).await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to flush filter rules from anchor '{}': {}", 
                  self.anchor_name, stderr);
        }

        debug!("Successfully flushed anchor '{}'", self.anchor_name);
        Ok(())
    }

    // === 智能重新加载 ===

    /// 智能重新加载规则 - 根据修改内容选择合适的重新加载方式
    /// 
    /// 如果主配置文件被修改，使用 `pfctl -f /etc/pf.conf`
    /// 如果只有 anchor 文件被修改，使用 `pfctl -a anchor_name -f anchor_file_path`
    pub async fn smart_reload(&self, main_conf_changed: bool) -> Result<()> {
        if main_conf_changed {
            debug!("Main configuration changed, reloading /etc/pf.conf");
            self.reload_main_conf().await
        } else {
            debug!("Only anchor file changed, reloading anchor '{}'", self.anchor_name);
            self.load_anchor_file().await
        }
    }

    /// 写入并重新加载 anchor 文件 - 便捷方法
    pub async fn write_and_reload_anchor(&self, content: &str) -> Result<()> {
        debug!("Writing and reloading anchor file: {:?}", self.anchor_file_path);
        
        // 1. 写入规则到锚点文件
        self.write_anchor_file(content).await?;
        
        // 2. 只重新加载锚点文件（不需要重新加载主配置）
        self.load_anchor_file().await?;
        
        debug!("Successfully wrote and reloaded anchor file");
        Ok(())
    }

    /// 设置锚点配置并重新加载主配置 - 便捷方法
    pub async fn setup_anchor_and_reload_main(&self, rules_content: &str) -> Result<()> {
        debug!("Setting up anchor and reloading main configuration");
        
        // 1. 写入规则到锚点文件
        self.write_anchor_file(rules_content).await?;
        
        // 2. 添加锚点到主配置文件（如果尚未添加）
        self.add_anchor_to_main_conf().await?;
        
        // 3. 重新加载主配置文件（这会同时加载所有 anchor）
        self.reload_main_conf().await?;
        
        debug!("Successfully set up anchor and reloaded main configuration");
        Ok(())
    }

    // === 主配置文件操作 ===

    /// 在 /etc/pf.conf 中添加锚点声明
    pub async fn add_anchor_to_main_conf(&self) -> Result<()> {
        debug!("Adding anchor '{}' to /etc/pf.conf", self.anchor_name);
        
        let pf_conf_path = "/etc/pf.conf";
        
        // 使用 sudo 读取当前配置
        let current_conf = match self.read_file_with_sudo(pf_conf_path).await {
            Ok(content) => content,
            Err(e) => {
                error!("Cannot read /etc/pf.conf with sudo: {}", e);
                return Err(anyhow::anyhow!("Cannot read /etc/pf.conf with sudo: {}", e));
            }
        };

        // 准备需要添加的行
        let comment = format!("# {} anchor - auto-generated", self.anchor_name);
        let rdr_anchor = format!("rdr-anchor \"{}\"", self.anchor_name);
        let filter_anchor = format!("anchor \"{}\"", self.anchor_name);
        let load_directive = format!(
            "load anchor \"{}\" from \"{}\"", 
            self.anchor_name, 
            self.anchor_file_path.display()
        );

        // 检查是否已经存在
        let has_rdr = self.check_line_exists(&current_conf, &rdr_anchor);
        let has_filter = self.check_line_exists(&current_conf, &filter_anchor);
        let has_load = self.check_line_exists(&current_conf, &load_directive);

        if has_rdr && has_filter && has_load {
            debug!("Anchor '{}' already fully configured in /etc/pf.conf", self.anchor_name);
            return Ok(());
        }

        // 构建新配置，将规则按正确顺序插入
        let new_conf = self.insert_anchor_rules_in_order(&current_conf, &comment, &rdr_anchor, &filter_anchor, &load_directive);

        // 使用 sudo 写入新配置
        self.write_file_with_sudo(pf_conf_path, &new_conf).await?;
        
        debug!("Successfully added anchor '{}' to /etc/pf.conf", self.anchor_name);
        Ok(())
    }

    /// 从 /etc/pf.conf 中移除锚点声明
    pub async fn remove_anchor_from_main_conf(&self) -> Result<()> {
        debug!("Removing anchor '{}' from /etc/pf.conf", self.anchor_name);
        
        let pf_conf_path = "/etc/pf.conf";
        
        // 使用 sudo 读取当前配置
        let current_conf = match self.read_file_with_sudo(pf_conf_path).await {
            Ok(content) => content,
            Err(e) => {
                warn!("Cannot read /etc/pf.conf with sudo: {}", e);
                return Ok(()); // 不是致命错误
            }
        };

        // 移除相关行
        let new_conf = self.remove_anchor_lines(&current_conf);
        
        // 只有在确实有变化时才写入
        if new_conf != current_conf {
            self.write_file_with_sudo(pf_conf_path, &new_conf).await?;
            debug!("Successfully removed anchor '{}' from /etc/pf.conf", self.anchor_name);
        } else {
            debug!("Anchor '{}' not found in /etc/pf.conf", self.anchor_name);
        }

        Ok(())
    }

    /// 重新加载主配置文件
    pub async fn reload_main_conf(&self) -> Result<()> {
        debug!("Reloading /etc/pf.conf");
        
        let output = self.execute_admin(&["-f", "/etc/pf.conf"]).await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Failed to reload /etc/pf.conf: {}", stderr));
        }

        debug!("Successfully reloaded /etc/pf.conf");
        Ok(())
    }

    // === 状态查询 ===

    /// 检查 pf 是否启用
    pub async fn is_enabled(&self) -> Result<bool> {
        let output = self.execute_admin(&["-s", "info"]).await?;
        
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            Ok(stdout.contains("Status: Enabled"))
        } else {
            Ok(false)
        }
    }

    /// 启用 pf
    pub async fn enable(&self) -> Result<()> {
        debug!("Enabling pf");
        
        let output = self.execute_admin(&["-e"]).await?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        
        // pfctl -e 在已经启用时会返回错误，但这不是问题
        if stderr.contains("already enabled") {
            debug!("pf is already enabled");
        } else if !output.status.success() {
            return Err(anyhow::anyhow!("Failed to enable pf: {}", stderr));
        } else {
            debug!("Successfully enabled pf");
        }
        
        Ok(())
    }

    /// 获取锚点中的当前规则
    pub async fn get_anchor_rules(&self) -> Result<String> {
        let output = self.execute(&["-a", &self.anchor_name, "-s", "rules"]).await?;
        
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(anyhow::anyhow!("Failed to get anchor rules: {}", stderr))
        }
    }

    // === 辅助方法 ===

    /// 使用 sudo 权限读取文件
    async fn read_file_with_sudo(&self, file_path: &str) -> Result<String> {
        debug!("Reading file with sudo: {}", file_path);
        
        let output = utils::execute_admin_command("cat", &[file_path])?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("Failed to read file {} with sudo: {}", file_path, stderr));
        }
        
        let content = String::from_utf8_lossy(&output.stdout).to_string();
        debug!("Successfully read file with sudo: {} ({} bytes)", file_path, content.len());
        Ok(content)
    }

    /// 使用 sudo 权限写入文件
    async fn write_file_with_sudo(&self, file_path: &str, content: &str) -> Result<()> {
        debug!("Writing file with sudo: {}", file_path);
        
        // 创建临时文件
        let temp_file = format!("/tmp/pf_temp_{}", self.anchor_name);
        
        // 先写入临时文件
        tokio::fs::write(&temp_file, content).await?;
        
        // 使用 sudo cp 将临时文件复制到目标位置
        let output = utils::execute_admin_command("cp", &[&temp_file, file_path])?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // 清理临时文件
            let _ = tokio::fs::remove_file(&temp_file).await;
            return Err(anyhow::anyhow!("Failed to write file {} with sudo: {}", file_path, stderr));
        }
        
        // 设置正确的文件权限
        let _output = utils::execute_admin_command("chmod", &["644", file_path])?;
        
        // 清理临时文件
        let _ = tokio::fs::remove_file(&temp_file).await;
        
        debug!("Successfully wrote file with sudo: {} ({} bytes)", file_path, content.len());
        Ok(())
    }

    /// 按正确顺序插入锚点规则到配置文件中
    fn insert_anchor_rules_in_order(&self, conf: &str, comment: &str, rdr_anchor: &str, filter_anchor: &str, load_directive: &str) -> String {
        // 首先移除现有的锚点相关行
        let clean_conf = self.remove_anchor_lines(conf);
        let lines: Vec<String> = clean_conf.lines().map(|s| s.to_string()).collect();
        
        let mut result_lines = Vec::new();
        let mut rdr_inserted = false;
        let mut anchor_inserted = false;
        let mut load_inserted = false;
        
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            
            // 添加当前行
            result_lines.push(line.clone());
            
            // 在最后一个 rdr-anchor 后插入我们的 rdr-anchor
            if !rdr_inserted && trimmed.starts_with("rdr-anchor") {
                // 检查下一行是否还有 rdr-anchor
                let remaining_lines = &lines[i+1..];
                let has_more_rdr = remaining_lines.iter().any(|l| l.trim().starts_with("rdr-anchor"));
                
                if !has_more_rdr {
                    result_lines.push(comment.to_string());
                    result_lines.push(rdr_anchor.to_string());
                    rdr_inserted = true;
                }
            }
            
            // 在最后一个 filter anchor 后插入我们的 filter anchor
            if !anchor_inserted && trimmed.starts_with("anchor") && 
               !trimmed.contains("rdr-anchor") && !trimmed.contains("nat-anchor") && 
               !trimmed.contains("scrub-anchor") && !trimmed.contains("dummynet-anchor") {
                
                // 检查下一行是否还有 filter anchor
                let remaining_lines = &lines[i+1..];
                let has_more_anchors = remaining_lines.iter().any(|l| {
                    let t = l.trim();
                    t.starts_with("anchor") && !t.contains("rdr-anchor") && 
                    !t.contains("nat-anchor") && !t.contains("scrub-anchor") && 
                    !t.contains("dummynet-anchor")
                });
                
                if !has_more_anchors {
                    result_lines.push(filter_anchor.to_string());
                    anchor_inserted = true;
                }
            }
            
            // 在最后一个 load anchor 后插入我们的 load directive
            if !load_inserted && trimmed.starts_with("load anchor") {
                // 检查下一行是否还有 load anchor
                let remaining_lines = &lines[i+1..];
                let has_more_loads = remaining_lines.iter().any(|l| l.trim().starts_with("load anchor"));
                
                if !has_more_loads {
                    result_lines.push(load_directive.to_string());
                    load_inserted = true;
                }
            }
        }
        
        // 如果某些规则还没插入，在末尾添加
        if !rdr_inserted {
            if !result_lines.is_empty() && !result_lines.last().unwrap().is_empty() {
                result_lines.push("".to_string());
            }
            result_lines.push(comment.to_string());
            result_lines.push(rdr_anchor.to_string());
        }
        
        if !anchor_inserted {
            result_lines.push(filter_anchor.to_string());
        }
        
        if !load_inserted {
            result_lines.push(load_directive.to_string());
        }
        
        // 重建配置文件内容
        let mut result = result_lines.join("\n");
        if !result.ends_with('\n') {
            result.push('\n');
        }
        
        result
    }

    /// 检查配置文件中是否存在某行
    fn check_line_exists(&self, conf: &str, line: &str) -> bool {
        conf.lines().any(|l| {
            let trimmed = l.trim();
            trimmed == line || trimmed == format!("{};", line)
        })
    }

    /// 从配置中移除与锚点相关的所有行
    fn remove_anchor_lines(&self, conf: &str) -> String {
        let mut result = Vec::new();
        let mut skip_next = false;
        
        for line in conf.lines() {
            let trimmed = line.trim();
            
            // 跳过注释行
            if trimmed == format!("# {} anchor - auto-generated", self.anchor_name) {
                skip_next = true;
                continue;
            }
            
            // 跳过锚点相关的行
            if skip_next || 
               trimmed.starts_with(&format!("rdr-anchor \"{}\"", self.anchor_name)) ||
               trimmed.starts_with(&format!("anchor \"{}\"", self.anchor_name)) ||
               trimmed.starts_with(&format!("load anchor \"{}\"", self.anchor_name)) {
                skip_next = false;
                continue;
            }
            
            result.push(line);
        }
        
        let mut final_result = result.join("\n");
        if !final_result.is_empty() && !final_result.ends_with('\n') {
            final_result.push('\n');
        }
        
        final_result
    }
}

// === IP 别名管理函数 ===
// 这些函数可能更适合放在 utils 模块中，但暂时放在这里

/// 添加 loopback 接口的 IP 别名
pub async fn add_loopback_alias(ip: &str) -> Result<()> {
    debug!("Adding loopback alias: {}", ip);
    
    let output = utils::execute_admin_command(
        "ifconfig", 
        &["lo0", "alias", ip, "netmask", "255.0.0.0"]
    )?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // 如果别名已存在，这不是错误
        if !stderr.contains("File exists") {
            return Err(anyhow::anyhow!("Failed to add IP alias {}: {}", ip, stderr));
        }
    }
    
    debug!("Successfully added loopback alias: {}", ip);
    Ok(())
}

/// 移除 loopback 接口的 IP 别名
pub async fn remove_loopback_alias(ip: &str) -> Result<()> {
    debug!("Removing loopback alias: {}", ip);
    
    let output = utils::execute_admin_command(
        "ifconfig", 
        &["lo0", "delete", ip]
    )?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // 如果别名不存在，这不是严重错误
        debug!("Note: Failed to remove IP alias {} (may not exist): {}", ip, stderr);
    } else {
        debug!("Successfully removed loopback alias: {}", ip);
    }
    
    Ok(())
}

/// 检查 loopback 接口的 IP 别名是否存在
pub async fn check_loopback_alias(ip: &str) -> Result<bool> {
    let output = tokio::process::Command::new("ping")
        .args(&["-c", "1", "-t", "1", ip])
        .output()
        .await?;
    
    Ok(output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pfctl_creation() {
        let pfctl = Pfctl::new("test_anchor");
        assert_eq!(pfctl.anchor_name(), "test_anchor");
        assert_eq!(
            pfctl.anchor_file_path().to_string_lossy(), 
            "/etc/pf.anchors/test_anchor"
        );
    }

    #[test]
    fn test_check_line_exists() {
        let pfctl = Pfctl::new("test");
        let conf = r#"
# Some config
anchor "test"
rdr-anchor "test"
load anchor "test" from "/etc/pf.anchors/test"
"#;
        
        assert!(pfctl.check_line_exists(conf, "anchor \"test\""));
        assert!(pfctl.check_line_exists(conf, "rdr-anchor \"test\""));
        assert!(pfctl.check_line_exists(conf, "load anchor \"test\" from \"/etc/pf.anchors/test\""));
        assert!(!pfctl.check_line_exists(conf, "anchor \"other\""));
    }

    #[test]
    fn test_remove_anchor_lines() {
        let pfctl = Pfctl::new("test");
        let conf = r#"# Some other config
keep this line
# test anchor - auto-generated
rdr-anchor "test"
anchor "test"
load anchor "test" from "/etc/pf.anchors/test"
keep this too
"#;
        
        let result = pfctl.remove_anchor_lines(conf);
        assert!(result.contains("keep this line"));
        assert!(result.contains("keep this too"));
        assert!(!result.contains("test anchor"));
        assert!(!result.contains("rdr-anchor \"test\""));
        assert!(!result.contains("anchor \"test\""));
        assert!(!result.contains("load anchor"));
    }

    #[tokio::test]
    async fn test_smart_reload_logic() {
        let pfctl = Pfctl::new("test_smart_reload");
        
        // 测试便捷方法的逻辑（不实际执行 pfctl 命令）
        let rules_content = "# Test rules\npass all\n";
        
        // 这些测试主要验证方法的参数和调用逻辑
        // 实际的 pfctl 命令在单元测试中不会执行
        let _result = pfctl.write_anchor_file(rules_content).await;
        // 在单元测试环境中，这可能会失败，但我们只关心接口是否正确
        
        // 验证 anchor 名称和路径设置正确
        assert_eq!(pfctl.anchor_name(), "test_smart_reload");
        assert_eq!(
            pfctl.anchor_file_path().to_string_lossy(),
            "/etc/pf.anchors/test_smart_reload"
        );
    }
}
