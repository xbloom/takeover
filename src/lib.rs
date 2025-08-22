pub mod config;
pub mod error;

#[cfg(target_os = "macos")]
pub mod transparent_proxy;

pub mod utils;

// 重新导出主要的公共接口
pub use config::{Config, ProxyConfig, ProxyType};
pub use error::TakeoverError;

#[cfg(target_os = "macos")]
pub use transparent_proxy::TransparentProxy;
