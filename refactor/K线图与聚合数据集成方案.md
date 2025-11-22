

# **统一的、GPGPU加速的金融图表数据架构（FCA-DA）蓝图**

## **I. 执行摘要：低延迟统一架构的强制要求**

本次系统改造旨在将 flowsurface-rs 升级为一个高性能的金融图表平台，能够同时处理高吞吐量的原始市场数据，并快速生成复杂的可视化指标，例如 **Session Volume Profile (S-VP) 筹码峰**。

### **统一的战略推荐**

架构升级的核心策略是采用基于 **Rust 语言**和 **Apache Arrow 生态系统**构建数据骨干 1。所有计算密集型的聚合任务，特别是针对足迹图所需的实时微观数据处理，应全面转向**通用图形处理器（GPGPU）计算着色器**执行，以实现计算加速 3。

### **架构原则总结：以原始数据为核心**

统一架构成功的关键在于定义一个以**最高数据粒度**（即原始Tick/L2数据）为标准的规范化数据模型 5。由于足迹图和热力图的本质需求，架构不能以简单的K线数据模型（OHLCV）为基础进行统一。因此，系统必须围绕存储和处理原始Tick和Level 2 (L2) 市场深度数据的能力来构建 。

### **K线数据集成策略总结**

对于K线数据的特殊集成需求（包括来自币安的历史数据），将通过一个智能的数据仲裁服务（Data Arbitrator Service）进行管理 7。该服务负责协调内部实时聚合生成的数据流与经过规范化处理的外部历史数据缓存之间的查询，确保数据的权威性和**一致性**。

## **II. 基础架构蓝图：统一高粒度数据**

### **II. A. 统一与隔离的权衡：数据需求差异**

要实现统一架构，首先必须明确所有图表类型对数据粒度的内在需求差异。

#### **高粒度图表 (Footprint, Heatmap, S-VP) 的数据要求**

* **足迹图（Footprint Charts）**：需要**逐笔成交 (tick-by-tick) 数据**，并包含买入（Bid）和卖出（Ask）的详细信息，以便准确地对买卖压力进行分类 。它需要计算 **Intra-Bar Volume Profile (IBVP)**，即单根 K 线内部的成交量分布 9。  
* **热力图（Heatmaps）**：通常实现为市场深度图（Market Depth Map），依赖于连续的、实时的 Level 2 (L2) 订单簿数据流 6。它需要将历史深度信息保存在本地数据库中以便进行历史回放 11。  
* **Session Volume Profile (S-VP) 筹码峰**：需要**海量**逐笔交易数据，用于在**可见屏幕范围或自定义时段**内进行价格聚合，识别关键兴趣点（POC, VA） 13。

#### **K线图的数据要求**

K线图（OHLCV）需要聚合后的数据，但为了与 Footprint 和 S-VP 保持一致性，其聚合也应尽可能基于内部 Tick/L2 流。对于历史 K 线，则依赖外部 API 获取 16。

#### **架构统一的关键**

统一的架构必须以最高粒度的**原始Tick/L2流**为基石进行数据 ingestion 和存储。数据架构的统一是通过**统一数据处理方法**（GPGPU加速聚合）和**统一数据模型**（Apache Arrow的规范化）来实现 2。

### **II. B. 实施规范化的列式数据模型（FDAP栈）**

内部规范化架构将建立在 Rust 和 FDAP (Flight, DataFusion, Arrow, Parquet) 技术栈之上 。

#### **Apache Arrow和Parquet的应用**

* **Apache Arrow作为内存标准**：提供列式内存数据格式，支持零拷贝访问，是高效馈送 GPGPU 计算流水线的基础 18。  
* **Parquet用于持久化**：针对存储效率优化的列式存储格式，适用于长期保存海量 Tick/L2 数据 17。

#### **数据结构优化：阵列结构（SoA）的强制要求**

为了实现 GPGPU 计算的最佳性能，所有 Tick/L2 数据结构**必须**采用**阵列结构（Structure of Arrays, SoA）**，而非结构阵列（AoS）20。SoA 布局确保了内存访问是高度连续的，从而最大化 CPU 缓存命中率和 GPU 内存合并（Coalescing）效率 23。

| 字段名称 | Arrow 类型 | 作用 | 所需图表 |
| :---- | :---- | :---- | :---- |
| timestamp\_ns | Timestamp (Nanosecond) | 主索引 | 所有图表 |
| price | Float64 | 执行价格/L2等级 | 所有图表 |
| volume | UInt64 | 成交量/L2深度 | 所有图表，Delta，IBVP |
| is\_bid\_aggressor | Boolean | 市价单是否撞击买价 | 足迹图 Delta 计算 |
| L2\_snapshot\_ID | UInt32 | DOM 深度外键 | 热力图持久化 6 |

## **III. 影响评估：图表数据要求与架构压力**

### **III. A. S-VP 筹码峰与 IBVP 单根 K 线分布：管理计算复杂度**

足迹图的 **Intra-Bar Volume Profile (IBVP)** 和新增的 **Session Volume Profile (S-VP) 筹码峰**是系统中最具挑战性的计算任务。

* **S-VP (筹码峰)**：需要对**整个可见时间范围**内海量 Tick 数据进行一次性聚合 。  
* **IBVP (足迹图)**：需要对**每根 K 线内部**的 Tick 数据重复执行聚合，同时结合 Delta 分析 5。

这些聚合任务的本质都是数据并行（Data Parallel）。在传统 CPU 架构中，聚合任务的复杂度是 $O(n)$，会迅速导致 UI 冻结 。将这些任务转移到 GPGPU 上，利用并行算法（如前缀和）25，可以显著优化计算时间至 $O(\\log n)$，这是实现亚秒级响应的关键 3。

### **III. B. 热力图（市场深度图）：L2历史数据管理**

热力图依赖 L2 订单簿的历史状态 6。系统必须能够通过内存映射（Mmap）或活动分区（Active Partition）快速访问最新的 L2 数据，以确保实时性 26。热力图的**实时可视化负载**也应使用 GPU 加速，通过纹理（Texture）或实例化（Instancing）技术快速绘制和更新价格块的颜色和深度 。

## **IV. GPGPU计算流水线：高速度聚合引擎**

GPGPU 计算流水线是所有复杂聚合任务的统一执行引擎。

### **IV. A. GPGPU架构与计算着色器原理**

* **框架选择**：采用 **WGPU** 作为主要的图形和计算抽象层，确保跨平台的可移植性 。对于需要极致性能的场景，可关注 **Rust-CUDA** 等项目 28。  
* **工作流程**：  
  1. **数据传输**：将 SoA 优化的 Arrow RecordBatches 推送至 GPU 1。  
  2. **调度管线**：创建 wgpu::ComputePipeline 30。  
  3. **核心执行**：计算着色器（WGSL Kernel）执行并行聚合。例如，对于 S-VP 筹码峰，内核执行一次针对整个数据集的原子累加，而对于 IBVP，内核执行多次针对单根 K 线 Tick 集合的累加 。  
* **性能优化**：计算着色器内核设计必须优化 L2 缓存局部性，例如通过\*\*工作组平铺（Thread-Group Tiling）\*\*或 ID 交换技术，以提高 VRAM 吞吐量 。

### **IV. B. GPGPU计算任务划分与调度**

| 聚合任务类型 | 计算目标 | GPU Kernel 策略 | CPU 协调职责 |
| :---- | :---- | :---- | :---- |
| **S-VP 筹码峰** | 聚合可见范围 Volume Profile | **单次**大规模并行原子累加 32 | I/O 隔离、异步回读、POC/VA 后处理 33 |
| **IBVP 单根分布** | 聚合每根 K 线 Delta/Volume Profile | **多次**针对 K 线 Tick 范围的并行聚合 5 | 逐 K 线调度、异步回读、平衡计算结果 10 |
| **K 线 OHLCV** | 聚合 OHLCV 指标 | 小规模、高频并行归约 | 数据仲裁、规范化、缓存管理 16 |

## **V. K线双源集成策略：数据仲裁服务**

### **V. A. Rust中的数据仲裁服务模式**

引入**数据仲裁服务**作为所有 K 线数据请求的唯一入口，遵循 **Controller $\\rightarrow$ Arbitrator Service $\\rightarrow$ Repository** 的服务分层模式 35。

* **异步状态机**：仲裁服务利用 Rust async 函数的**状态机**特性，管理外部 API 调用（如 Binance）的 I/O 等待、速率限制和重试，从而避免阻塞内部实时计算路径 38。  
* **集中式缓存**：外部获取的历史 K 线数据在规范化为 Arrow 格式后，存储在一个**集中式缓存**中，避免分布式缓存同步的复杂性 41。

### **V. B. 集成与冲突解决逻辑**

仲裁服务负责协调内部和外部数据源，确保一致性。

1. **权威数据源定义**：  
   * **实时窗口（Live Window）**：由 **内部 GPGPU 聚合引擎** 实时生成的 K 线数据为权威源，以确保与 Footprint 和 S-VP 的微观数据一致 16。  
   * **历史窗口（Historical Window）**：外部 API 提供的**集中式缓存**为权威源。  
2. **数据规范化**：外部 API 返回的数据必须转化为标准的 Arrow RecordBatch 格式 41。  
3. **合并与校正**：将两个来源的数据集合并，内部聚合的数据在实时窗口内具有最高优先级，用于校正外部源的潜在延迟或时序不一致性。

## **VI. 统一渲染架构：WGPU多层可视化**

所有图表功能（K线、S-VP、IBVP、Heatmap）的渲染必须统一到一个高性能的 **WGPU 渲染通道**内，并利用 iced::widget::shader 自定义组件进行绘制 。

* **K线图作为基础层**：传统的 K 线图作为渲染管线的基础，负责绘制 OHLCV 矩形。  
* **GPGPU 衍生数据渲染**：  
  * **S-VP 筹码峰**：通过 WGPU 实例渲染（Instancing）技术绘制 42。将稀疏直方图数据 (SparseBars) 上传到 GPU **实例缓冲区**，通过单次 Draw Call 高效绘制所有柱状条 42。  
  * **IBVP / Footprint**：通常作为 K 线内部的纹理或紧凑的实例群集进行渲染，由其内部的 Delta/Volume 数据驱动着色 。  
  * **热力图**：渲染为与价格轴对齐的彩色纹理或实例，颜色强度与 L2 订单簿的深度直接相关 。

通过这种统一渲染策略，可以实现高度的渲染效率和性能，同时避免传统 UI 框架可能导致的绘制瓶颈。

## **VII. 结论与建议**

本次改造的最终成果是建立了基于 Rust/Arrow 的现代化、低延迟、高可扩展性的金融图表数据架构。

1. **架构统一性得到保障**：通过采用以原始 Tick/L2 数据为基础的规范化 Schema，所有现有功能（K-Line, Footprint, Heatmap）和新增的 **Session Volume Profile (S-VP) 筹码峰**都可以在统一的框架内实现。所有聚合任务都从这一最高粒度数据模型中**衍生**出来。  
2. **计算性能飞跃**：通过 GPGPU 卸载和 **SoA 数据布局**，复杂聚合任务（如 S-VP 和 IBVP）的性能将实现量级提升。  
3. **数据流完整性**：数据仲裁服务确保了外部历史 K 线数据的可靠获取，同时隔离了其性能波动对内部实时系统的影响，保证了所有图表数据的一致性和低延迟。

#### **Works cited**

1. Rust \- Apache Arrow, accessed November 20, 2025, [https://arrow.apache.org/rust/arrow/index.html](https://arrow.apache.org/rust/arrow/index.html)  
2. Engineering a Time Series Database Using Open Source: Rebuilding InfluxDB 3 in Apache Arrow and Rust \- InfoQ, accessed November 20, 2025, [https://www.infoq.com/articles/timeseries-db-rust/](https://www.infoq.com/articles/timeseries-db-rust/)  
3. WebGPU Rendering: Part 11 Prefix Sum | by Matthew MacFarquhar | Medium, accessed November 20, 2025, [https://matthewmacfarquhar.medium.com/webgpu-rendering-part-11-prefix-sum-c26a32223f9f](https://matthewmacfarquhar.medium.com/webgpu-rendering-part-11-prefix-sum-c26a32223f9f)  
4. Using a Rust async function as a polled state machine | Jeff McBride, accessed November 20, 2025, [https://jeffmcbride.net/blog/2025/05/16/rust-async-functions-as-state-machines/](https://jeffmcbride.net/blog/2025/05/16/rust-async-functions-as-state-machines/)  
5. Footprint Charts vs Volume Profile: Which Trading Tool Wins? \- QuantVPS, accessed November 20, 2025, [https://www.quantvps.com/blog/footprint-charts-vs-volume-profile](https://www.quantvps.com/blog/footprint-charts-vs-volume-profile)  
6. Market Depth Map \- Overcharts Help Center, accessed November 20, 2025, [https://www.overcharts.com/en/helpcenter/docs/market-depth-map/](https://www.overcharts.com/en/helpcenter/docs/market-depth-map/)  
7. Best way to synchronize cache data between two servers \[closed\] \- Stack Overflow, accessed November 20, 2025, [https://stackoverflow.com/questions/16585798/best-way-to-synchronize-cache-data-between-two-servers](https://stackoverflow.com/questions/16585798/best-way-to-synchronize-cache-data-between-two-servers)  
8. Performance Comparison of Graph Representations Which Support Dynamic Graph Updates \- arXiv, accessed November 20, 2025, [https://arxiv.org/html/2502.13862v1](https://arxiv.org/html/2502.13862v1)  
9. Volume Profile — Indicators and Strategies \- TradingView, accessed November 20, 2025, [https://www.tradingview.com/scripts/volumeprofile/](https://www.tradingview.com/scripts/volumeprofile/)  
10. Insane gpu usage when hovering over the UI. · Issue \#2119 · iced-rs/iced \- GitHub, accessed November 20, 2025, [https://github.com/iced-rs/iced/issues/2119](https://github.com/iced-rs/iced/issues/2119)  
11. Data Integration from Multiple Sources: Methods, Best Practices, and 2025 Guide \- Domo, accessed November 20, 2025, [https://www.domo.com/learn/article/integrate-data-from-multiple-sources](https://www.domo.com/learn/article/integrate-data-from-multiple-sources)  
12. Why is SOA (Structures of Arrays) faster than AOS? : r/C\_Programming \- Reddit, accessed November 20, 2025, [https://www.reddit.com/r/C\_Programming/comments/9jg7hy/why\_is\_soa\_structures\_of\_arrays\_faster\_than\_aos/](https://www.reddit.com/r/C_Programming/comments/9jg7hy/why_is_soa_structures_of_arrays_faster_than_aos/)  
13. Don't Offload GGUF Layers, Offload Tensors\! 200%+ Gen Speed? Yes Please\!\!\! \- Reddit, accessed November 20, 2025, [https://www.reddit.com/r/LocalLLaMA/comments/1ki7tg7/dont\_offload\_gguf\_layers\_offload\_tensors\_200\_gen/](https://www.reddit.com/r/LocalLLaMA/comments/1ki7tg7/dont_offload_gguf_layers_offload_tensors_200_gen/)  
14. Christian Legnitto Interview, Maintainer: rust-gpu, rust-cuda \[Rust Project Content @ RustConf 2025\] \- YouTube, accessed November 20, 2025, [https://www.youtube.com/watch?v=monOq\_uHHcg](https://www.youtube.com/watch?v=monOq_uHHcg)  
15. WebGPU Textures, accessed November 20, 2025, [https://webgpufundamentals.org/webgpu/lessons/webgpu-textures.html](https://webgpufundamentals.org/webgpu/lessons/webgpu-textures.html)  
16. Benefits of Multiple Data Sources in Commodity Intelligence \- Vesper, accessed November 20, 2025, [https://vespertool.com/knowledge-hub/commodities/intelligence/benefits-of-using-multiple-data-sources/](https://vespertool.com/knowledge-hub/commodities/intelligence/benefits-of-using-multiple-data-sources/)  
17. parquet \- Rust \- Apache Arrow, accessed November 20, 2025, [https://arrow.apache.org/rust/parquet/index.html](https://arrow.apache.org/rust/parquet/index.html)  
18. Building High-Performance Time-Series Applications with tsink: A Rust Embedded Database \- DEV Community, accessed November 20, 2025, [https://dev.to/h2337/building-high-performance-time-series-applications-with-tsink-a-rust-embedded-database-5fa7](https://dev.to/h2337/building-high-performance-time-series-applications-with-tsink-a-rust-embedded-database-5fa7)  
19. Mmap in memmap \- Rust \- Docs.rs, accessed November 20, 2025, [https://docs.rs/memmap/latest/memmap/struct.Mmap.html](https://docs.rs/memmap/latest/memmap/struct.Mmap.html)  
20. Futures and the Async Syntax \- The Rust Programming Language \- Rust Documentation, accessed November 20, 2025, [https://doc.rust-lang.org/book/ch17-01-futures-and-syntax.html](https://doc.rust-lang.org/book/ch17-01-futures-and-syntax.html)  
21. Volume Profile Indicator: Understand the Basics \- ICICIdirect, accessed November 20, 2025, [https://www.icicidirect.com/futures-and-options/articles/understanding-volume-profile-indicator](https://www.icicidirect.com/futures-and-options/articles/understanding-volume-profile-indicator)  
22. Mastering Manual Profiling in Tickblaze: A Powerful Volume Profile Strategy, accessed November 20, 2025, [https://tickblaze.com/blog/mastering-manual-profiling-in-tickblaze-a-powerful-volume-profile-strategy/](https://tickblaze.com/blog/mastering-manual-profiling-in-tickblaze-a-powerful-volume-profile-strategy/)  
23. SoA vs AoS: Data Layout Optimization | ML & CV Consultant \- Abhik Sarkar, accessed November 20, 2025, [https://www.abhik.xyz/concepts/performance/soa-vs-aos](https://www.abhik.xyz/concepts/performance/soa-vs-aos)  
24. Defining shader @workgroup\_size(x,y,z) at runtime in wgpu? \- Stack Overflow, accessed November 20, 2025, [https://stackoverflow.com/questions/79741355/defining-shader-workgroup-sizex-y-z-at-runtime-in-wgpu](https://stackoverflow.com/questions/79741355/defining-shader-workgroup-sizex-y-z-at-runtime-in-wgpu)  
25. orderbook-tick-data · GitHub Topics, accessed November 20, 2025, [https://github.com/topics/orderbook-tick-data](https://github.com/topics/orderbook-tick-data)  
26. ComputePipeline in iced::widget::shader::wgpu \- Rust \- Docs.rs, accessed November 20, 2025, [https://docs.rs/iced/latest/iced/widget/shader/wgpu/struct.ComputePipeline.html](https://docs.rs/iced/latest/iced/widget/shader/wgpu/struct.ComputePipeline.html)  
27. High Dynamic Range Rendering | Learn Wgpu, accessed November 20, 2025, [https://sotrh.github.io/learn-wgpu/intermediate/tutorial13-hdr/](https://sotrh.github.io/learn-wgpu/intermediate/tutorial13-hdr/)  
28. Footprint Charts: A Complete Guide to Advanced Trading Analysis \- Optimus Futures, accessed November 20, 2025, [https://optimusfutures.com/blog/footprint-charts/](https://optimusfutures.com/blog/footprint-charts/)  
29. Rust-GPU/rust-cuda: Ecosystem of libraries and tools for writing and executing fast GPU code fully in Rust. \- GitHub, accessed November 20, 2025, [https://github.com/Rust-GPU/rust-cuda](https://github.com/Rust-GPU/rust-cuda)  
30. PipelineLayout in wgpu \- Rust \- Docs.rs, accessed November 20, 2025, [https://docs.rs/wgpu/latest/wgpu/struct.PipelineLayout.html](https://docs.rs/wgpu/latest/wgpu/struct.PipelineLayout.html)  
31. Optimizing Compute Shaders for L2 Locality using Thread-Group ID Swizzling, accessed November 20, 2025, [https://developer.nvidia.com/blog/optimizing-compute-shaders-for-l2-locality-using-thread-group-id-swizzling/](https://developer.nvidia.com/blog/optimizing-compute-shaders-for-l2-locality-using-thread-group-id-swizzling/)  
32. WebGPU for Scalable Client-Side Aggregate Visualization \- Research Unit of Computer Graphics | TU Wien, accessed November 20, 2025, [https://www.cg.tuwien.ac.at/research/publications/2023/webGPU\_aggregateVis-2023/webGPU\_aggregateVis-2023-extended%20abstract.pdf](https://www.cg.tuwien.ac.at/research/publications/2023/webGPU_aggregateVis-2023/webGPU_aggregateVis-2023-extended%20abstract.pdf)  
33. Volume Profile Strategies | TrendSpider Learning Center, accessed November 20, 2025, [https://trendspider.com/learning-center/volume-profile-strategies/](https://trendspider.com/learning-center/volume-profile-strategies/)  
34. Beginners Guide to Volume Profile Part 1 \- Trader-Dale.com, accessed November 20, 2025, [https://www.trader-dale.com/beginners-guide-to-volume-profile-part-1-what-is-volume-profile/](https://www.trader-dale.com/beginners-guide-to-volume-profile-part-1-what-is-volume-profile/)  
35. A Complete Guide to Heatmaps | Atlassian, accessed November 20, 2025, [https://www.atlassian.com/data/charts/heatmap-complete-guide](https://www.atlassian.com/data/charts/heatmap-complete-guide)  
36. Rust Data Access Pattern : r/learnrust \- Reddit, accessed November 20, 2025, [https://www.reddit.com/r/learnrust/comments/174bz6d/rust\_data\_access\_pattern/](https://www.reddit.com/r/learnrust/comments/174bz6d/rust_data_access_pattern/)  
37. How to organize modules for a Rust web service \- The Rust Programming Language Forum, accessed November 20, 2025, [https://users.rust-lang.org/t/how-to-organize-modules-for-a-rust-web-service/107977](https://users.rust-lang.org/t/how-to-organize-modules-for-a-rust-web-service/107977)  
38. How to provide wgpu instance while using iced winit infrastructure? \- Learn, accessed November 20, 2025, [https://discourse.iced.rs/t/how-to-provide-wgpu-instance-while-using-iced-winit-infrastructure/1053](https://discourse.iced.rs/t/how-to-provide-wgpu-instance-while-using-iced-winit-infrastructure/1053)  
39. ndhistogram \- Rust \- Docs.rs, accessed November 20, 2025, [https://docs.rs/ndhistogram](https://docs.rs/ndhistogram)  
40. Workgroup sizes \- Arm GPU Best Practices Developer Guide, accessed November 20, 2025, [https://developer.arm.com/documentation/101897/latest/Compute-shading/Workgroup-sizes](https://developer.arm.com/documentation/101897/latest/Compute-shading/Workgroup-sizes)  
41. parquet::arrow \- Rust \- Docs.rs, accessed November 20, 2025, [https://docs.rs/parquet/latest/parquet/arrow/index.html](https://docs.rs/parquet/latest/parquet/arrow/index.html)  
42. Efficiently create 2d histograms from large datasets \- Stack Overflow, accessed November 20, 2025, [https://stackoverflow.com/questions/8805601/efficiently-create-2d-histograms-from-large-datasets](https://stackoverflow.com/questions/8805601/efficiently-create-2d-histograms-from-large-datasets)