# 架构设计评审与优化建议 (Phase 4+)

## 一、 现有架构评审

### 亮点
1.  **数据粒度至上 (Raw Tick First)**：以 Tick/L2 数据为核心，保障了系统的扩展性和回溯能力。
2.  **计算卸载 (GPGPU Offloading)**：利用 Compute Shader 处理 S-VP 和 IBVP 聚合，解决了 CPU 瓶颈。
3.  **高效内存布局 (SoA + Mmap)**：Structure of Arrays 配合 Mmap，最大化了 I/O 和内存访问效率。

### 潜在风险
1.  **PCIe 带宽瓶颈**：频繁上传大量 Tick 数据可能导致交互卡顿。
2.  **CPU-GPU 往返延迟**：`MapAsync` 回读机制引入了不必要的同步开销。
3.  **数据仲裁复杂性**：物理合并异构数据源容易引入边缘 Case Bug。

---

## 二、 优化建议 (Future Roadmap)

### 1. 显存带宽优化：GPU 驻留模式 (GPU Residency)
*   **当前问题**：每次 UI 交互（缩放/平移）都重新上传 Tick 数据。
*   **优化方案**：
    *   维护 GPU 端**环形缓冲区 (Ring Buffer)**。
    *   仅上传增量数据（新 Ticks）或一次性上传 Session 数据。
    *   计算时仅更新 Uniform Buffer 中的时间窗口索引，避免重复传输大块数据。

### 2. 渲染管线优化：间接绘制 (Indirect Drawing)
*   **当前问题**：`GPU计算 -> CPU回读 -> CPU提交渲染` 的流程效率低。
*   **优化方案**：
    *   Compute Shader 直接填充 `IndirectBuffer` 和 `InstanceBuffer`。
    *   使用 `draw_indexed_indirect` 直接由 GPU 发起绘制。
    *   消除 CPU 回读步骤，实现 GPU 内部的计算-渲染闭环。

### 3. 数据一致性：Lambda 架构思想
*   **当前问题**：Arbiter Service 强行合并数据源可能导致状态冲突。
*   **优化方案**：
    *   分离 **Speed Layer** (实时聚合) 和 **Batch Layer** (历史文件)。
    *   在渲染层通过时间轴动态选择数据源，而非在数据层物理合并。

### 4. 渲染解耦：独立交换链
*   **当前问题**：图表渲染受限于 `iced` 的主消息循环。
*   **优化方案**：
    *   图表区域使用独立的 `wgpu::Surface`。
    *   在独立线程中驱动渲染循环，仅通过消息与 UI 线程同步状态。

---

## 三、 执行策略

1.  **短期 (Phase 4)**：按原计划执行。优先打通数据流和基础渲染管线，保留 CPU 回读以方便调试。
2.  **中期 (Phase 5)**：实施 **GPU Residency**，解决大数据量下的拖拽卡顿问题。
3.  **长期**：探索 **Indirect Drawing** 和 **独立渲染线程**，追求极致帧率。

