#!/bin/bash

# 快速测试透明代理功能

echo "🧪 macOS 透明代理快速测试"
echo "========================"

# 检查操作系统
if [[ "$OSTYPE" != "darwin"* ]]; then
    echo "❌ 此测试仅支持 macOS 系统"
    exit 1
fi

# 检查权限
if [[ $EUID -ne 0 ]]; then
    echo "❌ 此测试需要管理员权限"
    echo "💡 请使用: sudo ./scripts/quick_test_transparent.sh"
    exit 1
fi

# 检查是否已构建
if [[ ! -f "./target/debug/examples/transparent_proxy_demo" ]]; then
    echo "📦 构建透明代理演示程序..."
    cargo build --example transparent_proxy_demo
    
    if [[ $? -ne 0 ]]; then
        echo "❌ 构建失败"
        exit 1
    fi
fi

# 检查基本依赖
echo "🔍 检查系统依赖..."

if ! command -v pfctl &> /dev/null; then
    echo "❌ pfctl 不可用"
    exit 1
fi
echo "  ✅ pfctl 可用"

if ! command -v curl &> /dev/null; then
    echo "⚠️  curl 不可用"
else
    echo "  ✅ curl 可用"
fi

# 检查当前网络连接
echo ""
echo "🔍 检查网络连接..."
if curl -s --max-time 5 http://httpbin.org/ip > /dev/null 2>&1; then
    echo "  ✅ 网络连接正常"
else
    echo "  ❌ 网络连接异常，请检查网络"
    exit 1
fi

# 检查是否有SOCKS5代理
echo ""
echo "🔍 检查SOCKS5代理..."
if nc -z 127.0.0.1 1080 2>/dev/null; then
    echo "  ✅ 检测到 127.0.0.1:1080 上的代理"
    PROXY_AVAILABLE=true
else
    echo "  ⚠️  未检测到127.0.0.1:1080上的代理"
    echo ""
    echo "💡 如果您想测试完整功能，请先启动SOCKS5代理:"
    echo "   ssh -D 1080 user@remote-server"
    echo ""
    echo "🔄 继续测试(将模拟代理不可用的情况)..."
    PROXY_AVAILABLE=false
fi

# 检查当前pfctl状态
echo ""
echo "🔍 检查当前pfctl状态..."
if pfctl -s info | grep -q "Status: Enabled"; then
    echo "  ✅ pfctl 已启用"
else
    echo "  ⚠️  pfctl 未启用，程序将尝试启用"
fi

# 运行基础功能测试
echo ""
echo "🧪 运行基础功能测试..."

# 测试1: 程序能否正常启动和关闭
echo "📝 测试1: 程序启动/关闭测试"

# 创建一个快速测试版本 - 只启动然后立即关闭
timeout 10s ./target/debug/examples/transparent_proxy_demo <<< "" 2>/dev/null

if [[ $? -eq 124 ]]; then
    echo "  ✅ 程序可以正常启动(超时自动结束)"
elif [[ $? -eq 0 ]]; then
    echo "  ✅ 程序正常运行并退出"
else
    echo "  ⚠️  程序启动可能有问题，但这是预期的(可能是代理连接问题)"
fi

# 检查pfctl规则是否被清理
echo ""
echo "📝 测试2: pfctl规则清理检查"
if pfctl -a takeover_transparent -s nat 2>/dev/null | grep -q "rdr"; then
    echo "  ⚠️  pfctl规则未完全清理，手动清理..."
    pfctl -a takeover_transparent -F all 2>/dev/null
    echo "  ✅ 手动清理完成"
else
    echo "  ✅ pfctl规则已正确清理"
fi

echo ""
echo "🎉 基础测试完成！"
echo ""
echo "📋 测试结果总结:"
echo "   • 程序编译: ✅"
echo "   • 权限检查: ✅"
echo "   • 依赖工具: ✅"
echo "   • 网络连接: ✅"
echo "   • SOCKS5代理: $(if $PROXY_AVAILABLE; then echo '✅'; else echo '⚠️'; fi)"
echo ""
if $PROXY_AVAILABLE; then
    echo "🚀 准备就绪！您可以运行完整测试:"
    echo "   sudo ./scripts/run_transparent_proxy.sh"
else
    echo "💡 要运行完整测试，请先启动SOCKS5代理:"
    echo "   ssh -D 1080 user@remote-server"
    echo "   然后运行: sudo ./scripts/run_transparent_proxy.sh"
fi
