//! 数据管理层：统一管理数据获取、缓存和共享
//! 
//! 设计原则：
//! 1. 单一数据源（Single Source of Truth）
//! 2. 观察者模式（Observer Pattern）
//! 3. 请求去重（Request Deduplication）
//! 4. 引用计数缓存（Reference-counted Cache）

use exchange::{TickerInfo, Trade, Kline, Timeframe};
use exchange::fetcher::{FetchRange, FetchedData};
use std::collections::{HashMap, BTreeMap};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use std::time::Instant;

/// 数据键：唯一标识一组数据
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct DataKey {
    pub ticker: TickerInfo,
    pub range: FetchRange,
}

/// 数据订阅者 ID
pub type SubscriberId = Uuid;

/// 数据订阅者：需要数据的组件
pub trait DataSubscriber: Send + Sync {
    fn on_data_updated(&mut self, key: &DataKey, data: &FetchedData);
    fn subscriber_id(&self) -> SubscriberId;
}

/// 数据缓存项
struct CachedData {
    data: FetchedData,
    subscribers: Vec<SubscriberId>,
    last_accessed: Instant,
    fetch_time: Instant,
}

/// 全局数据管理器
pub struct DataManager {
    // 数据缓存：按 DataKey 存储
    cache: Arc<RwLock<HashMap<DataKey, CachedData>>>,
    
    // 进行中的请求：避免重复请求
    pending_requests: Arc<RwLock<HashMap<DataKey, Vec<SubscriberId>>>>,
    
    // 订阅者注册表
    subscribers: Arc<RwLock<HashMap<SubscriberId, Box<dyn DataSubscriber>>>>,
}

impl DataManager {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            pending_requests: Arc::new(RwLock::new(HashMap::new())),
            subscribers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 注册订阅者
    pub fn register_subscriber(&self, subscriber: Box<dyn DataSubscriber>) {
        let id = subscriber.subscriber_id();
        self.subscribers.write().unwrap().insert(id, subscriber);
    }

    /// 注销订阅者
    pub fn unregister_subscriber(&self, id: SubscriberId) {
        self.subscribers.write().unwrap().remove(&id);
        
        // 清理缓存中的订阅关系
        let mut cache = self.cache.write().unwrap();
        cache.values_mut().for_each(|cached| {
            cached.subscribers.retain(|&sid| sid != id);
        });
        
        // 移除没有订阅者的缓存项
        cache.retain(|_, cached| !cached.subscribers.is_empty());
    }

    /// 请求数据：如果已缓存则直接返回，否则发起请求
    pub fn request_data(
        &self,
        key: DataKey,
        subscriber_id: SubscriberId,
    ) -> Option<Arc<FetchedData>> {
        // 1. 检查缓存
        {
            let cache = self.cache.read().unwrap();
            if let Some(cached) = cache.get(&key) {
                // 更新访问时间
                drop(cache);
                let mut cache = self.cache.write().unwrap();
                if let Some(cached) = cache.get_mut(&key) {
                    cached.last_accessed = Instant::now();
                    if !cached.subscribers.contains(&subscriber_id) {
                        cached.subscribers.push(subscriber_id);
                    }
                    return Some(Arc::new(cached.data.clone()));
                }
            }
        }

        // 2. 检查是否有进行中的请求
        {
            let mut pending = self.pending_requests.write().unwrap();
            if let Some(subscribers) = pending.get_mut(&key) {
                // 已有请求在进行，只需添加订阅者
                if !subscribers.contains(&subscriber_id) {
                    subscribers.push(subscriber_id);
                }
                return None; // 等待请求完成
            } else {
                // 新请求，记录订阅者
                pending.insert(key.clone(), vec![subscriber_id]);
            }
        }

        // 3. 返回 None，表示需要发起新请求
        None
    }

    /// 数据获取完成：更新缓存并通知所有订阅者
    pub fn on_data_fetched(&self, key: DataKey, data: FetchedData) {
        let subscribers = {
            let mut pending = self.pending_requests.write().unwrap();
            pending.remove(&key).unwrap_or_default()
        };

        // 更新缓存
        {
            let mut cache = self.cache.write().unwrap();
            cache.insert(key.clone(), CachedData {
                data: data.clone(),
                subscribers: subscribers.clone(),
                last_accessed: Instant::now(),
                fetch_time: Instant::now(),
            });
        }

        // 通知所有订阅者
        let subscribers_map = self.subscribers.read().unwrap();
        for subscriber_id in subscribers {
            if let Some(subscriber) = subscribers_map.get(&subscriber_id) {
                // 注意：这里需要可变引用，实际实现中可能需要使用内部可变性
                // 或者使用消息通道来通知
                // subscriber.on_data_updated(&key, &data);
            }
        }
    }

    /// 清理过期缓存
    pub fn cleanup_expired(&self, max_age: std::time::Duration) {
        let now = Instant::now();
        let mut cache = self.cache.write().unwrap();
        cache.retain(|_, cached| {
            now.duration_since(cached.last_accessed) < max_age
        });
    }
}

impl Default for DataManager {
    fn default() -> Self {
        Self::new()
    }
}

