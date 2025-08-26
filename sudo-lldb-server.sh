#!/bin/bash

# 在 VS Code 扩展目录中查找 lldb-server 的路径
# 通常位于 ~/.vscode/extensions/vadimcn.vscode-lldb-*/adapter/codelldb
# ~/.cursor/extensions/vadimcn.vscode-lldb-1.11.4/adapter/codelldb
LLDB_SERVER_PATH=$(find ~/.cursor/extensions/vadimcn.vscode-lldb-*/adapter -type f -name "codelldb" | head -n 1)

if [ -z "$LLDB_SERVER_PATH" ]; then
    echo "错误：无法在 VS Code 扩展目录中找到 'codelldb' (lldb-server)。" >&2
    exit 1
fi

echo "使用 sudo 启动调试服务器: $LLDB_SERVER_PATH" >&2

# 使用 sudo 执行找到的 lldb-server，并将所有传递给此脚本的参数 ($@) 附加到后面
exec sudo "$LLDB_SERVER_PATH" "$@"