#!/bin/bash

# 默认包含所有文件和目录
include_dirs=()
include_files=()
include_patterns=()

# 解析命令行参数
while [[ $# -gt 0 ]]; do
    case "$1" in
        --include-dir)
            include_dirs+=("$2")
            shift 2
            ;;
        --include-file)
            include_files+=("$2")
            shift 2
            ;;
        --include-pattern)
            include_patterns+=("$2")
            shift 2
            ;;
        *)
            echo "未知选项: $1"
            exit 1
            ;;
    esac
done

# 构建find包含选项
find_includes=()
for dir in "${include_dirs[@]}"; do
    find_includes+=(-o -path "./$dir/*")
done

for file in "${include_files[@]}"; do
    find_includes+=(-o -path "./$file")
done

for pattern in "${include_patterns[@]}"; do
    find_includes+=(-o -name "$pattern")
done

# 如果有包含选项，则构建完整查找条件
if [ ${#find_includes[@]} -gt 0 ]; then
    # 移除第一个"-o"
    find_includes=("${find_includes[@]:1}")
    find_command=(find . -type f \( "${find_includes[@]}" \))
else
    # 没有包含选项时，包含所有文件
    find_command=(find . -type f)
fi

# 使用find命令查找文件
"${find_command[@]}" | while IFS= read -r file; do
    # 去除文件路径开头的"./"
    relative_path="${file#./}"
    
    # 打印文件名（相对路径）后跟冒号
    echo "$relative_path:"
    
    # 打印文件内容
    cat -- "$file"
    
    # 在文件之间添加一个空行
    echo
done

