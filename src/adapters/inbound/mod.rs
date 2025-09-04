//! 入站适配器模块
//! 
//! 负责接收外部连接并将其转换为系统内部的Connection对象

pub mod pf;

pub use pf::PfInboundAdapter;
