

# **第三阶段实施方案：GPGPU计算卸载与统一渲染架构（Session Volume Profile）**

## **I. 阶段目标与架构衔接**

### **1.1. 阶段核心目标**

第三阶段的目标是实现 **Session Volume Profile (S-VP) 筹码峰** 的高性能计算和渲染，并将此计算引擎推广为所有复杂聚合任务（包括 Footprint 的 **Intra-Bar Volume Profile (IBVP)**）的统一 GPGPU 解决方案。

* **计算卸载：** 将 Tick 数据的\*\*价格分桶（Price Bucketing）**和**成交量累加（Volume Accumulation）\*\*任务，从 CPU 完全卸载到 WGPU 计算着色器。  
* **性能目标：** 将数百万条 Tick 记录的聚合时间从潜在的数百毫秒（CPU 串行）优化到数十毫秒（GPU 并行）。  
* **统一渲染：** 通过 iced::widget::shader 机制，实现一个统一的 WGPU 渲染管线，高效绘制 K 线、S-VP 筹码峰和 IBVP 足迹图。

### **1.2. 架构衔接与数据输入**

本阶段直接消费第二阶段\*\*数据仲裁服务（Arbiter Service）\*\*提供的输入：

* **输入数据源：** HostStagingBuffer，一个包含可见时间范围内 **Struct-of-Arrays (SoA)** 布局的 Tick 数据切片。该切片在阻塞 I/O 线程中从 Mmap 文件中提取 1。  
* **计算驱动：** 用户缩放/平移 K 线图时，主 iced 运行时触发计算任务。

## **II. GPGPU 计算引擎实现 (vp\_compute\_engine 模块)**

核心逻辑被封装在 vp\_compute\_engine 模块中，该模块负责 WGPU 资源的生命周期管理和命令调度。

### **2.1. 核心数据契约：CPU/GPU 内存结构**

#### **A. 输入结构（Host Staging Buffer）**

此缓冲区是 Stage I Mmap Wrapper 的输出，包含价格和数量的连续数组。

| 字段名称 | 描述 | 内存布局 | 作用 |
| :---- | :---- | :---- | :---- |
| tick\_prices | &\[u64\] | **SoA** 连续数组 | GPU 读取 Tick 价格 |
| tick\_volumes | &\[f32\] | **SoA** 连续数组 | GPU 读取成交量 |

#### **B. 输出结构（GPU Dense Histogram）**

GPU 负责生成一个\*\*密集（Dense）\*\*的直方图数组。数组的索引代表价格桶，值代表累积成交量。

Code snippet

// WGSL Shader Output Buffer Structure  
// array\<atomic\<u32\>, N\>  
// N：价格桶总数 (取决于图表价格分辨率，例如 50,000 个桶)

### **2.2. WGPU 计算管线定义 (VpComputePipeline)**

VpComputePipeline 结构体负责管理 WGPU 设备、队列和管线资源。

| 资源组件 | WGPU 类型 | 作用 |
| :---- | :---- | :---- |
| device | wgpu::Device | GPU 抽象设备 |
| queue | wgpu::Queue | 命令提交队列 |
| compute\_pipeline | wgpu::ComputePipeline | 封装 WGSL 着色器和执行状态 3 |
| bind\_group\_layout | wgpu::BindGroupLayout | 定义输入/输出缓冲区和 Uniforms 的绑定契约 |

### **2.2. WGSL 计算着色器设计：原子累加核心**

为实现 S-VP 筹码峰所需的并行累加，WGSL（WebGPU Shading Language）内核必须使用 **原子操作（Atomic Operations）** 来安全地更新共享的直方图缓冲区，防止并行写入冲突。

Code snippet

// vp\_compute.wgsl  
// 输入: @group(0) @binding(0) read-only struct TickData {... }  
// 输出: @group(0) @binding(1) var\<storage, read\_write\> histogram: array\<atomic\<u32\>, 50000\>; 

\[\[stage(compute)\]\]  
fn main(\[\[builtin(global\_invocation\_id)\]\] global\_id: vec3\<u32\>) {  
    let tick\_index \= global\_id.x;  
    if (tick\_index \>= NUM\_TICKS) { return; }

    // 1\. 读取价格和数量  
    let price\_raw \= tick\_data.prices\[tick\_index\];  
    let volume \= tick\_data.volumes\[tick\_index\];

    // 2\. 价格分桶 (Binning)  
    // 根据图表缩放和平铺分辨率计算出价格桶索引  
    let price\_index \= compute\_price\_index(price\_raw, PRICE\_RESOLUTION); 

    // 3\. 原子累加 (Atomic Accumulation)  
    // 安全地将成交量累加到共享直方图数组的对应索引中  
    atomicAdd(\&histogram\[price\_index\], u32(volume));   
}

### **2.3. CPU 调度与异步回读 (run\_aggregation)**

run\_aggregation 函数是主异步运行时（Iced）调用 GPU 的核心入口。

1. **缓冲区分配与传输：**  
   * 创建 input\_buffer 和 output\_buffer。  
   * 使用 queue.write\_buffer 将主机侧的 HostStagingBuffer 高效上传到 input\_buffer。  
2. **调度计算（Dispatch）：**  
   * 编码 wgpu::ComputePass，绑定资源。  
   * 调用 pass.dispatch\_workgroups(num\_workgroups, 1, 1)。**工作组数量**必须根据 Tick 记录总数和预定的工作组大小（例如 64 或 128）精确计算，以最大化 GPU 占用率。  
3. **结果异步检索 (Mapping)：**  
   * 使用 output\_buffer.slice().map\_async(wgpu::MapMode::Read,...) 异步请求结果 5。  
   * **关键：** 异步映射是强制性的。它允许 CPU 在等待 GPU 完成计算时，继续处理 Iced 的 UI 渲染或其他后台任务，实现非阻塞 5。  
4. **CPU 后处理 (在回调中执行)：** 当 map\_async 完成时，回调函数将执行以下步骤：  
   * **密集转稀疏：** 遍历 GPU 返回的**密集**直方图，跳过零值，生成精简的 Vec\<SparseBar\> 列表。  
   * **金融指标计算：** 确定 Point of Control (POC) 和 Value Area (VA) 边界 6。  
   * **消息返回：** 将 Vec\<SparseBar\> 结果包装在 Message::VpComputeResult 中，返回给 iced 运行时。

## **III. 统一渲染架构集成 (vp\_renderer 模块)**

渲染必须集成到 flowsurface-rs 现有的 iced WGPU 渲染流程中，避免创建多个独立的渲染上下文。

### **3.1. 自定义 Iced Widget 封装**

* 在 src/chart/view.rs 中，实现 SessionVolumeProfileWidget 结构体，该结构体必须实现 iced::widget::shader::Custom 特性或等效的自定义 Widget 接口 8。这允许应用程序对底层 WGPU 渲染管线进行精确控制。

### **3.2. 统一 WGPU 渲染管线（Layered Rendering）**

所有图表元素（K线、S-VP、IBVP）都应在同一 WGPU **渲染通道（Render Pass）** 中作为不同的绘制调用或实例集进行处理，以实现性能优化 8。

1. **基础 K 线层：** 渲染管线的基础绘制 K 线图（OHLCV）。  
2. **S-VP 筹码峰层：** **强制使用实例渲染（Instancing）**。  
   * **实例缓冲区：** 将 CPU 后处理产生的 Vec\<SparseBar\> 上传到 GPU 的实例缓冲区。  
   * **单 Draw Call：** 使用 render\_pass.draw\_indexed(..., instances: 0..N)，其中 $N$ 是稀疏柱状条的数量 9。这通过一次绘制调用完成所有 S-VP 柱状条的渲染，消除了 CPU 绘制调用的开销 9。  
   * **Vertex Shader：** 读取每个实例的 SparseBar 数据（位置、宽度、颜色），计算其在屏幕上的最终几何形状。

### **3.3. IBVP (足迹图) 的集成策略**

足迹图内部的 IBVP 聚合结果（每个 K 线内部的体积分布）也应由 GPGPU 计算引擎产生。

* **IBVP 计算：** GPGPU 引擎执行 IBVP 聚合，将结果生成为 2D 纹理（Texture）或高度密集的实例集。  
* **渲染：** IBVP 渲染作为 K 线图的**叠加层**或**纹理着色层**在同一渲染通道中完成，实现高效的分层可视化 10。

## **IV. 鲁棒性与非偏离性检查**

### **4.1. 并发性保证**

* **计算隔离：** GPGPU 计算是异步的，不占用 tokio 异步运行时线程池的计算资源，完全隔离了计算密集型任务对 UI 的影响 12。  
* **数据共享：** GPGPU 计算完成后，通过 iced 消息机制（VpComputeResult）将数据结果传递给 UI 状态，遵循 iced 的单向数据流原则。

### **4.2. 性能验证与优化指导**

* **性能瓶颈识别：** GPGPU 性能的主要瓶颈将是数据从主机内存 (CPU) 传输到设备内存 (GPU) 的 PCIe 带宽 5。因此，Stage II 仅提取可见范围数据的策略至关重要。  
* **WGSL 优化：** 必须在 GPU 硬件上对 WGSL 内核进行性能分析（例如使用 Xcode 或 Nsight Tools），重点优化**工作组大小**和**内存访问模式**，确保内存合并和 L2 缓存命中率 13。

本方案确保了 Session Volume Profile (S-VP) 功能的集成将是高性能的，并遵循了 flowsurface-rs 现有 WGPU 架构的非阻塞设计原则。

#### **Works cited**

1. rust \- How to create and write to memory mapped files? \- Stack Overflow, accessed November 20, 2025, [https://stackoverflow.com/questions/28516996/how-to-create-and-write-to-memory-mapped-files](https://stackoverflow.com/questions/28516996/how-to-create-and-write-to-memory-mapped-files)  
2. Struct having vector of structs mmapped \- c++ \- Stack Overflow, accessed November 20, 2025, [https://stackoverflow.com/questions/46151543/struct-having-vector-of-structs-mmapped](https://stackoverflow.com/questions/46151543/struct-having-vector-of-structs-mmapped)  
3. ComputePipeline in iced::widget::shader::wgpu \- Rust \- Docs.rs, accessed November 20, 2025, [https://docs.rs/iced/latest/iced/widget/shader/wgpu/struct.ComputePipeline.html](https://docs.rs/iced/latest/iced/widget/shader/wgpu/struct.ComputePipeline.html)  
4. The Pipeline | Learn Wgpu, accessed November 20, 2025, [https://sotrh.github.io/learn-wgpu/beginner/tutorial3-pipeline/](https://sotrh.github.io/learn-wgpu/beginner/tutorial3-pipeline/)  
5. WebGPU Rendering: Part 11 Prefix Sum | by Matthew MacFarquhar | Medium, accessed November 20, 2025, [https://matthewmacfarquhar.medium.com/webgpu-rendering-part-11-prefix-sum-c26a32223f9f](https://matthewmacfarquhar.medium.com/webgpu-rendering-part-11-prefix-sum-c26a32223f9f)  
6. Volume Profile Strategies | TrendSpider Learning Center, accessed November 20, 2025, [https://trendspider.com/learning-center/volume-profile-strategies/](https://trendspider.com/learning-center/volume-profile-strategies/)  
7. Beginners Guide to Volume Profile Part 1 \- Trader-Dale.com, accessed November 20, 2025, [https://www.trader-dale.com/beginners-guide-to-volume-profile-part-1-what-is-volume-profile/](https://www.trader-dale.com/beginners-guide-to-volume-profile-part-1-what-is-volume-profile/)  
8. Render Pipelines in wgpu and Rust \- Ryosuke, accessed November 20, 2025, [https://whoisryosuke.com/blog/2022/render-pipelines-in-wgpu-and-rust](https://whoisryosuke.com/blog/2022/render-pipelines-in-wgpu-and-rust)  
9. Instancing | Learn Wgpu, accessed November 20, 2025, [https://sotrh.github.io/learn-wgpu/beginner/tutorial7-instancing/](https://sotrh.github.io/learn-wgpu/beginner/tutorial7-instancing/)  
10. High Dynamic Range Rendering | Learn Wgpu, accessed November 20, 2025, [https://sotrh.github.io/learn-wgpu/intermediate/tutorial13-hdr/](https://sotrh.github.io/learn-wgpu/intermediate/tutorial13-hdr/)  
11. WebGPU Textures, accessed November 20, 2025, [https://webgpufundamentals.org/webgpu/lessons/webgpu-textures.html](https://webgpufundamentals.org/webgpu/lessons/webgpu-textures.html)  
12. Where do computationally heavy parts go in async model of Rust, accessed November 20, 2025, [https://users.rust-lang.org/t/where-do-computationally-heavy-parts-go-in-async-model-of-rust/34881](https://users.rust-lang.org/t/where-do-computationally-heavy-parts-go-in-async-model-of-rust/34881)  
13. Workgroup sizes \- Arm GPU Best Practices Developer Guide, accessed November 20, 2025, [https://developer.arm.com/documentation/101897/latest/Compute-shading/Workgroup-sizes](https://developer.arm.com/documentation/101897/latest/Compute-shading/Workgroup-sizes)  
14. Optimizing Compute Shaders for L2 Locality using Thread-Group ID Swizzling, accessed November 20, 2025, [https://developer.nvidia.com/blog/optimizing-compute-shaders-for-l2-locality-using-thread-group-id-swizzling/](https://developer.nvidia.com/blog/optimizing-compute-shaders-for-l2-locality-using-thread-group-id-swizzling/)