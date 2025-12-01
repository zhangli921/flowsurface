//! 统一数据管理器：全局数据获取、缓存和共享
//! 
//! 这个模块实现了统一的数据管理架构，支持：
//! - 全局请求去重
//! - 数据缓存和共享
//! - 订阅者管理
//! 
//! 注意：这是新架构的基础设施，默认不启用，不影响现有功能

use exchange::{TickerInfo, Trade, Kline};
use exchange::fetcher::{FetchRange, FetchedData};
use data::chart::Basis;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use std::time::Instant;

/// 数据键：唯一标识一组数据
#[derive(Debug, Clone, PartialEq)]
pub struct DataKey {
    pub ticker: TickerInfo,
    pub range: FetchRange,
    pub basis: Basis,
}

impl std::hash::Hash for DataKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // 手动实现 Hash，因为 FetchRange 和 Basis 可能没有实现 Hash
        match self.range {
            FetchRange::Kline(from, to) => {
                state.write_u64(from);
                state.write_u64(to);
                state.write_u8(0);
            }
            FetchRange::OpenInterest(from, to) => {
                state.write_u64(from);
                state.write_u64(to);
                state.write_u8(1);
            }
            FetchRange::Trades(from, to) => {
                state.write_u64(from);
                state.write_u64(to);
                state.write_u8(2);
            }
        }
        // TickerInfo 和 Basis 需要实现 Hash 或手动处理
        // 简化：使用 ticker 的字符串表示
        format!("{:?}", self.ticker).hash(state);
        format!("{:?}", self.basis).hash(state);
    }
}

impl Eq for DataKey {}

impl DataKey {
    pub fn new(ticker: TickerInfo, range: FetchRange, basis: Basis) -> Self {
        Self { ticker, range, basis }
    }
}

/// 订阅者 ID
pub type SubscriberId = Uuid;

/// 数据需求描述
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRequirements {
    pub needs_klines: bool,
    pub needs_trades: bool,
    pub needs_depth: bool,
    pub needs_open_interest: bool,
    pub supports_historical: bool,
    pub supports_tick_basis: bool,
}

impl Default for DataRequirements {
    fn default() -> Self {
        Self {
            needs_klines: false,
            needs_trades: false,
            needs_depth: false,
            needs_open_interest: false,
            supports_historical: false,
            supports_tick_basis: false,
        }
    }
}

/// 请求结果
#[derive(Debug, Clone)]
pub enum RequestResult {
    /// 数据已缓存，直接返回
    Cached(Arc<FetchedData>),
    /// 请求已在进行中，等待完成
    Pending,
    /// 需要发起新请求
    NewRequest(DataKey),
}

/// 数据缓存项
struct CachedData {
    data: Arc<FetchedData>,
    subscribers: Vec<SubscriberId>,
    last_accessed: Instant,
    fetch_time: Instant,
}

/// 全局请求去重器
struct GlobalRequestDeduplicator {
    pending: Arc<RwLock<HashMap<DataKey, Vec<SubscriberId>>>>,
}

impl GlobalRequestDeduplicator {
    fn new() -> Self {
        Self {
            pending: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    fn check_pending(&self, key: &DataKey) -> Option<Vec<SubscriberId>> {
        self.pending.read().unwrap().get(key).cloned()
    }
    
    fn register_request(&self, key: DataKey, subscriber: SubscriberId) {
        let mut pending = self.pending.write().unwrap();
        pending.entry(key).or_insert_with(Vec::new).push(subscriber);
    }
    
    fn get_subscribers(&self, key: &DataKey) -> Vec<SubscriberId> {
        self.pending.read().unwrap().get(key).cloned().unwrap_or_default()
    }
    
    fn remove_request(&self, key: &DataKey) {
        self.pending.write().unwrap().remove(key);
    }
}

/// 统一数据管理器
pub struct UnifiedDataManager {
    // 原始数据缓存
    raw_trades_cache: Arc<RwLock<HashMap<DataKey, Arc<Vec<Trade>>>>>,
    raw_klines_cache: Arc<RwLock<HashMap<DataKey, Arc<Vec<Kline>>>>>,
    
    // 请求去重
    deduplicator: GlobalRequestDeduplicator,
    
    // 订阅者注册表
    subscribers: Arc<RwLock<HashMap<SubscriberId, DataRequirements>>>,
    
    // 最大缓存大小
    max_cache_size: usize,
}

impl UnifiedDataManager {
    pub fn new(max_cache_size: usize) -> Self {
        Self {
            raw_trades_cache: Arc::new(RwLock::new(HashMap::new())),
            raw_klines_cache: Arc::new(RwLock::new(HashMap::new())),
            deduplicator: GlobalRequestDeduplicator::new(),
            subscribers: Arc::new(RwLock::new(HashMap::new())),
            max_cache_size,
        }
    }
    
    /// 注册订阅者
    pub fn register_subscriber(&self, id: SubscriberId, requirements: DataRequirements) {
        self.subscribers.write().unwrap().insert(id, requirements);
    }
    
    /// 注销订阅者
    pub fn unregister_subscriber(&self, id: SubscriberId) {
        self.subscribers.write().unwrap().remove(&id);
    }
    
    /// 请求数据（统一入口）
    pub fn request_data(
        &self,
        key: DataKey,
        subscriber: SubscriberId,
        requirements: &DataRequirements,
    ) -> RequestResult {
        // 1. 检查缓存
        if requirements.needs_trades {
            if let Some(cached) = self.raw_trades_cache.read().unwrap().get(&key) {
                return RequestResult::Cached(Arc::new(FetchedData::Trades {
                    batch: (**cached).clone(),
                    until_time: match key.range {
                        FetchRange::Trades(_, to) => to,
                        _ => 0,
                    },
                }));
            }
        }
        
        if requirements.needs_klines {
            if let Some(cached) = self.raw_klines_cache.read().unwrap().get(&key) {
                return RequestResult::Cached(Arc::new(FetchedData::Klines {
                    data: (**cached).clone(),
                    req_id: None,
                }));
            }
        }
        
        // 2. 检查是否有进行中的请求（全局去重）
        if let Some(subscribers) = self.deduplicator.check_pending(&key) {
            // 添加到订阅列表
            self.deduplicator.register_request(key.clone(), subscriber);
            return RequestResult::Pending;
        }
        
        // 3. 创建新请求
        self.deduplicator.register_request(key.clone(), subscriber);
        RequestResult::NewRequest(key)
    }
    
    /// 数据到达后更新缓存并返回订阅者列表
    pub fn on_data_fetched(&self, key: DataKey, data: FetchedData) -> Vec<SubscriberId> {
        // 1. 更新缓存
        match &data {
            FetchedData::Trades { batch, .. } => {
                let mut cache = self.raw_trades_cache.write().unwrap();
                cache.insert(key.clone(), Arc::new(batch.clone()));
                
                // 清理过期缓存
                if cache.len() > self.max_cache_size {
                    self.cleanup_cache(&mut cache);
                }
            }
            FetchedData::Klines { data, .. } => {
                let mut cache = self.raw_klines_cache.write().unwrap();
                cache.insert(key.clone(), Arc::new(data.clone()));
                
                // 清理过期缓存
                if cache.len() > self.max_cache_size {
                    self.cleanup_cache(&mut cache);
                }
            }
            _ => {}
        }
        
        // 2. 获取所有订阅者
        let subscribers = self.deduplicator.get_subscribers(&key);
        
        // 3. 移除请求记录
        self.deduplicator.remove_request(&key);
        
        subscribers
    }
    
    /// 清理缓存：移除最久未访问的项
    fn cleanup_cache<T>(&self, cache: &mut HashMap<DataKey, Arc<T>>) {
        // 简单实现：移除最旧的 10%
        let to_remove = cache.len() / 10;
        if to_remove == 0 {
            return;
        }
        
        // 收集所有键
        let keys: Vec<DataKey> = cache.keys().cloned().collect();
        
        // 移除前 to_remove 个（简化实现）
        for key in keys.into_iter().take(to_remove) {
            cache.remove(&key);
        }
    }
}

impl Default for UnifiedDataManager {
    fn default() -> Self {
        Self::new(1000)  // 默认最大缓存 1000 项
    }
}

