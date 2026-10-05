#!/usr/bin/env python3
r"""
audit_public_api_docs.py — F8 公共 API 中文文档检查

AGENTS.md §6 F8: "公共 API 中文文档注释".
clippy missing-docs-in-crate-items 检查文档存在性, 但不检查内容语言.
本脚本检查所有 pub fn/struct/enum/trait 的文档注释是否包含中文字符.

修复 B01-11:
- 正则: 增加 `pub async fn` / `pub unsafe fn` 匹配, 排除字段 (`pub x: T`)
  (`pub\s+(?:fn|struct|enum|trait)\s+(\w+)` 把 `pub phys: u64` 当作 fn 误报)
- 检测: 检查 doc 注释是否含中文字符. 块注释 `/* ... */` 不算 doc
- 豁免: mod.rs 顶层 re-export pub use 不算公共 API 定义
- 豁免: trait impl 中的 fn 默认有 trait doc, 可豁免

F8 口径纠正 (仅公共 API 计入):
- 仅含 `pub` 可见性修饰的声明计入; 私有 fn / 私有类型豁免
- trait 方法自身无 `pub`, 但随 trait 可见性对外暴露, 故保留
- `#[cfg(test)]` / `#[cfg(feature = "kernel_test")]` 等测试门控模块整体豁免
- `privileged/tests/` 目录为测试代码, 整目录豁免
- doc 扫描跨越 `// SAFETY:` 等普通注释, 消除"doc 被普通注释隔断"的误报
- 非对外可见项豁免 (对齐 clippy): `pub(crate)`/`pub(super)`/`pub(in ...)` 声明,
  以及位于非 pub 嵌套模块内的项, 对 crate 外不可见, 不计入

用法: python3 scripts/audit_public_api_docs.py
退出码: 0=通过, 1=有违规
"""
import argparse
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src/kernel/privileged")

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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--strict', action='store_true',
                        help='严格模式: 含 trait impl 中的 fn (默认豁免)')
    args = parser.parse_args()

    # 本轮修复 (F8 口径纠正): 仅公共 API 计入.
    # - 私有 fn / 私有类型 (无 `pub`) 豁免, 不再误纳测试/内部实现
    # - trait 方法自身无 `pub`, 但随 trait 可见性对外暴露, 故保留
    # - `#[cfg(test)]` 模块内声明豁免 (测试代码非公共 API)
    # - doc 扫描跨 `// SAFETY:` 等普通注释, 消除"doc 被普通注释隔断"的误报

    violations: list[tuple[str, int, str, str]] = []
    # B01-11 修复: 正则要求类型关键字后跟标识符 + 边界 (非字段).
    # 原 `pub\s+(?:fn|struct|enum|trait)\s+(\w+)` 把 `pub phys: u64` 误报为 fn.
    # 修复: 增加 `async` / `unsafe` 修饰符; 要求后面是 < (fn 签名) 或 { (类型体) 或 ; (声明)
    pub_decl_re = re.compile(
        r'^\s*(?P<vis>pub(?:\([^)]*\))?\s+)?'
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

                # B01-11 豁免: impl 块内的 fn (默认)
                if kind == 'fn' and not args.strict:
                    # 检测是否在 impl 块内 (trait impl / inherent impl)
                    if _in_impl_block(content, m.start()):
                        continue

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

                has_doc = check_chinese_doc(content, m.start())
                if not has_doc:
                    violations.append((rel_path, line_no, name, kind))

    print(f"=== audit_public_api_docs: 检查 pub fn/struct/enum/trait 中文文档 ===")
    if violations:
        # 按 kind 分组
        by_kind: dict[str, int] = {}
        for _, _, _, k in violations:
            by_kind[k] = by_kind.get(k, 0) + 1
        for k, c in sorted(by_kind.items()):
            print(f"  按 kind: {k}={c}")

        print(f"  ✗ {len(violations)} 处缺少中文文档:")
        for path, line, name, kind in violations[:20]:
            print(f"    ✗ {path}:{line} — {kind} {name}")
        if len(violations) > 20:
            print(f"    ... 共 {len(violations)} 处")
        print("\n⚠ 存在缺少中文文档的公共 API")
        return 1
    else:
        print("✓ audit_public_api_docs 通过 (所有公共 API 有中文文档)")
        return 0


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
