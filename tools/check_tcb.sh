#!/bin/bash
# tools/check_tcb.sh — functions/ 零 unsafe 的快速预检 (F1)
#
# 历史名保留: 本脚本原名 "TCB 统计" (Phase 0.5 产物), 曾输出 privileged/functions
# 行数与 TCB 占比. TCB 度量职责现已由 scripts/audit_tcb_ratio.py 独占; 本脚本只剩
# 一件事: 在双架构构建 / clippy / 审计腿之前 (秒级) 判定 functions/ 是否出现 unsafe.
# 沿用旧文件名是因为仓内引用点 18 处 (ci/audit.sh / scripts/requirements.sh / docs),
# 改名收益小于同步成本.
#
# 权威判定 (本脚本是快速预检, 不是权威):
#   - 编译期: src/kernel/functions/mod.rs 的 #![deny(unsafe_code)]
#   - 审计期: scripts/audit_functions_boundary.py (F1+F2, ci/audit.sh 步骤 0.5k)
# 近似之处: 只做行级词法过滤 (状态机跳过 // 行注释与 /* */ 块注释), 不解析字符串
# 字面量; 字符串里出现 unsafe 会误报, 但真实 unsafe 代码不会漏报.
#
# 用法: ./tools/check_tcb.sh
# 退出码: 0 = functions/ 无 unsafe; 1 = 发现 unsafe 或扫描范围异常

set -euo pipefail
cd "$(dirname "$0")/.."

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

FUNC_DIR="src/kernel/functions"

if [ ! -d "$FUNC_DIR" ]; then
    echo -e "${RED}FAIL${NC}: 找不到 $FUNC_DIR (脚本不在仓库根?)"
    exit 1
fi

# 扫描面: 仅 .rs; 排除 vendored smoltcp 与构建产物目录 (target/), 后者避免
# src/kernel/other/target/ 一类产物进入统计. 与 scripts/audit_functions_boundary.py
# 的 VENDORED_EXCLUDE 保持一致.
SV_FILES=$(find "$FUNC_DIR" -name '*.rs' \
    -not -path "*/smoltcp/*" -not -path "*/target/*" | wc -l)

# 扫描范围为 0 视为失效, 不能静默 PASS.
if [ "$SV_FILES" -eq 0 ]; then
    echo -e "${RED}FAIL${NC}: $FUNC_DIR 下未扫描到任何 .rs (扫描范围为空)"
    exit 1
fi

# 状态机: 跳过 // 行注释与跨行 /* */ 块注释后匹配 unsafe (词边界, 不匹配 unsafe_code).
# SIMPLIFIED: 不处理字符串字面量与原始字符串; 影响 = 字符串含 "unsafe" 时误报;
# 权威判定以编译期 deny 与 audit_functions_boundary.py 为准.
SV_HITS=$(find "$FUNC_DIR" -name '*.rs' \
    -not -path "*/smoltcp/*" -not -path "*/target/*" -print0 \
    | xargs -0 awk '
    FNR == 1 { in_block = 0 }
    {
        code = ""
        i = 1
        while (i <= length($0)) {
            two = substr($0, i, 2)
            if (in_block) {
                if (two == "*/") { in_block = 0; i += 2 } else { i++ }
            } else if (two == "/*") {
                in_block = 1; i += 2
            } else if (two == "//") {
                break
            } else {
                code = code substr($0, i, 1); i++
            }
        }
        if (code ~ /(^|[^[:alnum:]_])unsafe([^[:alnum:]_]|$)/)
            printf "%s:%d:%s\n", FILENAME, FNR, $0
    }')

if [ -n "$SV_HITS" ]; then
    echo -e "${RED}FAIL${NC}: functions/ 中发现 unsafe 代码:"
    echo "$SV_HITS"
    echo "修复依据: src/kernel/functions/mod.rs 的 #![deny(unsafe_code)] (编译期权威)"
    echo "          scripts/audit_functions_boundary.py (F1+F2 权威审计, ci/audit.sh 步骤 0.5k)"
    exit 1
fi

echo -e "${GREEN}PASS${NC}: functions/ 无 unsafe (${SV_FILES} files scanned)"
