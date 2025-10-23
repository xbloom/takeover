//! 控制适配器模块
//! 
//! 双层架构：
//! - `ControlService`: Rust API（内部接口，实现 ControlPort）
//! - `HttpApiServer`: HTTP API（外部接口，调用 ControlService）

pub mod api;
pub mod http_server;

pub use api::ControlService;
pub use http_server::HttpApiServer;
