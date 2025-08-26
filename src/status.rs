/// 网络传输统计模块
use tracing::info;

/// 传输量统计信息
#[derive(Debug, Clone, Default)]
pub struct TrafficStats {
    /// 从客户端到服务器的字节数
    pub client_to_server_bytes: u64,
    /// 从服务器到客户端的字节数
    pub server_to_client_bytes: u64,
    /// 总传输字节数
    pub total_bytes: u64,
    /// 连接开始时间
    pub start_time: Option<std::time::Instant>,
    /// 连接结束时间
    pub end_time: Option<std::time::Instant>,
}

impl TrafficStats {
    /// 创建新的统计实例
    pub fn new() -> Self {
        Self {
            client_to_server_bytes: 0,
            server_to_client_bytes: 0,
            total_bytes: 0,
            start_time: Some(std::time::Instant::now()),
            end_time: None,
        }
    }
    
    /// 添加客户端到服务器的传输量
    pub fn add_client_to_server(&mut self, bytes: u64) {
        self.client_to_server_bytes += bytes;
        self.total_bytes += bytes;
    }
    
    /// 添加服务器到客户端的传输量
    pub fn add_server_to_client(&mut self, bytes: u64) {
        self.server_to_client_bytes += bytes;
        self.total_bytes += bytes;
    }
    
    /// 标记连接结束
    pub fn finish(&mut self) {
        self.end_time = Some(std::time::Instant::now());
    }
    
    /// 获取连接持续时间
    pub fn duration(&self) -> Option<std::time::Duration> {
        match (self.start_time, self.end_time) {
            (Some(start), Some(end)) => Some(end - start),
            _ => None,
        }
    }
    
    /// 格式化字节数为人类可读的格式
    pub fn format_bytes(bytes: u64) -> String {
        const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
        let mut size = bytes as f64;
        let mut unit_index = 0;
        
        while size >= 1024.0 && unit_index < UNITS.len() - 1 {
            size /= 1024.0;
            unit_index += 1;
        }
        
        if unit_index == 0 {
            format!("{} {}", bytes, UNITS[unit_index])
        } else {
            format!("{:.2} {}", size, UNITS[unit_index])
        }
    }
    
    /// 输出统计信息的日志
    pub fn log_stats(&self, connection_type: &str, target: &str) {
        let duration_str = if let Some(duration) = self.duration() {
            format!(" in {:.2}s", duration.as_secs_f64())
        } else {
            String::new()
        };
        
        info!("📊 {} to {} completed{} - Total: {} (↑ {}, ↓ {})", 
              connection_type, target, duration_str,
              Self::format_bytes(self.total_bytes),
              Self::format_bytes(self.client_to_server_bytes), 
              Self::format_bytes(self.server_to_client_bytes));
    }
}

/// 统计收集器特质
pub trait StatsCollector {
    /// 记录客户端到服务器的数据传输
    fn record_client_to_server(&mut self, bytes: u64);
    
    /// 记录服务器到客户端的数据传输
    fn record_server_to_client(&mut self, bytes: u64);
    
    /// 获取当前统计信息
    fn get_stats(&self) -> TrafficStats;
}

/// 简单的统计收集器实现
#[derive(Debug)]
pub struct SimpleStatsCollector {
    stats: TrafficStats,
}

impl SimpleStatsCollector {
    pub fn new() -> Self {
        Self {
            stats: TrafficStats::new(),
        }
    }
}

impl StatsCollector for SimpleStatsCollector {
    fn record_client_to_server(&mut self, bytes: u64) {
        self.stats.add_client_to_server(bytes);
    }
    
    fn record_server_to_client(&mut self, bytes: u64) {
        self.stats.add_server_to_client(bytes);
    }
    
    fn get_stats(&self) -> TrafficStats {
        self.stats.clone()
    }
}

impl Default for SimpleStatsCollector {
    fn default() -> Self {
        Self::new()
    }
}
