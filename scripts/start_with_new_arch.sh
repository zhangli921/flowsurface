#!/bin/bash
# 使用新架构启动 flowsurface 程序

cd "$(dirname "$0")/.." || exit 1

echo "=========================================="
echo "启动 flowsurface (新架构)"
echo "=========================================="
echo ""
echo "环境变量: FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=true"
echo ""

# 设置环境变量
export FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=true

# 检查可执行文件是否存在
if [ ! -f "./target/release/flowsurface" ]; then
    echo "错误: 可执行文件不存在，请先编译程序"
    echo "运行: cargo build --release"
    exit 1
fi

echo "启动程序..."
echo "提示: 程序启动后，在日志中查找 'UnifiedDataManager initialized' 来确认新架构已启用"
echo ""

# 启动程序
./target/release/flowsurface

