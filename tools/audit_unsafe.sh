#!/usr/bin/env bash
# ⚠ DEPRECATED (B01-10, 2026-08-19)
#
# Edgine/Edgine Privileged Unsafe 块 SAFETY 注释自动审计 (Bash 实现).
#
# 本脚本已废弃, 统一使用 Python 实现:
#   python3 tools/audit_unsafe.py                  # 人类可读
#   python3 tools/audit_unsafe.py --machine        # TSV
#   python3 tools/audit_unsafe.py --missing-only   # 只列缺 SAFETY
#   python3 tools/audit_unsafe.py --summary        # 统计
#
# 废弃原因 (B01-10):
#   1. 原脚本 `xargs bash -c 'scan_unsafe "$@"'` 调用方式导致函数不继承,
#      工具实测返 0 但零输出 (scan_unsafe 函数未定义).
#   2. 工具精度低于 Python 版 (5 行窗口 vs 60 行窗口 + 严格 SAFETY 匹配).
#   3. Python 版支持 --summary / --machine / --missing-only 等更多输出格式.
#
# 保留本脚本的过渡说明: 仍可调用, 但会转发到 Python 版并打印废弃警告.
# CI/文档引用清理: scripts/requirements.sh 与 tools/auto_fill_safety.py
# 已切换至 Python 版. 历史调用方请迁移.
#
# 原 Bash 用法 (保留为参考):
#   bash tools/audit_unsafe.sh                  # 人类可读表格
#   bash tools/audit_unsafe.sh --machine        # TSV (便于管道)
#   bash tools/audit_unsafe.sh --missing-only   # 只列缺 SAFETY 的
#   bash tools/audit_unsafe.sh --summary        # 统计数字
#
# 退出码 (与 Python 版一致):
#   0 = 审计完成
#   2 = 内部错误

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

# B01-10 修复: 转发到 Python 实现 (避免 xargs bash -c 函数不继承问题)
echo "⚠ DEPRECATED: tools/audit_unsafe.sh 已废弃, 转发到 Python 版 tools/audit_unsafe.py" >&2
echo "  新用法: python3 tools/audit_unsafe.py [OPTIONS]" >&2
echo "  本 Bash 调用: tools/audit_unsafe.sh $*" >&2
echo "" >&2

# 参数翻译: --missing-only / --machine / --summary 直接透传
exec python3 "$PROJECT_ROOT/tools/audit_unsafe.py" "$@"

if [ ! -d "$FW_DIR" ]; then
    echo "ERROR: $FW_DIR 不存在" >&2
    exit 2
fi

# 主扫描: 找出所有 unsafe { ... } 块的起始行
# 规则:
#   1. `unsafe { ` 或 `unsafe{`  (块起点)
#   2. `unsafe fn ` / `unsafe impl` / `unsafe trait` / `unsafe extern`  (声明)
#   3. 排除 注释 行 (`// unsafe`) 和 字符串中的 unsafe

# 输出 TSV: file<TAB>line<TAB>kind<TAB>has_safety<TAB>context
scan_unsafe() {
    local file="$1"

    # 用 ripgrep 的多行支持, 找出 unsafe 关键字出现的行
    # 然后检查它前 5 行内是否有 `// SAFETY:` 或 `//SAFETY:`
    grep -nE '\bunsafe\b' "$file" 2>/dev/null | \
    awk -F: -v fname="$file" '
        {
            line_num = $1
            # 取出该行内容 (原始 line 之后的所有 : 重新 join, 因为 awk -F: 只切首个 :)
            code = $0
            sub(/^[^:]+:/, "", code)

            # 排除 纯注释 行 (该行以 // 或 * 或 //! 或 /// 开头)
            stripped = code
            sub(/^[[:space:]]+/, "", stripped)
            if (stripped ~ /^\/\//) next
            if (stripped ~ /^\/\*\*?/) next
            if (stripped ~ /^\*/) next

            # 排除 `unsafe` 出现在 字符串 ("...") 中的情况 (粗略)
            # 这种情况极罕见, 暂不过滤

            # 判断 kind
            kind = "unknown"
            if (code ~ /unsafe[[:space:]]*\{/)            kind = "block"
            else if (code ~ /unsafe[[:space:]]+fn[[:space:]]/)        kind = "fn"
            else if (code ~ /unsafe[[:space:]]+impl/)                  kind = "impl"
            else if (code ~ /unsafe[[:space:]]+trait/)                 kind = "trait"
            else if (code ~ /unsafe[[:space:]]+extern/)                kind = "extern"
            else                                            kind = "ref"

            # 检查上方 5 行内是否有 SAFETY 注释
            has_safety = "MISSING"
            for (i = line_num - 5; i < line_num; i++) {
                cmd = "sed -n " i "p " fname " 2>/dev/null"
                cmd | getline prev
                close(cmd)
                if (prev ~ /SAFETY[: ]/) {
                    has_safety = "OK"
                    break
                }
            }

            # 输出 TSV
            printf "%s\t%d\t%s\t%s\t%s\n", fname, line_num, kind, has_safety, code
        }
    '
}

# 收集所有结果
ALL=$(mktemp)
trap 'rm -f "$ALL"' EXIT

find "$FW_DIR" -name "*.rs" -type f -print0 | \
    xargs -0 -I {} bash -c 'scan_unsafe "$@"' _ {} >> "$ALL" 2>/dev/null

# 注意: 上面的 find 输出会带上 src/kernel/privileged/ 前缀, 统一去掉
ALL_REL=$(mktemp)
sed "s|$PROJECT_ROOT/||" "$ALL" > "$ALL_REL"

case "$MODE" in
    machine)
        # TSV 输出, 第一行表头
        printf "file\tline\tkind\tsafety\tcode\n"
        cat "$ALL_REL"
        ;;

    missing)
        # 只输出缺 SAFETY 的
        printf "file\tline\tkind\tcode\n"
        awk -F'\t' '$4 == "MISSING" { printf "%s\t%s\t%s\t%s\n", $1, $2, $3, $5 }' "$ALL_REL"
        ;;

    summary)
        total=$(wc -l < "$ALL_REL" | tr -d ' ')
        missing=$(awk -F'\t' '$4 == "MISSING"' "$ALL_REL" | wc -l | tr -d ' ')
        ok=$(awk -F'\t' '$4 == "OK"' "$ALL_REL" | wc -l | tr -d ' ')
        blocks=$(awk -F'\t' '$3 == "block"' "$ALL_REL" | wc -l | tr -d ' ')
        fns=$(awk -F'\t' '$3 == "fn"' "$ALL_REL" | wc -l | tr -d ' ')
        impls=$(awk -F'\t' '$3 == "impl"' "$ALL_REL" | wc -l | tr -d ' ')

        echo "=== Privileged Unsafe 块 SAFETY 注释基线 ==="
        echo "扫描目录:     $FW_DIR"
        echo "扫描时间:     $(date -Iseconds)"
        echo ""
        echo "  unsafe 引用总数:  $total"
        echo "  ├─ 块 (unsafe { ... }):  $blocks"
        echo "  ├─ 函数 (unsafe fn):    $fns"
        echo "  └─ impl:                $impls"
        echo ""
        echo "  SAFETY 注释覆盖:  $ok / $total  ($(( ok * 100 / (total == 0 ? 1 : total) ))%)"
        echo "  缺 SAFETY:        $missing"
        echo ""
        echo "  验收标准: 缺 SAFETY = 0"
        if [ "$missing" -eq 0 ]; then
            echo "  ✅ 全部已覆盖"
        else
            echo "  ❌ 仍有 $missing 处需补 SAFETY 注释"
            echo ""
            echo "  按文件 Top 5 (缺 SAFETY 最多):"
            awk -F'\t' '$4 == "MISSING" { print $1 }' "$ALL_REL" | sort | uniq -c | sort -rn | head -5
        fi
        ;;

    human|*)
        # 人类可读表格
        echo "=== Privileged Unsafe 块 SAFETY 注释基线 ==="
        echo "扫描目录: $FW_DIR"
        echo "扫描时间: $(date -Iseconds)"
        echo ""
        printf "%-50s %6s %-8s %-10s\n" "FILE:LINE" "LINE" "KIND" "SAFETY"
        echo "------------------------------------------------------------------------------------------------"
        awk -F'\t' -v width=50 '
        {
            key = $1 ":" $2
            if (length(key) > width) {
                short = "..." substr(key, length(key) - width + 4)
            } else {
                short = key
            }
            printf "%-50s %6s %-8s %s\n", short, $2, $3, $4
        }' "$ALL_REL" | head -80

        echo ""
        echo "(仅显示前 80 行, 完整列表用 --machine 或 --missing-only)"
        echo ""
        bash "$0" --summary
        ;;
esac
