

# **第二阶段实施方案：异步 I/O 隔离与 K 线数据仲裁服务（Arbiter Service）**

## **I. 阶段目标与架构约束**

### **1.1. 阶段核心目标**

第二阶段旨在构建 flowsurface-rs 的数据服务层，专注于解决 K 线图 (OHLCV) 数据获取中的两大核心问题：**I/O 阻塞**和**双源数据一致性**。

1. **I/O 隔离 (Isolation Mandate)：** 严格隔离所有同步的磁盘 I/O（Mmap 访问）和耗时的网络 I/O（外部 API 调用），确保 iced UI 框架的主异步运行时不被阻塞，维持高帧率和响应性 1。  
2. **数据权威性 (Authority Mandate)：** 实现一个 **数据仲裁服务（Arbiter Service）**，负责协调内部实时聚合数据和外部历史数据，并在实时时间窗口内（Live Window）强制执行内部数据的最高权威性。

### **1.2. 强制技术栈与设计约束**

* **异步运行时：** 强制使用 tokio 作为异步执行器。  
* **同步 I/O 隔离：** 任何涉及磁盘文件读取、Mmap 查找或高密度 CPU 聚合（如实时 K 线聚合）的代码段，必须使用 tokio::task::spawn\_blocking 封装，并派发到专用的阻塞线程池 1。  
* **Mmap 集成：** 必须安全地持有第一阶段 MmapWrapper 实例的 Arc 引用。  
* **数据规范化：** 所有数据（无论来自外部还是内部聚合）在进入仲裁服务后，都必须转换为规范化的 KLine 结构体。

## **II. 核心数据结构与消息定义**

### **2.1. 规范化 K 线结构体 (data::kline::KLine)**

此结构体作为数据仲裁服务的统一输出，必须使用原始类型并支持方便的序列化和克隆，以适应 iced 消息传递。

Rust

// data/src/kline.rs  
\#  
pub struct KLine {  
    // 强制使用 u64 纳秒时间戳作为主键  
    pub open\_time\_ns: u64,   
    pub open: f64,  
    pub high: f64,  
    pub low: f64,  
    pub close: f64,  
    pub volume: f64,  
    pub num\_trades: u32,  
}

### **2.2. Iced 应用程序消息 (src/message.rs)**

引入新的消息类型以驱动数据服务层。

Table: Iced 消息定义

| 消息类型 | 职责 | 触发者/来源 |
| :---- | :---- | :---- |
| FetchKLines(TimeRange) | 触发仲裁服务开始查询。 | ChartState (用户缩放/平移) |
| KLineArbiterResult(Result\<Vec\<KLine\>, ArbiterError\>) | 接收最终、合并后的 K 线数据。 | ArbiterService (异步完成) |
| InternalLiveKLineResult(Result\<Vec\<KLine\>, IoError\>) | **内部消息：** 接收 Mmap 阻塞线程的聚合结果。 | IoService (阻塞线程) |
| InternalHistoricalKLineResult(Result\<Vec\<KLine\>, ExternalError\>) | **内部消息：** 接收外部 API 适配器的结果。 | ExternalAdapter (异步网络) |

## **III. 实施细节：双源适配器与 I/O 隔离**

### **3.1. 内部实时数据适配器 (data\_arbiter::io\_service)**

该模块负责从 Mmap 文件中提取 Tick 数据并实时聚合 K 线。

#### **A. Mmap 访问与聚合隔离 (fetch\_live\_kline\_blocking)**

该函数必须是同步的，并严格封装在 tokio::task::spawn\_blocking 内部。

1. **数据范围查找：** 使用 Stage 1 的 MmapWrapper 实例，在阻塞线程中执行 $O(\\log N)$ 的二分查找，定位可见范围内的 Tick 索引 3。  
2. **零拷贝切片：** 利用 Mmap Wrapper 的能力，安全获取 Tick 数据的 &\[u8\] 零拷贝切片。  
3. **实时聚合：** 使用高性能的聚合库（如 trade\_aggregation-rs 或自定义的循环逻辑）对 Tick 切片进行遍历，实时计算出规范化的 Vec\<KLine\>。  
4. **返回：** 将结果包装在 Message::InternalLiveKLineResult 中，返回给主异步运行时。

### **3.2. 外部历史数据适配器 (data\_arbiter::external\_adapter)**

该模块负责处理 Binance API 的网络 I/O，并管理本地缓存。

#### **A. 异步网络与缓存策略 (fetch\_historical\_kline)**

1. **缓存查询 (Cache Hit)：** 优先查询本地持久化缓存（例如，以 Parquet/Arrow 格式存储的历史文件 5）。如果缓存命中，异步读取文件并返回。  
2. **网络 I/O (Cache Miss)：** 如果缓存未命中，使用 reqwest 或类似的 **async 客户端**异步调用 Binance API。  
3. **数据规范化：** 外部 JSON 原始数据必须被解析并转换为规范化的 KLine 结构体。  
4. **缓存写入：** 异步将获取到的规范化数据写入本地 Parquet/Arrow 缓存，供后续查询使用。

## **IV. 核心：数据仲裁服务（Arbiter Service）**

### **4.1. 并发调度与等待 (arbiter\_service::arbitrate\_kline\_data)**

仲裁服务负责并发地启动内部和外部数据获取任务，以最小化总延迟。

Rust

// data\_arbiter/arbiter\_service.rs (核心异步逻辑)  
pub async fn arbitrate\_kline\_data(  
    io\_service: IoService,   
    external\_adapter: ExternalAdapter,   
    range: TimeRange  
) \-\> Result\<Vec\<KLine\>, ArbiterError\> {  
    // 1\. 启动 I/O 隔离任务 (实时数据)  
    let live\_task \= tokio::task::spawn\_blocking(move |

| {  
        io\_service.fetch\_live\_kline\_blocking(range)   
    });

    // 2\. 启动异步网络任务 (历史数据)  
    let historical\_task \= tokio::spawn(  
        external\_adapter.fetch\_historical\_kline(range)   
    );  
      
    // 3\. 并发等待两个结果  
    // 使用 tokio::try\_join\! 或类似宏并发等待，确保任一任务失败时可快速返回错误。  
    let (live\_result, historical\_result) \= tokio::join\!(live\_task, historical\_task);  
      
    // 4\. 解包并处理错误  
    let live\_data \= live\_result.map\_err(|e| ArbiterError::InternalTask(e.to\_string()))??;  
    let historical\_data \= historical\_result?;

    // 5\. 执行合并与冲突解决  
    merge\_and\_resolve(live\_data, historical\_data, range)  
}

### **4.2. 数据合并与权威性冲突解决 (merge\_and\_resolve)**

这是保证数据一致性的关键函数。它必须强制执行以下 **权威性规则**：

1. **定义 Live Window：** 定义一个明确的实时时间窗口（例如，当前时间倒推 12 小时）。  
2. **Live Data Authority：** 在 Live Window 内部，**内部 Tick 聚合数据**（live\_data）拥有最高权威性。  
3. **历史数据截断：** 过滤 historical\_data，**移除**所有落在 Live Window 内的 K 线。这样做是因为外部 API 提供的 K 线数据精度和实时性不如我们本地从 Tick 流聚合出的数据，避免数据冲突。  
4. **合并：** 将截断后的 historical\_data 与完整的 live\_data 按时间戳进行拼接。  
5. **最终排序与去重：** 确保合并后的数据集按时间戳严格排序，并处理边界处的潜在重复条目。

## **V. 鲁棒性与非偏离性检查**

### **5.1. 错误处理 (ArbiterError)**

所有潜在的 I/O 错误、网络失败、Mmap 校验失败和数据结构不一致都必须被封装在定制的 ArbiterError 枚举中 7。这确保了错误能够被清晰地分类，并在 iced 的 update 循环中被优雅地处理，而不是导致程序崩溃。

### **5.2. iced 集成模式总结**

数据服务的触发和结果回传必须通过 iced::Command 机制实现。

1. **请求：** update 接收 FetchKLines $\\rightarrow$ 启动 ArbiterService::run\_arbiter\_task (这是一个封装了 arbitrate\_kline\_data 的异步 Command)。  
2. **响应：** Command 完成 $\\rightarrow$ 返回 KLineArbiterResult $\\rightarrow$ update 函数更新 ChartState 模型。

通过此模式，所有耗时的 I/O 和聚合任务都被隔离在后台，UI 线程仅负责发送请求和接收结果，从而满足了 flowsurface-rs 高性能桌面的所有非阻塞要求。此方案为后续第三阶段的 GPGPU **Session Volume Profile (S-VP)** 并行聚合提供了稳定、可靠、规范化的 K 线时间轴数据。

#### **Works cited**

1. Where do computationally heavy parts go in async model of Rust, accessed November 20, 2025, [https://users.rust-lang.org/t/where-do-computationally-heavy-parts-go-in-async-model-of-rust/34881](https://users.rust-lang.org/t/where-do-computationally-heavy-parts-go-in-async-model-of-rust/34881)  
2. What is the benefit of using tokio instead of OS threads in Rust \- Stack Overflow, accessed November 20, 2025, [https://stackoverflow.com/questions/75836002/what-is-the-benefit-of-using-tokio-instead-of-os-threads-in-rust](https://stackoverflow.com/questions/75836002/what-is-the-benefit-of-using-tokio-instead-of-os-threads-in-rust)  
3. slice \- Rust Documentation, accessed November 20, 2025, [https://doc.rust-lang.org/std/primitive.slice.html](https://doc.rust-lang.org/std/primitive.slice.html)  
4. Optimize Binary Search of Rust \- Rust Magazine, accessed November 20, 2025, [https://rustmagazine.org/issue-2/optimize-binary-search/](https://rustmagazine.org/issue-2/optimize-binary-search/)  
5. parquet \- Rust \- Apache Arrow, accessed November 20, 2025, [https://arrow.apache.org/rust/parquet/index.html](https://arrow.apache.org/rust/parquet/index.html)  
6. parquet::arrow \- Rust \- Docs.rs, accessed November 20, 2025, [https://docs.rs/parquet/latest/parquet/arrow/index.html](https://docs.rs/parquet/latest/parquet/arrow/index.html)  
7. Error Handling \- The Rust Programming Language, accessed November 20, 2025, [https://doc.rust-lang.org/book/ch09-00-error-handling.html](https://doc.rust-lang.org/book/ch09-00-error-handling.html)  
8. memmap2: Complete Rust Crate Guide & Documentation \[2025\] \- Generalist Programmer, accessed November 20, 2025, [https://generalistprogrammer.com/tutorials/memmap2-rust-crate-guide](https://generalistprogrammer.com/tutorials/memmap2-rust-crate-guide)