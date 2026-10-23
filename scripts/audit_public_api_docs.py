#!/usr/bin/env python3
r"""
audit_public_api_docs.py — F8 公共 API 中文文档检查

AGENTS.md §5 F8: "公共 API 中文文档注释".
本脚本检查 `privileged/` 树下 pub fn/struct/enum/trait 的文档注释是否**存在且含中文**.

F8 执法口径的两层分工 (P-59 裁定 ①: 编译器作准):
- **存在性**由 rustc `missing_docs` 判定, 是唯一权威口径. 已完成补写的模块在文件内
  写 `#![deny(missing_docs)]`, 由门槛 1/2 直接硬执法 (无豁免); 未达标的存量由
  `scripts/audit_missing_docs_debt.py` 计数棘轮拦住新增.
- 本脚本判定的是**内容语言** (中文) + impl 块内 fn 的加严面, 是编译器面的补充,
  不取代它. 注: `clippy::missing_docs_in_crate_items` 是不存在的 lint 名 (真名为
  `clippy::missing_docs_in_private_items`, 属 restriction 组, 门槛从不启用),
  旧文档据此声称的"clippy 执法"从未生效.

修复 B01-11:
- 正则: 增加 `pub async fn` / `pub unsafe fn` 匹配, 排除字段 (`pub x: T`)
  (`pub\s+(?:fn|struct|enum|trait)\s+(\w+)` 把 `pub phys: u64` 当作 fn 误报)
- 检测: 检查 doc 注释是否含中文字符. 块注释 `/* ... */` 不算 doc
- 豁免: mod.rs 顶层 re-export pub use 不算公共 API 定义
- 豁免: impl 块 (trait impl 与 inherent impl 均含) 内的 fn 分属两面 ——
  **默认面**豁免它, **impl 面**把它计入并与机器派生基线对账;
  实测本仓 impl 面的 533 处**全是 inherent impl 方法**, trait impl 命中 0

F8 口径纠正 (仅公共 API 计入):
- 仅含 `pub` 可见性修饰的声明计入; 私有 fn / 私有类型豁免
- trait 方法自身无 `pub`, 但随 trait 可见性对外暴露, 故保留
- `#[cfg(test)]` / `#[cfg(feature = "kernel_test")]` 等测试门控模块整体豁免
- `privileged/tests/` 目录为测试代码, 整目录豁免
- doc 扫描跨越 `// SAFETY:` 等普通注释, 消除"doc 被普通注释隔断"的误报
- 非对外可见项豁免: `pub(crate)`/`pub(super)`/`pub(in ...)` 声明,
  以及位于非 pub 嵌套模块内的项, 对 crate 外不可见, 不计入
  (编译器 `missing_docs` 口径与本豁免集**互补**: 实测两侧 0 处重合)

用法 (CI 步骤 2e 裸调本脚本, 故一次扫描同时判两面, 不靠参数分流):
  python3 scripts/audit_public_api_docs.py                       # 两面都判, 取最严退出码
  python3 scripts/audit_public_api_docs.py --update-baseline     # 用当前读数重写基线
退出码: 0=两面都过, 1=默认面有违规或 impl 面与基线不符,
        2=基线不可用/扫描失败 (fail-closed)
"""
import argparse
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src/kernel/privileged")
BASELINE_FILE = os.path.join(ROOT, "scripts", "audit_public_api_docs.baseline.json")

# 中文字符正则
CJK_RE = re.compile(r'[\u4e00-\u9fff]')


def _strip_attr_lines(content: str) -> str:
    """将属性块 (`#[...]` / `#![...]`, 含多行) 整体替换为空行, 保留行数.

    供 `check_chinese_doc` 做行级 doc 扫描: 多行属性 (如 `#[expect(\n ... \n)]`)
    的续行 (`)]` 等) 既非 doc 也非普通注释, 原实现会误断扫描; 替换为空行后,
    doc 扫描可跨越属性块继续上溯.
    """
    lines = content.split('\n')
    out = list(lines)
    i = 0
    n = len(lines)
    while i < n:
        s = lines[i].lstrip()
        if s.startswith('#[') or s.startswith('#!['):
            depth = lines[i].count('[') - lines[i].count(']')
            j = i
            while depth > 0 and j + 1 < n:
                j += 1
                depth += lines[j].count('[') - lines[j].count(']')
            for k in range(i, j + 1):
                out[k] = ''
            i = j + 1
        else:
            i += 1
    return '\n'.join(out)


def check_chinese_doc(content: str, start_pos: int) -> bool:
    """检查某位置前方的 doc 注释是否含中文.

    B01-11 修复:
    - 跳过块注释 (含 `*` 但不是 `///`)
    - 跳过 `pub use` (re-export 不是公共 API 定义)
    - 跳过属性行 (#[derive(...)] 等)

    本轮修复: 非 doc 的普通注释行 (如 `// SAFETY:`) 不断开扫描.
    原实现遇到 `// SAFETY:` 即 break, 使"doc 注释被普通注释隔断"的公共项被误报.
    现仅 `///` / `//!` 计为 doc, 普通 `//` 行继续向上扫描.

    本轮修复 ②: 多行属性块 (`#[expect(\n ... \n)]`) 的续行 (如 `)]`) 不以 `#[`
    开头, 原实现遇之即 break, 使"doc 被多行属性块隔断"的公共项被误报.
    现将属性块整体视为空白后再扫描, 消除该误报.
    """
    decl_line = content[:start_pos].count('\n')
    lines_before = _strip_attr_lines(content).split('\n')[:decl_line]
    for line in reversed(lines_before):
        stripped = line.strip()
        # 块注释内 (/* * */) 不是 doc, 跳过
        if stripped.startswith('/*') or stripped.startswith('*'):
            continue
        if stripped.startswith('///') or stripped.startswith('//!'):
            if CJK_RE.search(stripped):
                return True
        elif stripped == '' or stripped.startswith('#['):
            continue
        elif stripped.startswith('//'):
            # 普通注释 (非 doc, 如 `// SAFETY:`): 不断开扫描
            continue
        elif stripped.startswith('pub use'):
            # pub use 为 re-export, 不是公共 API 定义
            return True
        else:
            break
    return False


def totals_of(entries: dict) -> dict:
    """由条目自身求和的自洽校验位 (手改快照而不重跑 --update-baseline 即不自洽)."""
    return {"sites": sum(entries.values()),
            "files": len({p for p, _, _ in entries}),
            "keys": len(entries)}


def dump_baseline(entries: dict) -> str:
    """确定性序列化 (排序 + 固定缩进), 保证 --update-baseline 幂等到字节级."""
    payload = {
        "_meta": {
            "generated_by": "python3 scripts/audit_public_api_docs.py --update-baseline",
            "口径": "impl 面 (impl 块内 pub fn 也计入) 的存量冻结, 键为 (文件, kind, 名称); "
                   "实测本仓 533 处全是 inherent impl 方法. 默认口径 (impl 豁免) 不进基线, 恒为硬零.",
            "disposition": "P-59 裁定 ②: 基线只拦新增, 不是豁免; 收缩归 P-66 分批补写工程",
        },
        "totals": totals_of(entries),
        "entries": [[p, k, n, c] for (p, k, n), c in sorted(entries.items())],
    }
    return json.dumps(payload, ensure_ascii=False, indent=2) + "\n"


def load_baseline():
    """读快照; 读不到/格式坏/totals 不自洽一律 fail-closed (返回 None + 成因)."""
    if not os.path.isfile(BASELINE_FILE):
        return None, (f"基线快照缺失: {os.path.basename(BASELINE_FILE)} "
                      f"(须显式跑 --update-baseline 生成并提交)")
    try:
        with open(BASELINE_FILE, encoding='utf-8') as f:
            payload = json.load(f)
        entries = {(e[0], e[1], e[2]): int(e[3]) for e in payload["entries"]}
    except (OSError, ValueError, KeyError, TypeError, IndexError) as exc:
        return None, f"基线快照无法解析: {exc}"
    declared = payload.get("totals", {})
    if declared != totals_of(entries):
        return None, (f"基线快照 totals 与 entries 不自洽: 声明 {declared} / 实算 {totals_of(entries)} "
                      f"—— 手改过快照, 请重跑 --update-baseline")
    return entries, ""


def entries_of(violations) -> dict:
    """把 (路径, 行号, 名称, kind) 归并成基线键 (行号随编辑漂移, 故不入键)."""
    out: dict = {}
    for path, _line, name, kind in violations:
        key = (path, kind, name)
        out[key] = out.get(key, 0) + 1
    return out


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--update-baseline', action='store_true',
                        help='用当前 impl 面读数重写基线快照 (唯一写者)')
    args = parser.parse_args()

    # 两面同源一次扫描: CI 步骤 2e 按 `python3 scripts/<name>.py` 裸调, 不传参数,
    # 所以两面不能靠 flag 分流. 命中项打上 in_impl 标记后派生两个集合 ——
    # 默认面 = 标记为 False 的子集 (恒硬零, 不进基线),
    # impl 面 = 全集 (与机器派生基线双向对账).
    #
    # 本轮修复 (F8 口径纠正): 仅公共 API 计入.
    # - 私有 fn / 私有类型 (无 `pub`) 豁免, 不再误纳测试/内部实现
    # - trait 方法自身无 `pub`, 但随 trait 可见性对外暴露, 故保留
    # - `#[cfg(test)]` 模块内声明豁免 (测试代码非公共 API)
    # - doc 扫描跨 `// SAFETY:` 等普通注释, 消除"doc 被普通注释隔断"的误报

    hits: list[tuple[str, int, str, str, bool]] = []
    # B01-11 修复: 正则要求类型关键字后跟标识符 + 边界 (非字段).
    # 原 `pub\s+(?:fn|struct|enum|trait)\s+(\w+)` 把 `pub phys: u64` 误报为 fn.
    # 修复: 增加 `async` / `unsafe` 修饰符; 要求后面是 < (fn 签名) 或 { (类型体) 或 ; (声明)
    # P-59 修复: 行首空白只吞 `[ \t]`, 不吞 `\n`. 原 `^\s*` 会把声明前的空行一并
    # 吃进 match, 使 `m.start()` 落在空行上 ⇒ 报告行号比声明行小 (实测 domain.rs 106→105、
    # 222→221). 判定结论不受影响 (doc 上溯对空行本就直接跳过), 但按行号施工会错锚.
    pub_decl_re = re.compile(
        r'^[ \t]*(?P<vis>pub(?:\([^)]*\))?\s+)?'
        r'(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?'
        r'(?P<kind>fn|struct|enum|trait|union|type)\s+(?P<name>\w+)'
        r'(?:\s*[<(]|\s*\{|\s+where|\s*=|\s*;)',
        re.MULTILINE,
    )

    for root, dirs, files in os.walk(SRC):
        # 跳过 smoltcp (vendored) 与 tests (测试/验证脚手架)
        # 本轮修复: `privileged/tests/` 为测试代码, 不是公共 API, 整目录豁免
        dirs[:] = [d for d in dirs if d not in ('smoltcp', 'tests')]
        for fname in files:
            if not fname.endswith('.rs'):
                continue
            fpath = os.path.join(root, fname)
            rel_path = os.path.relpath(fpath, ROOT)
            with open(fpath, 'r', encoding='utf-8', errors='replace') as f:
                content = f.read()

            for m in pub_decl_re.finditer(content):
                kind = m.group('kind')
                name = m.group('name')
                line_no = content[:m.start()].count('\n') + 1
                is_pub = m.group('vis') is not None

                # 本轮修复: `#[cfg(test)]` 内声明为测试代码, 非公共 API, 豁免
                if _in_cfg_test_mod(content, m.start()):
                    continue

                # B01-11 分面: impl 块内的 fn 只进 impl 面, 不进默认面.
                in_impl = kind == 'fn' and _in_impl_block(content, m.start())

                # 本轮修复: 仅公共 API 计入. 私有项 (无 `pub`) 豁免; trait 方法
                # 自身无 `pub`, 随 trait 可见性对外暴露, 故保留.
                if not is_pub and not (kind == 'fn' and _in_trait_body(content, m.start())):
                    continue

                # 本轮修复 (对齐 clippy 语义): 非对外可见项豁免, 只检查外部可达项.
                # - `pub(crate)`/`pub(super)`/`pub(in ...)` 声明对 crate 外不可见 -> 豁免
                # - 纯 `pub` 项若位于任一非 pub 外层嵌套模块内 -> 对外不可见 -> 豁免
                # - trait 方法随 `pub trait` 对外暴露, 但 trait 所在模块非 pub 时亦豁免
                if _in_nonpub_mod(content, m.start()):
                    continue
                if is_pub and m.group('vis').strip() != 'pub':
                    continue

                # B01-11 豁免: pub use re-export 不是公共 API 定义
                # 已经在正则层面处理 (pub use 后跟; 不匹配 fn/struct/enum/trait)

                if not check_chinese_doc(content, m.start()):
                    hits.append((rel_path, line_no, name, kind, in_impl))

    default_violations = [(p, l, n, k) for p, l, n, k, impl in hits if not impl]
    strict_violations = [(p, l, n, k) for p, l, n, k, _impl in hits]

    print("=== audit_public_api_docs: 检查 pub fn/struct/enum/trait 中文文档 "
          "[默认面硬零 + impl 面对账] ===")
    for label, vs in (("默认面", default_violations), ("impl 面", strict_violations)):
        by_kind: dict[str, int] = {}
        for _, _, _, k in vs:
            by_kind[k] = by_kind.get(k, 0) + 1
        print(f"  {label} 按 kind: " + (", ".join(f"{k}={c}" for k, c in sorted(by_kind.items()))
                                       or "无"))

    # ── 判 1: 默认面恒为硬零 (不进基线) ──────────────────────────
    if default_violations:
        print(f"  ✗ 默认面 {len(default_violations)} 处缺少中文文档:")
        for path, line, name, kind in default_violations[:20]:
            print(f"    ✗ {path}:{line} — {kind} {name}")
        if len(default_violations) > 20:
            print(f"    ... 共 {len(default_violations)} 处")
    else:
        print("✓ 默认面通过 (可达公共 API 全部有中文文档)")

    # ── 判 2: impl 面与机器派生基线双向对账 ───────────────────────
    entries = entries_of(strict_violations)
    if args.update_baseline:
        with open(BASELINE_FILE, 'w', encoding='utf-8') as f:
            f.write(dump_baseline(entries))
        t = totals_of(entries)
        print(f"\n基线快照已重写: {os.path.basename(BASELINE_FILE)} "
              f"({t['sites']} 处 / {t['keys']} 键 / {t['files']} 文件)")
        return 1 if default_violations else 0

    baseline, err_msg = load_baseline()
    if baseline is None:
        print(f"✗ {err_msg} (fail-closed, 不接受假绿)")
        return 2
    print(f"  基线快照: {totals_of(baseline)['sites']} 处 / {len(baseline)} 键 "
          f"({os.path.basename(BASELINE_FILE)})")

    over = sorted((key, entries[key], baseline.get(key, 0))
                  for key in entries if entries[key] > baseline.get(key, 0))
    under = sorted((key, baseline[key], entries.get(key, 0))
                   for key in baseline if baseline[key] > entries.get(key, 0))
    if not over and not under:
        print(f"✓ impl 面与基线一致 ({totals_of(entries)['sites']} 处存量记账; "
              f"新增未文档的 impl 方法即红, 补写后需重跑 --update-baseline)")
        return 1 if default_violations else 0

    for (path, kind, name), cur, base in over[:20]:
        print(f"    ✗ 新增 {path} — {kind} {name}: 当前 {cur} / 基线 {base}")
    for (path, kind, name), cur, base in under[:20]:
        print(f"    ✗ 快照过期 {path} — {kind} {name}: 当前 {cur} / 基线 {base}")
    if len(over) + len(under) > 40:
        print(f"    ... 新增 {len(over)} 项 / 减少 {len(under)} 项")
    print(f"  impl 面当前 {totals_of(entries)['sites']} 处 vs 基线 {totals_of(baseline)['sites']} 处 "
          f"(新增 {len(over)} 键 / 减少 {len(under)} 键)")
    print("新增 ⇒ 给该公共 API 补中文 doc, 而不是扩大基线; "
          "减少 ⇒ 确属补写, 跑 --update-baseline 重导快照.")
    return 1


def _in_impl_block(content: str, pos: int) -> bool:
    """检查 pos 位置是否在 impl 块内 (trait impl 或 inherent impl).

    B01-11 扩展: inherent impl (如 `impl Frame { ... }`) 内的 fn 也豁免,
    因 impl 块本身通常含 doc 说明整体功能.
    """
    # 匹配所有 impl 块: `impl ... {` 或 `impl ... for ... {`
    impl_start_re = re.compile(r'\bimpl\b[^;{]*\{')
    for m in reversediter(impl_start_re.finditer(content, 0, pos)):
        # 检查 impl 块是否闭合 (简化: 找最近一个闭合 `}`)
        impl_open_pos = m.end() - 1  # `{` 位置
        depth = 1
        i = impl_open_pos + 1
        while i < pos and i < len(content):
            c = content[i]
            if c == '{':
                depth += 1
            elif c == '}':
                depth -= 1
                if depth == 0:
                    break
            i += 1
        if depth > 0:
            # 仍未闭合, 仍在该 impl 块内
            return True
    return False


def _in_trait_impl(content: str, pos: int) -> bool:
    """检查 pos 位置是否在 trait impl 块内 (含 `for`)."""
    impl_start_re = re.compile(r'\bimpl\b[^;{]*\bfor\b[^;{]*\{')
    for m in reversediter(impl_start_re.finditer(content, 0, pos)):
        impl_open_pos = m.end() - 1
        depth = 1
        i = impl_open_pos + 1
        while i < pos and i < len(content):
            c = content[i]
            if c == '{':
                depth += 1
            elif c == '}':
                depth -= 1
                if depth == 0:
                    break
            i += 1
        if depth > 0:
            return True
    return False


def _in_cfg_test_mod(content: str, pos: int) -> bool:
    """检查 pos 是否位于测试门控模块内.

    覆盖 `#[cfg(test)]` / `#[cfg(feature = "kernel_test")]` /
    `#[cfg(any(feature = "kernel_test", feature = "host-test"))] mod ... { ... }`
    等测试/验证脚手架; 这类代码不是公共 API, 不计入 F8 检查.
    """
    mod_re = re.compile(
        r'#\[cfg\((?P<args>[^\]]*)\)\]\s*\n\s*'
        r'(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{'
    )
    for m in reversediter(list(mod_re.finditer(content, 0, pos))):
        if 'test' not in m.group('args'):
            continue
        open_pos = m.end() - 1
        depth = 1
        i = open_pos + 1
        while i < pos and i < len(content):
            c = content[i]
            if c == '{':
                depth += 1
            elif c == '}':
                depth -= 1
                if depth == 0:
                    break
            i += 1
        if depth > 0:
            return True
    return False


def _in_trait_body(content: str, pos: int) -> bool:
    """检查 pos 是否位于 `trait ... { ... }` 体内.

    trait 方法自身无 `pub`, 但随 trait 可见性对外暴露 (本 crate 内 trait 均为 pub).
    """
    trait_start_re = re.compile(r'\btrait\s+\w+[^;{]*\{')
    for m in reversediter(trait_start_re.finditer(content, 0, pos)):
        open_pos = m.end() - 1
        depth = 1
        i = open_pos + 1
        while i < pos and i < len(content):
            c = content[i]
            if c == '{':
                depth += 1
            elif c == '}':
                depth -= 1
                if depth == 0:
                    break
            i += 1
        if depth > 0:
            return True
    return False


def _in_nonpub_mod(content: str, pos: int) -> bool:
    """检查 pos 是否位于任一非对外可见的嵌套模块内.

    F8 对齐 clippy 语义: 仅"外部可达"项计公共 API.
    `pub(crate)` / `pub(super)` / `pub(in ...)` 模块, 以及私有模块,
    其内部项对 crate 外不可见, 故整体豁免.
    顶层模块可见性由 mod.rs 的 `pub mod` 决定, 此处只处理文件内嵌套模块.
    """
    mod_re = re.compile(
        r'^\s*(?P<vis>pub(?:\([^)]*\))?\s+)?mod\s+(?P<name>\w+)\s*\{',
        re.MULTILINE,
    )
    for m in reversediter(list(mod_re.finditer(content, 0, pos))):
        # 判断该 mod 块是否包住 pos (`{` 深度 > 0)
        open_pos = m.end() - 1
        depth = 1
        i = open_pos + 1
        while i < pos and i < len(content):
            c = content[i]
            if c == '{':
                depth += 1
            elif c == '}':
                depth -= 1
                if depth == 0:
                    break
            i += 1
        if depth == 0:
            continue  # 该 mod 未包住 pos
        vis = m.group('vis')
        if vis is None:
            return True  # 私有模块
        if vis.strip() != 'pub':
            return True  # pub(crate) / pub(super) / pub(in ...)
    return False


def reversediter(iterator):
    """生成器反向迭代 matches (从后往前)."""
    items = list(iterator)
    for item in reversed(items):
        yield item


if __name__ == "__main__":
    sys.exit(main())
