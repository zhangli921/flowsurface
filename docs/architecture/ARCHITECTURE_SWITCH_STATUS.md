# 新架构启用状态说明

## 当前状态

❌ **新架构默认未启用**

## 启用方式

新架构通过**环境变量**控制，默认情况下是**关闭**的。

### 如何启用新架构

在运行程序前设置环境变量：

```bash
# 方式 1: 临时设置（当前终端会话）
export FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=true
./target/release/flowsurface

# 方式 2: 临时设置（单次运行）
FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=true ./target/release/flowsurface

# 方式 3: 永久设置（添加到 ~/.bashrc 或 ~/.profile）
echo 'export FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=true' >> ~/.bashrc
source ~/.bashrc
```

### 环境变量值

- `true` 或 `1`: 启用新架构
- 其他值或未设置: 使用原有架构（默认）

## 代码实现

在 `flowsurface/src/screen/dashboard.rs` 中：

```rust
pub fn init_unified_data_manager(&mut self) {
    // 检查环境变量
    let enabled = std::env::var("FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);  // 默认 false，即未启用
    
    if enabled && self.unified_data_manager.is_none() {
        let data_manager = std::sync::Arc::new(
            unified_data_manager::UnifiedDataManager::new(1000)
        );
        let chart_registry = chart_registry::ChartRegistry::new(data_manager.clone());
        
        self.unified_data_manager = Some(data_manager);
        self.chart_registry = Some(chart_registry);
        
        log::info!("UnifiedDataManager initialized");
    }
}
```

在 `Dashboard::new()` 中会自动调用：

```rust
pub fn new() -> Self {
    let mut dashboard = Dashboard {
        // ...
        unified_data_manager: None,  // 默认未启用
        chart_registry: None,
    };
    
    // 尝试初始化新架构（如果环境变量启用）
    dashboard.init_unified_data_manager();
    
    dashboard
}
```

## 验证是否启用

程序启动时，如果新架构已启用，会在日志中看到：

```
INFO: UnifiedDataManager initialized
```

如果没有看到这条日志，说明新架构未启用，程序使用原有架构运行。

## 架构切换说明

### 原有架构（默认）

- ✅ 完全正常工作
- ✅ 所有功能正常
- ✅ 数据流：每个图表独立管理数据

### 新架构（需要环境变量启用）

- ✅ 向后兼容
- ✅ 全局数据管理和缓存
- ✅ 数据去重和共享
- ✅ 可选的增强功能

## 建议

1. **当前阶段**: 保持默认（原有架构），确保稳定性
2. **测试阶段**: 设置环境变量启用新架构进行测试
3. **生产阶段**: 根据测试结果决定是否默认启用

## 检查当前状态

运行程序时检查日志：

```bash
# 启用新架构
FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=true ./target/release/flowsurface 2>&1 | grep -i "unified"

# 应该看到：
# INFO: UnifiedDataManager initialized
```

如果没有设置环境变量，不会看到这条日志，程序使用原有架构。

