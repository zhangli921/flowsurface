# 使用新架构启动程序

## 启动命令

```bash
cd /home/zhangli/Develop/Biance/flow_surface/flowsurface
FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=true ./target/release/flowsurface
```

## 验证新架构已启用

程序启动时，在日志中查找：

```
INFO: UnifiedDataManager initialized
```

如果看到这条日志，说明新架构已成功启用。

## 当前状态

✅ **程序已启动（进程 ID: 42061）**
✅ **环境变量已设置: FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=true**

## 检查程序状态

```bash
# 检查进程是否运行
ps aux | grep flowsurface | grep -v grep

# 检查进程详细信息
ps -p <PID> -o pid,cmd,etime,rss,vsz
```

## 停止程序

```bash
# 找到进程 ID
ps aux | grep flowsurface | grep -v grep

# 停止程序
kill <PID>

# 或者强制停止
kill -9 <PID>
```

## 新架构功能

启用新架构后，程序将具备：

1. ✅ **全局数据管理**
   - 统一的数据缓存
   - 数据去重
   - 数据共享

2. ✅ **图表注册**
   - 自动注册图表实例
   - 生命周期管理

3. ✅ **数据订阅**
   - 图表可以订阅数据更新
   - 自动数据分发

## 注意事项

- 新架构与原有架构完全兼容
- 如果新架构出现问题，可以移除环境变量回退到原有架构
- 新架构是渐进式启用的，不影响现有功能


