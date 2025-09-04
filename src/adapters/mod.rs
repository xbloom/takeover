//! 适配器模块
//! 
//! 这个模块包含了六边形架构中的所有适配器实现。
//! 适配器负责将外部世界的接口转换为核心业务逻辑可以理解的形式。

pub mod inbound;
pub mod outbound;
pub mod control;
pub mod script;
pub mod api;
pub mod storage;
pub mod ui;

// 重新导出常用的适配器
pub use inbound::*;
pub use outbound::*;
pub use control::*;
pub use script::*;
pub use api::*;
