#!/bin/bash

# 获取当前脚本所在的目录
SOURCE_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )"
# 获取项目根目录（脚本在scripts/目录下）
PROJECT_ROOT="$(dirname "$SOURCE_DIR")"
# 获取当前登录的用户名
CURRENT_USER=$(stat -f%Su /dev/console)
# 定义程序路径（使用debug构建）
PROGRAM_PATH="$PROJECT_ROOT/target/debug/examples/test_proxy_manager"
# 定义 sudoers 规则
SUDOERS_RULE="$CURRENT_USER ALL=(ALL) NOPASSWD: $PROGRAM_PATH"
SUDOERS_FILE="/etc/sudoers.d/takeover-nopasswd" # 使用 sudoers.d 更安全

echo "正在配置 takeover..."

# 1. 检查程序是否存在
if [ ! -f "$PROGRAM_PATH" ]; then
    echo "错误: 未找到 takeover 程序，请先运行 'cargo build'"
    echo "程序应该位于: $PROGRAM_PATH"
    exit 1
fi

echo "找到程序: $PROGRAM_PATH"

# 2. 配置 sudoers 免密规则
echo "为了让 takeover 在运行时无需输入密码，需要为您配置一条 sudo 规则。"
echo "系统将提示您输入密码来完成此项设置。"

# 使用 /etc/sudoers.d/ 目录是更现代、更安全的方式，避免直接修改 /etc/sudoers
# 检查文件是否存在，不存在则创建
if [ ! -f "$SUDOERS_FILE" ]; then
    echo "创建 sudoers 配置文件..."
    # 使用 sudo tee 来写入需要 root 权限的文件
    echo "$SUDOERS_RULE" | sudo tee "$SUDOERS_FILE" > /dev/null
    # 设置正确的文件权限
    sudo chmod 440 "$SUDOERS_FILE"
    echo "配置完成。"
else
    echo "sudoers 规则已存在，无需更改。"
fi

echo "安装成功！现在您可以直接运行 'takeover'。"