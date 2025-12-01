//! HeatmapChart 的 ChartDataManager trait 实现
//! 
//! 注意：这是新架构的可选实现，默认不启用，不影响现有功能

use crate::screen::dashboard::chart_traits::ChartDataManager;
use crate::screen::dashboard::unified_data_manager::{DataRequirements, SubscriberId};
use super::heatmap::HeatmapChart;

impl ChartDataManager for HeatmapChart {
    fn data_requirements(&self) -> DataRequirements {
        // Heatmap 需要 trades 数据，支持历史数据和实时数据
        // 注意：Heatmap 目前只支持 Time-based aggregation，不支持 Tick-based
        DataRequirements {
            needs_klines: false,
            needs_trades: true,
            needs_depth: false,
            needs_open_interest: false,
            supports_historical: true,
            supports_tick_basis: false, // Heatmap 目前只支持 Time-based
        }
    }
    
    fn subscriber_id(&self) -> SubscriberId {
        self.subscriber_id
    }
    
    // 其他方法使用默认实现
}


