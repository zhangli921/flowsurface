# 项目阶段总结与下一步计划

## 一、工作总结与架构对齐

### 1.1、工作目标
在上阶段的工作中，核心目标是解决项目因大规模重构，从旧版 `iced` `canvas` 渲染机制转向新版 `shader` 渲染机制所导致的大量编译问题，为后续功能的开发和集成奠定坚实基础。

### 1.2、关键行动与成果
通过系统性的问题诊断与修复，我采取了以下关键行动：

1.  **解决了核心架构冲突：** 成功移除了对 `iced::widget::canvas` 中非线程安全 `RefCell` 实现的 `Cache` 结构体的依赖，这是导致 `Sync` trait 编译错误的关键障碍。
2.  **解耦旧渲染系统：** 全面清除了代码库中对旧版 `canvas` 绘图模式的依赖，涉及的文件包括：
    *   移除了 `KlineChart` 和 `HeatmapChart` 中对 `canvas::Program` trait 的实现，使其职责回归到纯粹的数据和状态管理。
    *   临时性地将 `scale` 模块（负责坐标轴标签）和 `indicator` 模块（负责指标图绘制）中与 `canvas` 相关的代码进行了存根化处理，以确保主项目能顺利编译。这为后续在新渲染架构下重新实现这些功能做好了准备。
    *   彻底移除了旧的 `Caches` 结构体，该结构体是旧 `canvas` 缓存机制的核心。
3.  **适配 `iced` API 变更：** 针对 `iced` 库在 `shader` 和 `widget` API 方面发生的重大变化，进行了以下适配：
    *   修正了 `shader::Program` trait 中 `update` 和 `draw` 方法的函数签名，使其与 `iced` 的最新版本API保持一致。
    *   为 `ChartRenderer` 结构体添加了 `shader::Primitive` trait 的占位符实现，从而满足了 `UnifiedChartProgram` 对渲染基元类型 (`Primitive`) 的类型约束。
4.  **修复数据流与类型系统问题：**
    *   修正了 `ViewState` 和 `ChartState` 结构体拆分后，对图表状态字段访问不一致的问题，统一了访问模式（例如，将 `chart.field` 修正为 `chart.state.field`）。
    *   通过实现 `From<exchange::Kline> for data::kline::KLine` trait，解决了不同模块间 `KLine` 数据结构转换的编译错误。
5.  **解决了边缘错误：** 修复了 `logger.rs` 文件中因错误处理 `From` trait 未实现而导致的编译问题，确保了项目在更低层级的稳定性。

### 1.3、架构对齐总结

通过上述工作，项目已成功过渡到一个可编译、且与总体架构设计高度对齐的状态。

*   **核心渲染架构已就位：** 实现了从旧 `canvas` 到新 `shader` 渲染范式的根本性转变，构建了 `UnifiedChartProgram` 和 `ChartRenderer` 这样的核心结构。
*   **清理了遗留障碍：** 移除了所有与旧 `canvas` 系统相关联的、阻碍新架构推进的遗留代码和概念。
*   **为未来开发扫清障碍：** 项目目前已具备在新 `iced` `shader` 渲染框架下，继续开发和集成高性能功能的条件。

### 二、下一步计划

鉴于当前代码已可顺利编译，下一步我们将聚焦于逐步恢复和实现核心功能，使图表能够正常工作。我们将遵循“先数据，后渲染，再交互”的原则：

1.  **实现 Session Volume Profile (S-VP) 数据流：**
    *   **目标：** 确保 GPGPU 计算出的 SVP 数据能被主应用正确接收和存储。
    *   **任务：**
        *   在 `flowsurface/src/main.rs` 中，实现对 `Message::ComputeVp` 和 `Message::VpComputed(VolumeProfile)` 消息的处理逻辑。
        *   当图表需要SVP数据时，会通过 `iced::Command` 触发 GPGPU 计算管线 (`VpComputePipeline::run_aggregation`)。
        *   计算完成后，通过 `VpComputed` 消息将结果 (`VolumeProfile`) 传回 `main.rs`，并更新 `KlineChart` 内部的 `vp_data` 状态。

2.  **实现统一渲染器的绘图逻辑：**
    *   **目标：** 在屏幕上绘制 K 线和 SVP。
    *   **任务：**
        *   填充 `ChartRenderer` 的 `draw` 方法（位于 `flowsurface/src/chart/renderer.rs`），使其作为渲染的总调度器。
        *   实现 `flowsurface/src/chart/kline_renderer.rs` 中的 `KlineRenderer::draw` 方法，负责绘制 K 线图的基本元素。
        *   实现 `flowsurface/src/chart/svp_renderer.rs` 中的 `SvpRenderer::draw` 方法，利用 WGPU 实例渲染技术高效绘制 SVP 筹码峰。

3.  **实现用户交互功能：**
    *   **目标：** 恢复图表的平移、缩放等基本交互功能。
    *   **任务：** 填充 `UnifiedChartProgram::update` 方法（位于 `flowsurface/src/chart/renderer.rs`），根据新的 `iced` 事件模型，实现对用户输入（如鼠标拖拽、滚轮滚动）的响应，更新图表的平移和缩放状态。

我将从“实现 Session Volume Profile (S-VP) 数据流”开始着手。