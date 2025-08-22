use crate::config::{Config, TargetProgram};
use crate::network_interceptor::NetworkInterceptor;
use crate::proxy_manager::ProxyManager;
use crate::utils::{get_process_executable_path, check_privileges};
use crate::error::TakeoverError;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use sysinfo::{Pid, System};
use tokio::sync::Mutex;
use tokio::time::{interval, sleep};
use tracing::{info, warn, error, debug};

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub executable_path: String,
    pub target_config: TargetProgram,
}

pub struct ProcessMonitor {
    config: Config,
    proxy_manager: Arc<Mutex<ProxyManager>>,
    network_interceptor: Arc<Mutex<NetworkInterceptor>>,
    monitored_processes: Arc<Mutex<HashMap<u32, ProcessInfo>>>,
    system: System,
}

impl ProcessMonitor {
    pub fn new(
        config: Config,
        proxy_manager: Arc<Mutex<ProxyManager>>,
        network_interceptor: Arc<Mutex<NetworkInterceptor>>,
    ) -> Result<Self> {
        // Check if we have sufficient privileges
        if !check_privileges()? {
            return Err(TakeoverError::PermissionDenied(
                "Root privileges required for process monitoring and network interception".to_string()
            ).into());
        }

        Ok(Self {
            config,
            proxy_manager,
            network_interceptor,
            monitored_processes: Arc::new(Mutex::new(HashMap::new())),
            system: System::new_all(),
        })
    }

    pub async fn start_monitoring(&mut self) -> Result<()> {
        info!("Starting process monitoring");
        
        // Initial scan of existing processes
        self.scan_existing_processes().await?;
        
        // Start periodic monitoring
        let mut interval = interval(Duration::from_secs(2));
        
        loop {
            interval.tick().await;
            
            if let Err(e) = self.scan_processes().await {
                error!("Error during process scan: {}", e);
                sleep(Duration::from_secs(5)).await;
            }
        }
    }

    async fn scan_existing_processes(&mut self) -> Result<()> {
        info!("Scanning existing processes");
        self.system.refresh_processes();
        
        let processes: Vec<_> = self.system.processes().iter().collect();
        
        for (pid, process) in processes {
            let pid_u32 = pid.as_u32();
            let process_name = process.name();
            
            // Check if this process matches any of our targets
            if let Some(target) = self.config.targets.get(process_name) {
                if !target.enabled {
                    continue;
                }
                
                match get_process_executable_path(pid_u32) {
                    Ok(executable_path) => {
                        if let Some(target_config) = self.config.get_target_for_path(&executable_path) {
                            info!("Found existing target process: {} (PID: {}, Path: {})", 
                                  process_name, pid_u32, executable_path);
                            
                            let process_info = ProcessInfo {
                                pid: pid_u32,
                                name: process_name.to_string(),
                                executable_path: executable_path.clone(),
                                target_config: target_config.clone(),
                            };
                            
                            self.handle_new_target_process(process_info).await?;
                        }
                    }
                    Err(e) => {
                        debug!("Could not get executable path for PID {}: {}", pid_u32, e);
                    }
                }
            }
        }
        
        Ok(())
    }

    async fn scan_processes(&mut self) -> Result<()> {
        self.system.refresh_processes();
        
        let current_pids: HashSet<u32> = self.system.processes()
            .keys()
            .map(|pid| pid.as_u32())
            .collect();
        
        let monitored = self.monitored_processes.lock().await;
        let monitored_pids: HashSet<u32> = monitored.keys().cloned().collect();
        drop(monitored);
        
        // Check for new processes
        let new_pids: Vec<u32> = current_pids.difference(&monitored_pids).cloned().collect();
        
        for pid in new_pids {
            if let Some(process) = self.system.process(Pid::from_u32(pid)) {
                let process_name = process.name();
                
                // Check if this process matches any of our targets
                if let Some(target) = self.config.targets.get(process_name) {
                    if !target.enabled {
                        continue;
                    }
                    
                    match get_process_executable_path(pid) {
                        Ok(executable_path) => {
                            if let Some(target_config) = self.config.get_target_for_path(&executable_path) {
                                info!("Detected new target process: {} (PID: {}, Path: {})", 
                                      process_name, pid, executable_path);
                                
                                let process_info = ProcessInfo {
                                    pid,
                                    name: process_name.to_string(),
                                    executable_path: executable_path.clone(),
                                    target_config: target_config.clone(),
                                };
                                
                                self.handle_new_target_process(process_info).await?;
                            }
                        }
                        Err(e) => {
                            debug!("Could not get executable path for PID {}: {}", pid, e);
                        }
                    }
                }
            }
        }
        
        // Check for terminated processes
        let terminated_pids: Vec<u32> = monitored_pids.difference(&current_pids).cloned().collect();
        
        for pid in terminated_pids {
            self.handle_terminated_process(pid).await?;
        }
        
        Ok(())
    }

    async fn handle_new_target_process(&self, process_info: ProcessInfo) -> Result<()> {
        info!("Setting up proxy for process: {} (PID: {})", process_info.name, process_info.pid);
        
        // Add to monitored processes
        {
            let mut monitored = self.monitored_processes.lock().await;
            monitored.insert(process_info.pid, process_info.clone());
        }
        
        // First configure proxy manager to get the local port
        let local_proxy_port = {
            let mut proxy_manager = self.proxy_manager.lock().await;
            proxy_manager.add_process_proxy(process_info.pid, &process_info.target_config.proxy_config)?;
            proxy_manager.get_local_proxy_port(process_info.pid)
                .ok_or_else(|| anyhow::anyhow!("Failed to get local proxy port for PID {}", process_info.pid))?
        };
        
        // Then setup network interception using the same port
        {
            let mut interceptor = self.network_interceptor.lock().await;
            interceptor.setup_process_interception_with_port(&process_info, local_proxy_port).await?;
        }
        
        Ok(())
    }

    async fn handle_terminated_process(&self, pid: u32) -> Result<()> {
        let process_info = {
            let mut monitored = self.monitored_processes.lock().await;
            monitored.remove(&pid)
        };
        
        if let Some(process_info) = process_info {
            info!("Cleaning up proxy for terminated process: {} (PID: {})", 
                  process_info.name, pid);
            
            // Remove network interception
            {
                let mut interceptor = self.network_interceptor.lock().await;
                interceptor.remove_process_interception(pid).await?;
            }
            
            // Remove from proxy manager
            {
                let mut proxy_manager = self.proxy_manager.lock().await;
                proxy_manager.remove_process_proxy(pid)?;
            }
        }
        
        Ok(())
    }

    pub async fn get_monitored_processes(&self) -> HashMap<u32, ProcessInfo> {
        let monitored = self.monitored_processes.lock().await;
        monitored.clone()
    }

    pub async fn stop_monitoring_process(&self, pid: u32) -> Result<()> {
        self.handle_terminated_process(pid).await
    }

    #[cfg(target_os = "macos")]
    async fn setup_kqueue_monitoring(&self) -> Result<()> {
        // TODO: Implement kqueue-based process monitoring for better performance
        // This would be more efficient than polling but more complex to implement
        warn!("KQueue monitoring not implemented yet, using polling");
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn setup_netlink_monitoring(&self) -> Result<()> {
        // TODO: Implement netlink-based process monitoring for Linux
        warn!("Netlink monitoring not implemented yet, using polling");
        Ok(())
    }
}
