//! 图表注册表：管理所有图表实例
//! 
//! 这个模块实现了图表注册表，用于：
//! - 统一管理所有图表实例
//! - 按类型索引图表
//! - 生命周期管理
//! 
//! 注意：这是新架构的基础设施，默认不启用，不影响现有功能

use super::unified_data_manager::{UnifiedDataManager, SubscriberId, DataRequirements};
use std::collections::HashMap;
use std::sync::Arc;

/// 图表类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChartType {
    Kline,      // Footprint + Candles
    Heatmap,
    Ladder,
    TimeAndSales,
    Comparison,
    // 未来可以扩展
}

/// 图表数据管理接口（简化版，用于注册表）
/// 
/// 注意：不要求 Send + Sync，因为图表可能包含非线程安全的内部状态
pub trait ChartDataManager {
    /// 获取数据需求
    fn data_requirements(&self) -> DataRequirements;
    
    /// 获取订阅者 ID
    fn subscriber_id(&self) -> SubscriberId;
}

/// 图表注册表
pub struct ChartRegistry {
    charts: HashMap<SubscriberId, Box<dyn ChartDataManager>>,
    charts_by_type: HashMap<ChartType, Vec<SubscriberId>>,
    data_manager: Arc<UnifiedDataManager>,
}

impl ChartRegistry {
    pub fn new(data_manager: Arc<UnifiedDataManager>) -> Self {
        Self {
            charts: HashMap::new(),
            charts_by_type: HashMap::new(),
            data_manager,
        }
    }
    
    /// 注册图表
    pub fn register_chart<C: ChartDataManager + 'static>(
        &mut self,
        chart: C,
        chart_type: ChartType,
    ) -> SubscriberId {
        let id = chart.subscriber_id();
        let requirements = chart.data_requirements();
        
        // 注册到数据管理器
        self.data_manager.register_subscriber(id, requirements);
        
        // 添加到注册表
        self.charts.insert(id, Box::new(chart));
        self.charts_by_type.entry(chart_type).or_insert_with(Vec::new).push(id);
        
        log::debug!("Chart registered: type={:?}, id={}", chart_type, id);
        id
    }
    
    /// 注销图表
    pub fn unregister_chart(&mut self, id: SubscriberId) {
        self.data_manager.unregister_subscriber(id);
        self.charts.remove(&id);
        
        // 从类型索引中移除
        for subscribers in self.charts_by_type.values_mut() {
            subscribers.retain(|&sid| sid != id);
        }
        
        log::debug!("Chart unregistered: id={}", id);
    }
    
    /// 获取图表数量
    pub fn chart_count(&self) -> usize {
        self.charts.len()
    }
    
    /// 获取指定类型的图表数量
    pub fn chart_count_by_type(&self, chart_type: ChartType) -> usize {
        self.charts_by_type.get(&chart_type).map(|v| v.len()).unwrap_or(0)
    }
}

