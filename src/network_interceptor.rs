use crate::process_monitor::ProcessInfo;
use crate::error::TakeoverError;
use anyhow::Result;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use tracing::{info, warn, error};

pub struct NetworkInterceptor {
    active_rules: HashMap<u32, InterceptionRule>,
    #[cfg(target_os = "macos")]
    pfctl_anchor: String,
}

#[derive(Debug, Clone)]
pub struct InterceptionRule {
    pub pid: u32,
    pub local_proxy_port: u16,
    pub rule_id: String,
}

impl NetworkInterceptor {
    pub fn new() -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            let anchor = "takeover".to_string();
            Ok(Self {
                active_rules: HashMap::new(),
                pfctl_anchor: anchor,
            })
        }
        
        #[cfg(target_os = "linux")]
        {
            Ok(Self {
                active_rules: HashMap::new(),
            })
        }
        
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(TakeoverError::NetworkInterception("Unsupported platform".to_string()).into())
        }
    }

    pub async fn setup_process_interception(&mut self, process_info: &ProcessInfo) -> Result<()> {
        // Use default port calculation for backward compatibility
        let local_proxy_port = 10000 + (process_info.pid % 10000) as u16;
        self.setup_process_interception_with_port(process_info, local_proxy_port).await
    }
    
    pub async fn setup_process_interception_with_port(&mut self, process_info: &ProcessInfo, local_proxy_port: u16) -> Result<()> {
        info!("Setting up network interception for PID: {} on port: {}", process_info.pid, local_proxy_port);
        
        #[cfg(target_os = "macos")]
        {
            self.setup_macos_interception_with_port(process_info, local_proxy_port).await
        }
        
        #[cfg(target_os = "linux")]
        {
            self.setup_linux_interception_with_port(process_info, local_proxy_port).await
        }
        
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(TakeoverError::NetworkInterception("Unsupported platform".to_string()).into())
        }
    }

    pub async fn remove_process_interception(&mut self, pid: u32) -> Result<()> {
        info!("Removing network interception for PID: {}", pid);
        
        if let Some(rule) = self.active_rules.remove(&pid) {
            #[cfg(target_os = "macos")]
            {
                self.remove_macos_rule(&rule).await?;
            }
            
            #[cfg(target_os = "linux")]
            {
                self.remove_linux_rule(&rule).await?;
            }
        }
        
        Ok(())
    }

    #[cfg(target_os = "macos")]
    async fn setup_macos_interception_with_port(&mut self, process_info: &ProcessInfo, local_proxy_port: u16) -> Result<()> {
        // On macOS, we need to use pfctl to redirect traffic
        // This implementation uses a safer anchor-based approach
        
        let rule_id = format!("takeover_pid_{}", process_info.pid);
        
        // First, ensure our anchor exists
        self.ensure_anchor_exists().await?;
        
        // Create a properly formatted pfctl rule
        // Note: This is a simplified approach - in production you'd want more sophisticated process targeting
        let pfctl_rules = format!(
            "# Takeover rules for PID {}\n\
             rdr pass on lo0 inet proto tcp from any to any port 80 -> 127.0.0.1 port {}\n\
             rdr pass on lo0 inet proto tcp from any to any port 443 -> 127.0.0.1 port {}\n\
             rdr pass on lo0 inet proto tcp from any to any port 8080 -> 127.0.0.1 port {}\n",
            process_info.pid, local_proxy_port, local_proxy_port, local_proxy_port
        );
        
        // Write rule to a temporary file
        let rule_file = format!("/tmp/{}.conf", rule_id);
        std::fs::write(&rule_file, &pfctl_rules)?;
        
        // Load the rule using pfctl with our anchor
        let output = Command::new("pfctl")
            .args(&["-a", &self.pfctl_anchor, "-f", &rule_file])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        
        info!("pfctl output - stdout: {}", stdout);
        info!("pfctl output - stderr: {}", stderr);
        
        if !output.status.success() {
            error!("Failed to load pfctl rule: {}", stderr);
            
            // Clean up the temp file before returning error
            std::fs::remove_file(&rule_file).ok();
            
            return Err(TakeoverError::NetworkInterception(
                format!("pfctl rule loading failed: {}", stderr)
            ).into());
        }
        
        // Verify the rules were actually loaded
        let verify_output = Command::new("pfctl")
            .args(&["-a", &self.pfctl_anchor, "-s", "nat"])
            .output()?;
        
        let verify_result = String::from_utf8_lossy(&verify_output.stdout);
        info!("pfctl verification - loaded rules: {}", verify_result);
        
        if verify_result.trim().is_empty() || verify_result.contains("No ALTQ support") {
            warn!("pfctl rules appear to be empty after loading!");
        }
        
        // Clean up temporary file
        std::fs::remove_file(&rule_file).ok();
        
        info!("Successfully loaded pfctl rules for PID: {}", process_info.pid);
        
        let rule = InterceptionRule {
            pid: process_info.pid,
            local_proxy_port,
            rule_id: rule_id.clone(),
        };
        
        self.active_rules.insert(process_info.pid, rule);
        
        info!("Successfully setup macOS interception for PID: {}", process_info.pid);
        
        // Note: This is a simplified implementation
        // A production version would need more sophisticated process-specific routing
        warn!("macOS network interception is simplified - may affect other processes");
        
        Ok(())
    }
    
    #[cfg(target_os = "macos")]
    async fn ensure_anchor_exists(&self) -> Result<()> {
        // Check if our anchor already exists, if not create it
        let check_output = Command::new("pfctl")
            .args(&["-s", "Anchors"])
            .output()?;
        
        let anchors = String::from_utf8_lossy(&check_output.stdout);
        
        if !anchors.contains(&self.pfctl_anchor) {
            // Enable pfctl if not already enabled
            let enable_output = Command::new("pfctl")
                .args(&["-e"])
                .output()?;
            
            if !enable_output.status.success() {
                let error_msg = String::from_utf8_lossy(&enable_output.stderr);
                warn!("Could not enable pfctl (may already be enabled): {}", error_msg);
            }
        }
        
        Ok(())
    }

    #[cfg(target_os = "macos")]
    async fn remove_macos_rule(&self, rule: &InterceptionRule) -> Result<()> {
        // Flush the specific anchor to remove our rules
        info!("Removing pfctl rules for PID: {}", rule.pid);
        
        let output = Command::new("pfctl")
            .args(&["-a", &self.pfctl_anchor, "-F", "nat"])
            .output()?;
        
        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to flush pfctl NAT rules: {}", error_msg);
            // Don't return error here as cleanup should be best-effort
        } else {
            info!("Removed pfctl NAT rules for PID: {}", rule.pid);
        }
        
        // Also try to flush redirect rules
        let rdr_output = Command::new("pfctl")
            .args(&["-a", &self.pfctl_anchor, "-F", "rules"])
            .output()?;
        
        if !rdr_output.status.success() {
            let error_msg = String::from_utf8_lossy(&rdr_output.stderr);
            warn!("Failed to flush pfctl redirect rules: {}", error_msg);
        }
        
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn setup_linux_interception_with_port(&mut self, process_info: &ProcessInfo, local_proxy_port: u16) -> Result<()> {
        // On Linux, we can use iptables or nftables for traffic redirection
        // This requires careful handling of netfilter rules
        
        let rule_id = format!("takeover_pid_{}", process_info.pid);
        
        // Method 1: Using iptables with owner match
        let iptables_rule = vec![
            "-t", "nat",
            "-A", "OUTPUT",
            "-m", "owner", "--pid-owner", &process_info.pid.to_string(),
            "-p", "tcp",
            "-j", "REDIRECT",
            "--to-port", &local_proxy_port.to_string(),
            "-m", "comment", "--comment", &rule_id,
        ];
        
        let output = Command::new("iptables")
            .args(&iptables_rule)
            .output()?;
        
        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            error!("Failed to add iptables rule: {}", error_msg);
            
            // Try alternative method using cgroup
            return self.setup_linux_cgroup_interception_with_port(process_info, local_proxy_port).await;
        }
        
        let rule = InterceptionRule {
            pid: process_info.pid,
            local_proxy_port,
            rule_id: rule_id.clone(),
        };
        
        self.active_rules.insert(process_info.pid, rule);
        
        info!("Successfully setup Linux iptables interception for PID: {}", process_info.pid);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn setup_linux_cgroup_interception_with_port(&mut self, process_info: &ProcessInfo, _local_proxy_port: u16) -> Result<()> {
        // Alternative method using cgroups for process isolation
        warn!("Cgroup-based interception not implemented yet");
        
        // This would involve:
        // 1. Creating a cgroup for the process
        // 2. Moving the process to the cgroup
        // 3. Setting up network namespace or routing rules for the cgroup
        
        Err(TakeoverError::NetworkInterception(
            "Cgroup interception not implemented".to_string()
        ).into())
    }

    #[cfg(target_os = "linux")]
    async fn remove_linux_rule(&self, rule: &InterceptionRule) -> Result<()> {
        // Remove the iptables rule
        let iptables_rule = vec![
            "-t", "nat",
            "-D", "OUTPUT",
            "-m", "owner", "--pid-owner", &rule.pid.to_string(),
            "-p", "tcp",
            "-j", "REDIRECT",
            "--to-port", &rule.local_proxy_port.to_string(),
            "-m", "comment", "--comment", &rule.rule_id,
        ];
        
        let output = Command::new("iptables")
            .args(&iptables_rule)
            .output()?;
        
        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to remove iptables rule: {}", error_msg);
        } else {
            info!("Removed iptables rule for PID: {}", rule.pid);
        }
        
        Ok(())
    }

    // Method for transparent proxy using SO_ORIGINAL_DST
    #[cfg(target_os = "linux")]
    async fn setup_transparent_proxy(&self, process_info: &ProcessInfo) -> Result<()> {
        // This would involve setting up a transparent proxy that can retrieve
        // the original destination using SO_ORIGINAL_DST sockopt
        warn!("Transparent proxy setup not implemented yet");
        Ok(())
    }

    // Enhanced method using eBPF for more precise control
    #[cfg(target_os = "linux")]
    async fn setup_ebpf_interception(&self, process_info: &ProcessInfo) -> Result<()> {
        // eBPF would allow for more granular control over network traffic
        // This is the most advanced method but also most complex
        warn!("eBPF interception not implemented yet");
        Ok(())
    }

    pub fn get_active_rules(&self) -> &HashMap<u32, InterceptionRule> {
        &self.active_rules
    }
    
    /// Cleanup all active network interception rules
    pub async fn cleanup_all_rules(&mut self) -> Result<()> {
        info!("Cleaning up all network interception rules");
        
        let active_pids: Vec<u32> = self.active_rules.keys().cloned().collect();
        
        for pid in active_pids {
            if let Err(e) = self.remove_process_interception(pid).await {
                warn!("Failed to cleanup rules for PID {}: {}", pid, e);
                // Continue cleanup for other PIDs even if one fails
            }
        }
        
        // Additional platform-specific global cleanup
        #[cfg(target_os = "macos")]
        {
            self.cleanup_macos_anchor().await?;
        }
        
        #[cfg(target_os = "linux")]
        {
            self.cleanup_linux_rules().await?;
        }
        
        info!("Network interception cleanup completed");
        Ok(())
    }
    
    #[cfg(target_os = "macos")]
    async fn cleanup_macos_anchor(&self) -> Result<()> {
        info!("Performing global macOS pfctl cleanup");
        
        // Flush all rules in our anchor
        let output = Command::new("pfctl")
            .args(&["-a", &self.pfctl_anchor, "-F", "all"])
            .output()?;
        
        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to flush all pfctl rules: {}", error_msg);
        } else {
            info!("Successfully flushed all pfctl rules from anchor: {}", self.pfctl_anchor);
        }
        
        Ok(())
    }
    
    #[cfg(target_os = "linux")]
    async fn cleanup_linux_rules(&self) -> Result<()> {
        info!("Performing global Linux iptables cleanup");
        
        // Find and remove all takeover-related rules
        let list_output = Command::new("iptables")
            .args(&["-t", "nat", "-L", "OUTPUT", "--line-numbers"])
            .output()?;
        
        if list_output.status.success() {
            let output_str = String::from_utf8_lossy(&list_output.stdout);
            for line in output_str.lines() {
                if line.contains("takeover") {
                    // Extract rule number and remove it
                    if let Some(rule_num) = line.split_whitespace().next() {
                        if let Ok(_num) = rule_num.parse::<u32>() {
                            let remove_output = Command::new("iptables")
                                .args(&["-t", "nat", "-D", "OUTPUT", rule_num])
                                .output();
                            
                            match remove_output {
                                Ok(output) if output.status.success() => {
                                    info!("Removed iptables rule: {}", rule_num);
                                }
                                Ok(output) => {
                                    let error_msg = String::from_utf8_lossy(&output.stderr);
                                    warn!("Failed to remove iptables rule {}: {}", rule_num, error_msg);
                                }
                                Err(e) => {
                                    warn!("Failed to execute iptables command: {}", e);
                                }
                            }
                        }
                    }
                }
            }
        }
        
        Ok(())
    }
}

// Implement Drop trait for automatic cleanup when the struct is dropped
impl Drop for NetworkInterceptor {
    fn drop(&mut self) {
        if !self.active_rules.is_empty() {
            warn!("NetworkInterceptor dropped with {} active rules, attempting cleanup", self.active_rules.len());
            
            // Perform synchronous cleanup since Drop can't be async
            #[cfg(target_os = "macos")]
            {
                self.sync_cleanup_macos();
            }
            
            #[cfg(target_os = "linux")]
            {
                self.sync_cleanup_linux();
            }
        }
    }
}

impl NetworkInterceptor {
    #[cfg(target_os = "macos")]
    fn sync_cleanup_macos(&self) {
        use std::process::{Command, Stdio};
        
        warn!("Performing emergency pfctl cleanup");
        
        let output = Command::new("pfctl")
            .args(&["-a", &self.pfctl_anchor, "-F", "all"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
        
        match output {
            Ok(result) if result.status.success() => {
                warn!("Emergency pfctl cleanup completed");
            }
            _ => {
                warn!("Emergency pfctl cleanup failed - manual cleanup may be required");
                warn!("Run: sudo pfctl -a {} -F all", self.pfctl_anchor);
            }
        }
    }
    
    #[cfg(target_os = "linux")]
    fn sync_cleanup_linux(&self) {
        use std::process::{Command, Stdio};
        
        warn!("Performing emergency iptables cleanup");
        
        // Try to flush NAT table OUTPUT chain rules with takeover comment
        let output = Command::new("bash")
            .args(&["-c", "iptables -t nat -L OUTPUT --line-numbers | grep takeover | awk '{print $1}' | tac | xargs -I {} iptables -t nat -D OUTPUT {}"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
        
        match output {
            Ok(result) if result.status.success() => {
                warn!("Emergency iptables cleanup completed");
            }
            _ => {
                warn!("Emergency iptables cleanup failed - manual cleanup may be required");
                warn!("Run: sudo iptables -t nat -L OUTPUT | grep takeover");
            }
        }
    }
}
