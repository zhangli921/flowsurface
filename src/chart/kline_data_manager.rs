//! KlineChart 的 ChartDataManager trait 实现
//! 
//! 注意：这是新架构的可选实现，默认不启用，不影响现有功能
//! 
//! 使用方式：
//! 1. 在 Dashboard 中初始化 UnifiedDataManager 和 ChartRegistry
//! 2. 注册 KlineChart 到 ChartRegistry
//! 3. 通过 UnifiedDataManager 统一管理数据请求

use crate::screen::dashboard::chart_traits::ChartDataManager;
use crate::screen::dashboard::unified_data_manager::{DataRequirements, SubscriberId};
use super::kline::KlineChart;

impl ChartDataManager for KlineChart {
    fn data_requirements(&self) -> DataRequirements {
        // 根据 chart kind 和 enabled studies 决定数据需求
        let needs_trades = match &self.kind {
            data::chart::KlineChartKind::Footprint { .. } => true,
            data::chart::KlineChartKind::Candles { .. } => false,
        };
        
        DataRequirements {
            needs_klines: true,
            needs_trades,
            needs_depth: false,
            needs_open_interest: false,
            supports_historical: true,
            supports_tick_basis: true,
        }
    }
    
    fn subscriber_id(&self) -> SubscriberId {
        self.subscriber_id
    }
    
    // 其他方法使用默认实现
}

