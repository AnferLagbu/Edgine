#!/usr/bin/env python3
"""
audit_volatile_access.py — LTO 错位防线: 树级发现式 volatile 访问检查
(2026-11-07 由 4 字段定点回归锁扩围; 裁定 A-4 丁 / #124)

三层执法:
  1. 钉死清单 (2 字段): 已知 LTO 错位病灶的显式底线 —— `free_list_head` /
     `heap_end` (kmalloc.rs) 必须走本地口径的合法访问形态.
  2. 树级发现: 扫描 `src/kernel` (排除 vendored `smoltcp`) 全部 struct 字段,
     凡字段类型经数组/Box 包裹后为 `Cell<...>` / `UnsafeCell<...>` 的即"内部
     可变单元". 下列形态自动豁免 (结构化判据, 不设人工注册表、不设存量基线):
       - 单字段 struct: 字段唯一偏移恒 0, 无同级字段可被错位命中;
       - 泛型载荷: 内层类型提及结构体泛型参数 (同步原语的数据槽,
         volatile 语义上不适用);
       - 原子载荷: 内层类型为 `Atomic*` (原子操作自带 volatile 语义与内存序);
       - 对象级载荷: 内层为聚合类型 (整对象经引用/方法访问, volatile 不适用;
         其布局面由 audit_repr_c 的 `#[repr(C)]` 兜住).
     其余"标量载荷" (整数/bool/裸指针/标量数组/透明单字段新类型) 必须走
     地址形态访问:
       - `read_volatile` / `write_volatile` 直接包裹 `.get()` / `.as_ptr()`;
       - `addr_of!` / `addr_of_mut!` 字段地址逃逸 (配合 *Ref 包装内部 volatile);
       - `Cell::as_ptr()` 地址逃逸.
     裸 `.get()` 取值 / `.set()` 赋值一律判违规; 每个标量单元至少存在一处
     地址形态访问 (无法验证即红).

fail-closed: 扫描根缺失 / 扫描面为空 / 文件读失败 / 结构体体解析失败 /
字段类型解析失败 / 泛型参数解析失败 / 多字段 tuple struct 含标量单元
(无法按字段名审计) ⇒ 一律判错误.
用法: python3 scripts/audit_volatile_access.py
退出码: 0=通过, 1=违规或无法验证
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src/kernel")
EXCLUDED_DIRS = {"smoltcp"}  # vendored 3rd-party, 不受本仓布局约束

# 钉死清单: 已知 LTO 错位 bug 的显式底线 (AGENTS §2.2 定点锁口径).
PINNED = [
    ("KernelHeap", "src/kernel/privileged/mm/kmalloc.rs", "free_list_head"),
    ("KernelHeap", "src/kernel/privileged/mm/kmalloc.rs", "heap_end"),
]

# 结构体定义 (行首锚定; 用 [ \t] 而非 \s, 防止 `\s*` 跨行吞掉 doc 注释).
STRUCT_RE = re.compile(r"^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?struct[ \t]+(\w+)", re.MULTILINE)
ATOMIC_RE = re.compile(r"^(?:core::sync::atomic::)?Atomic\w+$")
SCALAR_PRIMS = {
    "u8", "u16", "u32", "u64", "u128", "usize",
    "i8", "i16", "i32", "i64", "i128", "isize",
    "bool", "char", "f32", "f64",
}


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


def split_top_level(text, sep=","):
    """按顶层分隔符切分; 尖括号/圆括号/方括号/花括号内的分隔符不计."""
    parts, cur, depth = [], [], 0
    i = 0
    while i < len(text):
        ch = text[i]
        if ch in "([{<":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        elif ch == ">":
            if not (i > 0 and text[i - 1] == "-"):
                depth -= 1
        elif ch == sep and depth == 0:
            parts.append("".join(cur))
            cur = []
            i += 1
            continue
        cur.append(ch)
        i += 1
    parts.append("".join(cur))
    return [p.strip() for p in parts if p.strip()]


def split_at_top(text, sep):
    """按顶层分隔符切一次, 返回 (左, 右); 无顶层分隔符返回 None."""
    depth = 0
    for i, ch in enumerate(text):
        if ch in "([{<":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        elif ch == ">":
            if not (i > 0 and text[i - 1] == "-"):
                depth -= 1
        elif ch == sep and depth == 0:
            return text[:i], text[i + 1:]
    return None


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


def parse_generic_params(raw):
    """从 `<'a, T: Bound, const N: usize>` 里只取类型参数名.

    const 参数与生命周期不参与"泛型载荷"判定 —— 它们不构成数据槽类型.
    """
    params = []
    for part in split_top_level(raw):
        p = part.strip()
        if not p or p.startswith("'") or p.startswith("const "):
            continue
        name = re.split(r"[:=]", p, maxsplit=1)[0].strip()
        if re.fullmatch(r"\w+", name):
            params.append(name)
    return params


def classify_struct(text, start):
    """判定 start 之后的结构体形态, 返回 (kind, span, generics).

    kind = "named" / "tuple" / "unit" / None (解析失败);
    span = 体内区间 (named 为花括号内, tuple 为圆括号内).
    """
    i = start
    generics = []
    while i < len(text):
        ch = text[i]
        if ch == "<":
            close = find_close(text, i, "<", ">")
            if close is None:
                return (None, None, [])
            generics = parse_generic_params(text[i + 1 : close])
            i = close + 1
            continue
        if ch == "{":
            close = find_close(text, i, "{", "}")
            return ("named", (i + 1, close), generics) if close is not None else (None, None, [])
        if ch == "(":
            close = find_close(text, i, "(", ")")
            if close is None:
                return (None, None, [])
            j = close + 1
            while j < len(text) and text[j].isspace():
                j += 1
            if j < len(text) and text[j] == ";":
                return ("tuple", (i + 1, close), generics)
            i = close + 1
            continue
        if ch == ";":
            return ("unit", None, generics)
        i += 1
    return (None, None, [])


def split_field(field):
    """切出 (字段名, 字段类型); tuple 字段名为空串."""
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
                i += 1
            else:
                name = re.sub(r"^pub(?:\([^)]*\))?\s+", "", field[:i].strip())
                return name, field[i + 1 :].strip()
        i += 1
    # tuple 字段无名字, 类型仍可能带可见性前缀 (`Idx(pub u64)`) —— 一并剥掉.
    return "", re.sub(r"^pub(?:\([^)]*\))?\s+", "", field.strip())


def unwrap_cell_type(ty):
    """把数组/Box 包裹逐层剥掉, 返回 Cell/UnsafeCell 的内层类型.

    非内部可变单元字段 (含 `*const UnsafeCell<...>` 这类指针包装) 返回 None.
    """
    t = " ".join(ty.split())
    if t.startswith("["):
        close = find_close(t, 0, "[", "]")
        if close != len(t) - 1:
            return None
        parts = split_at_top(t[1:-1], ";")
        return unwrap_cell_type(parts[0] if parts else t[1:-1])
    for prefix in ("Box<", "std::boxed::Box<", "alloc::boxed::Box<"):
        if t.startswith(prefix):
            close = find_close(t, len(prefix) - 1, "<", ">")
            if close != len(t) - 1:
                return None
            return unwrap_cell_type(t[len(prefix) : close])
    for prefix in (
        "core::cell::UnsafeCell<",
        "UnsafeCell<",
        "core::cell::Cell<",
        "Cell<",
    ):
        if t.startswith(prefix):
            close = find_close(t, len(prefix) - 1, "<", ">")
            if close != len(t) - 1:
                return None
            return t[len(prefix) : close].strip()
    return None


def mentions_generic(inner, generics):
    """内层类型是否提及结构体的类型参数 (泛型载荷)."""
    return any(re.search(rf"\b{re.escape(g)}\b", inner) for g in generics)


def is_scalar_type(ty, newtypes, seen=frozenset()):
    """判定内层类型是否"标量载荷" (值读写会被 LTO 字段错位直接影响).

    标量 = 基本类型 / bool / char / 裸指针 / NonNull / 标量数组 / 标量元组 /
    透明单字段新类型 (递归). 其余具名类型按对象级载荷处理.
    """
    t = " ".join(ty.split())
    if not t:
        return False
    if t.startswith("*mut ") or t.startswith("*const "):
        return True
    if t.startswith("&") or t.startswith("dyn "):
        return False
    if t.startswith("("):
        close = find_close(t, 0, "(", ")")
        if close != len(t) - 1:
            return False
        inner = t[1:-1].strip()
        if not inner:
            return False
        return all(is_scalar_type(p, newtypes, seen) for p in split_top_level(inner))
    if t.startswith("["):
        close = find_close(t, 0, "[", "]")
        if close != len(t) - 1:
            return False
        parts = split_at_top(t[1:-1], ";")
        return is_scalar_type(parts[0] if parts else t[1:-1], newtypes, seen)
    if t.startswith("NonNull<") or t.startswith("core::ptr::NonNull<"):
        return True
    if "<" in t:
        return False
    name = t.split("::")[-1].strip()
    if name in SCALAR_PRIMS:
        return True
    if name in newtypes and name not in seen:
        return is_scalar_type(newtypes[name], newtypes, seen | {name})
    return False


def collect_files():
    files = []
    for dirpath, dirnames, filenames in os.walk(SRC):
        dirnames[:] = sorted(d for d in dirnames if d not in EXCLUDED_DIRS)
        for name in sorted(filenames):
            if name.endswith(".rs"):
                files.append(os.path.join(dirpath, name))
    return files


def scan_source(rel, content):
    """扫描单个源文件, 返回 (struct 记录列表, 错误列表)."""
    text = strip_comments(content)
    records = []
    errors = []
    for m in STRUCT_RE.finditer(text):
        name = m.group(1)
        kind, span, generics = classify_struct(text, m.end())
        line_no = text[: m.start()].count("\n") + 1
        if kind is None:
            errors.append(f"结构体体解析失败: {rel}:{line_no} {name} (fail-closed)")
            continue
        if kind == "unit":
            continue
        body = strip_attributes(text[span[0] : span[1]])
        fields = []
        if kind == "named":
            for raw in split_top_level(body):
                fname, ftype = split_field(raw)
                if fname:
                    fields.append((fname, ftype))
        else:
            for idx, raw in enumerate(split_top_level(body)):
                _, ftype = split_field(raw)
                fields.append((f"#{idx}", ftype))
        records.append(
            {
                "file": rel,
                "line": line_no,
                "name": name,
                "kind": kind,
                "generics": generics,
                "fields": fields,
                "text": text,
            }
        )
    return records, errors


def access_patterns(field):
    """返回 (直接单元 API 正则, 地址形态正则列表).

    `.get()` / `.set()` 直接取值赋值是违规形态; 地址形态 = volatile 包裹、
    addr_of! 字段地址逃逸、Cell::as_ptr 地址逃逸.
    """
    f = re.escape(field)
    index = r"(?:\s*\[[^\[\]]*\])?"
    direct = re.compile(rf"\.{f}{index}\s*\.\s*(?:get|set)\s*\(")
    sanctions = [
        re.compile(
            rf"(?:read|write)_volatile\s*\([^;{{}}]*?\.{f}{index}\s*\.\s*(?:get|as_ptr)\s*\(\s*\)"
        ),
        re.compile(rf"addr_of(?:_mut)?!\s*\([^;{{}}]*?\.{f}\b"),
        re.compile(rf"\.{f}{index}\s*\.\s*as_ptr\s*\(\s*\)"),
    ]
    return direct, sanctions


def audit_field(rec, field, inner):
    """审计一个标量单元字段; 返回违规描述列表."""
    direct, sanctions = access_patterns(field)
    text = rec["text"]
    sanctioned_spans = [m.span() for pat in sanctions for m in pat.finditer(text)]
    problems = []
    direct_hits = 0
    for m in direct.finditer(text):
        direct_hits += 1
        if any(a <= m.start() and m.end() <= b for a, b in sanctioned_spans):
            continue
        line_no = text[: m.start()].count("\n") + 1
        problems.append(
            f"{rec['file']}:{line_no} {rec['name']}.{field} 裸 .get()/.set() "
            f"(标量载荷 {inner}; 须 read_volatile/write_volatile 或 addr_of! 地址形态)"
        )
    # 已有裸访问时不再叠加"无地址形态"条目 (同一病灶不双计); 只在完全无访问
    # 痕迹时报"无法验证".
    if direct_hits == 0 and not sanctioned_spans:
        problems.append(
            f"{rec['file']}:{rec['line']} {rec['name']}.{field} 无地址形态访问 "
            f"(标量载荷 {inner}; 无法验证 volatile 访问)"
        )
    return problems


def self_test():
    """检测器形状自测 (fail-closed: 自测不过则本腿直接红).

    夹具覆盖 4 类豁免、标量单元的正负形态、透明新类型穿透与解析失败.
    """
    cases = [
        # (标签, 源码, 期望违规条数, 期望错误条数)
        (
            "标量单元裸 .get() => 红",
            "struct A { x: UnsafeCell<u32>, y: u32 }\n"
            "impl A { fn f(&self) -> u32 { unsafe { *self.x.get() } } }\n",
            1, 0,
        ),
        (
            "标量单元 read_volatile 包裹 => 白",
            "struct A { x: UnsafeCell<u32>, y: u32 }\n"
            "impl A { fn f(&self) -> u32 { unsafe { core::ptr::read_volatile(self.x.get()) } } }\n",
            0, 0,
        ),
        (
            "标量单元 addr_of! 逃逸 => 白",
            "struct A { x: UnsafeCell<u32>, y: u32 }\n"
            "impl A { fn f(&self) -> *const u32 { core::ptr::addr_of!(self.x).cast() } }\n",
            0, 0,
        ),
        (
            "Cell 标量裸 .set() => 红",
            "struct A { x: Cell<usize>, y: u32 }\n"
            "impl A { fn f(&self) { self.x.set(1); } }\n",
            1, 0,
        ),
        (
            "单字段 struct 自动豁免 => 白",
            "struct A { x: UnsafeCell<u32> }\n"
            "impl A { fn f(&self) -> u32 { unsafe { *self.x.get() } } }\n",
            0, 0,
        ),
        (
            "泛型载荷 (数据槽) 自动豁免 => 白",
            "struct A<T> { x: UnsafeCell<T>, y: u32 }\n"
            "impl<T> A<T> { fn f(&self) -> &mut T { unsafe { &mut *self.x.get() } } }\n",
            0, 0,
        ),
        (
            "原子载荷自动豁免 => 白",
            "struct A { x: UnsafeCell<AtomicU32>, y: u32 }\n"
            "impl A { fn f(&self) { unsafe { &*self.x.get() }.fetch_add(1, Ordering::Relaxed); } }\n",
            0, 0,
        ),
        (
            "对象级载荷自动豁免 => 白",
            "struct A { x: UnsafeCell<Vec<u8>>, y: u32 }\n"
            "impl A { fn f(&self) -> &mut Vec<u8> { unsafe { &mut *self.x.get() } } }\n",
            0, 0,
        ),
        (
            "透明单字段新类型按标量穿透 => 红",
            "pub struct Idx(pub u64);\n"
            "struct A { x: UnsafeCell<Idx>, y: u32 }\n"
            "impl A { fn f(&self) -> Idx { unsafe { *self.x.get() } } }\n",
            1, 0,
        ),
        (
            "标量数组按标量处理 => 红",
            "struct A { x: UnsafeCell<[u32; 8]>, y: u32 }\n"
            "impl A { fn f(&self) { unsafe { *self.x.get() = [0; 8]; } } }\n",
            1, 0,
        ),
        (
            "泛型单元无访问 => 红 (无法验证)",
            "struct A { x: UnsafeCell<u32>, y: u32 }\n",
            1, 0,
        ),
        (
            "Cell::as_ptr 地址逃逸 => 白",
            "struct A { x: Cell<usize>, y: u32 }\n"
            "impl A { fn f(&self) -> usize { unsafe { core::ptr::read_volatile(self.x.as_ptr()) } } }\n",
            0, 0,
        ),
        (
            "结构体体未闭合 => 解析失败",
            "struct A { x: u32\n",
            0, 1,
        ),
    ]
    failures = []
    for label, src, want_v, want_e in cases:
        records, errors = scan_source("probe.rs", src)
        violations = []
        if not errors:
            newtypes = {
                r["name"]: r["fields"][0][1]
                for r in records
                if r["kind"] == "tuple" and len(r["fields"]) == 1
            }
            for rec in records:
                if rec["kind"] == "unit":
                    continue
                for fname, ftype in rec["fields"]:
                    inner = unwrap_cell_type(ftype)
                    if inner is None or len(rec["fields"]) <= 1:
                        continue
                    if mentions_generic(inner, rec["generics"]):
                        continue
                    if ATOMIC_RE.match(inner.split("::")[-1].strip()):
                        continue
                    if not is_scalar_type(inner, newtypes):
                        continue
                    if rec["kind"] == "tuple":
                        errors.append("tuple")
                        continue
                    violations.extend(audit_field(rec, fname, inner))
        if len(violations) != want_v or len(errors) != want_e:
            failures.append(
                f"{label}: 期望 违规{want_v}/错误{want_e}, 实测 违规{len(violations)}/错误{len(errors)}"
            )
    if failures:
        print(f"  ✗ 检测器自测失败 ({len(failures)} 例):")
        for item in failures:
            print(f"    ✗ {item}")
        return False
    print(f"  ✓ 检测器自测通过 ({len(cases)} 例)")
    return True


def main():
    print("=== audit_volatile_access: LTO 错位防线 (树级发现式) ===")
    if not self_test():
        sys.exit(1)

    errors = []
    violations = []

    if not os.path.isdir(SRC):
        print(f"  ⚠ 扫描根缺失: {SRC} (fail-closed)")
        errors.append("扫描根缺失")

    files = collect_files() if not errors else []
    if not errors and not files:
        print("  ⚠ 扫描面为空 (fail-closed)")
        errors.append("扫描面为空")

    records = []
    for path in files:
        rel = os.path.relpath(path, ROOT)
        try:
            with open(path, "r", encoding="utf-8", errors="strict") as f:
                content = f.read()
        except (OSError, UnicodeDecodeError) as exc:
            print(f"  ⚠ 读取失败: {rel}: {exc} (fail-closed)")
            errors.append(f"读取失败 {rel}")
            continue
        recs, errs = scan_source(rel, content)
        records.extend(recs)
        errors.extend(errs)

    # 透明单字段新类型表 (如 `VirtAddr(pub u64)`) —— 标量判据递归穿透.
    newtypes = {}
    for rec in records:
        if rec["kind"] == "tuple" and len(rec["fields"]) == 1:
            newtypes.setdefault(rec["name"], rec["fields"][0][1])

    # 非豁免标量字段名跨 struct 出现时无法按字段名归属访问点 => fail-closed.
    scalar_owners = {}
    for rec in records:
        if rec["kind"] != "named" or len(rec["fields"]) <= 1:
            continue
        for fname, ftype in rec["fields"]:
            inner = unwrap_cell_type(ftype)
            if inner is None:
                continue
            if mentions_generic(inner, rec["generics"]):
                continue
            if ATOMIC_RE.match(inner.split("::")[-1].strip()):
                continue
            if not is_scalar_type(inner, newtypes):
                continue
            scalar_owners.setdefault(fname, set()).add(rec["name"])
    for fname, owners in sorted(scalar_owners.items()):
        if len(owners) > 1:
            print(f"  ⚠ 标量单元字段名歧义: {fname} 同时出现在 {sorted(owners)} (fail-closed)")
            errors.append(f"字段名歧义 {fname}")

    structs_total = len(records)
    cells_total = 0
    exempt_single = 0
    exempt_generic = 0
    exempt_atomic = 0
    exempt_aggregate = 0
    scalar_checked = []
    pinned_seen = set()

    for rec in records:
        if rec["kind"] == "unit" or not rec["fields"]:
            continue
        for fname, ftype in rec["fields"]:
            inner = unwrap_cell_type(ftype)
            if inner is None:
                continue
            cells_total += 1
            for pinned_name, pinned_path, pinned_field in PINNED:
                if rec["name"] == pinned_name and rec["file"] == pinned_path and fname == pinned_field:
                    pinned_seen.add((pinned_name, pinned_field))
            if len(rec["fields"]) <= 1:
                exempt_single += 1
                continue
            if mentions_generic(inner, rec["generics"]):
                exempt_generic += 1
                continue
            if ATOMIC_RE.match(inner.split("::")[-1].strip()):
                exempt_atomic += 1
                continue
            if not is_scalar_type(inner, newtypes):
                exempt_aggregate += 1
                continue
            if rec["kind"] == "tuple":
                print(
                    f"  ⚠ 多字段 tuple struct 含标量单元: {rec['file']}:{rec['line']} "
                    f"{rec['name']} (无法按字段名审计, fail-closed)"
                )
                errors.append(f"tuple 标量单元 {rec['name']}")
                continue
            scalar_checked.append((rec["name"], fname, inner))
            violations.extend(audit_field(rec, fname, inner))

    for pinned_name, pinned_path, pinned_field in PINNED:
        if (pinned_name, pinned_field) not in pinned_seen and not errors:
            print(
                f"  ✗ 钉死字段未扫描到: {pinned_name}.{pinned_field} ({pinned_path})"
            )
            errors.append(f"钉死字段缺失 {pinned_name}.{pinned_field}")

    print(
        f"  扫描面: {len(files)} 个 .rs / {structs_total} 个 struct / "
        f"{cells_total} 个内部可变单元字段"
    )
    print(
        f"  自动豁免: 单字段 {exempt_single} / 泛型载荷 {exempt_generic} / "
        f"原子载荷 {exempt_atomic} / 对象级载荷 {exempt_aggregate}"
    )
    print(f"  标量载荷受检: {len(scalar_checked)} 个")
    for struct_name, field, inner in scalar_checked:
        print(f"    · {struct_name}.{field}: {inner}")

    if violations or errors:
        if violations:
            print(f"  ✗ {len(violations)} 处非地址形态访问:")
            for item in violations:
                print(f"    ✗ {item}")
        if errors:
            print(f"  ✗ {len(errors)} 处 fail-closed 错误 (无法验证):")
            for item in errors:
                print(f"    ✗ {item}")
        print("\n⚠ 存在 LTO 错位风险 (或无法验证)")
        sys.exit(1)
    print("\n✓ audit_volatile_access 通过")
    sys.exit(0)


if __name__ == "__main__":
    main()
