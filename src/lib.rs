// 旧模块（保留用于兼容性）
pub mod config;
pub mod error;
pub mod socks5;
pub mod status;
pub mod utils;

pub mod pfctl;
pub mod transparent_proxy_manager;
pub mod routing_engine;
pub mod transparent_server;
pub mod protocol_handler;
pub mod tls_interception;
pub mod user_manager;

// 新架构模块
pub mod core;
pub mod adapters;

// 重新导出主要的公共接口

// 旧接口（保留用于兼容性）
pub use config::{Config, ProxyConfig, ProxyType};
pub use error::TakeoverError;
pub use socks5::{Socks5Client, DataForwarder};
pub use status::{TrafficStats, StatsCollector, SimpleStatsCollector};

pub use pfctl::Pfctl;
pub use transparent_proxy_manager::{TransparentProxyManager};
pub use routing_engine::{RoutingEngine, RoutingRules, RoutingDecision, TlsInterceptionMode};
pub use transparent_server::TransparentServer;
pub use user_manager::{UserManager, UserSwitcher};

// 新接口（六边形架构）
pub use core::{AppCore, AppConfig};
pub use core::ports::*;
pub use core::model::*;
pub use core::engine::*;
pub use core::middleware::*;
pub use core::services::*;

pub use adapters::*;
