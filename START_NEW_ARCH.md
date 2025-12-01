# 使用新架构启动程序

## ✅ 程序已准备好使用新架构

## 启动方式

### 方式 1: 使用启动脚本（推荐）

```bash
cd /home/zhangli/Develop/Biance/flow_surface/flowsurface
./scripts/start_with_new_arch.sh
```

### 方式 2: 直接运行

```bash
cd /home/zhangli/Develop/Biance/flow_surface/flowsurface
FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=true ./target/release/flowsurface
```

### 方式 3: 后台运行

```bash
cd /home/zhangli/Develop/Biance/flow_surface/flowsurface
FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=true nohup ./target/release/flowsurface > /tmp/flowsurface.log 2>&1 &
```

## 验证新架构已启用

程序启动后，检查日志输出中是否有：

```
INFO: UnifiedDataManager initialized
```

**如果有这条日志，说明新架构已成功启用！**

## 检查程序状态

```bash
# 检查进程
ps aux | grep flowsurface | grep -v grep

# 查看日志（如果使用后台运行）
tail -f /tmp/flowsurface.log | grep -i "unified\|initialized"
```

## 新架构功能

启用新架构后，程序将具备：

1. ✅ **全局数据管理 (UnifiedDataManager)**
   - 统一的数据缓存机制
   - 自动数据去重
   - 多图表数据共享

2. ✅ **图表注册 (ChartRegistry)**
   - 自动注册图表实例
   - 生命周期管理
   - 订阅者管理

3. ✅ **数据订阅机制**
   - 图表可以订阅数据更新
   - 自动数据分发
   - 减少重复下载

## 停止程序

```bash
# 查找进程 ID
ps aux | grep flowsurface | grep -v grep

# 停止程序
kill <PID>

# 或使用 pkill
pkill -f flowsurface
```

## 回退到原有架构

如果新架构出现问题，只需不设置环境变量即可：

```bash
# 使用原有架构（默认）
./target/release/flowsurface
```

## 注意事项

- ✅ 新架构与原有架构完全兼容
- ✅ 新架构是渐进式启用的，不影响现有功能
- ✅ 如果新架构出现问题，可以随时回退
- ✅ 所有图表类型都已支持新架构（Candles, Footprint, Heatmap, DOM）

## 当前状态

✅ **环境变量已配置**
✅ **启动脚本已创建**
✅ **程序已编译完成**

**现在可以启动程序测试新架构了！**

