#!/usr/bin/env python3
"""F9 死代码注释审计 (P-53 收口).

背景: §5 F9 的「检查方式」一栏原文只写「Rust 编译器 dead_code lint」, 而 rustc 只覆盖
被编译到的路径 (feature 关闭 / cfg 掩码下的区域无人可见); 仓内原有的兜底只有 4 份
host-tests 里手抄文件清单的 grep. 本脚本补上全仓执法路径.

射程 (2026-10-23 用户裁定, 2026-10-27 修订):
- dead_code / unused / unused_* 三族: **硬零, 不进基线**. 当前唯一真实违规
  (ebpf_verifier.rs 的 `#[allow(unused_imports)]`) 已在本轮按实现使用路径消除.
- clippy 抑制族 (`allow(clippy::…)` / `expect(clippy::…)`): 同样计入 F9. 存量经
  **逻辑属性行**实测 **1661 处 / 302 文件** (单行 292 + 跨行 1369; 跨行形态是仓内
  既有的 `#[expect(clippy::…, reason = "…")]` 折行风格, 单行口径会漏计) 冻结在侧车
  `audit_dead_code_comment.baseline.json`; 用户已裁定「全部清除, 另立工程」
  (P-64), 清理推进时用 `--update-baseline` 收缩快照, 清零后本族即为硬零.
- 一般 rustc lint 族 (非 clippy、非死代码族, 如 `non_snake_case`): P-67 缺口,
  与 clippy 族同口径冻结在侧车 (只拦新增), 清理由后续工程推进.
- 构建参数层逃逸口 (`-A dead_code` / `-A unused*` / `--cap-lints allow`, 扫
  `ci/` / `scripts/` 下的 `.sh` / `.py` / `.mk` / `.toml` 与无扩展名脚本, 另加
  `Makefile*` / `.cargo/config.toml`): **硬零** —— 源码层禁了抑制而构建层一关到底时,
  前一条执法会被整体绕过. 侧车 `.json` / `.patch` 里的同形文本是说明或差异, 不算构建输入.

判定口径 = **属性位置锚定**: 只认行首 (允许缩进) 的 `#[…]` / `#![…]`, 因此注释与字符串
字面量里出现的同形文本不会被误报 (host-tests 现有 15 行此类文本, 朴素 grep 会全部误报).
属性按**逻辑行**累积: 从行首 `#[` 起按方括号配平 (跳过字符串内的括号) 直到闭合, 因此
rustfmt 把长 `#[expect(clippy::…, reason = "…")]` 折行后仍会被认出; `#[cfg_attr(…)]`
内的 allow/expect 同样按逻辑行识别.

用法:
  python3 scripts/audit_dead_code_comment.py                 # 扫描并与基线对账
  python3 scripts/audit_dead_code_comment.py --update-baseline
  python3 scripts/audit_dead_code_comment.py --self-test     # 内置夹具自检

退出码:
  0 = 死代码抑制 0 处且 clippy 抑制与基线一致
  1 = 死代码抑制存在 / clippy 抑制超出或少于基线 / 快照缺失或不自洽
  2 = 扫描根缺失或读取失败 (fail-closed)
"""

import argparse
import json
import re
import sys
import tempfile
from collections import Counter
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
BASELINE_FILE = SCRIPT_DIR / "audit_dead_code_comment.baseline.json"

# 五树扫描根 (与 P-50 丙 的四树口径同源, 另加 ABI crate src/rust).
SCAN_ROOTS = ("src/kernel", "host-tests", "src/user", "src/rust", "kani-harness")

# vendored 目录按**路径分量**排除 (不按子串: 否则 net/smoltcp_impl.rs 会被误伤).
VENDORED_DIRS = {"smoltcp"}
SKIP_DIRS = {"target"}

# 行首属性: `#[allow(…)]` / `#![allow(…)]` / expect 同形.
RE_ATTR = re.compile(r"^\s*#!?\[\s*(allow|expect)\s*\(([^)]*)\)")
# cfg_attr 可把 allow 包在条件里, 是对 F9 的等价逃逸口.
RE_CFG_ATTR = re.compile(r"^\s*#!?\[\s*cfg_attr\s*\(")
RE_NESTED_LINT = re.compile(r"(?:allow|expect)\s*\(\s*([^)]*)\)")

# 一般 rustc lint 名 (小写下划线形态; `PATCHES` 之类全大写不算).
RE_RUSTC_LINT = re.compile(r"^[a-z][a-z0-9_]*$")

# 构建参数层的死代码抑制逃逸口 (语义无歧义, 不依赖上下文判定):
# `-A dead_code` / `-A unused_*` / `--cap-lints allow` (后者把全部 lint 降为 allow).
RE_BUILD_SUPPRESS = re.compile(
    r"(?:^|[\s\"'])-A\s+(?:dead_code|unused(?:_[a-z_]+)?)\b"
    r"|--cap-lints\s+allow\b"
)
BUILD_SCAN_DIRS = ("ci", "scripts")
BUILD_SCAN_FILES = ("Makefile", "Makefile.ci", ".cargo/config.toml")
# 只扫**可承载构建参数**的文件: 侧车 .json / .patch / .pyc 里的同形文本是说明或差异,
# 不是构建输入, 扫进来会让本审计误报自己 (基线 `_meta.口径` 即含该字面量).
BUILD_SCAN_SUFFIXES = (".sh", ".py", ".mk", ".toml")


def classify_attribute(line: str):
    """判定一行属性属于哪个 F9 族.

    返回 ("dead_code" | "clippy", 规范化属性文本) 或 None.
    规范化只保留属性本身 (去掉行尾注释) 并压缩空白, 作为基线键.
    """
    m = RE_ATTR.match(line)
    if m:
        family = classify_body(m.group(2))
        if family is None:
            return None
        return family, re.sub(r"\s+", "", m.group(0))
    if RE_CFG_ATTR.match(line):
        for nested in RE_NESTED_LINT.finditer(line):
            family = classify_body(nested.group(1))
            if family:
                return family, re.sub(r"\s+", " ", line.strip())
    return None


def classify_body(body: str):
    """按顶层逗号切分 lint 列表, 返回 dead_code / clippy / rustc / None.

    只取 `reason = "…"` 之前的 lint 项 (meta 实参含普通英文单词, 不参与判族);
    字符串内的逗号不切分.
    """
    tokens = []
    buf = ""
    in_str = False
    escaped = False
    for ch in body:
        if in_str:
            buf += ch
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_str = False
            continue
        if ch == '"':
            in_str = True
            buf += ch
        elif ch == ",":
            tokens.append(buf)
            buf = ""
        else:
            buf += ch
    tokens.append(buf)
    lints = []
    for tok in tokens:
        tok = tok.strip()
        if not tok or "=" in tok:
            continue
        lints.append(tok.strip('"'))
    if any(t == "dead_code" or t.startswith("unused") for t in lints):
        return "dead_code"
    if any(t.startswith("clippy::") for t in lints):
        return "clippy"
    if lints and all(RE_RUSTC_LINT.match(t) for t in lints):
        return "rustc"
    return None


def is_vendored(rel: str) -> bool:
    """按仓内相对路径的**分量**判定 vendored."""
    return any(part in VENDORED_DIRS for part in Path(rel).parts)


def _bracket_delta(text: str, depth: int) -> int:
    """累计方括号深度, 跳过字符串字面量内的括号 (reason 文本里常见)."""
    in_str = False
    escaped = False
    for ch in text:
        if in_str:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_str = False
        elif ch == '"':
            in_str = True
        elif ch == "[":
            depth += 1
        elif ch == "]":
            depth -= 1
    return depth


def iter_attributes(lines):
    """产出 (起始行号, 逻辑属性文本): 行首属性按方括号配平跨行累积."""
    i = 0
    while i < len(lines):
        stripped = lines[i].lstrip()
        if not (stripped.startswith("#[") or stripped.startswith("#![")):
            i += 1
            continue
        start = i
        buf = lines[i]
        depth = _bracket_delta(buf, 0)
        while depth > 0 and i + 1 < len(lines):
            i += 1
            buf += "\n" + lines[i]
            depth = _bracket_delta(lines[i], depth)
        yield start + 1, buf
        i += 1


def scan():
    """扫描五树, 返回 (死代码命中列表, 冻结抑制计数, 文件数, 构建参数命中列表)."""
    dead_hits = []
    frozen = Counter()
    file_count = 0
    for root in SCAN_ROOTS:
        base = REPO_ROOT / root
        if not base.is_dir():
            raise ScanError(f"扫描根不存在: {root} (fail-closed, P-50 丙 同口径)")
        for path in sorted(base.rglob("*.rs")):
            rel = path.relative_to(REPO_ROOT).as_posix()
            if is_vendored(rel) or any(p in SKIP_DIRS for p in Path(rel).parts):
                continue
            file_count += 1
            try:
                lines = path.read_text(encoding="utf-8").splitlines()
            except (OSError, UnicodeDecodeError) as exc:
                raise ScanError(f"无法读取 {rel}: {exc} (fail-closed)") from exc
            for line_no, attr_text in iter_attributes(lines):
                hit = classify_attribute(attr_text)
                if hit is None:
                    continue
                family, norm = hit
                if family == "dead_code":
                    dead_hits.append((rel, line_no, attr_text.splitlines()[0].strip()))
                else:
                    frozen[(rel, norm)] += 1
    return dead_hits, frozen, file_count, scan_build_flags()


def is_build_input(path: Path) -> bool:
    """判定文件是否可承载构建参数 (排除侧车快照 / 补丁 / 字节码)."""
    return path.suffix in BUILD_SCAN_SUFFIXES or not path.suffix


def scan_build_flags():
    """扫描构建脚本与 Makefile 里的死代码抑制参数 (构建参数层逃逸口)."""
    hits = []
    targets = []
    for d in BUILD_SCAN_DIRS:
        base = REPO_ROOT / d
        if not base.is_dir():
            raise ScanError(f"扫描根不存在: {d} (fail-closed)")
        targets.extend(sorted(p for p in base.rglob("*") if p.is_file() and is_build_input(p)))
    targets.extend(REPO_ROOT / f for f in BUILD_SCAN_FILES if (REPO_ROOT / f).is_file())
    for path in targets:
        rel = path.relative_to(REPO_ROOT).as_posix()
        if any(p in SKIP_DIRS for p in Path(rel).parts):
            continue
        if path.resolve() == Path(__file__).resolve():
            continue  # 本脚本含正则字面量, 不自匹配
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError as exc:
            raise ScanError(f"无法读取 {rel}: {exc} (fail-closed)") from exc
        for line_no, line in enumerate(lines, 1):
            if RE_BUILD_SUPPRESS.search(line):
                hits.append((rel, line_no, line.strip()))
    return hits


class ScanError(RuntimeError):
    """扫描无法完成 (根缺失 / 读取失败)."""


def totals_of(entries) -> dict:
    """由条目自身求和的自洽校验位 (手改快照而不重跑 --update-baseline 即不自洽)."""
    return {"sites": sum(entries.values()), "files": len({f for f, _ in entries})}


def dump_baseline(entries) -> str:
    """确定性序列化 (排序 + 固定缩进), 保证 --update-baseline 幂等到字节级."""
    payload = {
        "_meta": {
            "generated_by": "python3 scripts/audit_dead_code_comment.py --update-baseline",
            "口径": "clippy 与一般 rustc lint 抑制族 (allow/expect(…)) 的存量冻结, 按逻辑属性行"
                  "累积 (含 rustfmt 折行的 reason 形态); dead_code/unused 族与构建参数 "
                  "(-A dead_code / --cap-lints allow) 不进基线, 恒为硬零",
            "disposition": "P-53 冻结存量, 全部清除归 P-64 (用户裁定: 另立工程)",
        },
        "totals": totals_of(entries),
        "entries": [[f, a, n] for (f, a), n in sorted(entries.items())],
    }
    return json.dumps(payload, ensure_ascii=False, indent=2) + "\n"


def load_baseline():
    """读快照; 读不到/格式坏/totals 不自洽一律 fail-closed."""
    if not BASELINE_FILE.is_file():
        return None, (f"基线快照缺失: {BASELINE_FILE.name} "
                      f"(须显式跑 --update-baseline 生成并提交)")
    try:
        payload = json.loads(BASELINE_FILE.read_text(encoding="utf-8"))
        entries = {(e[0], e[1]): int(e[2]) for e in payload["entries"]}
    except (OSError, ValueError, KeyError, TypeError) as exc:
        return None, f"基线快照无法解析: {exc}"
    declared = payload.get("totals", {})
    actual = totals_of(entries)
    if declared != actual:
        return None, (f"基线快照 totals 与 entries 不自洽: 声明 {declared} / 实算 {actual} "
                      f"—— 手改过快照, 请重跑 --update-baseline")
    return entries, ""


def self_test() -> int:
    """检测逻辑退化会让门禁静默全绿, 故先自证."""
    cases = [
        ("真实 #[allow(dead_code)]", "#[allow(dead_code)]", "dead_code"),
        ("真实 #[expect(unused_imports)]", "    #[expect(unused_imports)]", "dead_code"),
        ("crate 级 #![allow(unused)]", "#![allow(unused)]", "dead_code"),
        ("clippy 抑制", "#[allow(clippy::upper_case_acronyms)]", "clippy"),
        ("clippy expect", "#[expect(clippy::cast_possible_truncation)]", "clippy"),
        ("注释里的形态", "// #[allow(dead_code)]", None),
        ("文档注释里的形态", "//! 曾用 #![allow(dead_code)]", None),
        ("字符串字面量里的形态", 'let s = "#[allow(dead_code)]";', None),
        ("cfg_attr 包裹的死代码抑制",
         '#[cfg_attr(feature = "x", allow(dead_code))]', "dead_code"),
        ("一般 rustc lint 抑制", "#[allow(non_snake_case)]", "rustc"),
        ("rustc lint 带 reason 元参",
         '#[expect(non_upper_case_globals, reason = "a, unused b")]', "rustc"),
        ("clippy 与 rustc 混合仍判 clippy", "#[allow(non_snake_case, clippy::x)]", "clippy"),
    ]
    failures = 0
    for title, line, want in cases:
        hit = classify_attribute(line)
        got = hit[0] if hit else None
        status = "PASS" if got == want else "FAIL"
        if status == "FAIL":
            failures += 1
        print(f"  [{status}] {title} (期望={want}, 实际={got})")

    vendored_cases = [
        ("vendored 子目录", "src/kernel/functions/net/smoltcp/socket.rs", True),
        ("同名前缀非 vendored", "src/kernel/functions/net/smoltcp_impl.rs", False),
    ]
    for title, rel, want in vendored_cases:
        got = is_vendored(rel)
        status = "PASS" if got == want else "FAIL"
        if status == "FAIL":
            failures += 1
        print(f"  [{status}] {title} (期望={want}, 实际={got})")

    logical_cases = [
        ("跨行属性被累积并判族", ["#[allow(", "    dead_code,", ")]"], 1, "dead_code"),
        ("字符串内圆括号不破坏配平",
         ['#[expect(clippy::x, reason = "取消 (a)")]', "#[test]"], 2, "clippy"),
        ("注释不进入属性流", ["// #[allow(dead_code)]", "fn f() {}"], 0, None),
    ]
    for title, lines, want_count, want_family in logical_cases:
        attrs = list(iter_attributes(lines))
        got_count = len(attrs)
        hit = classify_attribute(attrs[0][1]) if attrs else None
        got_family = hit[0] if hit else None
        status = "PASS" if (got_count, got_family) == (want_count, want_family) else "FAIL"
        if status == "FAIL":
            failures += 1
        print(f"  [{status}] {title} (期望={want_count}/{want_family}, "
              f"实际={got_count}/{got_family})")

    build_cases = [
        ("构建参数关掉 dead_code", "cargo rustc -- -A dead_code", True),
        ("构建参数关掉 unused 族", "RUSTFLAGS='-A unused_variables'", True),
        ("构建参数压 lint 上限", "cargo build -- --cap-lints allow", True),
        ("镜像工具的同形短选项 (不误报)", "mkimage -A arm64 -O linux", False),
        ("bash 关联数组的同形短选项 (不误报)", "declare -A PATCHES=(", False),
        ("clippy 族构建豁免 (不动)", "-A clippy::cast_possible_truncation", False),
    ]
    for title, line, want in build_cases:
        got = RE_BUILD_SUPPRESS.search(line) is not None
        status = "PASS" if got == want else "FAIL"
        if status == "FAIL":
            failures += 1
        print(f"  [{status}] {title} (期望={want}, 实际={got})")

    input_cases = [
        ("shell 脚本是构建输入", "ci/build.sh", True),
        ("python 脚本是构建输入", "scripts/audit_x.py", True),
        ("无扩展名脚本是构建输入", "scripts/qemu_boot_test", True),
        ("侧车快照非构建输入", "scripts/audit_x.baseline.json", False),
        ("补丁文件非构建输入", "ci/patches/0001-a.patch", False),
    ]
    for title, rel, want in input_cases:
        got = is_build_input(Path(rel))
        status = "PASS" if got == want else "FAIL"
        if status == "FAIL":
            failures += 1
        print(f"  [{status}] {title} (期望={want}, 实际={got})")

    total = (len(cases) + len(vendored_cases) + len(logical_cases)
             + len(build_cases) + len(input_cases))
    if failures:
        print("  ✗ 自检失败: 检测逻辑退化, 门禁不可信")
        return 1
    print(f"  ✓ 自检通过: F9 分类逻辑有效 ({total}/{total})")
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="F9 死代码注释审计")
    parser.add_argument("--update-baseline", action="store_true",
                        help="用当前扫描结果重写抑制基线快照 (clippy + 一般 rustc lint)")
    parser.add_argument("--self-test", action="store_true", help="内置夹具自检")
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    print("F9 死代码注释审计 (audit_dead_code_comment)")
    print("=" * 60)

    try:
        dead_hits, frozen, file_count, build_hits = scan()
    except ScanError as exc:
        print(f"[FAIL-CLOSED] {exc}", file=sys.stderr)
        return 2

    print(f"扫描: {file_count} 个 .rs (五树, 排除 vendored smoltcp)")
    print(f"死代码抑制族: {len(dead_hits)} 处 (硬零, 不进基线)")
    print(f"构建参数抑制: {len(build_hits)} 处 (硬零)")
    clippy_sites = sum(v for (_f, a), v in frozen.items() if "clippy::" in a)
    print(f"冻结抑制族: {sum(frozen.values())} 处 / {len(frozen)} 个 (文件, 属性) 项 "
          f"(clippy {clippy_sites} + 一般 rustc {sum(frozen.values()) - clippy_sites})")

    if dead_hits:
        print("\n[FAIL] 发现死代码抑制 (F9 零容忍, 无豁免):")
        for rel, line_no, code in dead_hits:
            print(f"  {rel}:{line_no}: {code[:96]}")
        print("消除方式 = 实现使用路径 / 移入测试模块, 不是再加一层 allow.")
        return 1

    if build_hits:
        print("\n[FAIL] 发现构建参数层的死代码抑制 (F9 零容忍, 无豁免):")
        for rel, line_no, code in build_hits:
            print(f"  {rel}:{line_no}: {code[:96]}")
        print("构建参数不得关掉 dead_code / unused* 或把 lint 上限压到 allow.")
        return 1

    if args.update_baseline:
        BASELINE_FILE.write_text(dump_baseline(dict(frozen)), encoding="utf-8")
        totals = totals_of(dict(frozen))
        print(f"\n基线快照已重写: {BASELINE_FILE.name} "
              f"({totals['sites']} 处 / {totals['files']} 文件)")
        print("该文件需随抑制项的增减一并提交; clippy 族清零归 P-64.")
        return 0

    baseline, err_msg = load_baseline()
    if baseline is None:
        print(f"\n[FAIL-CLOSED] {err_msg}", file=sys.stderr)
        return 1
    print(f"基线快照: {sum(baseline.values())} 处 / {len(baseline)} 项 "
          f"({BASELINE_FILE.name})")

    over = sorted((key, frozen[key], baseline.get(key, 0))
                  for key in frozen if frozen[key] > baseline.get(key, 0))
    under = sorted((key, frozen.get(key, 0), baseline[key])
                   for key in baseline if baseline[key] > frozen.get(key, 0))

    if over:
        print(f"\n[FAIL] 超出基线 {len(over)} 项 (新增抑制: clippy 或一般 rustc lint):")
        for (rel, attr), got, want in over:
            print(f"  {rel}: {attr} 实际 {got} 处 / 基线 {want} 处")
    if under:
        print(f"\n[FAIL] 基线过期 {len(under)} 项 (抑制已减少, 快照未同步):")
        for (rel, attr), got, want in under:
            print(f"  {rel}: {attr} 实际 {got} 处 / 基线 {want} 处")

    if over or under:
        print("-" * 60)
        print(f">>> F9 抑制族与基线不符: 实际 {sum(frozen.values())} 处 / "
              f"基线 {sum(baseline.values())} 处 <<<")
        print("确属新增 ⇒ 消除抑制而不是扩大基线; 确属减少 ⇒ 跑 --update-baseline.")
        return 1

    print("\nPASS: 死代码抑制 0 处, 构建参数 0 处, 冻结抑制与基线逐项一致")
    return 0


if __name__ == "__main__":
    sys.exit(main())
