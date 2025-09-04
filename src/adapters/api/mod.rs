//! API适配器模块
//! 
//! 负责为脚本和外部系统提供API访问

pub mod lua_api;

pub use lua_api::LuaApiAdapter;
