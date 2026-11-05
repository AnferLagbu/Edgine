#!/usr/bin/env python3
"""
B01-24 返工: audit 脚本统一自测.

按 docs/plan/audit-fix-01-audit-scripts.md §B01-24 复核要求:
- 落地 fixture + CI 断言
- 至少覆盖 functions_boundary / deadlock_matrix /
  block_registration / audit_unsafe / coupling 五脚本

设计:
- 统一入口 `python3 scripts/tests/audit_selftest.py`
- 每个用例在临时目录合成最小 fixture, 直接调用**检测器自身**的函数
  (P-50 ③): 此前用例在自测文件里复制检测算法, 检测器失效时自测仍全绿 —
  自测与检测器不同源就没有判别力.
- 注入面只有模块级路径常量 (block_registration.BASE / audit_unsafe.PROJECT_ROOT /
  audit_deadlock_matrix.LOCK_ORDER_TABLE 与 .LOCK_ORDER_DOC 与 .LIMITS_SOURCE):
  它们绑定仓库路径, 而 fixture 在临时目录.
- exit code: 0 (全部通过) / 1 (有失败)

九项真调入口:
1. audit_functions_boundary.py: check_forbidden_imports(filepath)
2. audit_deadlock_matrix.py: scan_file(filepath)
3. audit_block_registration.py: main() (注入临时扫描根)
4. tools/audit_unsafe.py: scan_file(path) (注入临时路径基准)
5. audit_coupling.py: scan_cross_module_deps(base, layer) + detect_circular_deps(matrix, layer)
6. audit_coupling.py: scan_reference_level_deps(base, layer) (P-73 丙 只报告矩阵)
7. audit_coupling.py: check_internal_access(base, layer, cross_layer) (P-74 甲 跨层穿透)
8. audit_deadlock_matrix.py: check_lock_order(真仓库根) —— 真序表三方对账必须零违规,
   且摘要的容量读数取自 limits.rs 实值
9. audit_deadlock_matrix.py: check_lock_order(临时 fixture) —— 九条失同步各判对应红
   (含跨行注册与容量到顶 / 上限单源不可解析), 并含反向负控制 (括号内的失效名与
   注释里的示例注册点都不该被判)
"""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import sys
import tempfile
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
SCRIPTS_DIR = PROJECT_ROOT / "scripts"


def _load(module_name: str, path: Path):
    """按文件路径加载检测器模块.

    scripts/ 不是 Python 包, 且 tools/ 与 scripts/ 存在同名模块风险,
    故用 importlib 按路径加载而非 sys.path + import.
    """
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"无法加载检测器: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _check(label: str, cond: bool, detail: str = "") -> bool:
    """打印检查结果并返回是否通过."""
    icon = "✅" if cond else "❌"
    msg = f"  {icon} {label}"
    if detail:
        msg += f" ({detail})"
    print(msg)
    return cond


def test_functions_boundary() -> bool:
    """B01-24 fixture 测试 1: 真调 check_forbidden_imports 识别禁止导入."""
    print("\n[Test 1/9] audit_functions_boundary.py")
    detector = _load(
        "audit_functions_boundary", SCRIPTS_DIR / "audit_functions_boundary.py"
    )
    with tempfile.TemporaryDirectory() as tmpdir:
        fixture = Path(tmpdir) / "functions" / "audit_test" / "mod.rs"
        fixture.parent.mkdir(parents=True)
        fixture.write_text(
            "//! 自测 fixture\n"
            "pub use crate::privileged::sync::raw;\n"
            "pub fn foo() {}\n",
            encoding="utf-8",
        )
        issues = detector.check_forbidden_imports(fixture)

    hits = [i for i in issues if i["type"] == "FORBIDDEN_PRIVILEGED_IMPORT"]
    return _check(
        "检测 pub use 禁止模块",
        len(hits) >= 1,
        f"fixture 含 'pub use crate::privileged::sync::raw', 实测 {len(hits)} 项",
    )


def test_deadlock_matrix() -> bool:
    """B01-24 fixture 测试 2: 真调 scan_file 识别带路径 spin 别名类型."""
    print("\n[Test 2/9] audit_deadlock_matrix.py")
    detector = _load("audit_deadlock_matrix", SCRIPTS_DIR / "audit_deadlock_matrix.py")
    with tempfile.TemporaryDirectory() as tmpdir:
        fixture = Path(tmpdir) / "privileged" / "audit_test" / "mod.rs"
        fixture.parent.mkdir(parents=True)
        fixture.write_text(
            "//! 自测 fixture\n"
            "use spin::mutex::SpinMutex;\n"
            "static AP_LOCK: SpinMutex<()> = SpinMutex::new(());\n"
            "fn sample_fn() {\n"
            "    let _l = AP_LOCK.lock();\n"
            "}\n",
            encoding="utf-8",
        )
        issues = detector.scan_file(fixture)

    hits = [i for i in issues if i["type"] == "NON_IRQ_SAFE_LOCK_USE"]
    return _check(
        "检测 spin 路径别名类型",
        len(hits) >= 1 and "AP_LOCK" in hits[0]["message"],
        f"期望 NON_IRQ_SAFE_LOCK_USE 命中 AP_LOCK, 实测 {len(hits)} 项",
    )


# ===========================================================================
# 锁类序表对账 (A-3 丙) 的正反控制
# ===========================================================================

# fixture 的基准形态: 一个 lockdep 锚 + 一个 symbol 锚, 序表 / 代码 / 文档三方一致.
_ORDER_LEVELS = [{"level": 0, "domain": "Zero"}, {"level": 1, "domain": "One"}]
_ORDER_CLASSES = [
    {"level": 0, "name": "MY_LOCK", "anchor": "lockdep"},
    {"level": 1, "name": "OTHER_SYM", "anchor": "symbol"},
]
_ORDER_CODE = (
    "//! fixture\n"
    'static TABLE: Mutex<X> = Mutex::named("MY_LOCK", X::new());\n'
    "static OTHER_SYM: IrqSpinLock<Y> = IrqSpinLock::new(Y::new());\n"
)


def _order_doc_body(entries: dict[int, str]) -> str:
    """按层级 -> 条目文本渲染文档「锁层级」围栏块."""
    lines = ["# fixture", "", "## 锁层级（从高到低）", "```"]
    for level in sorted(entries):
        lines.append(f"层级 {level}: 域")
        lines.append(f"  {entries[level]}")
        lines.append("")
    lines += ["```", "", "## 规则"]
    return "\n".join(lines)


def _order_violations(detector, tmpdir: str, tag: str, *, classes=None, levels=None,
                      doc_entries=None, code=None, limits=None,
                      limits_absent=False) -> set[str]:
    """注入临时序表 / 文档 / 代码根 / 容量上限单源, 真调 check_lock_order 取违规类型集合."""
    root = Path(tmpdir) / tag
    table_path = root / "lock_class_order.json"
    table_path.parent.mkdir(parents=True, exist_ok=True)
    table_path.write_text(
        json.dumps({"levels": levels or _ORDER_LEVELS,
                    "classes": classes if classes is not None else _ORDER_CLASSES},
                   ensure_ascii=False),
        encoding="utf-8",
    )
    doc_path = root / "ref-lock-order.md"
    doc_path.write_text(
        _order_doc_body(doc_entries if doc_entries is not None else {0: "MY_LOCK",
                                                                    1: "OTHER_SYM"}),
        encoding="utf-8",
    )
    src_root = root / "src" / "privileged"
    src_root.mkdir(parents=True, exist_ok=True)
    (src_root / "fixture.rs").write_text(code if code is not None else _ORDER_CODE,
                                        encoding="utf-8")
    # 三个路径常量绑定仓库位置, fixture 在临时目录 ⇒ 注入后调真实检测器.
    # 容量判据的上限必须可注入: 真实上限 (64) 远高于 fixture 条目数, 不改指向就构造不出
    # 「到顶即红」; 每个 case 都要显式复原, 否则上一条 case 的 fixture 会污染下一条.
    if limits is not None:
        limits_path = root / "limits.rs"
        limits_path.write_text(
            "".join(f"pub const {name}: usize = {value};\n"
                    for name, value in limits.items()),
            encoding="utf-8",
        )
    elif limits_absent:
        limits_path = root / "no-such-limits.rs"
    else:
        limits_path = (PROJECT_ROOT / "src" / "kernel" / "privileged"
                       / "constants" / "limits.rs")
    detector.LOCK_ORDER_TABLE = table_path
    detector.LOCK_ORDER_DOC = doc_path
    detector.LIMITS_SOURCE = limits_path
    issues, _summary = detector.check_lock_order([src_root])
    return {issue["type"] for issue in issues}


def test_lock_order_self_reconcile() -> bool:
    """A-3 丙 + A-127c 正向自检: 真序表与真代码 / 真文档对账零违规, 容量读数在位."""
    print("\n[Test 8/9] audit_deadlock_matrix.py 锁类序表 (真单源)")
    detector = _load("audit_deadlock_matrix", SCRIPTS_DIR / "audit_deadlock_matrix.py")
    issues, summary = detector.check_lock_order(
        [PROJECT_ROOT / "src" / "kernel" / "privileged",
         PROJECT_ROOT / "src" / "kernel" / "functions"])
    ok = _check("真序表三方对账零违规", not issues,
                f"实测违规 {[i['message'] for i in issues][:3]}")
    line = summary[0] if summary else ""
    counts_ok = _check(
        "摘要报出序表 lockdep 类与代码具名类等量 (16 / 16)",
        "lockdep 类 16" in line and "代码具名类 16" in line, line)
    # 容量读数必须由 limits.rs 实值拼出 (自测复用检测器同一个读数函数, 不抄数):
    # 上限一改, 摘要与判据同源跟随 —— 本断言因此不会挡住「提高上限」那次改动.
    limits = detector.read_capacity_limits()
    return (ok and counts_ok and _check(
        "摘要报出的容量读数取自 limits.rs 实值",
        limits is not None
        and f"类表上限 {limits['MAX_LOCK_CLASSES']}" in line
        and f"持栈上限 {limits['MAX_HELD_LOCKS']}" in line,
        f"上限读数 {limits}, 摘要行 {line}"))


def test_lock_order_negative_controls() -> bool:
    """A-3 丙 + A-127c 负控制: 九条失同步各判对应红 (含跨行注册与两条容量判据),
    另含基准形态、括号豁免与注释豁免三例不判."""
    print("\n[Test 9/9] audit_deadlock_matrix.py 锁类序表 (负控制)")
    detector = _load("audit_deadlock_matrix", SCRIPTS_DIR / "audit_deadlock_matrix.py")
    # 局部列表的变量名要避开 main() 里那个结果列表: 契约锁 ci_entrypoint_unified_test
    # 按文件中**首个**「results 紧跟方括号赋值」的位置定位 main() 的结果列表, 局部同名
    # 会让定位指到本函数, 三条自测腿契约 (P-65/P-73/P-74) 随即判红.
    case_checks: list[bool] = []
    with tempfile.TemporaryDirectory() as tmpdir:
        cases = [
            ("基准形态不判红", set(), None, None, None, None),
            ("代码新增未声明锁类即红", {"LOCK_ORDER_UNDECLARED_CLASS"},
             None, None, None,
             _ORDER_CODE + 'static EXTRA: Mutex<Z> = Mutex::named_lazy("NEW_CLASS", Z);\n'),
            # A-127a: 构造与类名字面量分行是 freg 域内锁 4 处的真实形态; 逐行扫会把
            # 它们看成「代码里没注册」, 上一条判据随即对跨行失效.
            ("跨行注册的锁类同样必须入表", {"LOCK_ORDER_UNDECLARED_CLASS"},
             None, None, None,
             _ORDER_CODE + 'static DOM: IrqSpinLock<W> = IrqSpinLock::named_lazy(\n'
             '    "CROSS_LINE_CLASS",\n    [None; 4],\n);\n'),
            ("序表 lockdep 类已无注册点即红", {"LOCK_ORDER_CLASS_STALE"},
             {0: "MY_LOCK", 1: "GONE_LOCK, OTHER_SYM"},
             _ORDER_CLASSES + [{"level": 1, "name": "GONE_LOCK", "anchor": "lockdep"}],
             None, None),
            ("序表 symbol 锚定位不到即红", {"LOCK_ORDER_SYMBOL_MISSING"},
             {0: "MY_LOCK", 1: "GONE_SYM"},
             [{"level": 0, "name": "MY_LOCK", "anchor": "lockdep"},
              {"level": 1, "name": "GONE_SYM", "anchor": "symbol"}], None, None),
            ("文档缺序表已声明的条目即红", {"LOCK_ORDER_DOC_MISSING_ENTRY"},
             {0: "（本层条目待补）", 1: "OTHER_SYM"}, None, None, None),
            ("文档出现序表未登记条目即红", {"LOCK_ORDER_DOC_UNDECLARED_ENTRY"},
             {0: "MY_LOCK, STRAY_SYM", 1: "OTHER_SYM"}, None, None, None),
            ("层级号不连续即判结构错", {"LOCK_ORDER_TABLE_MALFORMED"},
             {0: "MY_LOCK", 2: "OTHER_SYM"},
             [{"level": 0, "name": "MY_LOCK", "anchor": "lockdep"},
              {"level": 2, "name": "OTHER_SYM", "anchor": "symbol"}],
             [{"level": 0, "domain": "Zero"}, {"level": 2, "domain": "Two"}], None),
            ("括号内的失效名不判红", set(),
             {0: "MY_LOCK", 1: "OTHER_SYM（原 GONE_SYM 已删除）"}, None, None, None),
            # 整文件匹配带来的反面: 文档示例里的 `::named("X")` 不是注册点, 注释行
            # 必须整体跳过, 否则写一段用法说明就会凭空造出「未声明的锁类」.
            ("注释里的示例注册点不判红", set(),
             None, None, None,
             _ORDER_CODE + '//! 用法: static A = Mutex::named("DOC_ONLY_CLASS", ..);\n'
             '// Mutex::named("ALSO_DOC_CLASS", ..)\n'),
        ]
        for idx, (label, expected, doc_entries, classes, levels, code) in enumerate(
                cases, start=1):
            got = _order_violations(detector, tmpdir, f"case{idx}", classes=classes,
                                    levels=levels, doc_entries=doc_entries, code=code)
            # 判据取**相等**而非包含: 每条负控制只该触发自己那一类红,
            # 混进别类说明 fixture 或检测器把不相关的失同步也判了.
            case_checks.append(_check(
                label, got == expected,
                f"期望 {sorted(expected) or '零违规'}, 实测 {sorted(got) or '零违规'}"))
    # 判据 ④ 容量对账的两条负控制. 上限走 fixture: 真实上限 64 远高于 fixture 条目数,
    # 直接把 fixture 撑到 64 条要造 64 个类名, 那夹具本身的噪声会盖过判据.
    two_lockdep_classes = [
        {"level": 0, "name": "MY_LOCK", "anchor": "lockdep"},
        {"level": 1, "name": "OTHER_SYM", "anchor": "lockdep"},
    ]
    two_named_code = (
        "//! fixture\n"
        'static TABLE: Mutex<X> = Mutex::named("MY_LOCK", X::new());\n'
        'static OTHER: IrqSpinLock<Y> = IrqSpinLock::named("OTHER_SYM", Y::new());\n'
    )
    cap_checks: list[bool] = []
    with tempfile.TemporaryDirectory() as tmpdir:
        cap_checks.append(_check(
            "具名类数到达类表上限即判容量红 (序表与代码两侧同判)",
            _order_violations(detector, tmpdir, "cap_full", classes=two_lockdep_classes,
                              code=two_named_code,
                              limits={"MAX_LOCK_CLASSES": 2, "MAX_HELD_LOCKS": 8})
            == {"LOCK_CAPACITY_TABLE_FULL", "LOCK_CAPACITY_CODE_FULL"},
            "两侧计数都到 fixture 上限 2, 应各判一类容量红"))
        cap_checks.append(_check(
            "容量上限单源不可解析即判红 (fail-closed)",
            _order_violations(detector, tmpdir, "cap_unreadable", limits_absent=True)
            == {"LOCK_CAPACITY_LIMITS_UNREADABLE"},
            "limits.rs 定位不到时容量腿必须哑响成红, 不能静默放行"))
    return all(case_checks) and all(cap_checks)


def test_block_registration() -> bool:
    """B01-24 fixture 测试 3: 真调 main() 识别 egdf_register_block_dev 直调."""
    print("\n[Test 3/9] audit_block_registration.py")
    detector = _load(
        "audit_block_registration", SCRIPTS_DIR / "audit_block_registration.py"
    )
    with tempfile.TemporaryDirectory() as tmpdir:
        root = Path(tmpdir)
        fixture = root / "src" / "kernel" / "privileged" / "audit_test" / "sample.rs"
        fixture.parent.mkdir(parents=True)
        fixture.write_text(
            "//! 自测 fixture\n"
            "pub fn test() {\n"
            "    unsafe { egdf_register_block_dev(); }\n"
            "}\n",
            encoding="utf-8",
        )
        # BASE 绑定仓库路径, fixture 在临时目录 ⇒ 注入扫描根后调用真实 main()
        detector.BASE = root / "src" / "kernel"
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            rc = detector.main()

    out = buf.getvalue()
    return _check(
        "检测 egdf_register_block_dev 违规调用",
        rc == 1 and "sample.rs" in out,
        f"期望 main() 返回 1 并点名 fixture, 实测 rc={rc}",
    )


def test_audit_unsafe() -> bool:
    """B01-24 fixture 测试 4: 真调 scan_file 识别缺 SAFETY 注释的 unsafe 块."""
    print("\n[Test 4/9] tools/audit_unsafe.py")
    detector = _load("audit_unsafe", PROJECT_ROOT / "tools" / "audit_unsafe.py")
    with tempfile.TemporaryDirectory() as tmpdir:
        root = Path(tmpdir)
        fixture = root / "sample.rs"
        fixture.write_text(
            "//! 自测 fixture\n"
            "/// 文档注释 (非 SAFETY)\n"
            "pub fn test() {\n"
            "    unsafe { 0u8; }\n"
            "}\n",
            encoding="utf-8",
        )
        # scan_file 以 PROJECT_ROOT 为路径基准, fixture 在临时目录 ⇒ 注入临时根
        detector.PROJECT_ROOT = root
        hits = detector.scan_file(fixture)

    missing = [h for h in hits if not h.has_safety]
    return _check(
        "检测缺 SAFETY 注释的 unsafe 块",
        len(missing) >= 1,
        f"fixture L4 unsafe 块上方无 SAFETY, 实测 hits={[(h.line, h.has_safety) for h in hits]}",
    )


def test_coupling_edges() -> bool:
    """P-65 乙 fixture 测试 5: 真调 scan_cross_module_deps 识别跨行 use 与 pub use.

    病灶: 原边提取只认单行 `use ...;`, 于是跨行 use 与 `pub use` 全部不成边 —— 环
    检测的读数系统性偏低 (实测 privileged 371 → 411 处). 本用例同时钉住反面: 被
    `//` 与 `/* */` 注释掉的 use **不得**成边 (否则注释里的历史代码会造出幽灵环).
    """
    print("\n[Test 5/9] audit_coupling.py")
    detector = _load("audit_coupling", SCRIPTS_DIR / "audit_coupling.py")
    with tempfile.TemporaryDirectory() as tmpdir:
        base = Path(tmpdir) / "privileged"
        for mod, body in (
            ("alpha",
             "//! 自测 fixture\n"
             "/* 历史遗留: 旧实现的导入面\n"                # 块注释起于本行 ⇒ 内部整段
             "use crate::privileged::beta::{\n"            # 跨行 use 不成边
             "    ThingD,\n"
             "};\n"
             "*/\n"
             "use crate::privileged::beta::ThingF;\n"      # 块注释**之后**的单行 use → 成边
             "use crate::privileged::beta::{\n"            # 跨行 use (C 类) → 成边
             "    ThingA,\n"
             "};\n"
             "pub use crate::privileged::beta::ThingB;\n"  # 单行 pub use (B 类) → 成边
             "// use crate::privileged::beta::ThingC;\n"),  # 行注释 → 不成边
            ("beta", "//! 自测 fixture\nuse crate::privileged::alpha::ThingE;\n"),
        ):
            mod_dir = base / mod
            mod_dir.mkdir(parents=True)
            (mod_dir / "mod.rs").write_text(body, encoding="utf-8")
        # KERNEL_ROOT 绑定仓库路径, fixture 在临时目录 ⇒ 注入临时根后调真实函数
        detector.KERNEL_ROOT = Path(tmpdir)
        matrix, _details = detector.scan_cross_module_deps(base, "privileged")
        circular, _allowed = detector.detect_circular_deps(matrix, "privileged")

    got_ab = matrix["alpha"].get("beta", 0)
    got_ba = matrix["beta"].get("alpha", 0)
    return _check(
        "跨行 use 与 pub use 成边, 注释内的 use 不成边",
        got_ab == 3 and got_ba == 1 and len(circular) == 1,
        f"期望 alpha→beta=3 / beta→alpha=1 / 禁止环 1 对, 实测 {got_ab}/{got_ba}/{len(circular)}",
    )


def test_coupling_reference_edges() -> bool:
    """P-73 丙 fixture 测试 6: D 类限定路径只报告, 不并入声明级矩阵.

    病灶: 非 use 行的 `crate::privileged::<mod>` 限定路径原先完全不成边; 直接按文本
    计数会把注释、字符串字面量与 use 语句里的路径算成引用级边. 本用例同时钉住两面:
    D 类矩阵认出两条真调用与一条反向调用, 而声明级矩阵只看到那一条 use 边.
    """
    print("\n[Test 6/9] audit_coupling.py (P-73 丙 D 类引用级边)")
    detector = _load("audit_coupling", SCRIPTS_DIR / "audit_coupling.py")
    with tempfile.TemporaryDirectory() as tmpdir:
        base = Path(tmpdir) / "privileged"
        for mod, body in (
            ("alpha",
             "//! 自测 fixture\n"
             "use crate::privileged::beta::ViaUse;\n"       # 声明级边, D 类排除
             "/* crate::privileged::beta::in_block(); */\n"  # 块注释 → 不成 D 类边
             "// crate::privileged::beta::in_line();\n"      # 行注释 → 不成 D 类边
             "/// 文档中的 crate::privileged::beta::in_doc() 不成边\n"
             "const NOTE: &str = \"crate::privileged::beta::in_string()\";\n"
             "pub fn a() {\n"
             "    crate::privileged::beta::call_a();\n"
             "    crate::privileged::beta::call_b();\n"
             "}\n"),
            ("beta",
             "//! 自测 fixture\n"
             "pub fn b() {\n"
             "    crate::privileged::alpha::call_c();\n"
             "}\n"),
        ):
            mod_dir = base / mod
            mod_dir.mkdir(parents=True)
            (mod_dir / "mod.rs").write_text(body, encoding="utf-8")
        # KERNEL_ROOT 绑定仓库路径, fixture 在临时目录 ⇒ 注入临时根后调真实函数
        detector.KERNEL_ROOT = Path(tmpdir)
        use_matrix, _ = detector.scan_cross_module_deps(base, "privileged")
        use_cycles, _ = detector.detect_circular_deps(use_matrix, "privileged")
        ref_matrix, _ = detector.scan_reference_level_deps(base, "privileged")
        ref_cycles, _ = detector.detect_circular_deps(ref_matrix, "privileged")

    ref_ab = ref_matrix["alpha"].get("beta", 0)
    ref_ba = ref_matrix["beta"].get("alpha", 0)
    use_ab = use_matrix["alpha"].get("beta", 0)
    use_ba = use_matrix["beta"].get("alpha", 0)
    return _check(
        "限定路径成 D 类边, 注释/字符串/use 不成边, 且不并入声明级矩阵",
        ref_ab == 2 and ref_ba == 1 and len(ref_cycles) == 1
        and use_ab == 1 and use_ba == 0 and len(use_cycles) == 0,
        f"期望 D 类 2/1/1 且声明级 1/0/0, 实测 D 类 "
        f"{ref_ab}/{ref_ba}/{len(ref_cycles)}, 声明级 {use_ab}/{use_ba}/{len(use_cycles)}",
    )


def test_coupling_cross_layer_access() -> bool:
    """P-74 甲 fixture 测试 7: functions → privileged 内部访问跨层判定.

    病灶: `check_internal_access` 原先只扫 privileged 层, 且「目标模块 == 所在模块」
    自动跳过 —— 跨层扫描时 `functions/mm` 导入 `privileged::mm` 内部会被同名规则
    静默放行. 本用例钉住三面: (a) 同名模块不再自动跳过 (`mm/mod.rs` 的 numa 内部
    导入被拦); (b) 具名豁免路径 (`functions/mm/numa.rs`) 放行; (c) `syscall::raw`
    穿透被拦, 而公开出口 `syscall::api` 不匹配内部清单.
    """
    print("\n[Test 7/9] audit_coupling.py (P-74 甲 跨层内部访问)")
    detector = _load("audit_coupling", SCRIPTS_DIR / "audit_coupling.py")
    with tempfile.TemporaryDirectory() as tmpdir:
        base = Path(tmpdir) / "functions"
        for mod, files in (
            ("alpha", {"mod.rs": (
                "//! 自测 fixture\n"
                "use crate::privileged::syscall::raw::check_user_buf;\n"
                "use crate::privileged::syscall::api::validate_user_buf;\n"
            )}),
            ("mm", {
                "mod.rs": (
                    "//! 自测 fixture\n"
                    "use crate::privileged::mm::numa::NumaPolicy;\n"
                ),
                "numa.rs": (
                    "//! 自测 fixture\n"
                    "use crate::privileged::mm::numa::NumaPolicy;\n"
                ),
            }),
        ):
            mod_dir = base / mod
            mod_dir.mkdir(parents=True)
            for name, body in files.items():
                (mod_dir / name).write_text(body, encoding="utf-8")
        # KERNEL_ROOT 绑定仓库路径, fixture 在临时目录 ⇒ 注入临时根后调真实函数
        detector.KERNEL_ROOT = Path(tmpdir)
        default_files = sorted(
            issue["file"] for issue in detector.check_internal_access(base, "functions"))
        cross_files = sorted(
            issue["file"] for issue in
            detector.check_internal_access(base, "functions", cross_layer=True))

    return _check(
        "跨层拦同名模块内部导入与 syscall::raw, 具名豁免与 api 出口放行",
        cross_files == ["functions/alpha/mod.rs", "functions/mm/mod.rs"]
        and default_files == ["functions/alpha/mod.rs"],
        f"期望跨层 [alpha/mod.rs, mm/mod.rs] 且默认面仅 [alpha/mod.rs], "
        f"实测跨层 {cross_files}, 默认 {default_files}",
    )


def main() -> int:
    print("=" * 60)
    print("B01-24 audit 脚本自测 (按 docs/plan/audit-fix-01 §B01-24)")
    print("=" * 60)

    results = [
        test_functions_boundary(),
        test_deadlock_matrix(),
        test_block_registration(),
        test_audit_unsafe(),
        test_coupling_edges(),
        test_coupling_reference_edges(),
        test_coupling_cross_layer_access(),
        test_lock_order_self_reconcile(),
        test_lock_order_negative_controls(),
    ]
    passed = sum(results)
    total = len(results)

    print()
    print("=" * 60)
    print(f"结果: {passed}/{total} 通过")
    print("=" * 60)

    if passed == total:
        print("✅ 全部自测通过")
        return 0
    print(f"❌ {total - passed} 项失败")
    return 1


if __name__ == "__main__":
    sys.exit(main())
