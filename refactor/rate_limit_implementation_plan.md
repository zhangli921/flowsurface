# Binance 速率限制处理 - 优雅实现方案

## 一、设计原则

### 1.1 架构原则
- **单一职责**：速率限制逻辑独立成模块
- **可配置**：限制参数可配置，便于调整
- **可观测**：提供监控和日志
- **优雅降级**：遇到限制时优雅处理，不影响整体流程

### 1.2 设计模式
- **策略模式**：速率限制策略可替换
- **装饰器模式**：在现有下载逻辑外包装速率限制
- **观察者模式**：通过事件总线通知速率限制事件

## 二、架构设计

### 2.1 模块划分

```
┌─────────────────────────────────────────┐
│     HistoricalDownloadCoordinator       │
│  (任务队列、优先级、并发控制)            │
└──────────────────┬──────────────────────┘
                   │
                   ▼
┌─────────────────────────────────────────┐
│      RateLimiter (速率限制器)            │
│  - 请求间隔控制                          │
│  - 429 错误检测和处理                     │
│  - Retry-After 解析                      │
└──────────────────┬──────────────────────┘
                   │
                   ▼
┌─────────────────────────────────────────┐
│   HistoricalDownloadExecutor            │
│  (实际下载执行)                          │
└─────────────────────────────────────────┘
```

### 2.2 新增模块

#### 模块 1: `rate_limiter.rs` - 速率限制器
**职责**：
- 控制请求间隔
- 检测和处理 429 错误
- 解析 Retry-After header
- 提供速率限制状态监控

#### 模块 2: 扩展 `DataError` - 速率限制错误
**职责**：
- 添加 `RateLimitExceeded` 错误类型
- 包含 `retry_after_secs` 信息

## 三、详细实现方案

### 3.1 创建速率限制器模块

**文件**: `flowsurface/data/src/rate_limiter.rs`

```rust
//! Rate limiter for Binance Data Vision API requests.
//!
//! This module provides rate limiting functionality to prevent
//! exceeding Binance API rate limits and handle 429 errors gracefully.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Configuration for rate limiting.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Minimum interval between requests (milliseconds).
    pub min_request_interval_ms: u64,
    /// Maximum concurrent requests.
    pub max_concurrent: usize,
    /// Default retry delay when 429 is received without Retry-After header (seconds).
    pub default_retry_after_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            min_request_interval_ms: 200,  // 200ms between requests
            max_concurrent: 2,              // Max 2 concurrent downloads
            default_retry_after_secs: 60,  // Default 60 seconds
        }
    }
}

/// Rate limiter for API requests.
pub struct RateLimiter {
    config: RateLimitConfig,
    last_request_time: Arc<Mutex<Instant>>,
}

impl RateLimiter {
    /// Creates a new rate limiter with default configuration.
    pub fn new() -> Self {
        Self::with_config(RateLimitConfig::default())
    }

    /// Creates a new rate limiter with custom configuration.
    pub fn with_config(config: RateLimitConfig) -> Self {
        Self {
            config,
            last_request_time: Arc::new(Mutex::new(Instant::now())),
        }
    }

    /// Waits if necessary to maintain minimum request interval.
    ///
    /// This should be called before each API request.
    pub async fn wait_if_needed(&self) {
        let mut last_time = self.last_request_time.lock().await;
        let elapsed = last_time.elapsed();
        let interval = Duration::from_millis(self.config.min_request_interval_ms);
        
        if elapsed < interval {
            let wait_time = interval - elapsed;
            tokio::time::sleep(wait_time).await;
        }
        
        *last_time = Instant::now();
    }

    /// Gets the default retry delay for rate limit errors.
    pub fn default_retry_delay(&self) -> Duration {
        Duration::from_secs(self.config.default_retry_after_secs)
    }

    /// Gets the rate limit configuration.
    pub fn config(&self) -> &RateLimitConfig {
        &self.config
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Parses Retry-After header from HTTP response.
///
/// Returns the retry delay in seconds, or None if header is missing or invalid.
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get("Retry-After")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}
```

### 3.2 扩展 DataError

**文件**: `flowsurface/data/src/data_error.rs` (修改)

```rust
#[derive(Debug, Error)]
pub enum DataError {
    // ... existing variants ...
    
    /// Rate limit exceeded (HTTP 429).
    ///
    /// Contains the number of seconds to wait before retrying,
    /// as suggested by the server's Retry-After header.
    #[error("Rate limit exceeded. Retry after {retry_after_secs} seconds")]
    RateLimitExceeded {
        retry_after_secs: u64,
    },
}
```

### 3.3 修改 HistoricalDownloadExecutor

**文件**: `flowsurface/data/src/historical_download_executor.rs` (修改)

```rust
use crate::rate_limiter::{RateLimiter, parse_retry_after};

pub struct HistoricalDownloadExecutor {
    client: Client,
    cache_dir: PathBuf,
    downloading: Arc<Mutex<HashSet<String>>>,
    rate_limiter: Arc<RateLimiter>,  // 新增
}

impl HistoricalDownloadExecutor {
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        Self::with_rate_limiter(cache_dir, None)
    }

    pub fn with_rate_limiter(
        cache_dir: Option<PathBuf>,
        rate_limiter: Option<Arc<RateLimiter>>,
    ) -> Self {
        let cache_dir = cache_dir.unwrap_or_else(|| data_path(Some("cache")));
        let rate_limiter = rate_limiter.unwrap_or_else(|| Arc::new(RateLimiter::new()));
        
        Self {
            client: Client::new(),
            cache_dir,
            downloading: Arc::new(Mutex::new(HashSet::new())),
            rate_limiter,
        }
    }

    async fn download_kline_for_date(
        &self,
        symbol: &str,
        date: &str,
        timeframe: &str,
    ) -> Result<Vec<KLine>, DataError> {
        // ... existing code ...

        // Wait for rate limit before request
        self.rate_limiter.wait_if_needed().await;

        // Download ZIP file
        let response = self.client.get(&url).send().await?;
        
        // Check for rate limit error (429)
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = parse_retry_after(response.headers())
                .unwrap_or_else(|| self.rate_limiter.config().default_retry_after_secs);
            
            log::warn!(
                "Rate limit exceeded for {}/{}/{}. Retry after {} seconds.",
                symbol,
                timeframe,
                date,
                retry_after
            );
            
            return Err(DataError::RateLimitExceeded {
                retry_after_secs: retry_after,
            });
        }
        
        if !response.status().is_success() {
            // ... existing 404 handling ...
        }

        // ... rest of the function ...
    }

    async fn download_ticks_for_date(
        &self,
        symbol: &str,
        date: &str,
    ) -> Result<TickDataBuffer, DataError> {
        // ... existing code ...

        // Wait for rate limit before request
        self.rate_limiter.wait_if_needed().await;

        // Download ZIP file
        let response = self.client.get(&url).send().await?;
        
        // Check for rate limit error (429)
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = parse_retry_after(response.headers())
                .unwrap_or_else(|| self.rate_limiter.config().default_retry_after_secs);
            
            log::warn!(
                "Rate limit exceeded for {}/{}. Retry after {} seconds.",
                symbol,
                date,
                retry_after
            );
            
            return Err(DataError::RateLimitExceeded {
                retry_after_secs: retry_after,
            });
        }
        
        if !response.status().is_success() {
            // ... existing 404 handling ...
        }

        // ... rest of the function ...
    }
}
```

### 3.4 修改 HistoricalDownloadCoordinator

**文件**: `flowsurface/data/src/historical_download_coordinator.rs` (修改)

```rust
impl HistoricalDownloadCoordinator {
    pub fn new(
        availability_index: Arc<DataAvailabilityIndex>,
        executor: Arc<HistoricalDownloadExecutor>,
        event_bus: Arc<EventBus>,
    ) -> Self {
        Self {
            task_queue: Arc::new(Mutex::new(BinaryHeap::new())),
            availability_index,
            executor,
            event_bus,
            max_concurrent_downloads: 2,  // 从 5 降低到 2
            current_downloads: Arc::new(Mutex::new(HashSet::new())),
            shutdown: Arc::new(tokio::sync::Notify::new()),
        }
    }
}

impl DownloadCoordinatorClone {
    fn calculate_retry_delay(&self, error: &DataError, attempt: u32) -> Duration {
        match error {
            // 专门处理速率限制错误
            DataError::RateLimitExceeded { retry_after_secs } => {
                // 使用服务器建议的延迟时间
                Duration::from_secs(*retry_after_secs)
            }
            // 其他错误处理
            _ => {
                let error_str = error.to_string().to_lowercase();
                
                if error_str.contains("404") || error_str.contains("not found") {
                    return Duration::from_secs(3600 * (1 << (attempt.saturating_sub(1))));
                }
                
                if error_str.contains("403") || error_str.contains("forbidden") {
                    return Duration::from_secs(1);
                }
                
                Duration::from_secs(1 << (attempt.saturating_sub(1)))
            }
        }
    }
}
```

### 3.5 更新 lib.rs

**文件**: `flowsurface/data/src/lib.rs` (修改)

```rust
pub mod rate_limiter;  // 新增

// ... existing modules ...

pub use rate_limiter::{RateLimiter, RateLimitConfig};
```

## 四、实施步骤

### 步骤 1: 创建速率限制器模块
1. 创建 `flowsurface/data/src/rate_limiter.rs`
2. 实现 `RateLimiter` 和 `RateLimitConfig`
3. 实现 `parse_retry_after` 函数

### 步骤 2: 扩展错误类型
1. 在 `DataError` 中添加 `RateLimitExceeded` 变体

### 步骤 3: 集成到下载执行器
1. 在 `HistoricalDownloadExecutor` 中添加 `rate_limiter` 字段
2. 在每次请求前调用 `wait_if_needed()`
3. 检测和处理 429 错误

### 步骤 4: 更新协调器
1. 降低 `max_concurrent_downloads` 到 2
2. 在重试逻辑中处理 `RateLimitExceeded` 错误

### 步骤 5: 更新模块导出
1. 在 `lib.rs` 中导出新模块

## 五、配置选项

### 5.1 默认配置（保守）
```rust
RateLimitConfig {
    min_request_interval_ms: 200,  // 200ms 间隔
    max_concurrent: 2,              // 2 个并发
    default_retry_after_secs: 60,   // 默认 60 秒
}
```

### 5.2 可调整配置
```rust
// 如果需要更快的下载（但风险更高）
let config = RateLimitConfig {
    min_request_interval_ms: 150,
    max_concurrent: 3,
    default_retry_after_secs: 60,
};

let rate_limiter = Arc::new(RateLimiter::with_config(config));
let executor = HistoricalDownloadExecutor::with_rate_limiter(None, Some(rate_limiter));
```

## 六、优势

### 6.1 架构优势
- ✅ **模块化**：速率限制逻辑独立，易于测试和维护
- ✅ **可配置**：限制参数可调整，无需修改代码
- ✅ **可扩展**：未来可以添加更复杂的速率限制策略
- ✅ **可观测**：提供日志和监控点

### 6.2 代码质量
- ✅ **单一职责**：每个模块职责清晰
- ✅ **依赖注入**：RateLimiter 可注入，便于测试
- ✅ **错误处理**：专门的错误类型，处理更精确
- ✅ **优雅降级**：遇到限制时优雅处理

## 七、测试建议

### 7.1 单元测试
- `RateLimiter::wait_if_needed()` 的间隔控制
- `parse_retry_after()` 的 header 解析
- `calculate_retry_delay()` 对 `RateLimitExceeded` 的处理

### 7.2 集成测试
- 模拟 429 响应，验证重试逻辑
- 验证请求间隔控制
- 验证并发数限制

## 八、向后兼容性

### 8.1 保持兼容
- `HistoricalDownloadExecutor::new()` 使用默认配置，保持向后兼容
- 现有代码无需修改即可使用

### 8.2 可选增强
- 通过 `with_rate_limiter()` 提供自定义配置
- 逐步迁移到新配置

## 九、总结

这个方案：
1. ✅ **架构优雅**：模块化设计，职责清晰
2. ✅ **易于维护**：配置和逻辑分离
3. ✅ **安全可靠**：保守的默认配置，避免触发限制
4. ✅ **可扩展**：未来可以添加更复杂的策略
5. ✅ **向后兼容**：现有代码无需修改

