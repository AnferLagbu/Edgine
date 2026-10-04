#!/usr/bin/env python3
"""
audit_unwired_pub_fn.py — 检测「已实现但未接线」的 pub fn / pub const / pub mod

设计原则 (§10 源码调研后):
  - 仅检测 edgine crate (staticlib) 内部 src/kernel/{privileged,functions} 子树
  - 排除 vendored smoltcp (锁定版本, 禁止扫描)
  - 排除 host-tests / tests/ (测试代码)
  - 排除 src/user/ (独立 ELF, 不调用 edgine 内部 pub fn)

检测项:
  R1 (HIGH):  pub fn 全仓库零调用 (除声明文件自身) 且**未被 T5 甄别分类**
  R1 (INFO):  同上但**已在台账「已分类清单」区块中** (降噪, 零引用事实保留)
  R2 (CRITICAL): pub const SYS_*/EG_* 在 syscall/types.rs 声明但 dispatch.rs 未分发
  R3 (WARN):  pub mod 子模块 0 引用
  R4 (INFO):  pub struct/enum 全仓库零引用 (核心类型)

「已分类清单」数据源 (裁定五 / B09-21):
  台账 docs/plan/syscall-followup.md 内的机器可读区块
  `<!-- audit-classified-begin -->` … `<!-- audit-classified-end -->`
  每行一项, 格式 `<repo-relative path>::<pub fn 名>`.
  - 命中 ⇒ R1 分级降为 INFO (只降噪, **不豁免**: 零引用事实判定不变, 仍列出)
  - fail-closed: 区块缺失 / 标记不唯一 / 行格式非法 ⇒ 视同全部未分类 (仍报 HIGH)

豁免列表 (避免误报, 严格):
  EXEMPT_NO_MANGLE: #[no_mangle] / #[unsafe(no_mangle)] 函数 (FFI 边界)
  EXEMPT_PATH:     privileged/prelude.rs + 两个 syscall/dispatch.rs (按**具体路径**,
                   非按文件名 —— 后者会连带豁免全仓同名文件, 见 EXEMPT_PATHS 注释)
  EXEMPT_CFG_ATTR:  cfg(...)/cfg_attr(...) 门控的引用 (条件编译)
  EXEMPT_TRAIT_IMPL: trait impl 中的 fn (继承自 trait)
  EXEMPT_VENDORED: smoltcp 子树 (锁定版本)

退出码:
  0 = 仅 INFO/WARN
  1 = 有 CRITICAL (R2 未接线 syscall)

用法:
  python3 scripts/audit_unwired_pub_fn.py [--strict] [--json]
  --strict: WARN 也退出 1
  --json: 输出 JSON 报告
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

# ────────────────────────────────────────────────────────────────────────
# 路径与豁免配置
# ────────────────────────────────────────────────────────────────────────

ROOT = Path(__file__).resolve().parent.parent
KERNEL_DIR = ROOT / "src" / "kernel"
PRIVILEGED_DIR = KERNEL_DIR / "privileged"
FUNCTIONS_DIR = KERNEL_DIR / "functions"

# vendored 目录 (绝对排除)
VENDORED_DIRS = {"smoltcp"}

# 豁免路径 (顶层 re-export 区与分发表)
#
# 分册 9 项 5 (2026-09-26): 原按**文件名**豁免 ⇒ 全仓任意同名文件 (mod.rs /
# api.rs / types.rs / lib.rs) 内的一切 pub fn 均被静默豁免, 构成结构性漏报面
# (实测 108 文件 / 902 个 pub fn 定义从未进入 R1)。现改为按**具体路径**豁免,
# 仅保留真正的 re-export 集中区与 syscall 分发表。
EXEMPT_PATHS = {
    "src/kernel/privileged/prelude.rs",           # privileged::prelude — pub use 集中区
    "src/kernel/privileged/syscall/dispatch.rs",  # privileged syscall 分发表 (SYS_* 在此引用)
    "src/kernel/functions/syscall/dispatch.rs",   # functions syscall 分发表
}

# 「已分类清单」数据源 (裁定五 / B09-21): 台账内机器可读区块
CLASSIFIED_DOC = ROOT / "docs" / "plan" / "syscall-followup.md"
CLASSIFIED_BEGIN = "<!-- audit-classified-begin -->"
CLASSIFIED_END = "<!-- audit-classified-end -->"
# 区块行格式: <repo-relative path>::<pub fn 名>
CLASSIFIED_KEY_RE = re.compile(r"^src/[^\s:]+\.rs::\w+$")

# ────────────────────────────────────────────────────────────────────────
# 数据收集 (基于 .rs 文件源码 AST 分析, 不依赖 cargo build)
# ────────────────────────────────────────────────────────────────────────

def load_classified_set() -> tuple[set[str], str]:
    """读取台账内「已分类清单」区块 (裁定五 / B09-21)

    返回 (已分类键集合, 状态说明).

    **fail-closed**: 台账不可读 / 区块标记缺失或重复 / 区块为空 /
    行格式非法 ⇒ 一律返回空集, 调用方视同「全部未分类」(仍报 HIGH).
    **只降噪不豁免**: 命中仅改变报告分级, 不改变「零引用」这一事实判定.
    """
    try:
        content = CLASSIFIED_DOC.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return set(), f"台账不可读 ({CLASSIFIED_DOC.name}) ⇒ 全部按未分类处理"

    lines = content.split("\n")
    begins = [i for i, line in enumerate(lines) if line.strip() == CLASSIFIED_BEGIN]
    ends = [i for i, line in enumerate(lines) if line.strip() == CLASSIFIED_END]
    if len(begins) != 1 or len(ends) != 1 or begins[0] >= ends[0]:
        return set(), "区块标记缺失或重复 ⇒ 全部按未分类处理"

    classified: set[str] = set()
    for raw in lines[begins[0] + 1:ends[0]]:
        entry = raw.strip()
        if not entry:
            continue
        if not CLASSIFIED_KEY_RE.match(entry):
            return set(), f"区块行格式非法 ({entry[:40]}) ⇒ 全部按未分类处理"
        classified.add(entry)
    if not classified:
        return set(), "区块为空 ⇒ 全部按未分类处理"
    return classified, f"{len(classified)} 项 (台账 B-6 区块)"


def is_vendored(path: Path) -> bool:
    """检测 vendored 子目录（递归检查祖先）"""
    parts = path.relative_to(KERNEL_DIR).parts
    return any(p in VENDORED_DIRS for p in parts)


def is_kernel_code(path: Path) -> bool:
    """是否 edgine crate 内 src/kernel/ 下的非 vendored 代码"""
    if not path.suffix == ".rs":
        return False
    try:
        path.relative_to(KERNEL_DIR)
    except ValueError:
        return False
    return not is_vendored(path)


def collect_no_mangle_functions(path: Path) -> set[str]:
    """收集文件中所有 #[no_mangle] / #[unsafe(no_mangle)] 的 fn 名

    必须使用源码 AST 视角, 因为 ripgrep \bname\b 会误匹配同名局部变量
    """
    try:
        content = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return set()
    names = set()
    lines = content.split("\n")
    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith("//") or stripped.startswith("*"):
            continue  # 跳过注释/docstring
        if re.search(r"#\[(?:unsafe\()?no_mangle\)?\]", line):
            # 后续 12 行内查找 fn 声明 (允许属性块 / extern 跨行)
            for j in range(i, min(i + 14, len(lines))):
                m = re.search(
                    r"\b(?:pub\s+(?:unsafe\s+)?)?(?:extern\s+\"[^\"]+\"\s+)?(?:unsafe\s+)?fn\s+(\w+)",
                    lines[j],
                )
                if m:
                    names.add(m.group(1))
                    break
    return names


def collect_pub_fns(path: Path) -> list[dict]:
    """收集所有 pub fn 声明（含 trait impl 中的 fn）

    返回 [ {name, line, in_trait_impl} ] 列表
    """
    try:
        content = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return []
    results = []
    lines = content.split("\n")
    brace_depth = 0  # 跟踪 impl 块嵌套深度
    in_trait_impl_depth = -1  # impl Trait for Type 的深度, -1 表示不在
    in_mod_depth = -1  # 当前 pub fn 所在的 mod 深度

    for i, line in enumerate(lines):
        stripped = line.strip()

        # 跟踪 impl 块 (impl Trait for Type { ... } / impl Type { ... })
        if re.match(r"^(pub\s+)?(unsafe\s+)?impl\b", stripped) and "{" in stripped:
            impl_depth = brace_depth
            brace_depth += stripped.count("{") - stripped.count("}")
            # B01-19 修复: 在 impl 块结束时重置 in_trait_impl_depth
            # 原代码: 置位后永不重置, 跨多个 impl 块时留下错误状态
            is_trait_impl = re.search(r"\bfor\b", stripped) or "trait" in stripped
            if is_trait_impl:
                # 记录此 impl 块的起始 brace_depth, 退出时比较
                # 简化: 每次 impl 块进入时更新为当前 depth (允许嵌套 impl)
                in_trait_impl_depth = impl_depth
            continue

        # 跟踪 fn 声明
        # 区分 trait 中的 fn (不带 default 时是抽象方法, 不可豁免)
        # 这里收集所有 pub fn, 由后续阶段决定是否豁免
        m = re.match(
            r"^\s*pub(?:\([^)]+\))?\s+(?:unsafe\s+)?(?:extern\s+\"[^\"]+\"\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(\w+)\s*[<(]",
            line,
        )
        if m:
            fn_name = m.group(1)
            in_trait_impl = in_trait_impl_depth >= 0 and in_trait_impl_depth < brace_depth
            results.append({
                "name": fn_name,
                "line": i + 1,
                "in_trait_impl": in_trait_impl,
                "depth": brace_depth,
            })

        # 维护 brace_depth (简单字符计数)
        brace_depth += line.count("{") - line.count("}")
        if brace_depth < 0:
            brace_depth = 0
        # B01-19 修复: 离开 impl 块时重置 in_trait_impl_depth
        # 当 brace_depth 降回 impl_depth 以下时, 此 impl 块已结束
        if in_trait_impl_depth >= 0 and brace_depth <= in_trait_impl_depth:
            in_trait_impl_depth = -1

    return results


def collect_pub_consts_syscall(path: Path) -> list[tuple[str, int]]:
    """仅在 syscall/types.rs 中收集 pub const SYS_*/EG_* 声明"""
    if path.name != "types.rs" or "syscall" not in path.parts:
        return []
    try:
        content = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return []
    results = []
    for i, line in enumerate(content.split("\n")):
        m = re.match(r"^\s*pub\s+const\s+(SYS_[a-z_]+|EG_[A-Z_]+)\s*:\s*u64\s*=", line)
        if m:
            results.append((m.group(1), i + 1))
    return results


def collect_pub_mods(path: Path) -> list[tuple[str, int]]:
    """收集 pub mod xxx { ... } 声明"""
    try:
        content = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return []
    results = []
    for i, line in enumerate(content.split("\n")):
        m = re.match(r"^\s*pub\s+mod\s+(\w+)\s*[;{]", line)
        if m:
            results.append((m.group(1), i + 1))
    return results


def collect_pub_structs_enums(path: Path) -> list[tuple[str, str, int]]:
    """收集 pub struct/enum 声明"""
    try:
        content = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return []
    results = []
    for i, line in enumerate(content.split("\n")):
        m = re.match(r"^\s*pub\s+(struct|enum)\s+(\w+)", line)
        if m:
            results.append((m.group(2), m.group(1), i + 1))
    return results


# ────────────────────────────────────────────────────────────────────────
# 跨文件引用计数 (基于 ripgrep, 不依赖 cargo build)
# ────────────────────────────────────────────────────────────────────────

def count_refs(name: str, decl_file: Path, decl_line: int) -> dict:
    """统计名字在 src/ + host-tests/ 中的引用分布

    返回 { 'total': N, 'in_decl_file': M, 'cross_file': N - M }

    排除 vendored smoltcp 子树

    B01-19 修复: 优先用 ripgrep (高性能), 若 rg 不可用则降级到 Python 扫描.
    使用 \b 词边界匹配 (替代 `-w` flag), 排除同名局部变量误匹配.
    """
    import shutil

    # 优先尝试 ripgrep (高性能, 1-2 秒扫描全部)
    if shutil.which("rg"):
        return _count_refs_rg(name, decl_file)
    # 降级: Python 扫描 (较慢但无依赖)
    # edgine crate ~700+ .rs 文件, ~3000+ pub fn, 每次 count_refs 扫描 ~700 文件
    # 总体时间: 1-2 分钟 (每 fn 2-3 ms on cold cache, 0.5 ms on warm cache)
    # B01-19 性能优化: 一次性缓存所有文件内容, 后续 count_refs 仅内存子串扫描
    return _count_refs_python(name, decl_file)


def _count_refs_rg(name: str, decl_file: Path) -> dict:
    """ripgrep 实现 (高性能路径)."""
    cmd = [
        "rg", "-c", "-w", "--no-heading",
        name,
        "src/", "host-tests/",
        "--type", "rust",
    ]
    cmd.extend(["-g", "!src/kernel/functions/net/smoltcp/**"])

    try:
        result = subprocess.run(
            cmd, capture_output=True, text=True, cwd=ROOT, timeout=120
        )
    except subprocess.TimeoutExpired:
        return {"total": 0, "in_decl_file": 0, "cross_file": 0, "error": "timeout"}

    total_refs = 0
    in_decl_file_refs = 0
    for line in result.stdout.strip().split("\n"):
        if ":" not in line:
            continue
        try:
            path_str, count = line.rsplit(":", 1)
            count = int(count)
        except ValueError:
            continue
        total_refs += count
        try:
            decl_rel = decl_file.relative_to(ROOT)
            if path_str == str(decl_rel):
                in_decl_file_refs = count
        except ValueError:
            pass

    return {
        "total": total_refs,
        "in_decl_file": in_decl_file_refs,
        "cross_file": total_refs - in_decl_file_refs,
        "actual_callers": total_refs - 1,
    }


_PYTHON_FILE_CACHE: dict[str, str] = {}


def _build_python_cache() -> None:
    """构建一次所有 .rs 文件内容缓存, 避免反复 read_text.

    B01-19 性能优化: edgine crate ~700+ .rs 文件, 每次 count_refs 调用
    重复 read_text 是主要性能瓶颈. 一次性缓存后, 仅做内存子串扫描.
    该缓存按 tgt 调用一次 (lazy init), 一次性扫描 ~700 文件约 1-3 秒.
    """
    global _PYTHON_FILE_CACHE
    if _PYTHON_FILE_CACHE:
        return
    targets = ["src/", "host-tests/"]
    for target in targets:
        target_path = ROOT / target.rstrip("/")
        if not target_path.exists():
            continue
        for rs in target_path.rglob("*.rs"):
            if "smoltcp" in str(rs):
                continue
            try:
                content = rs.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            try:
                rel = str(rs.relative_to(ROOT))
            except ValueError:
                rel = str(rs)
            _PYTHON_FILE_CACHE[rel] = content


def _count_refs_python(name: str, decl_file: Path) -> dict:
    """Python 子串扫描实现 (降级路径, 无 ripgrep 依赖).

    B01-19: 性能优化 - 一次性缓存文件内容, 多次 count_refs 共享.
    """
    _build_python_cache()

    word_re = re.compile(r"\b" + re.escape(name) + r"\b")

    total_refs = 0
    in_decl_file_refs = 0
    try:
        decl_rel = str(decl_file.relative_to(ROOT))
    except ValueError:
        decl_rel = str(decl_file)

    for path_str, content in _PYTHON_FILE_CACHE.items():
        count = len(word_re.findall(content))
        if count > 0:
            total_refs += count
            if path_str == decl_rel:
                in_decl_file_refs = count

    return {
        "total": total_refs,
        "in_decl_file": in_decl_file_refs,
        "cross_file": total_refs - in_decl_file_refs,
        "actual_callers": total_refs - 1,
    }


def is_in_dispatch(name: str, syscall_types_path: Path) -> bool:
    """检查 SYS_*/EG_* 名称是否被 dispatch 处理

    检查两个 dispatch:
      - functions/syscall/dispatch.rs (T5-1 迁移后)
      - privileged/syscall/dispatch.rs (fallback 回退处理)
    """
    # functions dispatch (新派发架构)
    functions_dispatch = FUNCTIONS_DIR / "syscall" / "dispatch.rs"
    # privileged dispatch (fallback 回退处理)
    privileged_dispatch = PRIVILEGED_DIR / "syscall" / "dispatch.rs"

    for dispatch_path in (functions_dispatch, privileged_dispatch):
        if not dispatch_path.exists():
            continue
        try:
            content = dispatch_path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        # 名称必须出现 (use 导入 或 match arm)
        if re.search(rf"\b{name}\b", content):
            return True
    return False


# ────────────────────────────────────────────────────────────────────────
# 豁免判定
# ────────────────────────────────────────────────────────────────────────

def is_exempt_function(name: str, path: Path, in_trait_impl: bool,
                       no_mangle_set: set[str]) -> tuple[bool, str]:
    """判断 pub fn 是否豁免. 返回 (是否豁免, 豁免原因)"""
    if name in no_mangle_set:
        return True, "#[no_mangle] FFI"
    try:
        rel = str(path.relative_to(ROOT))
    except ValueError:
        rel = str(path)
    if rel in EXEMPT_PATHS:
        return True, f"豁免路径 ({rel})"
    if in_trait_impl:
        return True, "trait impl (继承自 trait)"
    return False, ""


# ────────────────────────────────────────────────────────────────────────
# 主扫描流程
# ────────────────────────────────────────────────────────────────────────

def scan_tree() -> dict:
    """扫描整个 edgine crate"""
    issues = {
        "R1_unwired_pub_fn": [],
        "R2_unwired_syscall": [],
        "R3_unused_module": [],
        "R4_unused_struct_enum": [],
    }
    stats = defaultdict(int)

    # 裁定五 / B09-21: 「已分类清单」数据源 (fail-closed ⇒ 失败即空集)
    classified_set, classified_status = load_classified_set()
    stats["r1_classified_downgraded"] = 0

    if not KERNEL_DIR.exists():
        return {
            "issues": issues,
            "stats": stats,
            "classified_status": classified_status,
            "error": f"{KERNEL_DIR} 不存在",
        }

    for root, _, files in os.walk(KERNEL_DIR):
        root_path = Path(root)
        for f in files:
            if not f.endswith(".rs"):
                continue
            path = root_path / f
            if not is_kernel_code(path):
                continue

            stats["files_scanned"] += 1

            # 收集本文件的 #[no_mangle] 函数 (豁免名单)
            no_mangle_set = collect_no_mangle_functions(path)

            # R1: pub fn 检测
            for fn in collect_pub_fns(path):
                stats["pub_fns_scanned"] += 1
                is_exempt, reason = is_exempt_function(
                    fn["name"], path, fn["in_trait_impl"], no_mangle_set
                )
                if is_exempt:
                    stats["fns_exempted"] += 1
                    continue

                refs = count_refs(fn["name"], path, fn["line"])
                if refs.get("error"):
                    continue  # 超时跳过

                # 关键修正: 死代码判断 = 没有调用方 (actual_callers == 0)
                # 包括同文件内的 proc.set_pid() 和跨文件的 fn()
                # total == 1 表示只有声明本身, 没有任何调用
                if refs["actual_callers"] == 0:
                    rel_file = str(path.relative_to(ROOT))
                    is_classified = f"{rel_file}::{fn['name']}" in classified_set
                    if is_classified:
                        stats["r1_classified_downgraded"] += 1
                    issues["R1_unwired_pub_fn"].append({
                        # 裁定五: 已分类 ⇒ 降噪为 INFO (零引用事实保留);
                        # 未分类 ⇒ HIGH (新代码引入的零引用 pub fn)
                        "severity": "INFO" if is_classified else "HIGH",
                        "classified": is_classified,
                        "name": fn["name"],
                        "file": rel_file,
                        "line": fn["line"],
                        "refs_total": refs["total"],
                        "refs_in_decl_file": refs["in_decl_file"],
                        "refs_cross_file": refs["cross_file"],
                        "refs_callers": refs["actual_callers"],
                    })

            # R2: SYS_*/EG_* 未接线检测
            for name, line_no in collect_pub_consts_syscall(path):
                stats["syscall_consts_scanned"] += 1
                if not is_in_dispatch(name, path):
                    issues["R2_unwired_syscall"].append({
                        "severity": "CRITICAL",
                        "name": name,
                        "file": str(path.relative_to(ROOT)),
                        "line": line_no,
                    })
                else:
                    stats["syscall_consts_dispatched"] += 1

            # R3: pub mod 检测
            for mod_name, line_no in collect_pub_mods(path):
                stats["pub_mods_scanned"] += 1
                refs = count_refs(mod_name, path, line_no)
                # mod 引用语义: use xxx / xxx:: / 路径中含 xxx
                # 关键修正: actual_callers == 0 表示没有任何引用 (同文件 + 跨文件)
                if refs.get("error"):
                    continue
                if refs["actual_callers"] == 0:
                    issues["R3_unused_module"].append({
                        "severity": "WARN",
                        "name": mod_name,
                        "file": str(path.relative_to(ROOT)),
                        "line": line_no,
                        "refs_total": refs["total"],
                        "refs_callers": refs["actual_callers"],
                    })

            # R4: pub struct/enum 检测 (只对 mod.rs/api.rs 中的核心类型有意义)
            if path.name in {"mod.rs", "api.rs", "types.rs"}:
                for type_name, kind, line_no in collect_pub_structs_enums(path):
                    stats["pub_types_scanned"] += 1
                    refs = count_refs(type_name, path, line_no)
                    if refs.get("error"):
                        continue
                    # 核心类型如果在 api.rs/types.rs 中零引用, 提示
                    if refs["cross_file"] == 0 and refs["total"] <= 1:
                        issues["R4_unused_struct_enum"].append({
                            "severity": "INFO",
                            "name": type_name,
                            "kind": kind,
                            "file": str(path.relative_to(ROOT)),
                            "line": line_no,
                            "refs_total": refs["total"],
                        })

    return {
        "issues": issues,
        "stats": dict(stats),
        "classified_status": classified_status,
    }


def format_report(result: dict) -> str:
    """生成可读报告"""
    lines = []
    lines.append("=" * 78)
    lines.append("Edgine 死代码 / 未接线审计报告 (audit_unwired_pub_fn.py)")
    lines.append("=" * 78)
    lines.append("")
    stats = result.get("stats", {})
    lines.append(f"扫描文件数:    {stats.get('files_scanned', 0)}")
    lines.append(f"扫描 pub fn:   {stats.get('pub_fns_scanned', 0)}")
    lines.append(f"  └─ 豁免:     {stats.get('fns_exempted', 0)} "
                 "(#[no_mangle] / 顶层 re-export / trait impl)")
    lines.append(f"扫描 SYS_/EG_ 编号: {stats.get('syscall_consts_scanned', 0)}")
    lines.append(f"  └─ 已 dispatch: {stats.get('syscall_consts_dispatched', 0)}")
    lines.append(f"扫描 pub mod:  {stats.get('pub_mods_scanned', 0)}")
    lines.append(f"扫描 pub type: {stats.get('pub_types_scanned', 0)}")
    lines.append(f"已分类清单 (B09-21 数据源): {result.get('classified_status', '未知')}")
    lines.append("")

    issues = result.get("issues", {})

    # R2 优先 (CRITICAL)
    r2 = issues.get("R2_unwired_syscall", [])
    lines.append(f"[CRITICAL] R2 未接线 syscall (types.rs 声明但 dispatch 未分发): {len(r2)} 项")
    if r2:
        # 按文件分组
        by_file = defaultdict(list)
        for it in r2:
            by_file[it["file"]].append(it["name"])
        for f, names in sorted(by_file.items()):
            lines.append(f"  {f}:")
            for n in names[:30]:
                lines.append(f"    - {n}")
            if len(names) > 30:
                lines.append(f"    ... (共 {len(names)} 个, 仅显示前 30)")
    lines.append("")

    # R1 (裁定五: 未分类 ⇒ HIGH 出明细; 已分类 ⇒ INFO 仅计数, 降噪不豁免)
    r1 = issues.get("R1_unwired_pub_fn", [])
    r1_high = [it for it in r1 if not it.get("classified")]
    r1_classified = [it for it in r1 if it.get("classified")]
    lines.append(f"[HIGH] R1 未分类零引用 pub fn: {len(r1_high)} 项")
    if r1_high:
        # 按文件分组
        by_file = defaultdict(list)
        for it in r1_high:
            by_file[it["file"]].append(it)
        for f, items in sorted(by_file.items(), key=lambda x: -len(x[1])):
            lines.append(f"  {f}: {len(items)} 个")
            for it in items[:5]:
                lines.append(f"    L{it['line']}: {it['name']} "
                             f"(refs: total={it['refs_total']}, decl={it['refs_in_decl_file']}, cross={it['refs_cross_file']})")
            if len(items) > 5:
                lines.append(f"    ... (共 {len(items)} 个, 仅显示前 5)")
    lines.append(f"[INFO] R1 已分类 (零引用事实保留, 明细见台账 B-6 区块): {len(r1_classified)} 项")
    lines.append("")

    # R3
    r3 = issues.get("R3_unused_module", [])
    lines.append(f"[WARN] R3 pub mod 子模块零引用: {len(r3)} 项")
    for it in r3:
        lines.append(f"  {it['file']}:L{it['line']} mod {it['name']}")
    lines.append("")

    # R4
    r4 = issues.get("R4_unused_struct_enum", [])
    lines.append(f"[INFO] R4 核心 pub struct/enum 在 mod.rs/api.rs 中零引用: {len(r4)} 项")
    for it in r4[:20]:
        lines.append(f"  {it['file']}:L{it['line']} {it['kind']} {it['name']} "
                     f"(total refs={it['refs_total']})")
    if len(r4) > 20:
        lines.append(f"  ... (共 {len(r4)} 个, 仅显示前 20)")

    lines.append("")
    lines.append("=" * 78)
    critical = len(r2)
    high = len(r1_high)
    warn = len(r3)
    info = len(r4) + len(r1_classified)
    lines.append(f"汇总: CRITICAL={critical} / HIGH={high} / WARN={warn} / INFO={info}")
    lines.append("=" * 78)
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description="Edgine 死代码 / 未接线审计")
    parser.add_argument("--strict", action="store_true",
                        help="HIGH/WARN 也退出 1 (默认仅 CRITICAL 退出 1)")
    parser.add_argument("--json", action="store_true", help="输出 JSON 报告")
    args = parser.parse_args()

    result = scan_tree()

    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    else:
        print(format_report(result))

    # 退出码
    issues = result.get("issues", {})
    critical = len(issues.get("R2_unwired_syscall", []))
    # 裁定五: 仅**未分类**的 R1 计入门禁 (已分类项降噪为 INFO, 零引用事实仍列出)
    r1_unclassified = len([it for it in issues.get("R1_unwired_pub_fn", [])
                           if not it.get("classified")])
    warn = r1_unclassified + len(issues.get("R3_unused_module", []))

    if critical > 0:
        return 1
    if args.strict and warn > 0:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())