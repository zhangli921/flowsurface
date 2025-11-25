# 架构改进方案：统一交易所通信接口

## 一、当前架构分析

### 1.1 当前架构问题

**问题 1：职责不清晰**
```
Exchange Crate:
  ✅ WebSocket 实时数据流
  ✅ REST API（需要 API Key）
  ❌ Binance Data Vision（历史数据，公开）→ 在 data crate

Data Crate:
  ✅ 数据存储（Mmap + Parquet）
  ✅ 数据服务（统一接口）
  ❌ Binance Data Vision 下载 → 应该是 exchange 的职责
```

**问题 2：依赖关系混乱**
- `data` crate 直接调用 Binance Data Vision API
- 但 Binance Data Vision 本质上是交易所通信
- 应该通过 `exchange` crate 统一管理

**问题 3：模块化不足**
- 交易所通信分散在两个 crate
- 难以统一管理速率限制
- 难以统一错误处理

### 1.2 当前数据流

```
┌─────────────────────────────────────────┐
│         Exchange Crate                   │
│  - WebSocket 实时数据流                  │
│  - REST API（需要 API Key）              │
└──────────────┬──────────────────────────┘
               │
               ▼
┌─────────────────────────────────────────┐
│         Data Crate                       │
│  - RealtimeIngesterService               │
│    └─→ 写入 Mmap                         │
│                                          │
│  - HistoricalDownloadExecutor            │
│    └─→ 直接调用 Binance Data Vision ❌   │
│    └─→ 写入 Parquet                      │
└─────────────────────────────────────────┘
```

## 二、改进方案

### 2.1 目标架构

```
┌─────────────────────────────────────────┐
│         Exchange Crate                   │
│  (所有交易所通信的统一接口)                │
│                                          │
│  ┌────────────────────────────────────┐  │
│  │  ExchangeAdapter (Trait)          │  │
│  │  - connect_websocket()              │  │
│  │  - fetch_realtime_data()           │  │
│  │  - fetch_historical_data()         │  │
│  └────────────────────────────────────┘  │
│                                          │
│  ┌────────────────────────────────────┐  │
│  │  BinanceAdapter                    │  │
│  │  - WebSocket (实时)                 │  │
│  │  - REST API (实时，需要 API Key)   │  │
│  │  - Data Vision (历史，公开数据)     │  │
│  └────────────────────────────────────┘  │
│                                          │
│  ┌────────────────────────────────────┐  │
│  │  UnifiedRateLimiter                │  │
│  │  - 统一速率限制管理                  │  │
│  └────────────────────────────────────┘  │
└──────────────┬──────────────────────────┘
               │
               ▼
┌─────────────────────────────────────────┐
│         Data Crate                       │
│  (数据存储和管理层)                        │
│                                          │
│  - RealtimeIngesterService               │
│    └─→ 通过 ExchangeAdapter 获取数据      │
│    └─→ 写入 Mmap                         │
│                                          │
│  - HistoricalDataService                  │
│    └─→ 通过 ExchangeAdapter 获取数据      │
│    └─→ 写入 Parquet                      │
│                                          │
│  - UnifiedDataService                    │
│    └─→ 统一数据访问接口                   │
└─────────────────────────────────────────┘
```

### 2.2 核心改进点

#### 改进 1：统一交易所通信接口

**在 Exchange Crate 中定义 Trait**：
```rust
// exchange/src/adapter.rs

pub trait ExchangeAdapter: Send + Sync {
    /// 连接 WebSocket 实时数据流
    async fn connect_websocket(&self, symbol: &str) -> Result<WebSocketStream, AdapterError>;
    
    /// 获取实时数据（REST API，需要 API Key）
    async fn fetch_realtime_data(&self, symbol: &str, range: TimeRange) -> Result<Vec<Trade>, AdapterError>;
    
    /// 获取历史数据（公开数据，如 Binance Data Vision）
    async fn fetch_historical_data(
        &self,
        symbol: &str,
        date: &str,
        data_type: HistoricalDataType,
    ) -> Result<HistoricalData, AdapterError>;
    
    /// 获取速率限制器
    fn rate_limiter(&self) -> &dyn RateLimiter;
}
```

#### 改进 2：Binance 适配器扩展

**在 `exchange/src/adapter/binance.rs` 中添加历史数据下载**：
```rust
impl ExchangeAdapter for BinanceAdapter {
    async fn fetch_historical_data(
        &self,
        symbol: &str,
        date: &str,
        data_type: HistoricalDataType,
    ) -> Result<HistoricalData, AdapterError> {
        match data_type {
            HistoricalDataType::Kline { timeframe } => {
                self.download_kline_from_data_vision(symbol, date, timeframe).await
            }
            HistoricalDataType::Tick => {
                self.download_ticks_from_data_vision(symbol, date).await
            }
        }
    }
}
```

#### 改进 3：Data Crate 通过接口获取数据

**修改 `HistoricalDownloadExecutor`**：
```rust
// data/src/historical_download_executor.rs

pub struct HistoricalDownloadExecutor {
    exchange_adapter: Arc<dyn ExchangeAdapter>,  // 通过接口获取数据
    cache_dir: PathBuf,
    downloading: Arc<Mutex<HashSet<String>>>,
}

impl HistoricalDownloadExecutor {
    pub async fn download_and_cache_kline(
        &self,
        symbol: &str,
        date: &str,
        timeframe: &str,
    ) -> Result<Vec<KLine>, DataError> {
        // 通过 exchange adapter 获取数据
        let klines = self.exchange_adapter
            .fetch_historical_data(
                symbol,
                date,
                HistoricalDataType::Kline { timeframe: timeframe.to_string() },
            )
            .await?;
        
        // 写入缓存
        self.save_klines_to_cache(&cache_path, &klines).await?;
        Ok(klines)
    }
}
```

## 三、详细设计方案

### 3.1 Exchange Crate 扩展

#### 新增模块：`exchange/src/historical.rs`

```rust
//! Historical data fetching from exchanges.
//!
//! This module provides interfaces for fetching historical data
//! from various exchange data sources (e.g., Binance Data Vision).

use crate::adapter::AdapterError;

/// Type of historical data to fetch.
#[derive(Debug, Clone)]
pub enum HistoricalDataType {
    /// K-line data with specific timeframe.
    Kline { timeframe: String },
    /// Tick/trade data.
    Tick,
}

/// Historical data result.
pub enum HistoricalData {
    Klines(Vec<crate::Kline>),
    Ticks(Vec<crate::Trade>),
}
```

#### 扩展 `exchange/src/adapter.rs`

```rust
pub trait ExchangeAdapter: Send + Sync {
    // ... existing methods ...
    
    /// Fetches historical data from exchange data sources.
    ///
    /// This includes public historical data (e.g., Binance Data Vision)
    /// that doesn't require API Key authentication.
    async fn fetch_historical_data(
        &self,
        symbol: &str,
        date: &str,  // Format: "YYYY-MM-DD"
        data_type: HistoricalDataType,
    ) -> Result<HistoricalData, AdapterError>;
}
```

#### 扩展 `exchange/src/adapter/binance.rs`

```rust
impl ExchangeAdapter for BinanceAdapter {
    async fn fetch_historical_data(
        &self,
        symbol: &str,
        date: &str,
        data_type: HistoricalDataType,
    ) -> Result<HistoricalData, AdapterError> {
        match data_type {
            HistoricalDataType::Kline { timeframe } => {
                let klines = self.download_kline_from_data_vision(symbol, date, &timeframe).await?;
                Ok(HistoricalData::Klines(klines))
            }
            HistoricalDataType::Tick => {
                let ticks = self.download_ticks_from_data_vision(symbol, date).await?;
                Ok(HistoricalData::Ticks(ticks))
            }
        }
    }
    
    // 将现有的 download_kline_for_date 和 download_ticks_for_date
    // 从 data crate 移到这里
}
```

### 3.2 Data Crate 重构

#### 修改 `HistoricalDownloadExecutor`

```rust
// data/src/historical_download_executor.rs

use exchange::adapter::ExchangeAdapter;
use exchange::historical::{HistoricalData, HistoricalDataType};

pub struct HistoricalDownloadExecutor {
    exchange_adapter: Arc<dyn ExchangeAdapter>,  // 通过接口获取数据
    cache_dir: PathBuf,
    downloading: Arc<Mutex<HashSet<String>>>,
}

impl HistoricalDownloadExecutor {
    pub fn new(
        exchange_adapter: Arc<dyn ExchangeAdapter>,
        cache_dir: Option<PathBuf>,
    ) -> Self {
        let cache_dir = cache_dir.unwrap_or_else(|| data_path(Some("cache")));
        Self {
            exchange_adapter,
            cache_dir,
            downloading: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub async fn download_and_cache_kline(
        &self,
        symbol: &str,
        date: &str,
        timeframe: &str,
    ) -> Result<Vec<KLine>, DataError> {
        // 检查缓存
        let cache_path = self.cache_dir.join(format!("{}_{}_{}.parquet", symbol, date, timeframe));
        if cache_path.exists() {
            return self.load_klines_from_cache(&cache_path).await;
        }

        // 通过 exchange adapter 获取数据
        let historical_data = self.exchange_adapter
            .fetch_historical_data(
                symbol,
                date,
                HistoricalDataType::Kline { timeframe: timeframe.to_string() },
            )
            .await
            .map_err(|e| DataError::Network(e.into()))?;

        let klines = match historical_data {
            HistoricalData::Klines(klines) => klines,
            _ => return Err(DataError::InvalidInput("Unexpected data type")),
        };

        // 写入缓存
        if !klines.is_empty() {
            self.save_klines_to_cache(&cache_path, &klines).await?;
        }

        Ok(klines)
    }

    // 类似地修改 download_and_cache_ticks
}
```

### 3.3 统一速率限制

#### 在 Exchange Crate 中统一管理

```rust
// exchange/src/limiter.rs

pub trait UnifiedRateLimiter: Send + Sync {
    /// 准备请求（实时 API）
    fn prepare_realtime_request(&mut self, weight: usize) -> Option<Duration>;
    
    /// 准备请求（历史数据下载）
    fn prepare_historical_request(&mut self) -> Option<Duration>;
    
    /// 更新速率限制状态
    fn update_from_response(&mut self, response: &Response, weight: Option<usize>);
}
```

## 四、实施步骤

### 阶段 1：扩展 Exchange Crate

1. ✅ 创建 `exchange/src/historical.rs` 模块
2. ✅ 在 `ExchangeAdapter` trait 中添加 `fetch_historical_data` 方法
3. ✅ 在 `BinanceAdapter` 中实现历史数据下载方法
4. ✅ 将 Binance Data Vision 下载逻辑从 `data` crate 移到 `exchange` crate

### 阶段 2：重构 Data Crate

1. ✅ 修改 `HistoricalDownloadExecutor` 使用 `ExchangeAdapter` 接口
2. ✅ 移除 `data` crate 中直接调用 Binance Data Vision 的代码
3. ✅ 更新依赖关系（`data` crate 依赖 `exchange` crate）

### 阶段 3：统一速率限制

1. ✅ 在 `exchange` crate 中统一速率限制管理
2. ✅ 移除 `data` crate 中的速率限制器（如果已创建）

## 五、优势分析

### 5.1 架构优势

- ✅ **职责清晰**：
  - Exchange = 所有交易所通信
  - Data = 数据存储和管理

- ✅ **模块化强**：
  - 所有交易所通信统一接口
  - 易于添加新的交易所
  - 易于统一管理速率限制

- ✅ **依赖清晰**：
  - Data 依赖 Exchange（合理）
  - Exchange 独立（不依赖 Data）

### 5.2 代码质量

- ✅ **单一职责**：每个 crate 职责明确
- ✅ **接口抽象**：通过 trait 实现解耦
- ✅ **易于测试**：可以 mock ExchangeAdapter
- ✅ **易于扩展**：添加新交易所只需实现 trait

### 5.3 维护性

- ✅ **统一管理**：所有交易所通信在一个地方
- ✅ **统一错误处理**：通过 AdapterError
- ✅ **统一速率限制**：在一个地方管理
- ✅ **统一日志**：交易所通信日志集中

## 六、潜在挑战

### 6.1 依赖关系

**当前**：
- `data` crate 不依赖 `exchange` crate

**改进后**：
- `data` crate 依赖 `exchange` crate

**评估**：
- ✅ **合理**：数据层依赖接口层是正常的架构模式
- ✅ **不会循环依赖**：Exchange 不依赖 Data

### 6.2 Binance Data Vision 的特殊性

**考虑**：
- Binance Data Vision 是公开数据，不需要 API Key
- 但仍然是 Binance 的接口
- 应该统一到 BinanceAdapter 中

**方案**：
- 在 `BinanceAdapter` 中实现 `fetch_historical_data`
- 使用统一的速率限制器
- 保持接口一致性

## 七、推荐方案

### 7.1 实施优先级

**P0（立即实施）**：
1. ✅ 扩展 `ExchangeAdapter` trait，添加历史数据接口
2. ✅ 在 `BinanceAdapter` 中实现历史数据下载
3. ✅ 修改 `HistoricalDownloadExecutor` 使用接口

**P1（本周实施）**：
4. ✅ 统一速率限制管理
5. ✅ 统一错误处理

**P2（可选）**：
6. ✅ 重构其他交易所适配器（如果需要历史数据）

### 7.2 实施建议

**建议采用此架构改进**，因为：

1. ✅ **更优雅**：职责清晰，模块化强
2. ✅ **更易维护**：所有交易所通信统一管理
3. ✅ **更易扩展**：添加新交易所或数据源更容易
4. ✅ **更符合设计原则**：单一职责、依赖倒置

## 八、总结

当前架构确实存在改进空间：

1. ❌ **职责不清晰**：Binance Data Vision 在 data crate，但应该是 exchange 的职责
2. ❌ **模块化不足**：交易所通信分散在两个 crate
3. ❌ **依赖关系混乱**：data 直接调用交易所 API

**改进后的架构**：

1. ✅ **职责清晰**：Exchange = 所有交易所通信，Data = 数据存储
2. ✅ **模块化强**：统一接口，易于扩展
3. ✅ **依赖清晰**：Data 依赖 Exchange（合理）

**建议立即实施此改进**，这将使架构更加优雅和可维护。

