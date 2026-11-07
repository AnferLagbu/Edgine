#!/usr/bin/env python3
r"""
恒真反向针扫描器 (Vacuous Negative Needle Scanner)

## 病灶

`str::contains` / `str::matches` / `str::find` 三个 API 收的是**逐字文本**, 不是正则.
把正则当针写进去时, 后果按断言方向分两种, 而且只有一种会暴露:

- **正向** `assert!(haystack.contains(r#"\[.+?"#))`: 匹配不到 ⇒ 当场红 ⇒ 写的人立刻知道
  针写错了. 藏不住, 不需要门禁.
- **反向** `assert!(!haystack.contains(r#"$r_i.*$R_MAX"#))`: 匹配不到正是这条断言想要的
  "通过" ⇒ **恒真**, 永远咬不住任何东西. 它在 CI 日志里与一条真会咬的判据完全同形,
  只能靠人逐行读源码才发现.

2026-11-07 M7 合格线批的 `host-tests/tests/lockdep_overhead_ab_line_test.rs` 就是这么
一条: 该文件里 `-v lim="$r_i"` 与 `$r_i.*$R_MAX` 两根反向针都是恒真的, 暴露它们的不是
任何防线, 而是 clippy 的 `needless_raw_string_hashes` 恰好路过 (该 lint 只在针内没有
裸 `"` 时才触发, 与"这条判据咬不咬得住"毫无因果关系). 本腿把这一形态从"靠 lint 巧合"
变成"靠门禁判据".

## 口径 (2026-11-07 裁定「树级形态腿」)

1. **射程 = 树级发现式**, 不写文件清单: `git ls-files --cached --others
   --exclude-standard` 列出的全部 `*.rs` (含本批尚未提交的在制品), 排除 vendored
   `src/kernel/functions/net/smoltcp/`. 判据对象是"反向针"这一**形态**, 与它属于哪个
   测试文件无关.
2. **反向针 = 接收者表达式被 `!` 否定的** `.contains(` / `.matches(` / `.find(` 调用,
   且首参是**字符串字面量** (`r"…"` / `r#"…"#` / 转义串, 解码后取运行时真实字面值).
   跨行形态 (`assert!(\n    !py.contains(…))`) 与闭包内形态 (`.filter(|l|
   !l.contains(…)`) 同样在射程内; 首参是常量 / 变量 / `format!` 时静态不可判, 不计.
3. **纯正则记号集** (裁定原文逐项, 不擅自增删): `.*` `.+` `\d` `\s` `\w` `\b`
   `[0-9]` `{n,m}` `(?:`. 针内出现任一记号即进入判据.
4. **自动判白 (不设人工豁免表)**: 针内含引号字符 (`'` 或 `"`) ⇒ 这段文本本身是**源码
   文本**, 其中的正则记号是被引用内容的一部分而不是匹配运算符 —— 断言"源码里没有这行
   代码"是逐字成立的判据. 判据口径是"这段文本不可能是任何源文件的逐字内容"才红.
   实测存量恰 1 条 (P-65 那条搜 `audit_coupling.py` 旧正则行的针) 由该规则自动判白,
   无需豁免清单, 也无需随批次维护.
5. **不用"针文本是否逐字出现在仓内别处"作判白依据** (曾考虑, 实测否决): 台账会原文
   引用被修掉的恒真针 —— `docs/plan/multithreading-project.md` 就逐字写着
   `$r_i.*$R_MAX`. 以仓内存在性作豁免等于"写进台账的缺陷形态永久免检", 恰好放过本腿要
   拦的复现.
6. **二值判定**: 无违规退 0; 有违规退 1 并逐条列出 (文件:行:列 + 方法 + 记号 + 针预览);
   扫描或读取失败退 2 (fail-closed, 读失败不静默 continue).
7. **检测器自检的执行点在 `scripts/tests/audit_selftest.py`** (P-50 ③ 同型要求: 真调
   `scan_text`, 不在自测文件里复制算法). 本脚本不内置 fixture 用例, 免两处漂移.

## 已知边界 (写明, 不假装是全类防线)

- 只封"形态像正则"这一子类: 恒真针还有别的写法 (针文本本身写错、目标文件路径解析错),
  本腿判不到.
- Python 侧同形态 (`assert "<needle>" not in text`) 实测有 10 个文件含 `not in` 字面量
  判据、其中 0 条带正则记号 ⇒ 本轮不纳入射程; 要纳入得另写 Python 词法 (三引号 /
  f-string / 前缀), 不该顺手扩.
- 带引号的判白规则可能被绕过: 一条恒真针若恰好含引号字符即判白. 这是"零人工豁免表"的
  对价, 不是遗漏 —— 负控制里专门钉了这条边界.

## 用法

    python3 scripts/audit_vacuous_needle.py            # 全仓扫描 (CI 用法)
    python3 scripts/audit_vacuous_needle.py --json     # JSON 报告
    python3 scripts/audit_vacuous_needle.py --show-all # 连正向针一起列 (取证用)
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[1]

# 判定面: 三个逐字文本 API. starts_with / ends_with 同族但形态上不产正则记号,
# find_iter 属 regex crate (真收正则), 都不在射程.
CALL_RE = re.compile(r"\.(?P<method>contains|matches|find)\s*\(")

# 裁定口径第 3 条的记号集, 逐项对应.
REGEX_TOKENS: tuple[tuple[str, re.Pattern[str]], ...] = (
    (".*", re.compile(r"\.\*")),
    (".+", re.compile(r"\.\+")),
    (r"\d", re.compile(r"\\d")),
    (r"\s", re.compile(r"\\s")),
    (r"\w", re.compile(r"\\w")),
    (r"\b", re.compile(r"\\b")),
    ("[0-9]", re.compile(r"\[0-9\]")),
    ("{n,m}", re.compile(r"\{\d+,\d*\}")),
    ("(?:", re.compile(r"\(\?:")),
)

# 裁定口径第 4 条的判白证据: 针内出现引号字符 => 针本身是源码文本.
SOURCE_TEXT_QUOTES = ("'", '"')

EXEMPT_SOURCE_TEXT = "针内含引号字符: 被搜索的是逐字源码文本"
EXEMPT_POSITIVE = "正向针: 匹配不到即当场红, 自带证明"

# Rust 字符串/字符字面量与注释的词法. 转义里的续行反斜杠必须按 [\s\S] 配对:
# `"…\` + 换行 + `…"` 是合法的 Rust 行继续, 用 `\\.` (dot 不含换行) 会配不上, 于是引号
# 从这一处起整体错位, 后半棵树都被读成字符串 —— 开发期实测踩过这个坑.
RAW_RE = re.compile(r'r(?P<hashes>\#*)"(?P<body>.*?)"(?P=hashes)', re.S)
ESC_RE = re.compile(r'"(?P<body>(?:\\[\s\S]|[^"\\])*)"')
CHAR_RE = re.compile(r"'(?:\\[\s\S]|[^'\\\n])'")

VENDOR_PREFIX = "src/kernel/functions/net/smoltcp/"
BINARY_PROBE_BYTES = 8192
CONTEXT_CHARS = 140

ESCAPE_MAP = {
    "n": "\n",
    "t": "\t",
    "r": "\r",
    "0": "\0",
    "b": "\b",
    "f": "\f",
    "\\": "\\",
    '"': '"',
    "'": "'",
}


class ScanError(RuntimeError):
    """扫描基础设施失败 (git ls-files / 文件读取), 必须 fail-closed."""


def decode_escapes(body: str) -> str:
    r"""把 Rust 转义串的字面量体解成运行时真实文本.

    必须解到真实文本: 记号集判的是"运行时拿去逐字匹配的字节", 不是源码写法.
    `"\bb"` 解出退格符 => 运行时文本里没有 `\b` 记号; 源码写 `"\\s"` 解出来才是 `\s` => 含记号.
    """
    out: list[str] = []
    i = 0
    while i < len(body):
        c = body[i]
        if c != "\\" or i + 1 >= len(body):
            out.append(c)
            i += 1
            continue
        nxt = body[i + 1]
        if nxt == "u" and body[i + 2 : i + 3] == "{":
            close = body.find("}", i + 3)
            if close != -1:
                try:
                    out.append(chr(int(body[i + 3 : close], 16)))
                except ValueError:
                    out.append(body[i : close + 1])
                i = close + 1
                continue
        out.append(ESCAPE_MAP.get(nxt, nxt))
        i += 2
    return "".join(out)


def tokenize(text: str) -> tuple[list[bool], list[tuple[int, int, str]]]:
    """单次扫描: 返回 (code_mask, literals).

    code_mask[i] 为 True 表示该位置是可执行文本 (不在字符串或注释内);
    literals 为按起始位置升序的 (start, end, decoded_value), 只收字符串字面量.
    """
    mask = [True] * len(text)
    literals: list[tuple[int, int, str]] = []
    i = 0
    n = len(text)

    def blank(a: int, b: int) -> None:
        for k in range(a, b):
            mask[k] = False

    while i < n:
        c = text[i]
        nxt = text[i + 1] if i + 1 < n else ""
        ident_before = i > 0 and (text[i - 1].isalnum() or text[i - 1] == "_")
        if c == "/" and nxt == "/":
            while i < n and text[i] != "\n":
                i += 1
            continue
        if c == "/" and nxt == "*":
            depth = 0
            while i < n:
                if text.startswith("/*", i):
                    depth += 1
                    i += 2
                elif text.startswith("*/", i):
                    depth -= 1
                    i += 2
                    if depth == 0:
                        break
                else:
                    i += 1
            continue
        if c == "b" and nxt == '"' and not ident_before:
            m = ESC_RE.match(text, i + 1)
            if m:
                blank(i, m.end())
                literals.append((i + 1, m.end(), decode_escapes(m.group("body"))))
                i = m.end()
                continue
            i += 1
            continue
        if c == "r" and not ident_before:
            m = RAW_RE.match(text, i)
            if m:
                blank(i, m.end())
                literals.append((i, m.end(), m.group("body")))
                i = m.end()
                continue
            i += 1
            continue
        if c == "'":
            m = CHAR_RE.match(text, i)
            if m:
                blank(i, m.end())
                i = m.end()
                continue
            i += 1
            continue
        if c == '"':
            m = ESC_RE.match(text, i)
            if m:
                blank(i, m.end())
                literals.append((i, m.end(), decode_escapes(m.group("body"))))
                i = m.end()
                continue
            blank(i, i + 1)
            i += 1
            continue
        i += 1
    return mask, literals


def receiver_start(text: str, mask: list[bool], dot: int) -> int:
    """从方法名的 `.` 往回走接收者表达式, 返回其起始下标.

    跨空白 (含换行)、标识符、`::`、`.`、`&`、`*` 与成对的 `()` / `[]`; 遇不认识的记号
    即停. 停处之前的那个记号决定这条判据是正向还是反向.
    """
    i = dot - 1
    depth = 0
    while i >= 0:
        if not mask[i]:
            break
        c = text[i]
        if c.isspace():
            i -= 1
            continue
        if c in ")]":
            depth += 1
            i -= 1
            continue
        if c in "([":
            if depth:
                depth -= 1
                i -= 1
                continue
            break
        if depth:
            i -= 1
            continue
        if c.isalnum() or c in "_:.&*":
            i -= 1
            continue
        break
    return i + 1


def preceded_by_bang(text: str, mask: list[bool], pos: int) -> bool:
    """接收者起始处的前一个可执行记号是不是 `!` (反向判据).

    `!=` 必须排除: `assert!(a != b && c.contains(r".*"))` 里回溯会停在 `=`, 其前一个记号
    正是 `!`, 若不去重则把一条正向合取判据读成反向 —— 反过来 (把真反向读成正向) 会让
    存量恒真针漏检, 两个方向都要判对.
    """
    i = pos - 1
    while i >= 0 and (text[i].isspace() or not mask[i]):
        i -= 1
    if i < 0 or text[i] != "!":
        return False
    j = i + 1
    while j < len(text) and (text[j].isspace() or not mask[j]):
        j += 1
    return j >= len(text) or text[j] != "="


def first_arg_literal(text: str, literals: list[tuple[int, int, str]], pos: int) -> str | None:
    """取紧跟调用括号的首个**字符串字面量参数**, 否则 None (静态不可判即不计).

    两道校验缺一个就会把别处的文本读成针:
    - 括号与字面量之间只允许空白与 `&` —— 否则 `contains(&format!("…"))` 的格式串会被
      当成字面针;
    - 字面量之后 (跳空白) 必须是 `,` 或 `)` —— 否则 `contains(NEEDLE)` 里被匹配到的是
      后面的断言文案而不是针.
    """
    for start, end, value in literals:
        if start < pos:
            continue
        if text[pos:start].strip("& \t\n\r"):
            continue
        rest = text[end:].lstrip()
        if not rest or rest[0] not in (",", ")"):
            return None
        return value
    return None


def regex_tokens(needle: str) -> list[str]:
    return [name for name, pat in REGEX_TOKENS if pat.search(needle)]


def exempt_reason_of(negated: bool, needle: str) -> str | None:
    """None = 违规. 正向针与"针内是逐字源码文本"的反向针各有判白理由."""
    if not negated:
        return EXEMPT_POSITIVE
    if any(q in needle for q in SOURCE_TEXT_QUOTES):
        return EXEMPT_SOURCE_TEXT
    return None


def scan_text(rel: str, text: str) -> list[dict]:
    """纯函数检测器: 输入 (相对路径, 文本), 输出全部带正则记号的针.

    每条形目带 negated / exempt 两面, 违规判定交给 violations_of —— 自测与门禁共用这
    一个入口, 不存在"自测另算一套"的漂移面.
    """
    mask, literals = tokenize(text)
    lines = text.splitlines()
    rows = []
    for m in CALL_RE.finditer(text):
        if not mask[m.start()]:
            continue
        needle = first_arg_literal(text, literals, m.end())
        if needle is None:
            continue
        tokens = regex_tokens(needle)
        if not tokens:
            continue
        line = text.count("\n", 0, m.start())
        negated = preceded_by_bang(text, mask, receiver_start(text, mask, m.start()))
        rows.append(
            {
                "file": rel,
                "line": line + 1,
                "col": m.start() - (text.rfind("\n", 0, m.start()) + 1) + 1,
                "method": m.group("method"),
                "needle": needle,
                "tokens": tokens,
                "negated": negated,
                "exempt": exempt_reason_of(negated, needle),
                "context": lines[line].strip()[:CONTEXT_CHARS] if line < len(lines) else "",
            }
        )
    return rows


def violations_of(rows: list[dict]) -> list[dict]:
    """违规 = 反向 + 带正则记号 + 无判白理由."""
    return [r for r in rows if r["negated"] and r["exempt"] is None]


def tracked_files() -> list[str]:
    """受控 + 在制 (未提交但不被 ignore) 的 .rs 文件, 供树级扫描."""
    try:
        proc = subprocess.run(
            [
                "git",
                "-C",
                str(PROJECT_ROOT),
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
                "--",
                "*.rs",
            ],
            capture_output=True,
            check=False,
        )
    except OSError as exc:
        raise ScanError(f"git 不可用: {exc}") from exc
    if proc.returncode != 0:
        raise ScanError(
            f"git ls-files 失败 (exit={proc.returncode}): "
            f"{proc.stderr.decode('utf-8', 'replace').strip()}"
        )
    return [p for p in proc.stdout.decode("utf-8", "surrogateescape").split("\0") if p]


def read_text(rel: str) -> str | None:
    """读文本; 二进制返回 None; 读取失败 fail-closed 抛 ScanError."""
    try:
        raw = (PROJECT_ROOT / rel).read_bytes()
    except OSError as exc:
        raise ScanError(f"{rel}: {exc}") from exc
    if b"\0" in raw[:BINARY_PROBE_BYTES]:
        return None
    return raw.decode("utf-8", errors="replace")


def collect() -> tuple[list[dict], list[dict], int]:
    rows: list[dict] = []
    scanned = 0
    for rel in tracked_files():
        if rel.startswith(VENDOR_PREFIX):
            continue
        text = read_text(rel)
        if text is None:
            continue
        scanned += 1
        rows.extend(scan_text(rel, text))
    return violations_of(rows), rows, scanned


def main() -> int:
    parser = argparse.ArgumentParser(
        description="检出反向 contains/matches/find 里的恒真正则针 (逐字 API + 正则写法 = 永不咬合)",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--json", action="store_true", help="输出 JSON 报告")
    parser.add_argument(
        "--show-all", action="store_true", help="连正向针与判白条目一起列出 (取证用)"
    )
    args = parser.parse_args()

    try:
        bad, rows, scanned = collect()
    except ScanError as exc:
        print(f"扫描失败 (fail-closed): {exc}", file=sys.stderr)
        return 2

    negs = [r for r in rows if r["negated"]]
    whites = [r for r in negs if r["exempt"] == EXEMPT_SOURCE_TEXT]
    positives = [r for r in rows if not r["negated"]]

    if args.json:
        print(
            json.dumps(
                {
                    "scan_summary": {
                        "files_scanned": scanned,
                        "token_needles": len(rows),
                        "negated": len(negs),
                        "negated_exempt_source_text": len(whites),
                        "positive": len(positives),
                        "violations": len(bad),
                    },
                    "violations": bad,
                    "negated_exempt": whites if args.show_all else [],
                    "positive": positives if args.show_all else [],
                },
                ensure_ascii=False,
                indent=2,
            )
        )
        return 1 if bad else 0

    print("=" * 70)
    print("恒真反向针扫描报告 (Vacuous Negative Needle)")
    print("=" * 70)
    print(f"受检 .rs 文件: {scanned}")
    print(f"带正则记号的针: {len(rows)}  (反向 {len(negs)} / 正向 {len(positives)})")
    print(f"反向判白 (针内是逐字源码文本): {len(whites)}")
    for row in whites:
        print(f"  白 {row['file']}:{row['line']} 记号={row['tokens']} {row['needle'][:60]!r}")
    print(f"违规 (恒真反向针): {len(bad)}")
    for row in bad:
        print(f"  {row['file']}:{row['line']}:{row['col']} .{row['method']}() 记号={row['tokens']}")
        print(f"    针={row['needle'][:CONTEXT_CHARS]!r}")
        print(f"    {row['context']}")
    if args.show_all:
        print(f"\n正向针 (匹配不到即当场红, 不判): {len(positives)}")
        for row in positives:
            print(f"  {row['file']}:{row['line']} .{row['method']}() {row['tokens']} {row['needle'][:60]!r}")
    if bad:
        print(
            "\n判据: contains/matches/find 收逐字文本, 正则写法的反向针永不匹配 => 恒真. "
            "改法是把判据换成会咬的字面针, 不是给针加引号绕过豁免."
        )
    else:
        print("\n(本腿只判「反向 + 正则形 + 非源码文本」这一条形态; 正向针与判白条目不计)")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
