//! 图表 Trait 定义
//! 
//! 定义统一的图表接口，用于数据管理
//! 
//! 注意：这是新架构的基础设施，默认不启用，不影响现有功能

use super::unified_data_manager::{DataRequirements, SubscriberId};
use exchange::fetcher::{FetchRange, FetchedData};
use crate::chart::Action;
use std::sync::Arc;

/// 实时数据类型
#[derive(Debug, Clone)]
pub enum RealtimeData {
    DepthAndTrades {
        depth: exchange::depth::Depth,
        trades: Vec<exchange::Trade>,
        time: u64,
    },
    Kline(exchange::Kline),
    // 未来可以扩展其他类型
}

/// 图表数据管理接口（完整版）
/// 
/// 注意：不要求 Send + Sync，因为图表可能包含非线程安全的内部状态（如 RefCell）
pub trait ChartDataManager {
    /// 获取数据需求
    fn data_requirements(&self) -> DataRequirements;
    
    /// 获取订阅者 ID
    fn subscriber_id(&self) -> SubscriberId;
    
    /// 请求数据（可选，实时图表可能不需要）
    fn request_data(&mut self, _range: FetchRange) -> Option<Action> {
        // 默认实现：不支持历史数据的图表返回 None
        None
    }
    
    /// 检查缺失数据（可选）
    fn missing_data_task(&mut self) -> Option<Action> {
        None
    }
    
    /// 插入实时数据（必须实现）
    fn insert_realtime_data(&mut self, _data: &RealtimeData) {
        // 默认实现：子类必须实现
    }
    
    /// 插入历史数据（可选）
    fn insert_historical_data(&mut self, _data: Arc<FetchedData>) {
        // 默认实现：不支持历史数据的图表忽略
    }
}

