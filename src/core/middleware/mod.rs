//! 中间件模块
//! 
//! 包含所有处理管道中的中间件实现

pub mod protocol_detection; // 协议检测中间件
pub mod process_info;      // 进程信息查询中间件
pub mod tls;
// pub mod http_parsing;  //已经废弃
pub mod http1_stream;  // 新的HTTP/1.1流中间件
pub mod http2_stream;  // 新的HTTP/2流中间件
pub mod rule_matching;
pub mod http_modification;
pub mod script;
pub mod user_intervention;
pub mod response_merge;
pub mod forwarding;    // 出站请求中间件
pub mod response_writeback; // 入站响应中间件

// 重新导出主要的中间件
pub use protocol_detection::ProtocolDetectionMiddleware; // 协议检测中间件
pub use process_info::ProcessInfoMiddleware;           // 进程信息查询中间件
pub use tls::TlsMiddleware;
// pub use http_parsing::HttpParsingMiddleware; //已经废弃
pub use http1_stream::Http1StreamMiddleware;  // 新的HTTP/1.1流中间件
pub use http2_stream::Http2StreamMiddleware;  // 新的HTTP/2流中间件
pub use rule_matching::{RuleMatchingMiddleware, RuleService, DefaultAction};
pub use http_modification::HttpModificationMiddleware;
pub use script::ScriptMiddleware;
pub use user_intervention::UserInterventionMiddleware;
pub use response_merge::ResponseMergeMiddleware;
pub use forwarding::OutboundRequestMiddleware; // 出站请求中间件
pub use response_writeback::InboundResponseMiddleware; // 入站响应中间件
