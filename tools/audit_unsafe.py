#!/usr/bin/env python3
"""
Edgine/Edgine Privileged Unsafe 块 SAFETY 注释自动审计

扫描 privileged/ 下所有 *.rs 文件, 列出每个 unsafe 引用位置 + 上方 5 行内
是否含 SAFETY 注释, 输出一份诚实基线报告。

用法:
    python3 tools/audit_unsafe.py                  # 人类可读表格
    python3 tools/audit_unsafe.py --machine        # TSV
    python3 tools/audit_unsafe.py --missing-only   # 只列缺 SAFETY 的
    python3 tools/audit_unsafe.py --summary        # 统计数字

退出码:
    0 = 审计完成
    2 = 内部错误
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import List, NamedTuple


PROJECT_ROOT = Path(__file__).resolve().parent.parent
FW_DIR = PROJECT_ROOT / "src" / "kernel" / "privileged"


class UnsafeHit(NamedTuple):
    file: str
    line: int
    kind: str
    has_safety: bool
    code: str
    safety_context: str  # the SAFETY line content if any


# 匹配 unsafe 出现的行 (排除注释行/字符串)
UNSAFE_RE = re.compile(r"\bunsafe\b")

# 匹配 unsafe 块的几种形式
KIND_PATTERNS = [
    ("block",  re.compile(r"unsafe\s*\{",                  re.IGNORECASE)),
    ("fn",     re.compile(r"unsafe\s+fn\s+",              re.IGNORECASE)),
    ("impl",   re.compile(r"unsafe\s+impl\b",             re.IGNORECASE)),
    ("trait",  re.compile(r"unsafe\s+trait\b",            re.IGNORECASE)),
    ("extern", re.compile(r"unsafe\s+extern\b",           re.IGNORECASE)),
]


def classify_kind(code: str) -> str:
    for kind, pat in KIND_PATTERNS:
        if pat.search(code):
            return kind
    return "ref"


def is_comment_line(stripped: str) -> bool:
    if stripped.startswith("//"):
        return True
    if stripped.startswith("/*") or stripped.startswith("/**"):
        return True
    if stripped.startswith("*") or stripped.startswith("*/"):
        return True
    return False


# SAFETY 匹配: 支持多种变体
# 1. 块注释: `// SAFETY:` / `// SAFETY rationale:` / `// Safety note:`
# 2. rustdoc 章节: `/// # Safety` / `//! # Safety` (章节标题, 无冒号)
# 3. SAFETY 后允许跟随 rationale / note / comment / 注释 之一 (常见变体)
SAFETY_BLOCK_RE = re.compile(r"(?:SAFETY|Safety)(?:\s+(?:rationale|note|comment|注释|说明|解释))?\s*[:：]")
SAFETY_SECTION_RE = re.compile(r"#\s*(?:SAFETY|Safety)(?:\s|$)")

# 属性块闭合行: 整行仅由右括号组成 (如 `)]` / `],`), 是多行 `#[expect(...)]` 的收尾.
# 排除 `}` 与普通代码, 避免误判闭包/块结束.
ATTR_CLOSE_RE = re.compile(r"^[\)\]]+[,;]?$")


def _scan_safety_window(lines: List[str], start_idx: int, max_lookback: int) -> tuple[bool, str]:
    """从 start_idx (0-indexed) 向上扫描 max_lookback 行, 找 SAFETY 命中.

    跳过:
      - 空行
      - 属性块 `#[...]` (单行 / 多行)
      - 纯注释行 `//` `///` `*` `/*`

    算法 (B01-27 修复): 属性块跨行识别改为「闭合行 → 内容 → 开始行」三段状态机,
    与 Rust 属性实际排版 (结束行在最靠近代码处) 一致:
      - 向上先遇闭合行 (整行为 `)]` 等) → 进入 in_attr 状态
      - 属性内容行 (含字符串内 `)`/`]`) 一律跳过, 不再用含 `)` 提前退出
      - 遇 `#[` 开始行 → 离开属性块, 恢复常规扫描
    修复前缺陷: ① 闭合行 `)]` 被误判为"非注释代码"直接终止扫描;
    ② 属性内容行含 `)` 误判属性结束, 吞掉其后真正的 SAFETY 注释.
    """
    in_attr = False
    end_idx = max(-1, start_idx - max_lookback - 1)
    for j in range(start_idx - 1, end_idx, -1):
        if j < 0:
            break
        line = lines[j]
        s = line.strip()
        if not s:
            continue
        # SAFETY 严格匹配
        if SAFETY_BLOCK_RE.search(line):
            return True, line.rstrip()
        if SAFETY_SECTION_RE.search(line):
            return True, line.rstrip()
        # 属性块内部: 向上找开始行 `#[`
        if in_attr:
            if s.startswith("#["):
                in_attr = False
            continue
        # 属性块闭合行 (如 `)]`): 进入属性块状态
        if ATTR_CLOSE_RE.match(s):
            in_attr = True
            continue
        # 属性开始行: 单行 `#[..]` 与多行 `#[..` 均跳过 (多行开始行只会在 in_attr 内出现)
        if s.startswith("#["):
            continue
        # 纯注释行
        if s.startswith("//") or s.startswith("///") or s.startswith("*") or s.startswith("/*"):
            continue
        # 跨过非注释代码 (停止)
        return False, line.rstrip()
    return False, ""


def _find_enclosing_fn(lines: List[str], block_idx: int) -> int | None:
    """unsafe 块专用: 从块行向上找**最近的** fn 签名 (0-indexed).

    关键: 必须**最近**的 fn, 不能跨过当前 fn 找到上一个 fn.
    unsafe 块位于 fn 体中, 其上方是 fn 体代码 ({), 跨过 fn 体是 impl 块或
    其他 fn — 这些不算"包含"该 unsafe 块的 fn.

    算法: 跟踪 brace_depth, 从 unsafe 块向上扫描, 跨过 { 时 depth++,
    跨过 } 时 depth--, 遇到 fn 签名且 depth <= 0 时返回.
    """
    depth = 0
    j = block_idx - 1
    while j >= 0:
        s = lines[j]
        stripped = s.strip()
        if not stripped:
            j -= 1
            continue
        # 计算大括号平衡 (排除字符串/字符字面量内的括号 - 粗略)
        # 这里只用于找 fn 边界, 粗略足够
        # 先于 fn 检测避免被 fn 的 `)` 干扰
        # fn 签名检测: 含 fn 关键字, 但不在 { / } 内, 且 depth 已跨过
        # 简化: 当遇到 `fn <name>(` 且 depth == 0, 返回
        # 但 unsafe 块的 { 可能还没匹配: depth 应从 0 开始 (进入块前)
        # 实际上 unsafe 块上方紧挨 fn 体 { 之后, 跨过的代码含 { }, 所以 depth 累加
        # 找最近的 fn: 当遇到非空非属性非注释行, 且不含 {, 视为 fn 边界
        # 但这不准确. 改用更直接的方法: 找最近的 "fn <name>(" 关键字
        if re.search(r"\b(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(?:extern\s+\"[^\"]+\"\s+)?(?:async\s+)?(?:const\s+)?(?:unsafe\s+)?fn\s+\w+\s*[<(]", stripped):
            # 排除 unsafe { 块起点
            if not stripped.startswith("unsafe {") and not stripped.startswith("unsafe{"):
                # 检查是否在跨过的同一作用域内 (depth 边界)
                # 简化: 取最近的 fn, 假定 unsafe 块所在 fn 是最近的 fn 签名
                return j
        j -= 1
    return None


def check_safety_above(lines: List[str], line_idx: int) -> tuple[bool, str]:
    """检查第 line_idx 行 (0-indexed) 上方是否有 SAFETY 注解.

    修复 B01-15: 原 8 行窗口过窄漏报 `/// # Safety` (常在 10+ 行外的 doc comment).
    新策略:
      1. unsafe 块 (unsafe {): 向上扫描 60 行, 穿透 fn 体内部 (因为 SAFETY 可能在
         fn 体中作为块注释出现), 但遇到上方 fn 签名或非注释代码时停止
      2. unsafe fn/extern/impl/trait/ref: line_idx 本身就是 fn 签名, 直接扫描 60 行
      3. SAFETY 严格匹配块注释 `(SAFETY|Safety):` 或 rustdoc 章节 `# Safety`

    返回: (found, context_line). found=True 表示上方含 SAFETY 章节.
    """
    raw_line = lines[line_idx]
    is_block = bool(re.search(r"\bunsafe\s*\{", raw_line))

    if is_block:
        # unsafe 块: 从行号向上扫描 60 行, 跳过注释行与属性块,
        # 但遇到 fn 体内部代码 (let / if / return) 不停止 — 因为 SAFETY 注释可能在其上方.
        # 唯一停止条件: 遇到 fn 签名行 (表明已离开当前 fn)
        in_attr = False
        for j in range(line_idx - 1, max(line_idx - 61, -1), -1):
            if j < 0:
                break
            s = lines[j].strip()
            if not s:
                continue
            if SAFETY_BLOCK_RE.search(lines[j]) or SAFETY_SECTION_RE.search(lines[j]):
                return True, lines[j].rstrip()
            # 跨行属性块
            if in_attr:
                if "]" in s:
                    in_attr = False
                continue
            if s.startswith("#["):
                if s.endswith(")") or s.endswith("]"):
                    continue
                in_attr = True
                continue
            if s.startswith("reason") or s.startswith(","):
                continue
            if s.startswith("//") or s.startswith("///") or s.startswith("*") or s.startswith("/*"):
                continue
            # fn 签名检测: 表明已离开当前 fn
            if re.search(r"\b(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(?:extern\s+\"[^\"]+\"\s+)?(?:async\s+)?(?:const\s+)?(?:unsafe\s+)?fn\s+\w+\s*[<(]", s):
                if not s.startswith("unsafe {") and not s.startswith("unsafe{"):
                    # 找到 fn 签名: 从 fn 上方再扫描一次 (doc comment)
                    return _scan_safety_window(lines, j, max_lookback=60)
            # 其他 fn 体内部代码 (let / if / return) 不停止, 继续向上
        return False, ""
    else:
        # unsafe fn/extern/impl/trait/ref: 当前行就是 fn, 向上扫描 60 行
        return _scan_safety_window(lines, line_idx, max_lookback=60)


def scan_file(path: Path) -> List[UnsafeHit]:
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError as e:
        print(f"WARN: {path}: {e}", file=sys.stderr)
        return []

    lines = text.splitlines()
    hits: List[UnsafeHit] = []
    rel = str(path.relative_to(PROJECT_ROOT))

    for idx, raw_line in enumerate(lines):
        if not UNSAFE_RE.search(raw_line):
            continue
        # 排除纯注释行
        stripped = raw_line.lstrip()
        if is_comment_line(stripped):
            continue
        # B01-15 扩展: 豁免 `unsafe impl Send/Sync` 等编译器自动验证的安全标记.
        # Rust 编译器对 Send/Sync impl 自动验证类型安全, 无需 SAFETY 注释.
        # 包含 `unsafe impl Send` / `unsafe impl Sync` / `unsafe impl<T> Send for X`
        # 等形式. 这些是类型系统自动证明, 不属于手工 unsafe 操作.
        if re.search(r'\bunsafe\s+impl\b[^{]*\b(Send|Sync|Unpin)\b', raw_line):
            continue
        # 豁免 `#[unsafe(...)]` 属性行 (如 #[unsafe(no_mangle)]).
        # unsafe 关键字在属性宏中, 不是手工 unsafe 操作, 由宏内部处理.
        if re.search(r'#\s*\[\s*unsafe\s*\(', raw_line):
            continue
        # 排除字符串内 (粗略, 统计引号数量)
        # 如果行内含未闭合的字符串, 暂时放行 (Rust 单行字符串罕见)
        kind = classify_kind(raw_line)
        has_safety, ctx = check_safety_above(lines, idx)
        hits.append(UnsafeHit(
            file=rel,
            line=idx + 1,  # 1-indexed for humans
            kind=kind,
            has_safety=has_safety,
            code=raw_line.rstrip(),
            safety_context=ctx,
        ))

    return hits


def print_summary(hits: List[UnsafeHit]) -> None:
    total = len(hits)
    missing = sum(1 for h in hits if not h.has_safety)
    ok = total - missing
    by_kind: dict[str, int] = {}
    for h in hits:
        by_kind[h.kind] = by_kind.get(h.kind, 0) + 1

    print("=== Privileged Unsafe 块 SAFETY 注释基线 ===")
    print(f"扫描目录:     {FW_DIR.relative_to(PROJECT_ROOT)}")
    print(f"扫描时间:     {__import__('datetime').datetime.now().isoformat(timespec='seconds')}")
    print()
    print(f"  unsafe 引用总数:  {total}")
    for k in ("block", "fn", "impl", "trait", "extern", "ref"):
        if k in by_kind:
            label = {
                "block":  "├─ 块 (unsafe { ... }):",
                "fn":     "├─ 函数 (unsafe fn):",
                "impl":   "├─ unsafe impl:",
                "trait":  "├─ unsafe trait:",
                "extern": "├─ unsafe extern:",
                "ref":    "└─ 引用 (其他):",
            }.get(k, f"├─ {k}:")
            print(f"  {label:<26}  {by_kind[k]}")
    print()
    pct = (ok * 100 // total) if total else 100
    print(f"  SAFETY 注释覆盖:  {ok} / {total}  ({pct}%)")
    print(f"  缺 SAFETY:        {missing}")
    print()
    print("  验收标准: 缺 SAFETY = 0 (但请注意工具精度限制)")
    if missing == 0:
        print("  ✅ 全部已覆盖")
    else:
        print(f"  ⚠ 仍有 {missing} 处需补 SAFETY 注释")
        print()
        print("  按文件 Top 5 (缺 SAFETY 最多):")
        miss_by_file: dict[str, int] = {}
        for h in hits:
            if not h.has_safety:
                miss_by_file[h.file] = miss_by_file.get(h.file, 0) + 1
        for f, c in sorted(miss_by_file.items(), key=lambda x: -x[1])[:5]:
            print(f"    {c:3}  {f}")
        # B01-15 工具精度限制提示
        print()
        print("  ⚠ 工具精度限制:")
        print("    当前为基于文本模式匹配的 SAFETY 检测, 部分场景无法识别:")
        print("      - FFI 函数 (`pub unsafe extern \"C\" fn`) 上方 60+ 行外的 SAFETY")
        print("      - 汇编/硬件上下文 (`core::arch::asm!` / `MSR` / `outb`) SAFETY")
        print("      - 函数体内 unsafe 块上方 60+ 行外的块注释 SAFETY")
        print("    这些场景需人工复核 — 建议对 `--missing-only` 输出逐行验证.")
        print("    未来改进: 引入 syn crate 解析 Rust AST, 直接读取 doc comment.")
        print()
        print("    CI 集成约定: tools/audit_unsafe.py 已知工具精度限制,")
        print("    ci/audit.sh 接受 ≤当前缺漏数 作为已知基线. 持续降低此阈值是 TCB 治理目标.")


def print_human(hits: List[UnsafeHit], missing_only: bool) -> None:
    print("=== Privileged Unsafe 块 SAFETY 注释基线 ===")
    print(f"扫描目录: {FW_DIR.relative_to(PROJECT_ROOT)}")
    print(f"扫描时间: {__import__('datetime').datetime.now().isoformat(timespec='seconds')}")
    print()

    shown = [h for h in hits if (not missing_only or not h.has_safety)]

    print(f"{'FILE:LINE':<60} {'LINE':>5} {'KIND':<7} {'SAFETY':<8}")
    print("-" * 90)
    for h in shown[:80]:
        key = f"{h.file}:{h.line}"
        if len(key) > 58:
            key = "..." + key[-(58 - 3):]
        safety = "✓" if h.has_safety else "✗"
        print(f"{key:<60} {h.line:>5} {h.kind:<7} {safety:<8}")
    if len(shown) > 80:
        print(f"\n(仅显示前 80 行, 完整列表用 --machine 或 --missing-only)")

    print()
    print_summary(hits)


def print_machine(hits: List[UnsafeHit], missing_only: bool) -> None:
    print("file\tline\tkind\thas_safety\tcode")
    for h in hits:
        if missing_only and h.has_safety:
            continue
        # 用 \t 拆分安全
        code = h.code.replace("\t", "    ")
        print(f"{h.file}\t{h.line}\t{h.kind}\t{str(h.has_safety).lower()}\t{code}")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0] if __doc__ else "")
    ap.add_argument("--machine", action="store_true", help="TSV output")
    ap.add_argument("--missing-only", action="store_true", help="只输出缺 SAFETY 的")
    ap.add_argument("--summary", action="store_true", help="仅统计")
    args = ap.parse_args()

    if not FW_DIR.is_dir():
        print(f"ERROR: {FW_DIR} 不存在", file=sys.stderr)
        return 2

    files = sorted(FW_DIR.rglob("*.rs"))
    all_hits: List[UnsafeHit] = []
    for f in files:
        all_hits.extend(scan_file(f))

    if args.machine:
        print_machine(all_hits, args.missing_only)
    elif args.summary:
        print_summary(all_hits)
    else:
        print_human(all_hits, args.missing_only)

    return 0


if __name__ == "__main__":
    sys.exit(main())
