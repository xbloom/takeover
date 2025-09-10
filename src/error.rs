use thiserror::Error;

#[derive(Error, Debug)]
pub enum TakeoverError {
    #[error("Configuration error: {0}")]
    Config(String),
    
    #[error("Process monitoring error: {0}")]
    ProcessMonitor(String),
    
    #[error("Network interception error: {0}")]
    NetworkInterception(String),

    #[error("tls handshake error: {0}")]
    Handshake(String),
    
    #[error("Proxy error: {0}")]
    Proxy(String),

    #[error("Stream read/write: {0}")]
    Stream(String),
    
    #[error("Script error: {0}")]
    Script(String),
    
    #[error("Lua error: {0}")]
    Lua(#[from] mlua::Error),
    
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    
    #[error("System error: {0}")]
    System(String),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    
    #[error("Nix error: {0}")]
    Nix(#[from] nix::Error),
}
