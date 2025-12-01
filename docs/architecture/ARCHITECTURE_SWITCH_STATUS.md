# 新架构启用状态说明

## 当前状态

✅ **新架构默认已启用**

## 启用/禁用方式

新架构默认启用，可以通过**环境变量**控制。

### 如何禁用新架构

如果需要禁用新架构（回退到原有架构），在运行程序前设置环境变量：

```bash
# 方式 1: 临时设置（当前终端会话）
export FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=false
./target/release/flowsurface

# 方式 2: 临时设置（单次运行）
FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=false ./target/release/flowsurface

# 方式 3: 永久设置（添加到 ~/.bashrc 或 ~/.profile）
echo 'export FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=false' >> ~/.bashrc
source ~/.bashrc
```

### 环境变量值

- 未设置或 `true` 或 `1`: 启用新架构（默认）
- `false` 或 `0` 或 `no`: 禁用新架构，使用原有架构

## 代码实现

在 `flowsurface/src/screen/dashboard.rs` 中：

```rust
pub fn init_unified_data_manager(&mut self) {
    // 检查环境变量（默认启用，除非明确设置为 false）
    let enabled = std::env::var("FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER")
        .map(|v| v != "false" && v != "0" && v != "no")
        .unwrap_or(true);  // 默认 true，即启用
    
    if enabled && self.unified_data_manager.is_none() {
        let data_manager = std::sync::Arc::new(
            unified_data_manager::UnifiedDataManager::new(1000)
        );
        let chart_registry = chart_registry::ChartRegistry::new(data_manager.clone());
        
        self.unified_data_manager = Some(data_manager);
        self.chart_registry = Some(chart_registry);
        
        log::info!("UnifiedDataManager initialized (enabled by default)");
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

### 新架构（默认启用）

- ✅ 全局数据管理和缓存
- ✅ 数据去重和共享
- ✅ 向后兼容原有功能
- ✅ 自动图表注册和生命周期管理

### 原有架构（通过环境变量禁用新架构后使用）

- ✅ 完全正常工作
- ✅ 所有功能正常
- ✅ 数据流：每个图表独立管理数据

## 建议

1. **当前阶段**: 新架构已默认启用，经过测试验证
2. **如遇问题**: 可通过环境变量 `FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=false` 回退到原有架构
3. **性能优化**: 新架构提供数据共享和去重，减少重复下载

## 检查当前状态

运行程序时检查日志：

```bash
# 默认启用新架构
./target/release/flowsurface 2>&1 | grep -i "unified"

# 应该看到：
# INFO: UnifiedDataManager initialized (enabled by default)
```

如果看到 `UnifiedDataManager disabled via environment variable`，说明新架构已被禁用，程序使用原有架构。

