# 数据架构设计规范

## 文档说明

本文档是基于 `data_architecture_refactor_plan.md` 整理的统一设计规范，用于指导具体程序设计与实现。

**目标读者**：程序员、架构师、测试人员

**文档目的**：提供清晰、完整、可执行的设计规范，确保实现的一致性

---

## 一、架构概述

### 1.1 设计目标

1. **解决当前问题**：
   - 消除数据仲裁服务的复杂性
   - 明确组件职责，避免职责混乱
   - 确保数据源选择的确定性

2. **架构原则**：
   - **Lambda 架构**：分离 Speed Layer（实时）和 Batch Layer（历史）
   - **不物理合并数据**：数据源选择在统一服务中完成
   - **关注点分离**：渲染层不需要知道数据来源
   - **动态时间边界**：根据历史数据发布延迟动态调整

3. **性能目标**：
   - 实时数据性能：保持原有高性能（Mmap 零拷贝，~1-10ms）
   - GPU 计算性能：完全保持不变（~10-50ms）
   - 历史数据性能：通过缓存接近原有性能（~1-10ms）

### 1.2 架构层次

```
┌─────────────────────────────────────────────────────────────┐
│                    UI Layer (Rendering)                      │
│  - 只关心如何渲染数据                                        │
│  - 不需要知道数据来源                                        │
│  - 可选：显示数据状态（通过元数据）                          │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼ (只传递时间范围)
        ┌───────────────────────────────────────┐
        │   UnifiedDataService (统一接口)        │
        │  - 自动选择数据源（根据时间范围）       │
        │  - 封装数据源选择逻辑                  │
        │  - 返回数据和元数据                    │
        │  ┌──────────────┐  ┌──────────────┐  │
        │  │ Speed Layer  │  │ Batch Layer  │  │
        │  │ (实时数据)    │  │ (历史数据)    │  │
        │  └──────────────┘  └──────────────┘  │
        └───────────────────────────────────────┘
                            │
        ┌───────────────────┴───────────────────┐
        │                                       │
        ▼                                       ▼
┌──────────────────────┐          ┌──────────────────────┐
│ RealtimeDataService  │          │ HistoricalDataService│
│ (实时数据读取)        │          │ (历史数据读取)        │
│                      │          │                      │
│ - 从 Mmap 读取       │          │ - 从缓存读取          │
│ - 聚合 K 线          │          │ - 自动触发下载        │
└──────────────────────┘          └──────────────────────┘
        ▲                                       ▲
        │                                       │
        │                                       │
┌──────────────────────┐          ┌──────────────────────┐
│RealtimeIngesterService│          │HistoricalIngesterService│
│ (实时数据写入)        │          │ (历史数据写入)        │
│                      │          │                      │
│ - WebSocket 接收     │          │ - 从 Binance         │
│ - 写入 Mmap          │          │   Data Vision 下载   │
│                      │          │ - 写入 Parquet 缓存  │
└──────────────────────┘          └──────────────────────┘
        │                                       │
        ▼                                       ▼
┌──────────────────────┐          ┌──────────────────────┐
│  market_data/        │          │  cache/              │
│  {SYMBOL}/           │          │  {DATE}-*.parquet    │
│  realtime.mmap       │          │                      │
└──────────────────────┘          └──────────────────────┘
```

### 1.3 核心概念

#### 1.3.1 动态时间边界（safe_cutoff）

**定义**：安全的历史数据截止时间，作为实时数据和历史数据的分界点

**计算逻辑**：
```rust
/// 计算"安全的历史数据截止时间"
/// 考虑到 Binance Data Vision 的数据发布延迟（通常 2-6 小时）
fn calculate_safe_historical_cutoff() -> u64 {
    let now = Utc::now();
    let today = now.date_naive();
    let midnight = today.and_hms_opt(0, 0, 0).unwrap();
    let cutoff = midnight.and_utc().timestamp_micros() as u64;
    
    // 如果当前时间距离 UTC 0点不足 6 小时，使用前一天的边界
    let hours_since_midnight = (now.timestamp_micros() as u64 - cutoff) / 3_600_000_000;
    if hours_since_midnight < 6 {
        // 使用前一天的边界（历史数据可能还未发布）
        let yesterday = today - chrono::Duration::days(1);
        let yesterday_midnight = yesterday.and_hms_opt(0, 0, 0).unwrap();
        yesterday_midnight.and_utc().timestamp_micros() as u64
    } else {
        // 使用当天的边界（历史数据应该已经发布）
        cutoff
    }
}
```

**时间周期示例**：

| 当前时间 | 安全截止时间 | 实时数据时间周期 | 说明 |
|---------|------------|----------------|------|
| UTC 01:00 | 昨天 00:00 | 25小时 | 历史数据未发布，实时数据需要保存更长时间 |
| UTC 03:00 | 昨天 00:00 | 27小时 | 历史数据可能还未发布 |
| UTC 06:00 | 今天 00:00 | 6小时 | 历史数据应该已发布，实时数据只需要保存当天数据 |
| UTC 12:00 | 今天 00:00 | 12小时 | 历史数据已发布，实时数据只需要保存当天数据 |

**查询规则**：
- 如果查询时间 **>= safe_cutoff**：使用实时数据（从 Mmap 读取）
- 如果查询时间 **< safe_cutoff**：使用历史数据（从 Binance Data Vision 下载）

#### 1.3.2 数据源选择

**三种场景**：

1. **纯实时数据**（`range.start_us >= safe_cutoff`）：
   - 数据源：Speed Layer (Mmap)
   - 操作：直接读取

2. **纯历史数据**（`range.end_us < safe_cutoff`）：
   - 数据源：Batch Layer (Binance Data Vision)
   - 操作：下载或从缓存读取

3. **跨边界数据**（`range.start_us < safe_cutoff && range.end_us >= safe_cutoff`）：
   - 数据源：Batch Layer + Speed Layer
   - 操作：分别获取并合并

---

## 二、核心组件设计

### 2.1 UnifiedDataService（统一数据服务）

**职责**：提供统一的数据访问接口，自动选择数据源

**位置**：`flowsurface/data/src/unified_data_service.rs`

**接口定义**：

```rust
pub struct UnifiedDataService {
    speed_layer: Arc<RealtimeDataService>,      // 实时数据读取服务（Mmap 访问）
    batch_layer: Arc<HistoricalDataService>,    // 历史数据读取服务（缓存访问）
}

impl UnifiedDataService {
    /// 获取 K-line 数据（自动选择数据源）
    /// 
    /// # 参数
    /// - `symbol`: 交易对标识（如 "BTCUSDT"）
    /// - `range`: 时间范围（微秒）
    /// - `timeframe`: 周期（如 M1, M5, M15）
    /// 
    /// # 返回
    /// - `Ok(Vec<KLine>)`: K线数据
    /// - `Err(ArbiterError)`: 错误信息
    pub async fn fetch_klines(
        &self,
        symbol: String,
        range: TimeRange,
        timeframe: Timeframe,
    ) -> Result<Vec<KLine>, ArbiterError> {
        let safe_cutoff = calculate_safe_historical_cutoff();
        
        if range.start_us >= safe_cutoff {
            // 纯实时数据：从Mmap读取Tick，聚合为K线
            tokio::task::spawn_blocking({
                let service = self.speed_layer.clone();
                move || service.fetch_kline_blocking(range, timeframe)
            }).await?
        } else if range.end_us < safe_cutoff {
            // 纯历史数据：从缓存读取（自动触发下载如果未命中）
            self.batch_layer.fetch_kline(
                &symbol,
                range,
                timeframe,
            ).await
        } else {
            // 跨边界：分别获取并拼接
            let (historical, realtime) = tokio::try_join!(
                self.batch_layer.fetch_kline(
                    &symbol,
                    TimeRange {
                        start_us: range.start_us,
                        end_us: safe_cutoff,
                    },
                    timeframe,
                ),
                tokio::task::spawn_blocking({
                    let service = self.speed_layer.clone();
                    move || service.fetch_kline_blocking(TimeRange {
                        start_us: safe_cutoff,
                        end_us: range.end_us,
                    }, timeframe)
                })
            )?;
            
            // 拼接（历史数据在前，实时数据在后）
            let mut result = historical?;
            result.extend(realtime?);
            Ok(result)
        }
    }
    
    /// 获取 Tick 数据（支持实时和历史，包括跨边界情况）
    /// 
    /// # 参数
    /// - `symbol`: 交易对标识
    /// - `range`: 时间范围（微秒）
    /// 
    /// # 返回
    /// - `Ok(TickDataBuffer)`: Tick数据
    /// - `Err(ArbiterError)`: 错误信息
    pub async fn fetch_ticks(
        &self,
        symbol: String,
        range: TimeRange,
    ) -> Result<TickDataBuffer, ArbiterError> {
        let safe_cutoff = calculate_safe_historical_cutoff();
        
        if range.start_us >= safe_cutoff {
            // 纯实时数据范围（>= safe_cutoff）
            tokio::task::spawn_blocking({
                let service = self.speed_layer.clone();
                move || service.fetch_ticks_blocking(range)
            }).await?
        } else if range.end_us < safe_cutoff {
            // 纯历史数据范围（< safe_cutoff）
            self.batch_layer.fetch_ticks(
                &symbol,
                range,
            ).await
        } else {
            // 跨边界情况：查询范围跨越了 safe_cutoff
            let (historical_result, realtime_result) = tokio::try_join!(
                // 获取历史部分（< safe_cutoff）
                self.batch_layer.fetch_ticks(
                    &symbol,
                    TimeRange {
                        start_us: range.start_us,
                        end_us: safe_cutoff,
                    },
                ),
                // 获取实时部分（>= safe_cutoff）
                tokio::task::spawn_blocking({
                    let service = self.speed_layer.clone();
                    move || service.fetch_ticks_blocking(TimeRange {
                        start_us: safe_cutoff,
                        end_us: range.end_us,
                    })
                })
            )?;
            
            // 合并历史数据和实时数据
            let historical = historical_result?;
            let realtime = realtime_result?;
            
            // 合并 TickDataBuffer
            // 注意：prices 和 volumes 必须保持一一对应关系
            // 历史数据在前（时间更早），实时数据在后（时间更晚）
            Ok(TickDataBuffer {
                prices: {
                    let mut p = historical.prices;
                    p.extend(realtime.prices);
                    p
                },
                volumes: {
                    let mut v = historical.volumes;
                    v.extend(realtime.volumes);
                    v
                },
            })
        }
    }
}
```

**关键设计原则**：
- ✅ 自动选择数据源，上层不需要知道数据来源
- ✅ 支持跨边界查询，自动合并数据
- ✅ 统一错误处理
- ✅ 支持缓存（可选）

### 2.2 RealtimeIngesterService（实时数据写入服务）

**职责**：只负责实时数据流（WebSocket），写入 Mmap

**位置**：`flowsurface/data/src/realtime_ingester.rs`（原 `ingester.rs` 重命名）

**命名说明**：为了与历史数据的 `HistoricalIngesterService` 对称，将 `IngesterService` 重命名为 `RealtimeIngesterService`

**接口定义**：

```rust
pub struct RealtimeIngesterService {
    data_dir: PathBuf,
    active_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    command_tx: mpsc::Sender<IngestCommand>,
}

pub enum IngestCommand {
    Subscribe(String),  // 订阅币种
    Unsubscribe(String), // 取消订阅
}

impl RealtimeIngesterService {
    /// 运行主循环，处理命令
    pub async fn run(&self) {
        let mut command_rx = self.command_rx.clone();
        
        while let Some(cmd) = command_rx.recv().await {
            match cmd {
                IngestCommand::Subscribe(symbol) => {
                    self.start_ingest_task(symbol).await;
                }
                IngestCommand::Unsubscribe(symbol) => {
                    self.stop_ingest_task(&symbol).await;
                }
            }
        }
    }
    
    /// 启动单个币种的数据流
    async fn start_ingest_task(&self, symbol: String) {
        // 1. 计算安全的历史数据截止时间（动态边界）
        let safe_cutoff = calculate_safe_historical_cutoff();
        
        // 2. 创建币种特定的Mmap文件
        let normalized_symbol = normalize_binance_symbol(&symbol);
        let mmap_path = self.data_dir.join(&normalized_symbol).join("realtime.mmap");
        let mut writer = MmapWriter::open_or_create(&mmap_path).await?;
        
        // 3. 连接WebSocket实时数据流
        let ws_url = format!("wss://stream.binance.com:9443/ws/{}@aggTrade", normalized_symbol);
        let (mut ws_stream, _) = connect_async(&ws_url).await?;
        
        // 4. 处理实时数据流
        while let Some(message) = ws_stream.next().await {
            let trade = parse_websocket_message(message)?;
            
            // 5. 写入 >= safe_cutoff 的数据（动态时间周期）
            // 注意：safe_cutoff 可能小于 today_start（如果历史数据未发布）
            // 这意味着实时数据可能包含超过24小时的数据
            if trade.time >= safe_cutoff {
                writer.append_chunk(&[trade], trade.time).await?;
            }
        }
    }
    
    /// 确保Mmap文件存在（同步创建）
    pub fn ensure_mmap_file(symbol: &str, data_dir: &Path) {
        let normalized_symbol = normalize_binance_symbol(symbol);
        let mmap_path = data_dir.join(&normalized_symbol).join("realtime.mmap");
        
        if !mmap_path.exists() {
            // 创建空的Mmap文件
            MmapWriter::create_empty(&mmap_path).unwrap();
        }
    }
}
```

**关键设计原则**：
- ✅ 只处理实时数据，不处理历史数据
- ✅ 写入动态时间周期的数据（可能超过24小时）
- ✅ 支持多币种并发
- ✅ 自动重连机制

### 2.3 HistoricalIngesterService（历史数据写入服务）

**职责**：从 Binance Data Vision 下载历史数据，写入本地缓存

**位置**：`flowsurface/data/src/historical_ingester.rs`（新增）

**接口定义**：

```rust
pub struct HistoricalIngesterService {
    client: reqwest::Client,
    cache_dir: PathBuf,
}

impl HistoricalIngesterService {
    /// 保存K线数据到缓存
    async fn save_klines_to_cache(
        &self,
        cache_path: &Path,
        klines: &[KLine],
    ) -> Result<(), ArbiterError> {
        // 1. 创建目录
        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        // 2. 转换为Parquet格式
        let record_batch = klines_to_record_batch(klines)?;
        
        // 3. 写入Parquet文件
        let file = File::create(cache_path)?;
        let mut writer = ParquetFileWriter::try_new(file, record_batch.schema())?;
        writer.write(&record_batch)?;
        writer.close()?;
        
        Ok(())
    }
    
    /// 保存Tick数据到缓存
    async fn save_ticks_to_cache(
        &self,
        cache_path: &Path,
        ticks: &TickDataBuffer,
    ) -> Result<(), ArbiterError> {
        // 类似逻辑，但保存Tick数据
        // ...
    }

impl HistoricalIngesterService {
    /// 下载指定日期的历史K线数据并写入缓存
    pub async fn download_and_cache_kline(
        &self,
        symbol: &str,
        date: &str,
        timeframe: Timeframe,
    ) -> Result<Vec<KLine>, ArbiterError> {
        // 1. 检查缓存是否已存在
        let cache_key = CacheKey::for_klines(symbol, date, &timeframe.to_string());
        let cache_path = self.cache_dir.join(cache_key.to_path());
        
        if cache_path.exists() {
            // 缓存已存在，直接返回
            return self.load_from_cache(&cache_path).await;
        }
        
        // 2. 从 Binance Data Vision 下载
        let klines = self.download_kline_for_date(symbol, date, timeframe).await?;
        
        // 3. 写入缓存
        self.save_klines_to_cache(&cache_path, &klines).await?;
        
        Ok(klines)
    }
    
    /// 下载指定日期的历史Tick数据并写入缓存
    pub async fn download_and_cache_ticks(
        &self,
        symbol: &str,
        date: &str,
    ) -> Result<TickDataBuffer, ArbiterError> {
        // 类似逻辑，但下载Tick数据
        // ...
    }
    
    /// 下载指定日期的历史K线数据（不写入缓存）
    async fn download_kline_for_date(
        &self,
        symbol: &str,
        date: &str,
        timeframe: Timeframe,
    ) -> Result<Vec<KLine>, ArbiterError> {
        let interval_str = timeframe.to_binance_interval();
        let url = format!(
            "https://data.binance.vision/data/spot/daily/klines/{}/{}/{}-{}-{}.zip",
            symbol, interval_str, symbol, interval_str, date
        );
        
        // 下载、解压、解析
        self.download_and_parse_kline_file(&url).await
    }
}
```

**关键设计原则**：
- ✅ 只负责下载和写入缓存，不负责读取
- ✅ 按需下载，只下载缺失的数据
- ✅ 支持异步下载，不阻塞主流程

### 2.4 HistoricalDataService（历史数据读取服务）

**职责**：从本地缓存读取历史数据

**位置**：`flowsurface/data/src/historical_data_service.rs`（原 `external_adapter.rs` 重命名）

**接口定义**：

```rust
pub struct HistoricalDataService {
    ingester: Arc<HistoricalIngesterService>,  // 用于触发下载
    cache_dir: PathBuf,
}

impl HistoricalDataService {
    /// 从缓存读取K线数据
    async fn load_klines_from_cache(
        &self,
        cache_path: &Path,
    ) -> Result<Vec<KLine>, ArbiterError> {
        // 1. 读取Parquet文件
        let file = File::open(cache_path)?;
        let reader = ParquetFileReader::try_new(file)?;
        
        // 2. 解析为KLine数据
        let klines = parse_parquet_to_klines(reader)?;
        
        Ok(klines)
    }
    
    /// 从缓存读取Tick数据
    async fn load_ticks_from_cache(
        &self,
        cache_path: &Path,
    ) -> Result<TickDataBuffer, ArbiterError> {
        // 1. 读取Parquet文件
        let file = File::open(cache_path)?;
        let reader = ParquetFileReader::try_new(file)?;
        
        // 2. 解析为TickDataBuffer
        let ticks = parse_parquet_to_ticks(reader)?;
        
        Ok(ticks)
    }

impl HistoricalDataService {
    /// 获取历史K线数据（自动触发下载如果缓存未命中）
    pub async fn fetch_kline(
        &self,
        symbol: &str,
        range: TimeRange,
        timeframe: Timeframe,
    ) -> Result<Vec<KLine>, ArbiterError> {
        // 1. 计算需要的数据日期范围（只计算可见范围内的日期）
        let dates = calculate_date_range(range);
        
        let mut all_klines = Vec::new();
        
        for date in dates {
            // 2. 检查缓存（只检查可见范围内的日期）
            let cache_key = CacheKey::for_klines(symbol, &date, &timeframe.to_string());
            let cache_path = self.cache_dir.join(cache_key.to_path());
            
            let klines = if cache_path.exists() {
                // 3. 缓存命中：从缓存读取
                match self.load_klines_from_cache(&cache_path).await {
                    Ok(cached_klines) => cached_klines,
                    Err(e) => {
                        log::warn!("Cache file corrupted, will re-download: {}", e);
                        // 缓存损坏，触发下载
                        self.ingester.download_and_cache_kline(symbol, &date, timeframe).await?
                    }
                }
            } else {
                // 4. 缓存未命中：触发下载（HistoricalIngesterService 负责下载和写入缓存）
                self.ingester.download_and_cache_kline(symbol, &date, timeframe).await?
            };
            
            // 5. 过滤到可见范围（只保留需要的数据）
            let filtered: Vec<KLine> = klines.into_iter()
                .filter(|k| k.open_time_us >= range.start_us && k.open_time_us < range.end_us)
                .collect();
            
            all_klines.extend(filtered);
        }
        
        Ok(all_klines)
    }
    
    /// 获取历史Tick数据（自动触发下载如果缓存未命中）
    pub async fn fetch_ticks(
        &self,
        symbol: &str,
        range: TimeRange,
    ) -> Result<TickDataBuffer, ArbiterError> {
        // 类似逻辑，但下载Tick数据
        // ...
    }
    
}
```

**关键设计原则**：
- ✅ 只负责读取，不负责下载
- ✅ 自动触发 HistoricalIngesterService 下载缺失数据
- ✅ 按日期检查缓存，不是按整个范围
- ✅ 支持缓存损坏时的自动恢复

### 2.5 RealtimeDataService（实时数据读取服务）

**职责**：从 Mmap 文件读取实时数据

**位置**：`flowsurface/data/src/realtime_data_service.rs`（原 `io_service.rs` 重命名）

**接口定义**：

```rust
pub struct RealtimeDataService {
    store: Arc<MmapStore>,
}

impl RealtimeDataService {
    /// 从Mmap读取Tick数据并聚合为K线
    pub fn fetch_kline_blocking(
        &self,
        range: TimeRange,
        timeframe: Timeframe,
    ) -> Result<Vec<KLine>, ArbiterError> {
        // 1. 从Mmap读取Tick数据
        let ticks = self.fetch_ticks_blocking(range)?;
        
        // 2. 根据周期聚合为K线
        let aggregation_interval_us = timeframe.to_microseconds();
        let klines = aggregate_ticks_to_klines(ticks, aggregation_interval_us);
        
        Ok(klines)
    }
    
    /// 从Mmap读取Tick数据
    pub fn fetch_ticks_blocking(&self, range: TimeRange) -> Result<TickDataBuffer, ArbiterError> {
        // 从Mmap读取并解析Parquet数据
        // ...
    }
}
```

**关键设计原则**：
- ✅ 阻塞操作，需要在 `spawn_blocking` 中调用
- ✅ 支持按周期聚合K线
- ✅ 零拷贝读取（Mmap）

### 2.6 数据读取服务总结

**数据读取服务对比**：

| 服务 | 职责 | 操作方向 | 数据源/目标 | 文件格式 | 使用场景 |
|------|------|---------|------------|---------|---------|
| **RealtimeIngesterService** | 实时数据写入 | 写入 | WebSocket → Mmap | Mmap（内部Parquet） | 实时数据流 |
| **RealtimeDataService** | 实时数据读取 | 读取 | Mmap → 应用 | Mmap（内部Parquet） | 实时数据查询 |
| **HistoricalIngesterService** | 历史数据写入 | 写入 | Binance Data Vision → 缓存 | Parquet | 历史数据下载 |
| **HistoricalDataService** | 历史数据读取 | 读取 | 缓存 → 应用 | Parquet | 历史数据查询 |

**历史数据读取流程**：

```
查询历史数据
    │
    ▼
HistoricalDataService::fetch_*()
    │
    ├─→ 检查缓存
    │   │
    │   ├─→ 缓存命中：
    │   │   │
    │   │   └─→ HistoricalDataService::load_from_cache()
    │   │       │
    │   │       └─→ 从本地Parquet文件读取 ✅
    │   │
    │   └─→ 缓存未命中：
    │       │
    │       └─→ HistoricalIngesterService::download_and_cache_*()
    │           │
    │           ├─→ 从Binance Data Vision下载
    │           │
    │           └─→ 写入本地Parquet缓存文件
```

**关键点**：
- ✅ **RealtimeIngesterService**：只用于写入实时数据（WebSocket → Mmap）
- ✅ **RealtimeDataService**：只用于读取实时数据（Mmap → 应用）
- ✅ **HistoricalIngesterService**：只用于写入历史数据（Binance Data Vision → 缓存），包含缓存写入功能
- ✅ **HistoricalDataService**：只用于读取历史数据（缓存 → 应用），包含缓存读取功能
- ✅ **对称性**：实时数据和历史数据都采用"写入服务 + 读取服务"的对称架构，缓存功能直接集成在服务内部，不独立存在

### 2.7 实时数据与历史数据的对称架构

**核心关系**：**生产者-消费者模式**，通过共享的存储文件进行通信

#### 2.7.1 实时数据架构（对称）

| 服务 | 职责 | 操作方向 | 数据源/目标 | 文件位置 |
|------|------|---------|------------|---------|
| **RealtimeIngesterService** | 实时数据**写入** | **写入** | WebSocket → Mmap | `market_data/{SYMBOL}/realtime.mmap` |
| **RealtimeDataService** | 实时数据**读取** | **读取** | Mmap → 应用 | `market_data/{SYMBOL}/realtime.mmap` |

#### 2.7.2 历史数据架构（对称）

| 服务 | 职责 | 操作方向 | 数据源/目标 | 文件位置 |
|------|------|---------|------------|---------|
| **HistoricalIngesterService** | 历史数据**写入** | **写入** | Binance Data Vision → 缓存 | `cache/{DATE}-*.parquet` |
| **HistoricalDataService** | 历史数据**读取** | **读取** | 缓存 → 应用 | `cache/{DATE}-*.parquet` |

#### 2.7.3 实时数据流

```
┌─────────────────────────────────────────────────────────────┐
│                    Binance WebSocket                         │
│              (实时交易数据流)                                 │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
        ┌───────────────────────────────────────┐
        │   RealtimeIngesterService              │
        │  - 接收 WebSocket 数据                │
        │  - 解析交易数据                        │
        │  - 写入 Mmap 文件                      │
        │  - 更新索引                            │
        └───────────────────────────────────────┘
                            │
                            ▼ (写入)
        ┌───────────────────────────────────────┐
        │    market_data/{SYMBOL}/              │
        │         realtime.mmap                  │
        │  - Header (元数据)                     │
        │  - Index (索引表)                      │
        │  - Payload (Parquet 数据块)            │
        └───────────────────────────────────────┘
                            │
                            ▼ (读取)
        ┌───────────────────────────────────────┐
        │   RealtimeDataService                 │
        │  - 打开 Mmap 文件                      │
        │  - 读取 Tick 数据                      │
        │  - 聚合为 K 线                         │
        │  - 返回给 UnifiedDataService           │
        └───────────────────────────────────────┘
                            │
                            ▼
        ┌───────────────────────────────────────┐
        │      UnifiedDataService               │
        │  - 统一数据访问接口                    │
        └───────────────────────────────────────┘
```

#### 2.7.4 历史数据流

```
┌─────────────────────────────────────────────────────────────┐
│              Binance Data Vision API                        │
│              (历史数据下载)                                  │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
        ┌───────────────────────────────────────┐
        │   HistoricalIngesterService            │
        │  - 下载历史数据                        │
        │  - 解析数据                            │
        │  - 写入 Parquet 缓存                   │
        └───────────────────────────────────────┘
                            │
                            ▼ (写入)
        ┌───────────────────────────────────────┐
        │    cache/{DATE}-*.parquet              │
        │  - K线数据（按日期、周期）              │
        │  - Tick数据（按日期）                   │
        └───────────────────────────────────────┘
                            │
                            ▼ (读取)
        ┌───────────────────────────────────────┐
        │   HistoricalDataService                │
        │  - 检查缓存                             │
        │  - 读取 Parquet 数据                    │
        │  - 过滤到可见范围                        │
        │  - 返回给 UnifiedDataService            │
        └───────────────────────────────────────┘
                            │
                            ▼
        ┌───────────────────────────────────────┐
        │      UnifiedDataService               │
        │  - 统一数据访问接口                    │
        └───────────────────────────────────────┘
```

#### 2.7.5 关键设计点

1. **共享存储**：
   - **实时数据**：两个服务操作**同一个 Mmap 文件**
     - `RealtimeIngesterService` 负责**写入**（追加数据）
     - `RealtimeDataService` 负责**读取**（查询数据）
   - **历史数据**：两个服务操作**同一个 Parquet 缓存文件**
     - `HistoricalIngesterService` 负责**写入**（下载并写入缓存）
     - `HistoricalDataService` 负责**读取**（从缓存读取）

2. **职责分离**：
   - **RealtimeIngesterService**：专注于实时数据获取和持久化
     - 连接 WebSocket
     - 解析数据
     - 写入 Mmap
     - 管理索引
   - **RealtimeDataService**：专注于实时数据查询和聚合
     - 打开 Mmap
     - 读取数据
     - 聚合 K 线
     - 返回结果
   - **HistoricalIngesterService**：专注于历史数据获取和持久化
     - 从 Binance Data Vision 下载
     - 解析数据
     - 写入 Parquet 缓存
   - **HistoricalDataService**：专注于历史数据查询
     - 检查缓存
     - 读取 Parquet 数据
     - 过滤到可见范围
     - 返回结果

3. **并发安全**：
   - **实时数据**：Mmap 文件支持**多读单写**
     - `RealtimeIngesterService` 是唯一的写入者
     - `RealtimeDataService` 可以有多个实例同时读取
     - 通过文件系统锁或原子操作保证写入安全
   - **历史数据**：Parquet 文件支持**多读单写**
     - `HistoricalIngesterService` 是唯一的写入者（按日期）
     - `HistoricalDataService` 可以有多个实例同时读取
     - 通过文件系统锁保证写入安全

4. **性能优化**：
   - **实时数据**：
     - **零拷贝读取**：`RealtimeDataService` 直接访问内存映射的数据
     - **异步写入**：`RealtimeIngesterService` 异步处理 WebSocket 数据流
     - **索引优化**：通过索引快速定位数据块
   - **历史数据**：
     - **列式存储**：Parquet 格式支持高效列式读取
     - **按需下载**：`HistoricalIngesterService` 只下载缺失的数据
     - **缓存优化**：通过缓存避免重复下载

#### 2.7.6 使用场景

**实时数据场景1：启动时**
```
1. RealtimeIngesterService 启动，开始接收 WebSocket 数据
2. 写入数据到 realtime.mmap
3. RealtimeDataService 打开 realtime.mmap（可能文件为空或很小）
4. 查询时返回已有数据（可能为空）
```

**实时数据场景2：运行时查询**
```
1. UI 请求实时 K 线数据
2. UnifiedDataService 调用 RealtimeDataService
3. RealtimeDataService 从 realtime.mmap 读取数据
4. 同时，RealtimeIngesterService 持续写入新数据到同一个文件
5. 返回查询结果
```

**历史数据场景1：首次查询**
```
1. UI 请求历史 K 线数据
2. UnifiedDataService 调用 HistoricalDataService
3. HistoricalDataService 检查缓存，发现未命中
4. HistoricalDataService 触发 HistoricalIngesterService 下载
5. HistoricalIngesterService 下载并写入缓存
6. HistoricalDataService 从缓存读取并返回
```

**历史数据场景2：缓存命中**
```
1. UI 请求历史 K 线数据
2. UnifiedDataService 调用 HistoricalDataService
3. HistoricalDataService 检查缓存，发现已存在
4. HistoricalDataService 直接从缓存读取并返回
5. 无需触发下载
```

#### 2.7.7 架构对称性对比

| 特性 | 实时数据架构 | 历史数据架构 |
|------|------------|------------|
| **写入服务** | RealtimeIngesterService | HistoricalIngesterService |
| **读取服务** | RealtimeDataService | HistoricalDataService |
| **数据来源** | WebSocket 实时流 | Binance Data Vision API |
| **存储格式** | Mmap（零拷贝） | Parquet（压缩存储） |
| **文件位置** | `market_data/{SYMBOL}/realtime.mmap` | `cache/{DATE}-*.parquet` |
| **时间范围** | 实时数据（>= safe_cutoff） | 历史数据（< safe_cutoff） |
| **更新频率** | 持续更新（实时） | 按需下载（一次性） |
| **架构模式** | 写入服务 + 读取服务 | 写入服务 + 读取服务 ✅ |

### 2.8 命名规范说明

**命名原则**：
- ✅ **对称性**：实时数据和历史数据采用完全对称的命名
  - `RealtimeIngesterService` ↔ `HistoricalIngesterService`（写入服务）
  - `RealtimeDataService` ↔ `HistoricalDataService`（读取服务）
- ✅ **职责明确**：服务名称直接表明其处理的数据类型（实时 vs 历史）和操作方向（写入 vs 读取）
- ✅ **与架构对应**：
  - `RealtimeIngesterService` + `RealtimeDataService` 对应 Speed Layer
  - `HistoricalIngesterService` + `HistoricalDataService` 对应 Batch Layer

**重命名映射**：
- `IngesterService` → `RealtimeIngesterService`（明确表达实时数据写入职责，与历史数据对称）
- `IoService` → `RealtimeDataService`（明确表达实时数据读取职责）
- `ExternalAdapter` → `HistoricalIngesterService` + `HistoricalDataService`（分离写入和读取职责，与实时数据对称）

/// 按指定间隔聚合Tick数据为K线
fn aggregate_ticks_to_klines(
    ticks: TickDataBuffer,
    interval_us: u64,  // 聚合间隔（微秒）
) -> Vec<KLine> {
    // 按时间间隔聚合逻辑
    // ...
}
```

**关键设计原则**：
- ✅ 阻塞操作，需要在 `spawn_blocking` 中调用
- ✅ 支持按周期聚合K线
- ✅ 零拷贝读取（Mmap）

### 2.5 VpComputeService（VP计算服务）

**职责**：执行 GPU 加速的 VP 计算

**位置**：`flowsurface/data/src/compute/vp.rs`

**接口定义**：

```rust
impl VpComputeService {
    /// VP 计算需要 Tick 数据（每笔交易的价格和成交量）
    /// 
    /// 数据源策略：
    /// 1. **实时数据**（>= safe_cutoff）：从 Speed Layer (Mmap) 获取 Tick 数据
    /// 2. **历史数据**（< safe_cutoff）：从 Binance Data Vision 下载历史 Tick 数据
    /// 3. **跨边界情况**：分别获取历史数据和实时数据，然后合并
    /// 
    /// GPU 计算：无论是实时数据还是历史数据，都使用 GPU 加速计算
    pub async fn compute_vp(
        &self,
        symbol: String,
        range: TimeRange,
        data_service: &UnifiedDataService,
    ) -> Result<VolumeProfile, ComputeError> {
        // 1. 通过 UnifiedDataService 获取 Tick 数据
        let ticks = data_service.fetch_ticks(symbol, range).await?;
        
        if ticks.prices.is_empty() {
            return Err(ComputeError::InvalidInput(
                format!("No tick data found for range {:?}", range)
            ));
        }
        
        // 2. 执行 GPU 加速计算
        self.compute_from_ticks_gpu(ticks).await
    }
    
    /// GPU 加速的 VP 计算（统一处理实时和历史数据）
    async fn compute_from_ticks_gpu(
        &self,
        ticks: TickDataBuffer,
    ) -> Result<VolumeProfile, ComputeError> {
        // 使用 GPU Compute Shader 计算成交量分布
        // 性能：~10-50ms（取决于数据量，与数据来源无关）
        self.gpu_pipeline.run_aggregation(
            &self.device,
            &self.queue,
            &ticks,
            &self.compute_params,
            self.histogram_buckets,
        ).await
    }
}
```

**关键设计原则**：
- ✅ 所有 VP 计算都使用 GPU 加速
- ✅ GPU 计算性能一致（~10-50ms），与数据来源无关
- ✅ 不区分数据来源，统一处理

---

## 三、数据流设计

### 3.1 K线数据流

**完整流程**：

```
用户交互（拖动、缩放图表）
    │
    ▼
界面渲染层检测到可见范围变化
    │
    ├─→ 计算可见时间范围：visible_range = {start_us, end_us}
    │
    └─→ 判断是否需要历史数据
        │
        ├─→ 如果 visible_range.start_us >= safe_cutoff
        │   │
        │   └─→ 只需要实时数据，从Mmap读取（不需要下载）✅
        │
        └─→ 如果 visible_range.start_us < safe_cutoff
            │
            └─→ 需要历史数据，触发自动下载
                │
                ▼
UnifiedDataService::fetch_klines(symbol, visible_range, timeframe)
    │
    ├─→ 计算需要的数据日期范围（只计算可见范围内的日期）
    │   │
    │   └─→ dates = [2025-11-23, 2025-11-24]  // 只包含可见范围的日期
    │
    ├─→ 对每个日期检查缓存
    │   │
    │   ├─→ 缓存命中：
    │   │   │
    │   │   ├─→ 从缓存读取完整日期数据
    │   │   │
    │   │   └─→ 过滤到可见范围（只使用需要的部分）
    │   │
    │   └─→ 缓存未命中：
    │       │
    │       ├─→ 下载这个日期的数据（只下载缺失的日期）
    │       │
    │       ├─→ 过滤到可见范围（只保留需要的数据）
    │       │
    │       └─→ 异步写入缓存（完整日期数据，用于下次查询）
    │
    └─→ 返回过滤后的K线数据（只包含可见范围的数据）
        │
        ▼
界面渲染层更新显示
```

**数据源选择**：

| 场景 | 数据源 | 操作 |
|------|--------|------|
| **纯实时**（>= safe_cutoff） | Mmap | 从Tick聚合为K线 |
| **纯历史**（< safe_cutoff） | Binance Data Vision | 下载或缓存读取 |
| **跨边界**（跨越 safe_cutoff） | Mmap + Binance Data Vision | 分别获取并拼接 |

### 3.2 VP计算数据流

**完整流程**：

```
用户交互（拖动、缩放图表）
    │
    ▼
界面渲染层检测到需要更新VP
    │
    ├─→ 计算可见时间范围：visible_range = {start_us, end_us}
    │
    └─→ 发送 Message::ComputeVp(symbol, visible_range)
        │
        ▼
VP 计算层接收请求
    │
    ▼
调用 UnifiedDataService::fetch_ticks(symbol, visible_range)
    │
    ├─→ 判断数据源（实时/历史/跨边界）
    │
    ├─→ 实时数据：从 Mmap 缓存读取
    │   │
    │   └─→ Ingester 持续写入（后台）
    │
    ├─→ 历史数据：检查 Parquet 缓存
    │   │
    │   ├─→ 缓存命中：直接返回
    │   │
    │   └─→ 缓存未命中：从 Binance Data Vision 下载
    │       │
    │       └─→ 异步写入缓存（不阻塞）
    │
    └─→ 跨边界：分别获取并合并
    │
    ▼
返回 TickDataBuffer
    │
    ▼
执行 GPU 加速计算（~10-50ms）
    │
    ▼
返回 VolumeProfile
    │
    ▼
通过 Message::VpComputed 返回给渲染层
    │
    ▼
界面渲染层更新显示
```

**数据源选择**：

| 场景 | 数据源 | GPU计算 |
|------|--------|---------|
| **纯实时**（>= safe_cutoff） | Mmap | ✅ GPU（~10-50ms） |
| **纯历史**（< safe_cutoff） | Binance Data Vision | ✅ GPU（~10-50ms） |
| **跨边界**（跨越 safe_cutoff） | Mmap + Binance Data Vision | ✅ GPU（~10-50ms） |

### 3.3 数据缓存流

**实时数据缓存**：

```
WebSocket 实时流
    │
    ▼
Ingester 接收交易数据
    │
    ▼
写入 Mmap (realtime.mmap)
    │
    └─→ ✅ 持续写入，零拷贝
```

**历史数据缓存**：

```
查询历史数据
    │
    ├─→ 检查缓存
    │   │
    │   ├─→ 缓存命中：从 Parquet 读取 ✅
    │   │
    │   └─→ 缓存未命中：从 Binance Data Vision 下载
    │       │
    │       ├─→ 返回数据
    │       │
    │       └─→ 异步写入缓存（不阻塞）
```

---

## 四、数据结构设计

### 4.1 时间范围

```rust
/// 时间范围（微秒精度）
pub struct TimeRange {
    pub start_us: u64,  // 开始时间（微秒）
    pub end_us: u64,    // 结束时间（微秒）
}

impl TimeRange {
    /// 计算可见时间范围
    pub fn from_visible_region(state: &ChartState) -> Option<Self> {
        // 从图表状态计算可见时间范围
        // ...
    }
}
```

### 4.2 周期枚举

```rust
/// 支持的周期
pub enum Timeframe {
    M1,   // 1分钟
    M3,   // 3分钟
    M5,   // 5分钟
    M15,  // 15分钟
    M30,  // 30分钟
    H1,   // 1小时
    H4,   // 4小时
    D1,   // 1天
}

impl Timeframe {
    pub fn to_milliseconds(self) -> u64 {
        match self {
            Timeframe::M1 => 60 * 1_000,
            Timeframe::M5 => 5 * 60 * 1_000,
            Timeframe::M15 => 15 * 60 * 1_000,
            // ...
        }
    }
    
    pub fn to_microseconds(self) -> u64 {
        self.to_milliseconds() * 1_000
    }
    
    pub fn to_binance_interval(self) -> &'static str {
        match self {
            Timeframe::M1 => "1m",
            Timeframe::M5 => "5m",
            Timeframe::M15 => "15m",
            // ...
        }
    }
}
```

### 4.3 缓存键设计

```rust
/// 缓存键包含币种、日期和周期
pub struct CacheKey {
    symbol: String,
    date: String,      // 格式：YYYY-MM-DD
    interval: String,  // 格式：1m, 5m, 15m, 1h, 4h, 1d
    data_type: String, // "klines" 或 "ticks"
}

impl CacheKey {
    pub fn for_klines(symbol: &str, date: &str, interval: &str) -> Self {
        Self {
            symbol: symbol.to_string(),
            date: date.to_string(),
            interval: interval.to_string(),
            data_type: "klines".to_string(),
        }
    }
    
    pub fn for_ticks(symbol: &str, date: &str) -> Self {
        Self {
            symbol: symbol.to_string(),
            date: date.to_string(),
            interval: "".to_string(),  // Tick数据不区分周期
            data_type: "ticks".to_string(),
        }
    }
    
    pub fn to_path(&self, cache_dir: &Path) -> PathBuf {
        cache_dir.join(format!(
            "{}-{}-{}.parquet",
            self.date, self.interval, self.data_type
        ))
    }
}
```

### 4.4 Tick数据缓冲区

```rust
/// Tick数据缓冲区（用于VP计算）
pub struct TickDataBuffer {
    pub prices: Vec<u32>,   // 价格（缩放后的整数）
    pub volumes: Vec<f32>,  // 成交量
}

impl TickDataBuffer {
    /// 合并两个Tick数据缓冲区
    pub fn merge(mut self, other: Self) -> Self {
        self.prices.extend(other.prices);
        self.volumes.extend(other.volumes);
        self
    }
}
```

---

## 五、文件系统设计

### 5.1 目录结构

```
market_data/
  ├── BTCUSDT/
  │   ├── realtime.mmap              # 实时Tick数据缓存（必须）
  │   └── cache/                     # 历史数据缓存（可选）
  │       ├── 2025-11-23-klines-1m.parquet   # 1分钟K线缓存
  │       ├── 2025-11-23-klines-5m.parquet   # 5分钟K线缓存
  │       ├── 2025-11-23-klines-15m.parquet  # 15分钟K线缓存
  │       └── 2025-11-23-ticks.parquet       # Tick数据缓存（用于VP计算）
  ├── ETHUSDT/
  │   ├── realtime.mmap
  │   └── cache/
  └── SOLUSDT/
      ├── realtime.mmap
      └── cache/
```

### 5.2 文件命名规范

**实时数据**：
- 文件名：`realtime.mmap`
- 位置：`market_data/{SYMBOL}/realtime.mmap`
- 格式：Mmap（内部使用 Parquet 格式存储）

**历史数据缓存**：
- K线缓存：`{DATE}-klines-{INTERVAL}.parquet`
  - 示例：`2025-11-23-klines-1m.parquet`
- Tick缓存：`{DATE}-ticks.parquet`
  - 示例：`2025-11-23-ticks.parquet`
- 位置：`market_data/{SYMBOL}/cache/`

### 5.3 数据隔离

**实时数据和历史数据完全分离**：
- **实时数据**：`realtime.mmap`（动态时间周期，可能超过24小时）
- **历史数据**：`cache/{DATE}-*.parquet`（按日期分离，可选）

**无物理冲突**：
- 不同文件，不同写入者
- Ingester 写入：`realtime.mmap`
- HistoricalDataService 写入：`cache/{DATE}-*.parquet`

---

## 六、缓存策略设计

### 6.1 缓存策略总结

| 数据类型 | 实时数据 | 历史数据 | 缓存原因 |
|---------|---------|---------|---------|
| **Tick数据** | ✅ Mmap缓存（必须） | ✅ Parquet缓存（可选） | VP计算必需，不能从K线还原 |
| **K线数据** | ❌ 不缓存（从Tick聚合） | ✅ Parquet缓存（可选） | 加速图表显示，支持多周期 |

### 6.2 缓存检查机制

**按日期检查缓存，不是按整个范围**：

```rust
// 1. 计算需要的数据日期范围（只计算可见范围内的日期）
let dates = calculate_date_range(range);  // 例如：[2025-11-23, 2025-11-24]

for date in dates {
    // 2. 检查缓存（只检查可见范围内的日期）
    let cache_key = CacheKey::for_klines(symbol, &date, &timeframe.to_string());
    let cache_path = self.cache_dir.join(cache_key.to_path());
    
    if cache_path.exists() {
        // 3. 缓存命中：从缓存读取
        // 过滤到可见范围（可能只使用部分缓存数据）
        continue;  // 跳过下载
    }
    
    // 4. 缓存未命中：只下载这个日期的数据
    // ...
}
```

### 6.3 缓存写入策略

**异步写入，不阻塞数据返回**：

```rust
// 下载数据后，异步写入缓存
let cache_path_clone = cache_path.clone();
let data_for_cache = data.clone();
tokio::spawn(async move {
    if let Err(e) = self.save_to_cache(&cache_path_clone, &data_for_cache).await {
        log::warn!("Failed to cache data: {}", e);
    }
});
```

### 6.4 缓存性能

**性能提升**：
- 首次查询：~650ms - 2.7s（需要下载）
- 缓存命中：~1-10ms（从本地 Parquet 读取）
- 性能提升：~65-270 倍

---

## 七、多币种和周期切换设计

### 7.1 多币种处理

**每个币种使用独立的数据文件**：

```rust
// 币种切换机制
impl KlineChart {
    pub fn switch_symbol(&mut self, new_symbol: String) {
        // 1. 更新币种标识
        self.symbol = new_symbol.clone();
        
        // 2. 触发数据重新获取
        let range = self.chart.state.visible_time_range_us();
        if let Some(range) = range {
            // 发送K线数据获取请求
            self.request_handler.send(Message::FetchKLines(
                new_symbol.clone(),
                range,
            ));
            
            // 发送VP计算请求
            self.request_handler.send(Message::ComputeVp(
                new_symbol,
                range,
            ));
        }
        
        // 3. 清空当前缓存
        self.render_cache_kline = Arc::new(Vec::new());
        self.chart.state.volume_profile = None;
        
        // 4. 触发重绘
        self.invalidate_all();
    }
}
```

**主应用层协调**：

```rust
// main.rs: 处理币种切换
impl Flowsurface {
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Dashboard(_, dashboard::Event::ResolveStreams(streams)) => {
                // 1. 提取新币种
                let new_symbol = extract_symbol_from_streams(&streams);
                
                // 2. 触发Ingester订阅新币种
                if let Some(symbol) = new_symbol {
                    let _ = self.ingest_tx.try_send(IngestCommand::Subscribe(symbol.clone()));
                    
                    // 3. 确保Mmap文件存在
                    let normalized_symbol = normalize_binance_symbol(&symbol);
                    let data_dir = data::data_path(Some("market_data"));
                    IngestionService::ensure_mmap_file(&normalized_symbol, &data_dir);
                }
            }
            // ...
        }
    }
}
```

### 7.2 周期切换

**实时数据：从Tick聚合**：

```rust
// RealtimeDataService: 根据周期聚合Tick数据
impl RealtimeDataService {
    pub fn fetch_kline_blocking(
        &self,
        range: TimeRange,
        timeframe: Timeframe,  // 周期参数
    ) -> Result<Vec<KLine>, ArbiterError> {
        // 1. 从Mmap读取Tick数据
        let ticks = self.fetch_ticks_blocking(range)?;
        
        // 2. 根据周期聚合为K线
        let aggregation_interval_us = timeframe.to_microseconds();
        let klines = aggregate_ticks_to_klines(ticks, aggregation_interval_us);
        
        Ok(klines)
    }
}
```

**历史数据：从Binance Data Vision下载对应周期**：

```rust
// HistoricalDataService: 根据周期下载历史K线
impl HistoricalDataService {
    pub async fn fetch_kline(
        &self,
        symbol: &str,
        range: TimeRange,
        timeframe: Timeframe,  // 周期参数
    ) -> Result<Vec<KLine>, ArbiterError> {
        // 1. 将周期转换为Binance API格式
        let interval_str = timeframe.to_binance_interval();
        
        // 2. URL格式：.../klines/{SYMBOL}/{INTERVAL}/{SYMBOL}-{INTERVAL}-{DATE}.zip
        let url = format!(
            "https://data.binance.vision/data/spot/daily/klines/{}/{}/{}-{}-{}.zip",
            symbol, interval_str, symbol, interval_str, date
        );
        
        // 下载、解压、解析
        // ...
    }
}
```

**界面层：周期切换**：

```rust
// KlineChart: 周期切换
impl KlineChart {
    pub fn switch_timeframe(&mut self, new_timeframe: Timeframe) {
        // 1. 更新周期
        self.chart.state.basis = Basis::Time(new_timeframe);
        
        // 2. 触发数据重新获取（使用新周期）
        let range = self.chart.state.visible_time_range_us();
        if let Some(range) = range {
            // 发送K线数据获取请求（包含周期参数）
            self.request_handler.send(Message::FetchKLines(
                self.symbol.clone(),
                range,
                new_timeframe,  // 新周期
            ));
        }
        
        // 3. 清空当前缓存
        self.render_cache_kline = Arc::new(Vec::new());
        
        // 4. 触发重绘
        self.invalidate_all();
    }
}
```

---

## 八、自动下载机制设计

### 8.1 触发机制

**界面渲染层自动触发**：

```rust
// 界面渲染层：检测到需要更新数据时触发
impl KlineChart {
    fn check_data_update_needed(&self) -> Option<Action> {
        // 1. 获取当前屏幕可见的时间范围
        if let Some(visible_range) = self.chart.state.visible_time_range_us() {
            // 2. 判断是否需要历史数据
            let safe_cutoff = calculate_safe_historical_cutoff();
            
            if visible_range.start_us < safe_cutoff {
                // 需要历史数据，触发数据获取请求
                return Some(Action::RequestFetchKLines(
                    self.symbol.clone(),
                    visible_range,
                ));
            }
        }
        None
    }
}
```

### 8.2 按需下载机制

**只下载需要的数据**：

```rust
// HistoricalDataService: 检查缓存，只下载缺失的数据
impl HistoricalDataService {
    pub async fn fetch_kline_with_auto_download(
        &self,
        symbol: &str,
        range: TimeRange,  // 屏幕可见范围
        timeframe: Timeframe,
    ) -> Result<Vec<KLine>, ArbiterError> {
        // 1. 计算需要的数据日期范围（只计算可见范围内的日期）
        let dates = calculate_date_range(range);
        
        let mut all_klines = Vec::new();
        
        for date in dates {
            // 2. 检查缓存（只检查可见范围内的日期）
            let cache_key = CacheKey::for_klines(symbol, &date, &timeframe.to_string());
            let cache_path = self.cache_dir.join(cache_key.to_path());
            
            if cache_path.exists() {
                // 3. 缓存命中：从缓存读取，过滤到可见范围
                // ...
                continue;  // 跳过下载
            }
            
            // 4. 缓存未命中：只下载这个日期的数据
            let klines = self.download_historical_kline_for_date(
                symbol,
                &date,
                timeframe,
            ).await?;
            
            // 5. 过滤到可见范围（只保留需要的数据）
            let filtered: Vec<KLine> = klines.into_iter()
                .filter(|k| k.open_time_us >= range.start_us && k.open_time_us < range.end_us)
                .collect();
            
            // 6. 异步写入缓存（完整日期数据，不阻塞）
            // ...
            
            all_klines.extend(filtered);
        }
        
        Ok(all_klines)
    }
}
```

### 8.3 关键优化点

**✅ 只下载需要的数据**：
- 只计算可见范围内的日期
- 只下载缺失日期的数据
- 过滤到可见范围，不下载多余数据

**✅ 智能缓存检查**：
- 按日期检查缓存，不是按整个范围
- 如果某个日期缓存命中，跳过下载
- 如果某个日期缓存未命中，只下载这个日期

**✅ 异步缓存写入**：
- 下载后异步写入缓存，不阻塞数据返回
- 缓存完整日期数据，便于下次查询

---

## 九、协调机制设计

### 9.1 数据缓存、VP计算、界面渲染的协调

**架构层次关系**：

```
界面渲染层 (UI Rendering)
    ↓ (请求：时间范围 + 交易对)
VP 计算层 (VpComputeService)
    ↓ (请求：时间范围 + 交易对)
数据访问层 (UnifiedDataService)
    ↓ (读取/下载)
数据缓存层 (Mmap/Parquet)
```

**协调方式**：

1. **请求-响应模式**：
   - 渲染层 → VP 计算层：`Message::ComputeVp`
   - VP 计算层 → 渲染层：`Message::VpComputed`

2. **统一接口模式**：
   - VP 计算层 → 数据访问层：`UnifiedDataService::fetch_ticks`
   - 数据访问层自动处理缓存逻辑

3. **后台服务模式**：
   - Ingester 持续写入 Mmap（后台）
   - HistoricalDataService 按需下载并缓存（后台）

### 9.2 K线数据和VP计算数据的协调

**数据源统一性**：

```
相同的Tick数据源
    │
    ├─→ K线数据路径
    │   │
    │   └─→ 按时间间隔聚合（1分钟）→ K线（OHLCV）
    │
    └─→ VP计算数据路径
        │
        └─→ 按价格水平聚合 → VP（价格-成交量分布）
```

**协调机制**：

1. **数据源共享**：K线数据和VP计算数据都来自相同的Tick数据源
2. **时间范围协调**：使用相同的可见时间范围
3. **数据一致性保证**：确保来自相同的数据源和时间范围
4. **渲染协调**：K线数据和VP数据一起传递给渲染器

---

## 十、性能设计

### 10.1 性能目标

| 场景 | 性能目标 | GPU计算 |
|------|---------|---------|
| **实时数据访问** | ~1-10ms | - |
| **实时 VP 计算** | ~10-50ms | ✅ GPU |
| **历史数据访问（缓存后）** | ~1-10ms | - |
| **历史 VP 计算（缓存后）** | ~10-50ms | ✅ GPU |
| **跨边界 VP 计算（缓存后）** | ~10-50ms | ✅ GPU |

### 10.2 性能优化策略

**1. 缓存策略（关键优化）**：
- 本地缓存历史数据到 Parquet 文件
- 缓存命中时性能接近原有架构
- 性能提升：~65-270 倍

**2. 并发优化**：
- 并发下载多个交易对的数据
- 并发获取历史数据和实时数据（跨边界查询）

**3. 异步操作**：
- 异步缓存写入，不阻塞数据返回
- 后台预加载常用数据

### 10.3 GPU计算性能

**关键**：**所有 VP 计算都使用 GPU 加速**

- ✅ **实时数据 VP**：GPU 计算（~10-50ms）
- ✅ **历史数据 VP**：GPU 计算（~10-50ms）
- ✅ **跨边界 VP**：GPU 计算（~10-50ms）
- ✅ **GPU 计算性能一致**：与数据来源无关，只取决于数据量

---

## 十一、错误处理设计

### 11.1 错误类型

```rust
pub enum ArbiterError {
    /// 数据不可用（例如：历史数据未发布）
    DataNotAvailable(String),
    /// 网络错误
    NetworkError(String),
    /// 文件错误
    FileError(String),
    /// 数据格式错误
    ParseError(String),
    /// 其他错误
    Other(String),
}
```

### 11.2 错误处理策略

**1. 历史数据未准备好**：
- 检查数据可用性（HEAD 请求）
- 使用 Fallback 机制（如果范围跨越边界）
- 返回友好的错误信息

**2. 网络错误**：
- 重试机制（最多 3 次）
- 记录日志
- 返回错误给上层

**3. 文件错误**：
- 检查文件完整性
- 如果文件损坏，重新下载
- 记录日志

---

## 十二、实施步骤

### Phase 1: 创建新组件（不破坏现有功能）

1. **创建 `UnifiedDataService`**
   - `flowsurface/data/src/unified_data_service.rs`
   - 实现统一的数据访问接口

2. **扩展 `HistoricalDataService`**
   - 添加 Binance Data Vision 支持
   - 实现缓存机制

3. **重构 `IngesterService`**
   - 明确职责：只处理实时数据
   - 添加动态时间边界检查

### Phase 2: 迁移现有代码

1. **更新 `main.rs`**
   - 使用 `UnifiedDataService` 替代 `ArbiterService`
   - 更新 VP 计算逻辑

2. **更新 UI 层**
   - 根据时间范围自动选择数据源
   - 处理跨边界情况

### Phase 3: 测试和优化

1. **单元测试**
   - 测试数据源选择逻辑
   - 测试缓存机制
   - 测试跨边界查询

2. **集成测试**
   - 测试完整数据流
   - 测试多币种切换
   - 测试周期切换

3. **性能测试**
   - 测试缓存性能
   - 测试GPU计算性能
   - 测试并发下载性能

### Phase 4: 清理和优化

1. **移除 `ArbiterService`**
2. **优化数据访问性能**
3. **添加监控和日志**

---

## 十三、关键设计原则总结

### 13.1 架构原则

1. ✅ **Lambda 架构**：分离 Speed Layer（实时）和 Batch Layer（历史）
2. ✅ **不物理合并数据**：数据源选择在统一服务中完成
3. ✅ **关注点分离**：渲染层不需要知道数据来源
4. ✅ **动态时间边界**：根据历史数据发布延迟动态调整

### 13.2 数据原则

1. ✅ **实时数据**：存储在 Mmap 中，动态时间周期（可能超过24小时）
2. ✅ **历史数据**：从 Binance Data Vision 下载，可选缓存到 Parquet
3. ✅ **数据隔离**：实时数据和历史数据完全分离，不同文件
4. ✅ **缓存策略**：按币种、日期、周期分别缓存

### 13.3 性能原则

1. ✅ **实时数据性能**：保持原有高性能（Mmap 零拷贝，~1-10ms）
2. ✅ **GPU 计算性能**：完全保持不变（~10-50ms）
3. ✅ **历史数据性能**：通过缓存接近原有性能（~1-10ms）
4. ✅ **按需下载**：只下载可见范围内的缺失数据

### 13.4 协调原则

1. ✅ **请求-响应模式**：渲染层 ↔ VP 计算层
2. ✅ **统一接口模式**：VP 计算层 → 数据访问层
3. ✅ **后台服务模式**：数据写入在后台完成
4. ✅ **异步协调**：所有协调都是异步的，不阻塞 UI

---

## 十四、依赖项

### 14.1 新增依赖

```toml
# flowsurface/data/Cargo.toml
[dependencies]
reqwest = { version = "0.12", features = ["json"] }
zip = "0.6"
csv = "1.3"
tokio-tungstenite = "0.24"
futures-util = { version = "0.3", features = ["sink", "compat"] }
```

### 14.2 现有依赖

- `tokio`: 异步运行时
- `wgpu`: GPU 计算
- `bytemuck`: 零拷贝转换
- `arrow`: Parquet 支持

---

## 十五、测试策略

### 15.1 单元测试

**测试数据源选择逻辑**：
```rust
#[test]
fn test_data_source_selection() {
    let safe_cutoff = calculate_safe_historical_cutoff();
    
    // 测试纯实时数据
    let range = TimeRange {
        start_us: safe_cutoff + 1000,
        end_us: safe_cutoff + 2000,
    };
    assert_eq!(select_data_source(range, safe_cutoff), DataSource::SpeedLayer);
    
    // 测试纯历史数据
    let range = TimeRange {
        start_us: safe_cutoff - 2000,
        end_us: safe_cutoff - 1000,
    };
    assert_eq!(select_data_source(range, safe_cutoff), DataSource::BatchLayer);
    
    // 测试跨边界
    let range = TimeRange {
        start_us: safe_cutoff - 1000,
        end_us: safe_cutoff + 1000,
    };
    assert_eq!(select_data_source(range, safe_cutoff), DataSource::Both);
}
```

**测试缓存机制**：
```rust
#[tokio::test]
async fn test_cache_mechanism() {
    // 测试缓存命中
    // 测试缓存未命中
    // 测试异步写入
}
```

### 15.2 集成测试

**测试完整数据流**：
```rust
#[tokio::test]
async fn test_complete_data_flow() {
    // 1. 创建 UnifiedDataService
    // 2. 请求K线数据
    // 3. 验证数据源选择
    // 4. 验证数据返回
}
```

**测试多币种切换**：
```rust
#[tokio::test]
async fn test_multi_symbol_switching() {
    // 1. 切换到BTCUSDT
    // 2. 切换到ETHUSDT
    // 3. 验证数据文件隔离
}
```

### 15.3 性能测试

**测试缓存性能**：
```rust
#[tokio::test]
async fn test_cache_performance() {
    // 1. 首次查询（下载）
    // 2. 第二次查询（缓存命中）
    // 3. 验证性能提升
}
```

**测试GPU计算性能**：
```rust
#[tokio::test]
async fn test_gpu_compute_performance() {
    // 1. 实时数据VP计算
    // 2. 历史数据VP计算
    // 3. 验证性能一致
}
```

---

## 十六、监控和日志

### 16.1 关键日志点

1. **数据源选择**：
   ```rust
   log::debug!("Selecting data source: {:?} for range {:?}", source, range);
   ```

2. **缓存操作**：
   ```rust
   log::debug!("Cache hit for {} on {}", symbol, date);
   log::info!("Cache miss for {} on {}, downloading...", symbol, date);
   ```

3. **下载操作**：
   ```rust
   log::info!("Downloading historical data for {} on {}", symbol, date);
   log::warn!("Historical data not available for {} on {}", symbol, date);
   ```

4. **VP计算**：
   ```rust
   log::info!("VP computation started for {} in range {:?}", symbol, range);
   log::info!("VP computation completed: {} bars", profile.bars.len());
   ```

### 16.2 性能监控

**关键指标**：
- 数据源选择时间
- 缓存命中率
- 下载时间
- GPU计算时间
- 总延迟

---

## 十七、总结

本文档提供了完整的数据架构设计规范，包括：

1. **架构概述**：Lambda 架构、动态时间边界、数据源选择
2. **核心组件设计**：UnifiedDataService、IngesterService、HistoricalDataService、RealtimeDataService、VpComputeService
3. **数据流设计**：K线数据流、VP计算数据流、缓存流
4. **数据结构设计**：时间范围、周期枚举、缓存键、Tick数据缓冲区
5. **文件系统设计**：目录结构、文件命名规范、数据隔离
6. **缓存策略设计**：缓存检查、缓存写入、缓存性能
7. **多币种和周期切换设计**：币种切换、周期切换
8. **自动下载机制设计**：触发机制、按需下载、优化点
9. **协调机制设计**：数据缓存、VP计算、界面渲染的协调
10. **性能设计**：性能目标、优化策略、GPU计算性能
11. **错误处理设计**：错误类型、处理策略
12. **实施步骤**：分阶段实施计划
13. **测试策略**：单元测试、集成测试、性能测试
14. **监控和日志**：关键日志点、性能监控

**关键设计原则**：
- ✅ Lambda 架构：分离实时和历史数据
- ✅ 不物理合并：数据源选择在统一服务中完成
- ✅ 关注点分离：渲染层不需要知道数据来源
- ✅ 动态时间边界：根据历史数据发布延迟动态调整
- ✅ 所有 VP 计算都使用 GPU 加速
- ✅ 按需下载：只下载可见范围内的缺失数据
- ✅ 智能缓存：按日期检查缓存，提升性能

**下一步**：按照本文档的设计规范，开始具体程序设计与实现。

