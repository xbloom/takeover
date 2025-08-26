use anyhow::Result;
use clap::{Arg, Command};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, error};

mod config;
mod error;
mod process_monitor;
mod network_interceptor;
mod proxy_manager;
mod socks5;
mod utils;
mod status;
#[cfg(target_os = "macos")]
pub mod transparent_proxy;

use config::Config;
use process_monitor::ProcessMonitor;
use network_interceptor::NetworkInterceptor;
use proxy_manager::ProxyManager;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let matches = Command::new("takeover")
        .version("0.1.0")
        .about("Transparent proxy for applications like Proxifier")
        .arg(
            Arg::new("config")
                .short('c')
                .long("config")
                .value_name("FILE")
                .help("Configuration file path")
                .default_value("config.json")
        )
        .arg(
            Arg::new("target")
                .short('t')
                .long("target")
                .value_name("PROGRAM_NAME")
                .help("Target program name to proxy")
                .required(true)
        )
        .arg(
            Arg::new("proxy")
                .short('p')
                .long("proxy")
                .value_name("PROXY_URL")
                .help("Proxy URL (e.g., socks5://127.0.0.1:1080)")
                .required(true)
        )
        .arg(
            Arg::new("daemon")
                .short('d')
                .long("daemon")
                .help("Run as daemon")
                .action(clap::ArgAction::SetTrue)
        )
        .get_matches();

    let config_path = matches.get_one::<String>("config").unwrap();
    let target_program = matches.get_one::<String>("target").unwrap();
    let proxy_url = matches.get_one::<String>("proxy").unwrap();
    let daemon_mode = matches.get_flag("daemon");

    // Load configuration
    let mut config = Config::load_or_create(config_path)?;
    config.add_target_program(target_program.clone(), proxy_url.clone())?;
    
    // Save the updated configuration
    config.save(config_path)?;
    
    info!("Starting takeover with target: {}, proxy: {}", target_program, proxy_url);
    info!("Configuration targets: {:?}", config.targets.keys().collect::<Vec<_>>());

    // Initialize components
    let proxy_manager = Arc::new(Mutex::new(ProxyManager::new()?));
    let network_interceptor = Arc::new(Mutex::new(NetworkInterceptor::new()?));
    let mut process_monitor = ProcessMonitor::new(config.clone(), proxy_manager.clone(), network_interceptor.clone())?;

    // Setup signal handlers for graceful shutdown
    setup_signal_handlers(network_interceptor.clone()).await?;

    if daemon_mode {
        info!("Running in daemon mode");
        daemonize()?;
    }

    // Start monitoring
    let monitoring_result = process_monitor.start_monitoring().await;
    
    // Cleanup on normal exit
    info!("Process monitoring ended, performing cleanup...");
    
    // Cleanup network interception rules
    match network_interceptor.lock().await.cleanup_all_rules().await {
        Ok(()) => {
            info!("Network cleanup completed successfully");
        }
        Err(e) => {
            error!("Network cleanup failed: {}", e);
        }
    }
    
    monitoring_result
}

async fn setup_signal_handlers(network_interceptor: Arc<Mutex<NetworkInterceptor>>) -> Result<()> {
    use tokio::signal;
    
    tokio::spawn(async move {
        let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate()).unwrap();
        let mut sigint = signal::unix::signal(signal::unix::SignalKind::interrupt()).unwrap();
        
        tokio::select! {
            _ = sigterm.recv() => {
                info!("Received SIGTERM, shutting down gracefully");
            }
            _ = sigint.recv() => {
                info!("Received SIGINT, shutting down gracefully");
            }
        }
        
        // Cleanup logic
        info!("Performing cleanup before exit...");
        
        // Cleanup network interception rules
        match network_interceptor.lock().await.cleanup_all_rules().await {
            Ok(()) => {
                info!("Network cleanup completed successfully");
            }
            Err(e) => {
                error!("Network cleanup failed: {}", e);
            }
        }
        
        info!("Cleanup completed, exiting...");
        std::process::exit(0);
    });
    
    Ok(())
}

#[cfg(unix)]
fn daemonize() -> Result<()> {
    use nix::unistd::{fork, ForkResult, setsid};
    use std::process;

    match unsafe { fork() } {
        Ok(ForkResult::Parent { .. }) => {
            process::exit(0);
        }
        Ok(ForkResult::Child) => {
            setsid()?;
            // Continue running as daemon
        }
        Err(err) => {
            error!("Fork failed: {}", err);
            return Err(err.into());
        }
    }
    
    Ok(())
}

#[cfg(not(unix))]
fn daemonize() -> Result<()> {
    warn!("Daemon mode not supported on this platform");
    Ok(())
}
