#!/bin/bash
# QueenX Rust 化工程 - 一键阶段性验收脚本
# 用途: 快速运行所有 Rust 验收检查并生成报告

set -e

cd "$(dirname "$0")/../"

echo "╔══════════════════════════════════════════════════════════╗"
echo "║  🦀 QueenX Rust 内核 - 阶段性验收                       ║"
echo "║  $(date) ║"
echo "╚══════════════════════════════════════════════════════════╝"

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
REPORT_DIR="other/tests-reports"
REPORT_FILE="${REPORT_DIR}/rust_acceptance_${TIMESTAMP}.json"
MARKDOWN_FILE="${REPORT_DIR}/rust_acceptance_${TIMESTAMP}.md"

mkdir -p "$REPORT_DIR"

echo ""
echo "🔍 开始验收检查..."
echo ""

# 运行 Python 验收模块
python3 tests/rust_acceptance.py --json > "$REPORT_FILE" 2>&1 || true

# 同时保存可读的 Markdown 报告
cat > "$MARKDOWN_FILE" << HEADER
# 🦀 QueenX Rust 内核阶段性验收报告

> **时间**: $(date)
> **版本**: Phase 1-4 完成 (核心基础设施 + 内存管理 + 调度 + 定时器)

## 验收结果概览

HEADER

# 解析 JSON 并生成 Markdown 表格
if [ -f "$REPORT_FILE" ]; then
    python3 << PYEOF
import json
from datetime import datetime

report_file = "$REPORT_FILE"
md_file = "$MARKDOWN_FILE"

with open(report_file, 'r') as f:
    data = json.load(f)

passed = data.get('total_passed', 0)
failed = data.get('total_failed', 0)
skipped = data.get('total_skipped', 0)

with open(md_file, 'a') as f:
    f.write(f"\n### 统计汇总\n\n")
    f.write(f"| 指标 | 数量 |\n|------|------|\n")
    f.write(f"| ✅ 通过 | {passed} |\n")
    f.write(f"| ❌ 失败 | {failed} |\n")
    f.write(f"| ⏭️ 跳过 | {skipped} |\n")
    f.write(f"| 📊 总计 | {passed + failed + skipped} |\n")
    
    f.write(f"\n### 详细结果\n\n")
    f.write("| 模块 | 测试项 | 状态 | 备注 |\n|------|--------|------|------|\n")
    
    for result in data.get('results', []):
        status_icon = "✅" if result['result'] == "PASS" else ("❌" if result['result'] == "FAIL" else "⚠️")
        message = result.get('message', '')[:50]
        f.write(f"| {result['module']} | {result['name']} | {status_icon} {result['result']} | {message} |\n")
    
    f.write(f"\n---\n*报告自动生成于 {datetime.now()}*\n")

print(f"✅ Markdown 报告已生成: {md_file}")
PYEOF
fi

echo ""
echo "=========================================="
echo "📄 报告文件:"
echo "   JSON: $REPORT_FILE"
echo "   MD:   $MARKDOWN_FILE"
echo ""
echo "📊 验收完成!"
echo "=========================================="
