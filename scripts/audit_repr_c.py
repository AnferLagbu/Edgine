#!/usr/bin/env python3
"""
audit_repr_c.py — LTO 字段错位防线 (2026-07-02 新增)

检查关键 struct 是否有 #[repr(C)], 防止 LTO 在 release 模式重排字段.
已知 LTO 错位 bug: PhysicalMemoryManager (bitmap_size), KernelHeap (free_list_head),
IdentityTable (entries 数组溢出栈).

用法: python3 scripts/audit_repr_c.py
退出码: 0=通过, 1=有违规
"""
import re
import sys
import os

# 项目根目录
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src/kernel")

# 高风险 struct: 有 UnsafeCell/Cell/NonNull 字段 + 用于静态实例的
# 这些 struct 如果没有 repr(C), LTO 可能错位字段
CRITICAL_STRUCTS = [
    ("PhysicalMemoryManager", "src/kernel/framework/mm/pmm.rs", "bitmap/buddy_meta/buddy_heads 字段"),
    ("KernelHeap", "src/kernel/framework/mm/kmalloc.rs", "free_list_head UnsafeCell 字段"),
    # IdentityTable 已改为 Vec<PwmEntry> (heap 分配), 不再需要 repr(C)
    # PiMutexInner 是 private struct, 在 framework 内部使用
]

def find_repr_c(filename, struct_name):
    """检查文件中指定 struct 是否有 #[repr(C)]."""
    path = os.path.join(ROOT, filename)
    if not os.path.exists(path):
        return None, "文件不存在"
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        content = f.read()
    # 找 struct 定义位置, 检查前面几行是否有 repr(C)
    pattern = re.compile(
        r'pub\s+struct\s+' + re.escape(struct_name) + r'\s*\{',
        re.MULTILINE,
    )
    m = pattern.search(content)
    if not m:
        return None, "struct 未找到"
    # 取 struct 前 5 行
    start = max(0, m.start() - 500)
    before = content[start:m.start()]
    has_repr = re.search(r'#\[repr\(C\)\]', before) is not None
    return has_repr, None

def main():
    violations = []
    ok = []
    errors = []  # B01-17: fail-closed, err 分支视为违规
    for name, path, risk in CRITICAL_STRUCTS:
        found, err = find_repr_c(path, name)
        if err:
            # B01-17 修复: fail-closed 原则. err 分支不再放行, 视为违规.
            # "文件不存在" 或 "struct 未找到" 都意味着无法验证, 应阻断 CI.
            print(f"  ⚠ {name}: {err} (fail-closed: 视为违规)")
            errors.append((name, risk, path))
        elif found:
            ok.append((name, risk))
        else:
            violations.append((name, risk, path))

    print(f"=== audit_repr_c: 检查 {len(CRITICAL_STRUCTS)} 个关键 struct ===")
    print(f"  ✓ repr(C) 已加: {len(ok)}")
    print(f"  ✗ repr(C) 缺失: {len(violations)}")
    if errors:
        print(f"  ✗ 无法验证 (fail-closed): {len(errors)}")
    for name, risk, path in violations:
        print(f"    ✗ {name} ({risk}) — {path}")
    for name, risk, path in errors:
        print(f"    ⚠ {name} ({risk}) — {path} [fail-closed]")

    # B01-17: violations 或 errors 任一非空即视为违规
    if violations or errors:
        print("\n⚠ 存在未加 repr(C) 的 LTO 高风险 struct (或无法验证)")
        sys.exit(1)
    else:
        print("\n✓ audit_repr_c 通过")
        sys.exit(0)

if __name__ == "__main__":
    main()
