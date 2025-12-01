# 日志文件位置说明

## 日志文件路径

### Release 模式（默认）

日志文件位置取决于环境变量 `FLOWSURFACE_DATA_PATH`：

**如果设置了 `FLOWSURFACE_DATA_PATH`：**
```
$FLOWSURFACE_DATA_PATH/flowsurface-current.log
```

**如果未设置（默认）：**
```
~/.local/share/flowsurface/flowsurface-current.log
```

在 Linux 系统上，完整路径通常是：
```
/home/<用户名>/.local/share/flowsurface/flowsurface-current.log
```

### Debug 模式

在 Debug 模式下，日志会输出到 **标准输出 (stdout)**，而不是文件。

## 日志文件命名

- **当前日志**: `flowsurface-current.log`
- **上一次日志**: `flowsurface-previous.log`（程序启动时会自动重命名）

## 日志格式

日志格式为：
```
HH:MM:SS.mmm:LEVEL -- 消息内容
```

例如：
```
12:10:05.123:INFO -- UnifiedDataManager initialized
12:10:05.456:DEBUG -- Processing 100 footprints with 64 total price levels
```

## 查看日志

### 方式 1: 直接查看日志文件

```bash
# 查看完整日志
cat ~/.local/share/flowsurface/flowsurface-current.log

# 实时查看日志（tail -f）
tail -f ~/.local/share/flowsurface/flowsurface-current.log

# 查看最后 50 行
tail -n 50 ~/.local/share/flowsurface/flowsurface-current.log

# 搜索特定内容（例如 UnifiedDataManager）
grep -i "unified\|initialized" ~/.local/share/flowsurface/flowsurface-current.log
```

### 方式 2: 使用环境变量指定路径

```bash
# 设置自定义日志路径
export FLOWSURFACE_DATA_PATH=/tmp/my_logs
./target/release/flowsurface

# 日志将写入: /tmp/my_logs/flowsurface-current.log
```

### 方式 3: Debug 模式（输出到终端）

```bash
# Debug 模式，日志输出到终端
cargo run

# 或者设置 RUST_LOG 环境变量控制日志级别
RUST_LOG=debug cargo run
```

## 验证新架构是否启用

在日志文件中搜索：

```bash
grep -i "UnifiedDataManager initialized" ~/.local/share/flowsurface/flowsurface-current.log
```

如果找到这条日志，说明新架构已成功启用。

## 日志级别控制

可以通过环境变量 `RUST_LOG` 控制日志级别：

```bash
# 只显示 Info 及以上级别（默认）
RUST_LOG=info ./target/release/flowsurface

# 显示 Debug 及以上级别
RUST_LOG=debug ./target/release/flowsurface

# 只显示 Error
RUST_LOG=error ./target/release/flowsurface
```

## 日志文件大小限制

- 最大日志文件大小: **50 MB**
- 如果超过限制，程序会终止并记录 FATAL 错误

## 日志轮转

程序启动时会自动进行日志轮转：
1. 删除旧的 `flowsurface-previous.log`（如果存在）
2. 将当前的 `flowsurface-current.log` 重命名为 `flowsurface-previous.log`
3. 创建新的 `flowsurface-current.log`

## 常见问题

### Q: 找不到日志文件？

**A:** 可能的原因：
1. 程序还未运行过（日志文件在首次运行时创建）
2. 使用的是 Debug 模式（日志输出到终端，不写入文件）
3. 使用了自定义的 `FLOWSURFACE_DATA_PATH` 路径

### Q: 如何查看实时日志？

**A:** 使用 `tail -f`：
```bash
tail -f ~/.local/share/flowsurface/flowsurface-current.log
```

### Q: 如何清空日志？

**A:** 
```bash
# 清空当前日志
> ~/.local/share/flowsurface/flowsurface-current.log

# 或删除日志文件（程序会重新创建）
rm ~/.local/share/flowsurface/flowsurface-current.log
```

## 快速检查命令

```bash
# 检查日志文件是否存在
ls -lh ~/.local/share/flowsurface/flowsurface-current.log

# 查看最后 20 行日志
tail -n 20 ~/.local/share/flowsurface/flowsurface-current.log

# 搜索新架构初始化日志
grep -i "unified" ~/.local/share/flowsurface/flowsurface-current.log

# 实时监控日志
tail -f ~/.local/share/flowsurface/flowsurface-current.log | grep -i "unified\|error\|warn"
```

