#!/usr/bin/env python3
"""
audit_orphaned_init.py — 检测「无调用者的初始化编排 FFI 入口」(P-8 缺陷类防回归)

背景 (docs/plan/net-e2e-tcp-echo-flakiness.md §6 P-8):
  `privileged/proc/sched_ops.rs` 曾存在 `#[unsafe(no_mangle)] pub extern "C" fn
  scheduler_init()`, 内部挂着 10 项子系统 init (cgroup / numa / pm / sgeg / CET /
  tickless / timesync / kexec / uefi / tick_query 注册). 它是 `no_mangle` 导出,
  Rust 编译器不报 dead_code; `audit_unwired_pub_fn.py` 又显式豁免 no_mangle
  (FFI 边界) —— 两者之间的盲区. 全仓无任何调用者 (Rust / asm / 链接脚本),
  后果是这 10 项初始化在生产环境**从未执行**, 直到 SYS_getcpu / SYS_tickless
  等 syscall 接线后才暴露.

与 audit_unwired_pub_fn.py 的分工 (不重复实现):
  - audit_unwible_pub_fn.py R1: 普通 `pub fn` 零调用 (显式豁免 no_mangle)
  - 本脚本: **no_mangle 入口**的初始化编排零调用 (对方的盲区)

规则:
  O1 (CRITICAL, 门禁):
      `#[unsafe(no_mangle)]` 入口 + 名称为初始化语义 (`*_init` / `init_*` /
      `*_init_ffi` / `*_late_init`) + 全仓零调用 + **函数体内嵌套 ≥2 个 init 调用**
      ⇒ 判定为「孤儿编排入口」: 一个聚合多项子系统初始化的入口没人调, 这些子系统
      必然从未初始化. 这是 P-8 的精确形态.
  O2 (WARN, 仅报告):
      同上但函数体内 init 嵌套 < 2 ⇒ 判定为「疑似退化的 FFI 包裹」(机制初始化
      实际由别处完成, 只余一个无调用者的 C ABI 壳). 属存量债, 由台账跟踪,
      不阻塞 CI.

调用者判定范围 (fail-closed, 宁可多判不可漏判):
  - src/kernel/{privileged,functions}/**.rs (排除 vendored smoltcp)
  - src/kernel/**/*.asm / **/*.S (汇编入口确实会直接 call Rust 符号)
  - src/kernel/**/*.ld (链接脚本按符号点名)
  - 注释行不计入调用 (P-8 修复后代码里大量注释引用 scheduler_init 这个名字);
    `use` / `pub use` 行不计入调用 (re-export 不是调用).

退出码:
  0 = 无 O1
  1 = 有 O1 (孤儿编排入口, P-8 类缺陷回归)
  2 = 脚本自身无法完成扫描 (目录缺失等, fail-closed)

用法:
  python3 scripts/audit_orphaned_init.py            # 扫描全仓
  python3 scripts/audit_orphaned_init.py --strict   # O2 也退出 1
  python3 scripts/audit_orphaned_init.py --self-test # 内置夹具自检 (CI 会跑)
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
KERNEL = PROJECT_ROOT / "src" / "kernel"

# vendored 第三方 (锁定版本), 与项目审计一致排除
EXCLUDE_PARTS = {"smoltcp"}

ATTR_NO_MANGLE = re.compile(r"#\[unsafe\(\s*no_mangle\s*\)\]|#\[no_mangle\]")
FN_NAME = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)")
INITISH = re.compile(r"(_init$|^init_|_init_ffi$|_late_init$|^init$)")
# 函数体内的初始化调用: `foo_init(` / `init(` / `xxx::init(`
INIT_CALL = re.compile(r"\b(?:[a-z0-9_]*_)?init(?:(_[a-z0-9]+)?)?\s*\(")
USE_LINE = re.compile(r"^\s*(?:pub\s+)?(?:use|mod)\s")


def strip_comment(line: str) -> str:
    """去掉行注释 (与 scripts/audit_once_cell.py 同口径: 首个 `//` 之后全弃)."""
    if "//" in line:
        return line.split("//", 1)[0]
    return line


def collect_rust_files(root: Path) -> list[Path]:
    if not root.is_dir():
        return []
    out = []
    for p in sorted(root.rglob("*.rs")):
        if EXCLUDE_PARTS & set(p.parts):
            continue
        out.append(p)
    return out


def collect_foreign_text(root: Path) -> str:
    """汇编与链接脚本文本 (no_mangle 符号可能被它们直接引用)."""
    chunks = []
    for pattern in ("**/*.asm", "**/*.S", "**/*.s", "**/*.ld"):
        for p in sorted(root.glob(pattern)):
            if EXCLUDE_PARTS & set(p.parts):
                continue
            try:
                chunks.append(p.read_text(encoding="utf-8", errors="replace"))
            except OSError:
                continue
    return "\n".join(chunks)


def code_lines(path: Path) -> list[str]:
    """逐行去注释, 保留行号对齐 (索引即 0-based 行号); `use`/`mod` 行清空."""
    raw = path.read_text(encoding="utf-8", errors="replace").splitlines()
    out = []
    for line in raw:
        code = strip_comment(line)
        if USE_LINE.match(code):
            out.append("")
        else:
            out.append(code)
    return out


def find_no_mangle_entries(files: list[Path]) -> list[dict]:
    """定位 no_mangle 入口: 返回 {name, file, def_line, body}."""
    entries = []
    for p in files:
        lines = code_lines(p)
        i = 0
        while i < len(lines):
            if ATTR_NO_MANGLE.search(lines[i]):
                name = None
                fn_line = None
                for j in range(i, min(i + 8, len(lines))):
                    m = FN_NAME.search(lines[j])
                    if m:
                        name, fn_line = m.group(1), j
                        break
                if name:
                    entries.append(
                        {
                            "name": name,
                            "file": p,
                            "def_line": fn_line,
                            # 取体时剔除签名行: 否则 `fn foo_init(` 自身会被
                            # INIT_CALL 计为一次嵌套, 使薄包裹被误判为聚合入口.
                            "body": extract_body(lines, fn_line),
                        }
                    )
            i += 1
    return entries


def extract_body(lines: list[str], fn_line: int) -> str:
    """取函数体内部文本 (按大括号深度), 并剔除签名 (首个 `{` 之前)."""
    depth = 0
    started = False
    parts = []
    for line in lines[fn_line:]:
        parts.append(line)
        for ch in line:
            if ch == "{":
                depth += 1
                started = True
            elif ch == "}":
                depth -= 1
        if started and depth <= 0:
            break
    text = "\n".join(parts)
    brace = text.find("{")
    return text[brace + 1 :] if brace >= 0 else text


def count_refs(name: str, per_file: dict[Path, list[str]], defs: list[dict], foreign: str) -> int:
    """统计调用点数: 排除定义行本身, 排除注释与 use 行 (已在上游清空)."""
    pat = re.compile(r"\b" + re.escape(name) + r"\b")
    skip = {d["file"]: d["def_line"] for d in defs if d["name"] == name}
    n = 0
    for path, lines in per_file.items():
        skip_line = skip.get(path)
        for idx, line in enumerate(lines):
            if idx == skip_line:
                continue
            if pat.search(line):
                n += 1
    n += len(pat.findall(foreign))
    return n


def scan(strict: bool) -> int:
    files = collect_rust_files(KERNEL)
    if not files:
        print(f"  [ERR] 未找到内核源码目录: {KERNEL}")
        return 2
    per_file = {p: code_lines(p) for p in files}
    entries = find_no_mangle_entries(files)
    foreign = collect_foreign_text(KERNEL)

    initish = [e for e in entries if INITISH.search(e["name"])]
    orphans: list[tuple[dict, int]] = []
    for e in initish:
        if count_refs(e["name"], per_file, entries, foreign) == 0:
            orphans.append((e, len(INIT_CALL.findall(e["body"]))))

    critical = [(e, k) for e, k in orphans if k >= 2]
    warn = [(e, k) for e, k in orphans if k < 2]

    print(f"  扫描文件: {len(files)} 个 .rs + 汇编/链接脚本")
    print(f"  no_mangle 入口: {len(entries)} 个 (其中初始化语义 {len(initish)} 个)")
    print("  " + "-" * 60)

    for e, k in critical:
        rel = e["file"].relative_to(PROJECT_ROOT)
        print(f"  [O1 孤儿编排入口] {rel}:{e['def_line'] + 1} {e['name']}()")
        print(f"      全仓 0 调用者, 却嵌套 {k} 项子系统 init ⇒ 这些初始化从未执行")
        print("      → 把各项 init 接到 lib.rs::kernel_init 编排块, 删除该 FFI 入口")

    if warn:
        print(f"  [O2 退化 FFI 包裹] {len(warn)} 个 (存量债, 不阻塞):")
        for e, k in sorted(warn, key=lambda x: str(x[0]["file"])):
            rel = e["file"].relative_to(PROJECT_ROOT)
            print(f"      {rel}:{e['def_line'] + 1} {e['name']}() (体内 init 嵌套 {k})")

    print("  " + "-" * 60)
    if critical:
        print(f"  ✗ 发现 {len(critical)} 个孤儿初始化编排入口 (P-8 缺陷类回归)")
        return 1
    if warn and strict:
        print("  ✗ --strict: O2 退化 FFI 包裹也视为失败")
        return 1
    print("  ✓ 无孤儿初始化编排入口 (P-8 不变式成立)")
    return 0


# ── 内置夹具自检 ────────────────────────────────────────────────────────
# 门禁脚本本身也必须被验证: 若解析逻辑退化 (如漏剥注释), 扫描会静默全绿.
# 夹具复刻 P-8 的真实形态 —— 一个挂着多项 init 的 no_mangle 入口, 断言必被 O1 命中.
FIXTURE_ORPHAN = """
#[unsafe(no_mangle)]
pub extern "C" fn fixture_orphan_init() {
    subsystem_a_init();
    subsystem_b_init();
    driver::power::pm_init(4);
}
"""
FIXTURE_WIRED = """
#[unsafe(no_mangle)]
pub extern "C" fn fixture_wired_init() {
    subsystem_a_init();
    subsystem_b_init();
}
// 下面这行是调用点, 使该入口不再是孤儿
fn caller() { fixture_wired_init(); }
"""
FIXTURE_COMMENT_ONLY = """
// 注释里提到 fixture_ghost_init, 不得被算作调用者
#[unsafe(no_mangle)]
pub extern "C" fn fixture_ghost_init() {
    a_init();
    b_init();
}
"""


def self_test() -> int:
    import tempfile

    cases = [
        ("孤儿编排入口必须被 O1 命中", FIXTURE_ORPHAN, True),
        ("有调用者的入口不得误报", FIXTURE_WIRED, False),
        ("注释里的名字不得当作调用者", FIXTURE_COMMENT_ONLY, True),
    ]
    failures = 0
    with tempfile.TemporaryDirectory() as td:
        root = Path(td) / "src" / "kernel"
        root.mkdir(parents=True)
        for title, text, should_hit in cases:
            # 逐例写入独立文件, 避免互相污染引用计数
            for stale in root.rglob("*.rs"):
                stale.unlink()
            (root / "fixture.rs").write_text(text, encoding="utf-8")
            files = collect_rust_files(root)
            per_file = {p: code_lines(p) for p in files}
            entries = find_no_mangle_entries(files)
            hit = False
            for e in entries:
                if not INITISH.search(e["name"]):
                    continue
                if count_refs(e["name"], per_file, entries, "") == 0:
                    if len(INIT_CALL.findall(e["body"])) >= 2:
                        hit = True
            status = "PASS" if hit == should_hit else "FAIL"
            if status == "FAIL":
                failures += 1
            print(f"  [{status}] {title} (期望命中={should_hit}, 实际命中={hit})")
    if failures:
        print("  ✗ 自检失败: 检测逻辑退化, 门禁不可信")
        return 1
    print("  ✓ 自检通过: O1 检测逻辑有效 (3/3)")
    return 0


def main(argv: list[str]) -> int:
    if "--self-test" in argv:
        return self_test()
    return scan(strict="--strict" in argv)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
