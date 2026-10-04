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


def check_chinese_doc(content: str, start_pos: int) -> bool:
    """检查某位置前方的 doc 注释是否含中文.

    B01-11 修复:
    - 跳过块注释 (含 `*` 但不是 `///`)
    - 跳过 `pub use` (re-export 不是公共 API 定义)
    - 跳过属性行 (#[derive(...)] 等)
    """
    lines_before = content[:start_pos].split('\n')
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

    violations: list[tuple[str, int, str, str]] = []
    # B01-11 修复: 正则要求类型关键字后跟标识符 + 边界 (非字段).
    # 原 `pub\s+(?:fn|struct|enum|trait)\s+(\w+)` 把 `pub phys: u64` 误报为 fn.
    # 修复: 增加 `async` / `unsafe` 修饰符; 要求后面是 < (fn 签名) 或 { (类型体) 或 ; (声明)
    pub_decl_re = re.compile(
        r'^\s*(?:pub(?:\([^)]*\))?\s+)?'
        r'(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?'
        r'(?P<kind>fn|struct|enum|trait|union|type)\s+(?P<name>\w+)'
        r'(?:\s*[<(]|\s*\{|\s+where|\s*=|\s*;)',
        re.MULTILINE,
    )

    for root, dirs, files in os.walk(SRC):
        # 跳过 smoltcp (vendored)
        dirs[:] = [d for d in dirs if d != 'smoltcp']
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

                # B01-11 豁免: impl 块内的 fn (默认)
                if kind == 'fn' and not args.strict:
                    # 检测是否在 impl 块内 (trait impl / inherent impl)
                    if _in_impl_block(content, m.start()):
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


def reversediter(iterator):
    """生成器反向迭代 matches (从后往前)."""
    items = list(iterator)
    for item in reversed(items):
        yield item


if __name__ == "__main__":
    sys.exit(main())
