#!/bin/bash

# macOS 透明代理启动脚本

set -e  # 遇到错误时退出

echo "🍎 macOS 透明代理启动脚本"
echo "========================"

# 检查操作系统
if [[ "$OSTYPE" != "darwin"* ]]; then
    echo "❌ 此脚本仅支持 macOS 系统"
    exit 1
fi

# 检查是否有root权限
if [[ $EUID -ne 0 ]]; then
    echo "❌ 此脚本需要管理员权限"
    echo "💡 请使用: sudo $0"
    exit 1
fi

# 检查依赖工具
echo "🔍 检查系统依赖..."

if ! command -v pfctl &> /dev/null; then
    echo "❌ pfctl 不可用"
    exit 1
fi
echo "  ✅ pfctl 可用"

if ! command -v curl &> /dev/null; then
    echo "⚠️  curl 不可用，将跳过curl测试"
else
    echo "  ✅ curl 可用"
fi

if ! command -v node &> /dev/null; then
    echo "⚠️  Node.js 不可用，将跳过Node.js测试"
else
    echo "  ✅ Node.js 可用"
fi

# 检查是否已有SOCKS5代理
echo ""
echo "🔍 检查SOCKS5代理..."
if nc -z 127.0.0.1 1080 2>/dev/null; then
    echo "  ✅ 检测到 127.0.0.1:1080 上的SOCKS5代理"
else
    echo "  ⚠️  未检测到127.0.0.1:1080上的代理"
    echo ""
    echo "💡 请先启动一个SOCKS5代理:"
    echo "   ssh -D 1080 user@remote-server"
    echo "   或使用其他SOCKS5代理工具"
    echo ""
    read -p "有代理可用了吗？按Enter继续，或Ctrl+C取消..."
fi

# 构建项目
echo ""
echo "🔨 构建项目..."
cargo build --example transparent_proxy_demo --release

if [[ $? -ne 0 ]]; then
    echo "❌ 构建失败"
    exit 1
fi

echo "  ✅ 构建成功"

# 运行透明代理演示
echo ""
echo "🚀 启动透明代理演示..."
echo "⚠️  注意: 此操作将修改系统网络规则"
echo ""

./target/release/examples/transparent_proxy_demo

echo ""
echo "✅ 透明代理演示完成"
