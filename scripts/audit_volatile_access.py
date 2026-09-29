#!/usr/bin/env python3
"""
audit_volatile_access.py — LTO 错位防线: volatile 字段访问检查 (2026-07-02 新增)

检查关键字段 (bitmap_size, free_list_head, heap_end 等) 是否使用 volatile 访问.
已知 LTO 错位 bug: set_bit 内 self.bitmap_size.get() 被错位到 self.failed_allocs.

2026-07-31 扩展: 新增 heap_end 字段检测, 兼容 Ref 抽象访问模式
(FreeListHeadRef / HeapEndRef, 通过 addr_of! + read_volatile/write_volatile).

用法: python3 scripts/audit_volatile_access.py
退出码: 0=通过, 1=有风险
"""
import re
import sys
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src/kernel")

# 高风险字段: 曾经被 LTO 错位或有 LTO 错位风险
# 格式: (文件, 字段名, Ref 抽象名 or None, 访问模式)
# - 访问模式 'ref':    通过 XxxRef::new(addr_of!(self.field)) 访问 (内部 read_volatile/write_volatile)
# - 访问模式 'direct': 通过 self.field.get() 直接访问 (pmm.rs 模式, 需 volatile/raw ptr 上下文)
# - 访问模式 'atomic': 通过 AtomicXxx 的 load/store 访问 (pi_mutex.rs 模式).
#   Atomic 类型由编译器保证对齐, load/store 自带 volatile 语义与内存序,
#   天然免疫 LTO 字段错位 (与 Ref 抽象同等安全, 优于 UnsafeCell+get()).
RISKY_FIELDS = [
    ("framework/mm/pmm.rs", "bitmap_size", None, "direct"),
    ("framework/mm/kmalloc.rs", "free_list_head", "FreeListHeadRef", "ref"),
    ("framework/mm/kmalloc.rs", "heap_end", "HeapEndRef", "ref"),
    ("framework/sync/pi_mutex.rs", "effective_priority", None, "atomic"),
]

# Ref 抽象使用模式: XxxRef::new(addr_of!(self.field))
REF_ACCESS_RE_TEMPLATE = r'{ref_name}\s*::\s*new\s*\(\s*core\s*::\s*ptr\s*::\s*addr_of!\s*\(\s*self\.{field}\s*\)\s*\)'

# 直接 UnsafeCell.get() 访问模式: self.field.get()
DIRECT_ACCESS_RE_TEMPLATE = r'self\.{field}\.get\(\)'

# AtomicXxx 访问模式: `.field.load(` / `.field.store(` (可跨行, 如 self.inner\n.field\n.store)
ATOMIC_ACCESS_RE_TEMPLATE = r'\.{field}\s*\.\s*(?:load|store)\s*\('

def check_ref_access(content, field_name, ref_name):
    """检查通过 Ref 抽象访问的字段: XxxRef::new(addr_of!(self.field))."""
    pattern = re.compile(
        REF_ACCESS_RE_TEMPLATE.format(ref_name=re.escape(ref_name), field=re.escape(field_name)),
        re.MULTILINE,
    )
    return list(pattern.finditer(content))

def check_direct_access(content, field_name):
    """检查直接 self.field.get() 访问 (pmm.rs 模式: 搭配 read_volatile/raw ptr)."""
    pattern = re.compile(
        DIRECT_ACCESS_RE_TEMPLATE.format(field=re.escape(field_name)),
        re.MULTILINE,
    )
    return list(pattern.finditer(content))

def check_direct_access_violations(content, field_name, accesses):
    """检查直接访问是否在 raw pointer + read_volatile 上下文中."""
    violations = []
    for m in accesses:
        start = max(0, m.start() - 500)
        end = min(len(content), m.end() + 200)
        context = content[start:end]
        # 排除注释行
        line_start = content.rfind('\n', 0, m.start()) + 1
        line_text = content[line_start:m.start()]
        if line_text.strip().startswith('//'):
            continue
        # 排除 init-only 路径 (count_free_pages 仅在 init 时调用, 非热路径)
        # 注意: 取窗口内最近 (最后一个) fn 而非最左 fn — re.search 取最左,
        # 当包裹函数较短时, 上一个函数头 (如 fn test_bit) 也会落入 500 字符
        # 窗口, 导致豁免判定失配 (2026-09-13 pmm.rs:1212 误报根因,
        # DECISION-O ① mm 专项核实修正; fail-closed 路径不变)
        fn_search_start = max(0, m.start() - 500)
        fn_matches = re.findall(r'fn\s+(\w+)', content[fn_search_start:m.start()])
        if fn_matches and fn_matches[-1] == 'count_free_pages':
            continue
        has_volatile = 'read_volatile' in context or 'addr_of' in context
        has_raw_ptr = 'self as *const Self as *const u8' in context or 'p.add(' in context
        if not has_volatile and not has_raw_ptr:
            line_no = content[:m.start()].count('\n') + 1
            violations.append((line_no, content[m.start():m.end()]))
    return violations

def check_atomic_access(content, field_name):
    """检查 AtomicXxx 字段的 load/store 访问 (pi_mutex.rs 模式).

    Atomic 类型的 load/store 由编译器保证对齐 + volatile 语义 + 内存序,
    是 LTO 错位防护的最优形态 (优于 UnsafeCell+get() 手写 volatile).
    """
    pattern = re.compile(
        ATOMIC_ACCESS_RE_TEMPLATE.format(field=re.escape(field_name)),
        re.MULTILINE,
    )
    return list(pattern.finditer(content))

def check_field_access(filename, field_name, ref_name, mode):
    """检查指定字段的访问方式 (ref / direct / atomic)."""
    path = os.path.join(SRC, filename)
    if not os.path.exists(path):
        return None, f"文件不存在: {filename}"
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        content = f.read()

    if mode == "ref":
        # Ref 抽象模式: 检查 XxxRef::new(addr_of!(self.field))
        ref_accesses = check_ref_access(content, field_name, ref_name)
        if not ref_accesses:
            return None, f"未找到 {ref_name}::new(addr_of!(self.{field_name})) 访问"
        # Ref 抽象内部用 read_volatile/write_volatile (已审计), 外部访问点视为安全
        return [], None
    elif mode == "atomic":
        # 原子访问模式: 检查 .field.load()/.store() (自封装, 无需 volatile 上下文)
        atomic_accesses = check_atomic_access(content, field_name)
        if not atomic_accesses:
            return None, f"未找到 .{field_name}.load()/.store() 原子访问"
        return [], None
    else:
        # 直接访问模式 (pmm.rs 模式): 检查 self.field.get() 是否在 volatile 上下文中
        direct_accesses = check_direct_access(content, field_name)
        if not direct_accesses:
            return None, f"未找到 self.{field_name}.get() 访问"
        return check_direct_access_violations(content, field_name, direct_accesses), None

def main():
    all_violations = []
    all_errors = []
    for filename, field, ref_name, mode in RISKY_FIELDS:
        violations, err = check_field_access(filename, field, ref_name, mode)
        if err:
            # B01-17 修复: fail-closed 原则. err 分支不再放行, 视为违规.
            # 例如: 文件不存在 / 未找到 Ref 访问 / 未找到直接访问 → 视为
            # "无法验证 = 违规", 强制开发者确保访问模式可见.
            print(f"  ⚠ {filename}:{field}: {err}")
            all_errors.append((filename, field, err))
        elif violations:
            all_violations.extend([(filename, field, lv) for lv in violations])
        else:
            access_desc = {
                "ref": f"{ref_name}::new(addr_of!(self.{field}))",
                "atomic": f".{field}.load()/.store() 原子访问",
                "direct": "volatile/raw pointer",
            }[mode]
            print(f"  ✓ {filename}:{field}: 已用 {access_desc} 访问")

    print(f"=== audit_volatile_access: 检查 {len(RISKY_FIELDS)} 个高风险字段 ===")
    # B01-17: fail-closed. err 或 violations 任一非空即视为违规
    if all_violations or all_errors:
        if all_violations:
            print(f"  ✗ {len(all_violations)} 处非 volatile 访问:")
            for fn, field, (line, text) in all_violations:
                print(f"    ✗ {fn}:{line} — self.{field}.get()")
        if all_errors:
            print(f"  ✗ {len(all_errors)} 处 fail-closed 错误 (无法验证):")
            for fn, field, err in all_errors:
                print(f"    ✗ {fn}:{field}: {err}")
        print("\n⚠ 存在 LTO 错位风险 (或无法验证访问模式)")
        sys.exit(1)
    else:
        print("✓ audit_volatile_access 通过")
        sys.exit(0)

if __name__ == "__main__":
    main()
