//! 出站适配器模块
//! 
//! 负责建立到目标服务器的连接

pub mod socks5;
pub mod direct;
pub mod composite;

pub use socks5::Socks5OutboundAdapter;
pub use direct::DirectOutboundAdapter;
pub use composite::CompositeOutboundAdapter;
