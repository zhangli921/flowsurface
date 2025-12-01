//! Ladder 的 ChartDataManager trait 实现
//! 
//! 注意：这是新架构的可选实现，默认不启用，不影响现有功能

use crate::screen::dashboard::chart_traits::ChartDataManager;
use crate::screen::dashboard::unified_data_manager::{DataRequirements, SubscriberId};
use super::ladder::Ladder;

impl ChartDataManager for Ladder {
    fn data_requirements(&self) -> DataRequirements {
        // Ladder (DOM) 需要 depth 和 trades 数据，仅实时数据，不支持历史数据
        DataRequirements {
            needs_klines: false,
            needs_trades: true,
            needs_depth: true,
            needs_open_interest: false,
            supports_historical: false, // Ladder 是实时图表
            supports_tick_basis: false,
        }
    }
    
    fn subscriber_id(&self) -> SubscriberId {
        self.subscriber_id
    }
    
    // 其他方法使用默认实现
}


