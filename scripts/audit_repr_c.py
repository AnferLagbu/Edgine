#!/usr/bin/env python3
"""
audit_repr_c.py — LTO 字段错位防线 (2026-07-02 新增; P-61 乙 转树级发现式)

两层执法:
  1. 钉死清单 (2 struct): 已知 LTO 错位病灶的显式底线 —— 无论字段构成如何,
     `PhysicalMemoryManager` (bitmap_size) 与 `KernelHeap` (free_list_head) 都必须
     带 `#[repr(C)]`.
  2. 树级发现: 扫描 `src/kernel` (排除 vendored `smoltcp`) 的全部 `pub struct`,
     凡体内字段类型出现 `UnsafeCell<` / `Cell<` / `NonNull<` 且字段数 >= 2 的,
     必须有 `#[repr(C)]`. 多字段才有字段重排面 —— LTO 错位的可观测形态是 A 字段
     的访问被错位到 B 字段; 单字段唯一偏移恒为 0, 没有可错位目标, `#[repr(C)]`
     对其是空操作, 故自动豁免 (不设人工豁免注册表, 避免注册表与源码双写漂移).

fail-closed: 扫描根缺失 / 扫描面为空 / 文件读失败 / 结构体体解析失败 / 钉死结构体
缺失 ⇒ 一律判违规.
用法: python3 scripts/audit_repr_c.py
退出码: 0=通过, 1=有违规
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src/kernel")
EXCLUDED_DIRS = {"smoltcp"}  # vendored 3rd-party, 不受本仓布局约束

# 钉死清单: 已知 LTO 错位 bug 的显式底线 (AGENTS §2.2 定点锁口径).
PINNED = [
    ("PhysicalMemoryManager", "src/kernel/privileged/mm/pmm.rs", "bitmap/buddy_meta/buddy_heads 字段"),
    ("KernelHeap", "src/kernel/privileged/mm/kmalloc.rs", "free_list_head UnsafeCell 字段"),
]

# 顶层 pub struct 定义 (行首锚定; 用 [ \t] 而非 \s, 防止 `\s*` 跨行吞掉 doc 注释).
STRUCT_RE = re.compile(r"^[ \t]*pub[ \t]+struct[ \t]+(\w+)", re.MULTILINE)
RISKY_TYPE_RE = re.compile(r"\b(?:UnsafeCell|Cell|NonNull)\s*<")


def strip_comments(src):
    """把注释字符替换为空格 (保留换行与长度), 消除注释里的花括号/逗号噪声."""
    out = list(src)
    i, n = 0, len(src)
    while i < n:
        if src.startswith("//", i):
            while i < n and src[i] != "\n":
                out[i] = " "
                i += 1
        elif src.startswith("/*", i):
            depth = 1
            out[i] = out[i + 1] = " "
            i += 2
            while i < n and depth > 0:
                if src.startswith("/*", i):
                    depth += 1
                    out[i] = out[i + 1] = " "
                    i += 2
                elif src.startswith("*/", i):
                    depth -= 1
                    out[i] = out[i + 1] = " "
                    i += 2
                else:
                    if src[i] != "\n":
                        out[i] = " "
                    i += 1
        else:
            i += 1
    return "".join(out)


def find_close(text, open_idx, open_ch, close_ch):
    """返回与 open_idx 处开括号配对的闭括号下标; 不配对返回 None."""
    depth = 0
    i = open_idx
    while i < len(text):
        if text[i] == open_ch:
            depth += 1
        elif text[i] == close_ch:
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return None


def classify_struct(text, start):
    """判定 start 之后的结构体形态, 返回 (kind, span).

    kind = "named" / "tuple" / "unit" / None (解析失败);
    span = 体内区间 (named 为花括号内, tuple 为圆括号内).
    where 子句里的 `Fn(u8)` 先配对圆括号、再看其后是否以 `;` 收尾,
    避免把带 where 子句的具名结构体误判成 tuple struct.
    """
    i = start
    while i < len(text):
        ch = text[i]
        if ch == "{":
            close = find_close(text, i, "{", "}")
            return ("named", (i + 1, close)) if close is not None else (None, None)
        if ch == "(":
            close = find_close(text, i, "(", ")")
            if close is None:
                return (None, None)
            j = close + 1
            while j < len(text) and text[j].isspace():
                j += 1
            if j < len(text) and text[j] == ";":
                return ("tuple", (i + 1, close))
            i = close + 1
            continue
        if ch == ";":
            return ("unit", None)
        i += 1
    return (None, None)


def strip_attributes(text):
    """去掉体内属性 (如 #[cfg(...)]), 避免其逗号参与字段切分."""
    out = []
    i = 0
    while i < len(text):
        if text.startswith("#[", i) or text.startswith("#![", i):
            bracket = text.find("[", i)
            close = find_close(text, bracket, "[", "]")
            if close is None:
                out.append(text[i:])
                break
            i = close + 1
            out.append(" ")
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


def split_top_level(text):
    """按顶层逗号切分字段; 泛型/数组/函数指针里的逗号不计."""
    parts, cur, depth = [], [], 0
    for i, ch in enumerate(text):
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        elif ch == "<":
            depth += 1
        elif ch == ">":
            if not (i > 0 and text[i - 1] == "-"):
                depth -= 1
        elif ch == "," and depth == 0:
            parts.append("".join(cur))
            cur = []
            continue
        cur.append(ch)
    parts.append("".join(cur))
    return [p.strip() for p in parts if p.strip()]


def field_type(field):
    """取字段类型: 命名字段取顶层 `:` 之后; tuple 字段整项即类型."""
    depth = 0
    i = 0
    while i < len(field):
        ch = field[i]
        if ch in "([{<":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        elif ch == ">":
            if not (i > 0 and field[i - 1] == "-"):
                depth -= 1
        elif ch == ":" and depth == 0:
            if i + 1 < len(field) and field[i + 1] == ":":
                i += 1  # 路径 `::` — 跳过第二个冒号
            else:
                return field[i + 1 :]
        i += 1
    return field


def has_repr_c(text, pos):
    """判断紧邻 pos 之前的属性串是否含 #[repr(C)] (允许 doc 注释/空行/其它属性)."""
    prefix = text[:pos]
    while True:
        prefix = prefix.rstrip()
        if not prefix.endswith("]"):
            return False
        close = len(prefix) - 1
        bracket = close
        depth = 0
        while bracket >= 0:
            if prefix[bracket] == "]":
                depth += 1
            elif prefix[bracket] == "[":
                depth -= 1
                if depth == 0:
                    break
            bracket -= 1
        if bracket < 0:
            return False
        j = bracket - 1
        while j >= 0 and prefix[j].isspace():
            j -= 1
        if j < 0 or prefix[j] != "#":
            return False
        attr = prefix[j : close + 1]
        if "".join(attr.split()).startswith("#[repr(C)]"):
            return True
        prefix = prefix[:j]


def collect_files():
    files = []
    for dirpath, dirnames, filenames in os.walk(SRC):
        dirnames[:] = sorted(d for d in dirnames if d not in EXCLUDED_DIRS)
        for name in sorted(filenames):
            if name.endswith(".rs"):
                files.append(os.path.join(dirpath, name))
    return files


def main():
    errors = []
    violations = []

    if not os.path.isdir(SRC):
        print(f"  ⚠ 扫描根缺失: {SRC} (fail-closed)")
        errors.append("扫描根缺失")

    files = collect_files() if not errors else []
    if not errors and not files:
        print("  ⚠ 扫描面为空 (fail-closed)")
        errors.append("扫描面为空")

    pinned_index = {(path, name) for name, path, _risk in PINNED}
    pinned_seen = set()
    structs_total = 0
    risky_single = []
    risky_multi_ok = 0

    for path in files:
        rel = os.path.relpath(path, ROOT)
        try:
            with open(path, "r", encoding="utf-8", errors="strict") as f:
                content = f.read()
        except (OSError, UnicodeDecodeError) as exc:
            print(f"  ⚠ 读取失败: {rel}: {exc} (fail-closed)")
            errors.append(f"读取失败 {rel}")
            continue

        text = strip_comments(content)

        for name, pinned_path, risk in PINNED:
            if rel != pinned_path:
                continue
            matches = [m for m in STRUCT_RE.finditer(text) if m.group(1) == name]
            if not matches:
                print(f"  ✗ 钉死结构体缺失: {name} ({pinned_path}, {risk})")
                errors.append(f"钉死结构体缺失 {name}")
                continue
            pinned_seen.add(name)
            if not has_repr_c(text, matches[0].start()):
                line_no = text[: matches[0].start()].count("\n") + 1
                print(f"  ✗ 钉死结构体缺 #[repr(C)]: {rel}:{line_no} {name} ({risk})")
                violations.append(f"{rel}:{line_no} {name}")

        for m in STRUCT_RE.finditer(text):
            name = m.group(1)
            structs_total += 1
            if (rel, name) in pinned_index:
                continue
            kind, span = classify_struct(text, m.end())
            line_no = text[: m.start()].count("\n") + 1
            if kind is None:
                print(f"  ⚠ 结构体体解析失败: {rel}:{line_no} {name} (fail-closed)")
                errors.append(f"解析失败 {rel}:{line_no} {name}")
                continue
            if kind == "unit":
                continue
            body = strip_attributes(text[span[0] : span[1]])
            fields = split_top_level(body)
            if not any(RISKY_TYPE_RE.search(field_type(f)) for f in fields):
                continue
            if len(fields) <= 1:
                risky_single.append(f"{name} ({rel}:{line_no})")
                continue
            if has_repr_c(text, m.start()):
                risky_multi_ok += 1
            else:
                print(f"  ✗ 多字段结构体缺 #[repr(C)]: {rel}:{line_no} {name} ({len(fields)} 字段)")
                violations.append(f"{rel}:{line_no} {name}")

    for name, pinned_path, _risk in PINNED:
        if name not in pinned_seen and not errors:
            print(f"  ✗ 钉死结构体未扫描到: {name} ({pinned_path})")
            errors.append(f"钉死结构体未扫描到 {name}")

    print("=== audit_repr_c: LTO 字段错位防线 (钉死清单 2 + 树级发现) ===")
    print(f"  扫描面: {len(files)} 个 .rs / {structs_total} 个 pub struct (排除 vendored smoltcp)")
    print(f"  钉死清单: {len(pinned_seen)}/{len(PINNED)} 定位成功")
    print(
        f"  含内部可变/裸指针字段: 多字段已标注 {risky_multi_ok} 个, "
        f"单字段自动豁免 {len(risky_single)} 个"
    )
    for item in risky_single:
        print(f"    豁免 {item} (单字段偏移恒 0)")

    if violations or errors:
        if violations:
            print(f"  ✗ {len(violations)} 处缺 #[repr(C)]:")
            for item in violations:
                print(f"    ✗ {item}")
        if errors:
            print(f"  ✗ {len(errors)} 处 fail-closed 错误 (无法验证):")
            for item in errors:
                print(f"    ✗ {item}")
        print("\n⚠ 存在未加 repr(C) 的 LTO 高风险 struct (或无法验证)")
        sys.exit(1)
    print("\n✓ audit_repr_c 通过")
    sys.exit(0)


if __name__ == "__main__":
    main()
