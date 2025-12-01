//! 数据共享架构示例代码
//! 
//! 这是一个简化的实现示例，展示核心概念

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;
use uuid::Uuid;
use exchange::{TickerInfo, Trade, Kline};
use exchange::fetcher::{FetchRange, FetchedData};

// ============================================================================
// 1. 数据键：唯一标识数据
// ============================================================================

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct DataKey {
    pub ticker: TickerInfo,
    pub range: FetchRange,
}

// ============================================================================
// 2. 缓存项
// ============================================================================

struct CachedData {
    data: Arc<FetchedData>,
    subscribers: Vec<Uuid>,
    last_accessed: Instant,
}

// ============================================================================
// 3. 数据源管理器（全局单例）
// ============================================================================

pub struct DataSourceManager {
    // 数据缓存：Key -> CachedData
    cache: Arc<RwLock<HashMap<DataKey, CachedData>>>,
    
    // 进行中的请求：Key -> [Subscriber IDs]
    pending: Arc<RwLock<HashMap<DataKey, Vec<Uuid>>>>,
    
    // 最大缓存大小
    max_cache_size: usize,
}

impl DataSourceManager {
    pub fn new(max_cache_size: usize) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(RwLock::new(HashMap::new())),
            max_cache_size,
        }
    }

    /// 请求数据：返回缓存数据或请求状态
    pub fn request_data(
        &self,
        key: DataKey,
        subscriber_id: Uuid,
    ) -> DataRequestResult {
        // 1. 检查缓存
        {
            let mut cache = self.cache.write().unwrap();
            if let Some(cached) = cache.get_mut(&key) {
                cached.last_accessed = Instant::now();
                if !cached.subscribers.contains(&subscriber_id) {
                    cached.subscribers.push(subscriber_id);
                }
                return DataRequestResult::Cached(Arc::clone(&cached.data));
            }
        }

        // 2. 检查是否有进行中的请求
        {
            let mut pending = self.pending.write().unwrap();
            if let Some(subscribers) = pending.get_mut(&key) {
                // 已有请求，添加订阅者
                if !subscribers.contains(&subscriber_id) {
                    subscribers.push(subscriber_id);
                }
                return DataRequestResult::Pending;
            } else {
                // 新请求，记录订阅者
                pending.insert(key.clone(), vec![subscriber_id]);
            }
        }

        // 3. 需要发起新请求
        DataRequestResult::NewRequest(key)
    }

    /// 数据获取完成：更新缓存并返回订阅者列表
    pub fn on_data_fetched(&self, key: DataKey, data: FetchedData) -> Vec<Uuid> {
        let subscribers = {
            let mut pending = self.pending.write().unwrap();
            pending.remove(&key).unwrap_or_default()
        };

        let data_arc = Arc::new(data);
        
        // 更新缓存
        {
            let mut cache = self.cache.write().unwrap();
            cache.insert(key, CachedData {
                data: Arc::clone(&data_arc),
                subscribers: subscribers.clone(),
                last_accessed: Instant::now(),
            });
            
            // 清理过期缓存（简单实现：按大小限制）
            if cache.len() > self.max_cache_size {
                self.cleanup_cache(&mut cache);
            }
        }

        subscribers
    }

    /// 取消订阅
    pub fn unsubscribe(&self, key: &DataKey, subscriber_id: Uuid) {
        let mut cache = self.cache.write().unwrap();
        if let Some(cached) = cache.get_mut(key) {
            cached.subscribers.retain(|&id| id != subscriber_id);
            if cached.subscribers.is_empty() {
                cache.remove(key);
            }
        }
    }

    /// 清理缓存：移除最久未访问的项
    fn cleanup_cache(&self, cache: &mut HashMap<DataKey, CachedData>) {
        // 简单实现：移除最旧的 10%
        let to_remove = cache.len() / 10;
        let mut entries: Vec<_> = cache.iter().collect();
        entries.sort_by_key(|(_, cached)| cached.last_accessed);
        
        for (key, _) in entries.iter().take(to_remove) {
            cache.remove(key);
        }
    }
}

// ============================================================================
// 4. 请求结果
// ============================================================================

pub enum DataRequestResult {
    /// 数据已缓存，直接返回
    Cached(Arc<FetchedData>),
    /// 请求已在进行中，等待完成
    Pending,
    /// 需要发起新请求
    NewRequest(DataKey),
}

// ============================================================================
// 5. 使用示例：修改后的 KlineChart
// ============================================================================

pub struct KlineChart {
    id: Uuid,
    data_manager: Arc<DataSourceManager>,
    shared_data: Option<Arc<FetchedData>>,
    // ... 其他字段
}

impl KlineChart {
    pub fn new(data_manager: Arc<DataSourceManager>) -> Self {
        Self {
            id: Uuid::new_v4(),
            data_manager,
            shared_data: None,
        }
    }

    /// 请求数据（新方法）
    pub fn request_data(&mut self, ticker: TickerInfo, range: FetchRange) {
        let key = DataKey { ticker, range };
        
        match self.data_manager.request_data(key.clone(), self.id) {
            DataRequestResult::Cached(data) => {
                // 直接使用缓存数据
                self.shared_data = Some(data);
                self.invalidate();
            }
            DataRequestResult::Pending => {
                // 等待数据到达（通过事件通知）
                // 实际实现中可以使用 channel 或 callback
            }
            DataRequestResult::NewRequest(key) => {
                // 发起新请求
                // 这里应该调用 Dashboard 的 fetch 方法
                // 示例：self.dashboard.request_fetch(key);
            }
        }
    }

    /// 数据到达回调
    pub fn on_data_received(&mut self, data: Arc<FetchedData>) {
        self.shared_data = Some(data);
        self.invalidate();
    }

    fn invalidate(&mut self) {
        // 触发重绘
    }
}

// ============================================================================
// 6. Dashboard 集成示例
// ============================================================================

pub struct Dashboard {
    data_manager: Arc<DataSourceManager>,
    // ... 其他字段
}

impl Dashboard {
    pub fn new() -> Self {
        Self {
            data_manager: Arc::new(DataSourceManager::new(1000)),
            // ...
        }
    }

    /// 处理数据获取请求
    pub fn handle_fetch_request(&mut self, key: DataKey) {
        // 检查是否已有相同请求
        let pending = self.data_manager.pending.read().unwrap();
        if pending.contains_key(&key) {
            // 请求已在进行，无需重复
            return;
        }
        drop(pending);

        // 创建下载任务
        let data_manager = Arc::clone(&self.data_manager);
        let task = self.create_fetch_task(key.clone(), move |data| {
            // 数据到达后，通知所有订阅者
            let subscribers = data_manager.on_data_fetched(key.clone(), data);
            
            // 通过事件总线通知订阅者（简化示例）
            for subscriber_id in subscribers {
                // 实际实现中应该通过消息系统通知
                // self.event_bus.send(DataEvent::Fetched { key, subscriber_id });
            }
        });

        // 执行任务
        self.execute_task(task);
    }

    fn create_fetch_task<F>(&self, key: DataKey, callback: F) -> Task
    where
        F: FnOnce(FetchedData) + Send + 'static,
    {
        // 创建实际的下载任务
        // 示例实现
        Task::none()
    }

    fn execute_task(&mut self, _task: Task) {
        // 执行任务
    }
}

// ============================================================================
// 7. 使用场景示例
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_sharing() {
        let manager = Arc::new(DataSourceManager::new(100));
        
        // 创建两个图表
        let mut chart1 = KlineChart::new(Arc::clone(&manager));
        let mut chart2 = KlineChart::new(Arc::clone(&manager));
        
        let ticker = TickerInfo::default(); // 示例
        let range = FetchRange::Trades(1000, 2000);
        
        // 两个图表请求相同数据
        chart1.request_data(ticker.clone(), range.clone());
        chart2.request_data(ticker.clone(), range.clone());
        
        // 结果：
        // - chart1 发起新请求
        // - chart2 发现请求已在进行，等待
        // - 数据到达后，两个图表都收到通知
        // - 数据只下载一次，内存中只存储一份
    }
}


