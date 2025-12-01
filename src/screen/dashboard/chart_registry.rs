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

/// 图表元数据（不拥有图表本身）
#[derive(Debug, Clone)]
pub struct ChartMetadata {
    pub subscriber_id: SubscriberId,
    pub chart_type: ChartType,
    pub requirements: DataRequirements,
    pub pane_id: uuid::Uuid,  // 用于在 Dashboard 中查找对应的 pane
}

/// 图表注册表
/// 
/// 注意：只存储图表的元数据，不拥有图表本身
/// 图表仍然由 Content 管理，通过 pane_id 来访问
pub struct ChartRegistry {
    charts: HashMap<SubscriberId, ChartMetadata>,
    charts_by_type: HashMap<ChartType, Vec<SubscriberId>>,
    charts_by_pane: HashMap<uuid::Uuid, SubscriberId>,  // pane_id -> subscriber_id 映射
    data_manager: Arc<UnifiedDataManager>,
}

impl ChartRegistry {
    pub fn new(data_manager: Arc<UnifiedDataManager>) -> Self {
        Self {
            charts: HashMap::new(),
            charts_by_type: HashMap::new(),
            charts_by_pane: HashMap::new(),
            data_manager,
        }
    }
    
    /// 注册图表（只注册元数据，不拥有图表）
    pub fn register_chart(
        &mut self,
        subscriber_id: SubscriberId,
        chart_type: ChartType,
        requirements: DataRequirements,
        pane_id: uuid::Uuid,
    ) {
        // 注册到数据管理器
        self.data_manager.register_subscriber(subscriber_id, requirements.clone());
        
        // 添加到注册表
        let metadata = ChartMetadata {
            subscriber_id,
            chart_type,
            requirements,
            pane_id,
        };
        self.charts.insert(subscriber_id, metadata.clone());
        self.charts_by_type.entry(chart_type).or_insert_with(Vec::new).push(subscriber_id);
        self.charts_by_pane.insert(pane_id, subscriber_id);
        
        log::debug!("Chart registered: type={:?}, id={}, pane_id={}", chart_type, subscriber_id, pane_id);
    }
    
    /// 注销图表
    pub fn unregister_chart(&mut self, id: SubscriberId) {
        if let Some(metadata) = self.charts.remove(&id) {
            self.data_manager.unregister_subscriber(id);
            
            // 从类型索引中移除
            if let Some(subscribers) = self.charts_by_type.get_mut(&metadata.chart_type) {
                subscribers.retain(|&sid| sid != id);
            }
            
            // 从 pane 索引中移除
            self.charts_by_pane.remove(&metadata.pane_id);
            
            log::debug!("Chart unregistered: id={}, pane_id={}", id, metadata.pane_id);
        }
    }
    
    /// 通过 pane_id 注销图表
    pub fn unregister_chart_by_pane(&mut self, pane_id: uuid::Uuid) {
        if let Some(&subscriber_id) = self.charts_by_pane.get(&pane_id) {
            self.unregister_chart(subscriber_id);
        }
    }
    
    /// 获取图表元数据
    pub fn get_metadata(&self, id: SubscriberId) -> Option<&ChartMetadata> {
        self.charts.get(&id)
    }
    
    /// 通过 pane_id 获取订阅者 ID
    pub fn get_subscriber_id(&self, pane_id: uuid::Uuid) -> Option<SubscriberId> {
        self.charts_by_pane.get(&pane_id).copied()
    }
    
    /// 获取图表数量
    pub fn chart_count(&self) -> usize {
        self.charts.len()
    }
    
    /// 获取指定类型的图表数量
    pub fn chart_count_by_type(&self, chart_type: ChartType) -> usize {
        self.charts_by_type.get(&chart_type).map(|v| v.len()).unwrap_or(0)
    }
    
    /// 获取所有订阅者 ID（用于数据分发）
    pub fn all_subscriber_ids(&self) -> Vec<SubscriberId> {
        self.charts.keys().copied().collect()
    }
}

