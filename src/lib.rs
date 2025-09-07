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
// 核心端口接口
pub use core::ports::{
    InboundPort, OutboundPort, ControlPort, ApiPort, EventPort, ScriptPort
};

// 核心模型
pub use core::model::*;

// 核心引擎
pub use core::engine::*;

// 中间件（避免与adapters的冲突）
pub use core::middleware::{
    TlsMiddleware, Http1StreamMiddleware, Http2StreamMiddleware,
    RuleMatchingMiddleware, HttpModificationMiddleware,
    ForwardingMiddleware, ResponseMergeMiddleware, UserInterventionMiddleware
};

// 服务（使用具体导入避免ConnectionState冲突）
pub use core::services::{StateManager, ReplayManager};

// 适配器
pub use adapters::{
    inbound::*, outbound::*, api::*
};
// 注意：暂时不导出adapters::script以避免与core::middleware::script冲突
