pub mod config;
pub mod error;
pub mod socks5;
pub mod status;
pub mod utils;

pub mod pfctl;
pub mod transparent_proxy_manager;
pub mod routing_engine;
pub mod transparent_server;
pub mod transparent_proxy;

pub mod user_manager;

// 重新导出主要的公共接口
pub use config::{Config, ProxyConfig, ProxyType};
pub use error::TakeoverError;
pub use socks5::{Socks5Client, DataForwarder};
pub use status::{TrafficStats, StatsCollector, SimpleStatsCollector};

pub use pfctl::Pfctl;
pub use transparent_proxy_manager::{TransparentProxyManager};
pub use routing_engine::{RoutingEngine, RoutingRules, RoutingDecision};
pub use transparent_server::TransparentServer;
pub use transparent_proxy::{TransparentProxy, ProxyMode};
pub use user_manager::{UserManager, UserSwitcher};
