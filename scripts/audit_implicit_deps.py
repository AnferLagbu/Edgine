#!/usr/bin/env python3
"""functions 层隐式依赖审计 (P-49 收口).

检查 functions 层模块对 privileged 全局静态状态的引用,
检测通过全局静态变量产生的隐式依赖 (F2 / DECISION-J 存量债).

口径依据与 audit_reverse_deps.py 同源 (docs/plan/framekernel-paradigm-enforcement.md §7.3):
- 整行注释 (`//` / `///` / `//!` / `/* */`) 内的名字是文档交叉引用, 不构成代码依赖;
- 内联 `#[cfg(test)] mod X { ... }` 块 (花括号深度跟踪) 属测试载体, 不纳入整治;
- 判定不了的按生产计 (fail-closed). functions 层当前无 `tests/` 测试载体目录, 也没有
  `#[cfg(test)] mod X;` 外部测试文件声明, 故只复用上述两条; 若将来出现, 其引用会按生产
  计并使基线转红 —— 过度报告是 fail-closed 方向.

P-49 甲・修正则: 原 `static_pattern` 允许 `[=;{]` 收尾, 于是
`pub fn bpf_subsystem() -> &'static BpfSubsystem {` 里**类型位置**上的 `'static` 被当成
全局定义收割, 17 处伪命中 (BpfSubsystem / KexecSubsystem / UefiSubsystem / CetSubsystem /
SecureBootSubsystem / TpmSubsystem / TicklessSubsystem / TimeSyncSubsystem / NetOps ...)
全部由此而来. 现改为行首锚定的 static 项声明模式: Rust 的 static 项必须写类型, 故匹配到
`NAME:` 即可判定, 不再要求 `=` 与声明同行 —— 跨行声明 `static GENERAL_CACHES: IrqSpinLock<`
也因此重新纳入收割. 同时删除 `PRIVILEGED_GLOBALS` 硬编码兜底名单 (14 项中 NET_DEVICE /
NET_STACK / SLAB_CACHES / VGA_DRIVER 四项在仓库中已无定义, 属会随改名腐烂的定点名单;
动态收割已是单源, 自测锁见 scripts/tests/audit_implicit_deps_selftest.py).

P-49 甲・基线冻结 (机器派生, 不手写数字): 本轮只接线不修代码, 故以现存生产站点为基线
逐项冻结到 (文件, 全局名) 粒度. 基线**不是**写在脚本里的字面量而是侧车快照
`scripts/audit_implicit_deps.baseline.json`, 由 `--update-baseline` 从同一份扫描代码
导出 —— 人只负责批准快照, 不负责重算它. 这么定的原因是一个具体的失败案例: 本轮据
「40 处」立案, 实装时 detector 变准, 真实存量是 48 处; 若数字是手打的, 这类偏差会长期
存在且无法自证.
对账方向是**双向**的: 超出基线 = 新增隐式依赖 ⇒ 阻断; 少于基线 = 债项已减少而快照未同步
⇒ 同样阻断 (单向 ratchet 会让快照里的死条目继续掩盖同一位置的新增).
快照的 `totals` 由其自身 `entries` 求和得出, 手改条目而不重跑 `--update-baseline` 会
因 totals 不自洽直接判红.

用法: python3 scripts/audit_implicit_deps.py [--update-baseline]
退出码: 0 = 与基线完全一致; 1 = 新增依赖 / 基线过期 / 快照缺失或不自洽 / 扫描根缺失
"""

import argparse
import json
import re
import sys
from collections import Counter
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
FUNCTIONS_BASE = REPO_ROOT / 'src' / 'kernel' / 'functions'
PRIVILEGED_BASE = REPO_ROOT / 'src' / 'kernel' / 'privileged'
BASELINE_FILE = SCRIPT_DIR / 'audit_implicit_deps.baseline.json'

sys.path.insert(0, str(SCRIPT_DIR))
import audit_reverse_deps as reverse_deps  # noqa: E402  复用 §7.3 口径分类器

# privileged 全局状态变量 (functions 不应直接访问) —— 由 harvest_privileged_globals()
# 动态收割, 不再维护硬编码名单.
#
# 行首锚定: 可选可见性 -> `static` / `static mut` -> 名字 -> `:`.
# 类型位置上的 `'static T` 因前面还有 `fn` / `let` / `->` 等记号而不可能命中本模式.
RE_STATIC_ITEM = re.compile(
    r'^\s*(?:pub(?:\([^)]*\))?\s+)?static\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*:'
)

# 关键字过滤 (收割名字必须是标识符, 不能落在这些词上)
RUST_KEYWORDS = {
    'Self', 'self', 'static', 'const', 'let', 'mut', 'ref', 'pub',
    'use', 'fn', 'struct', 'enum', 'trait', 'impl', 'mod', 'crate',
    'super', 'as', 'in', 'if', 'else', 'for', 'while', 'loop', 'match',
    'return', 'break', 'continue', 'true', 'false', 'usize', 'u8', 'u16',
    'u32', 'u64', 'u128', 'isize', 'i8', 'i16', 'i32', 'i64', 'i128',
    'f32', 'f64', 'bool', 'char', 'str', 'String', 'Vec', 'Option',
    'Result', 'Box', 'Rc', 'Arc', 'Cell', 'RefCell',
}

# 允许的直接引用 (privileged 层安全 API / functions 自有同名全局)
ALLOWED_PATTERNS = [
    r'crate::functions::fs::VFS_MANAGER',
    r'crate::privileged::ipc::IPC_NAMESPACE',
]

# 存量债基线 = 侧车快照 BASELINE_FILE (机器派生, 见模块 docstring).
# 键 = (仓库相对路径, privileged 全局名), 值 = 该组合的引用行数.


def totals_of(entries: dict[tuple[str, str], int]) -> dict[str, int]:
    """由条目自身求和得出的自洽校验位 (手改条目而不重跑 --update-baseline 即不自洽)."""
    return {
        'sites': sum(entries.values()),
        'items': len(entries),
        'files': len({f for f, _ in entries}),
        'globals': len({g for _, g in entries}),
    }


def dump_baseline(entries: dict[tuple[str, str], int]) -> str:
    """确定性序列化 (排序 + 固定缩进), 保证 --update-baseline 幂等到字节级."""
    payload = {
        '_meta': {
            'generated_by': 'python3 scripts/audit_implicit_deps.py --update-baseline',
            '口径': 'functions 层生产上下文对 privileged static 项的引用 (§7.3: '
                  '注释与内联 cfg(test) 块不计)',
            'disposition': 'P-49 冻结存量债, 消解归 framekernel-paradigm-enforcement §7 批次 Z',
        },
        'totals': totals_of(entries),
        'entries': [[f, g, n] for (f, g), n in sorted(entries.items())],
    }
    return json.dumps(payload, ensure_ascii=False, indent=2) + '\n'


def parse_baseline(raw: str) -> dict[tuple[str, str], int]:
    payload = json.loads(raw)
    return {(e[0], e[1]): int(e[2]) for e in payload['entries']}


def load_baseline() -> tuple[dict[tuple[str, str], int] | None, str]:
    """读快照; 返回 (条目, 错误说明). 读不到/不自洽都 fail-closed."""
    if not BASELINE_FILE.is_file():
        return None, (f'基线快照缺失: {repo_rel(BASELINE_FILE)} '
                      f'(须显式跑 --update-baseline 生成并提交, 不接受自动放行)')
    try:
        payload = json.loads(BASELINE_FILE.read_text(encoding='utf-8'))
    except (OSError, json.JSONDecodeError) as exc:
        return None, f'基线快照无法解析: {exc}'
    entries = {(e[0], e[1]): int(e[2]) for e in payload['entries']}
    declared = payload.get('totals', {})
    actual = totals_of(entries)
    if declared != actual:
        return None, (f'基线快照 totals 与 entries 不自洽: 声明 {declared} / 实算 {actual} '
                      f'—— 手改过快照, 请重跑 --update-baseline')
    return entries, ''


def repo_rel(path: Path) -> str:
    """仓库内路径显示为相对路径; 仓库外 (自测用临时快照) 原样显示."""
    try:
        return str(path.relative_to(REPO_ROOT))
    except ValueError:
        return str(path)


def read_lines(path: Path) -> list[str]:
    """读文件; 读不动即 fail-closed 抛出, 由调用方计为违规."""
    return path.read_text(encoding='utf-8').splitlines()


def rust_files(base: Path):
    for path in sorted(base.rglob('*.rs')):
        if 'smoltcp' in str(path):
            continue
        yield path


def harvest_privileged_globals() -> set[str]:
    """收割 privileged 中定义的 `static [mut] NAME: TYPE` 全局名 (单源)."""
    found: set[str] = set()
    for path in rust_files(PRIVILEGED_BASE):
        try:
            lines = read_lines(path)
        except (OSError, UnicodeDecodeError):
            continue
        for line in lines:
            m = RE_STATIC_ITEM.match(line)
            if m and m.group(1) not in RUST_KEYWORDS:
                found.add(m.group(1))
    return found


def build_name_matcher(privileged_globals: set[str]) -> re.Pattern[str]:
    """把收割到的全局名合成单条词边界正则.

    364 个名字逐行 `re.search` 会让本腿单次跑满 59s; 合成一条 alternation 后每行只扫一遍.
    前后用 lookaround 表达词边界, 与逐名 `\\bNAME\\b` 等义 (每行每个名字最多计一处).
    """
    ordered = sorted(privileged_globals, key=lambda n: (-len(n), n))
    body = '|'.join(re.escape(name) for name in ordered)
    return re.compile(r'(?<![A-Za-z0-9_])(?:' + body + r')(?![A-Za-z0-9_])')


def scan_functions(privileged_globals: set[str]):
    """扫描 functions 层生产上下文对 privileged 全局状态的引用.

    返回 (生产命中, 注释命中, 测试上下文命中); 命中项 = {file, line, global, code}.
    词边界匹配以免 SCHEDULER 误命中 SCHEDULER_READY.
    """
    prod: list[dict] = []
    comment: list[dict] = []
    test: list[dict] = []
    matcher = build_name_matcher(privileged_globals)

    for path in rust_files(FUNCTIONS_BASE):
        try:
            lines = read_lines(path)
        except (OSError, UnicodeDecodeError) as exc:
            prod.append({'file': str(path), 'line': 0, 'global': '<unreadable>',
                         'code': str(exc)})
            continue
        comment_lines = reverse_deps.scan_comment_lines(lines)
        test_lines = reverse_deps.scan_inline_cfg_test_blocks(lines)

        for idx, line in enumerate(lines, 1):
            matched = {m.group(0) for m in matcher.finditer(line)}
            for name in sorted(matched):
                if any(re.search(p, line) for p in ALLOWED_PATTERNS):
                    continue
                entry = {'file': str(path), 'line': idx, 'global': name,
                         'code': line.strip()}
                if idx in comment_lines:
                    comment.append(entry)
                elif idx in test_lines:
                    test.append(entry)
                else:
                    prod.append(entry)
    return prod, comment, test


def relative(path: str) -> str:
    return str(Path(path).resolve().relative_to(REPO_ROOT))


def aggregate(hits: list[dict]) -> Counter:
    counter: Counter = Counter()
    for h in hits:
        counter[(relative(h['file']), h['global'])] += 1
    return counter


def diff_baseline(actual: Counter,
                  baseline: dict[tuple[str, str], int]) -> tuple[list, list]:
    """返回 (超出基线的项, 基线未被命中的项)."""
    over = [(key, actual[key], baseline.get(key, 0))
            for key in actual if actual[key] > baseline.get(key, 0)]
    under = [(key, actual.get(key, 0), baseline[key])
             for key in baseline if baseline[key] > actual.get(key, 0)]
    return sorted(over), sorted(under)


def measure() -> tuple[Counter, list[dict], list[dict], list[dict], int]:
    """跑一次真实扫描, 返回 (生产命中聚合, 生产命中明细, 注释命中, 测试命中, 收割数)."""
    privileged_globals = harvest_privileged_globals()
    if not privileged_globals:
        raise RuntimeError('privileged 全局收割结果为空 —— 收割正则或扫描根已失效')
    prod, comment, test = scan_functions(privileged_globals)
    return aggregate(prod), prod, comment, test, len(privileged_globals)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description='functions 层隐式依赖审计')
    parser.add_argument('--update-baseline', action='store_true',
                        help='用当前扫描结果重写基线快照 (消解/新增债项后显式执行)')
    args = parser.parse_args(argv)

    print("functions 层隐式依赖审计 (audit_implicit_deps)")
    print("=" * 60)

    for base, label in ((PRIVILEGED_BASE, 'privileged'), (FUNCTIONS_BASE, 'functions')):
        if not base.is_dir():
            print(f"[FAIL-CLOSED] {label} 扫描根不存在: {base}", file=sys.stderr)
            return 1
    try:
        actual, prod, comment, test, global_count = measure()
    except RuntimeError as exc:
        print(f'[FAIL-CLOSED] {exc}', file=sys.stderr)
        return 1

    totals = totals_of(dict(actual))
    print(f"\n收割 privileged 全局变量: {global_count} 个 (行首 static 项声明)")
    print(f"生产上下文引用: {sum(actual.values())} 处 / {totals['items']} 个 (文件, 全局) 项")
    print(f"注释引用 (文档交叉引用, 不计违规): {len(comment)} 处")
    print(f"测试上下文引用 (§7.3 合理, 不纳入整治): {len(test)} 处")

    if args.update_baseline:
        BASELINE_FILE.write_text(dump_baseline(dict(actual)), encoding='utf-8')
        print(f"\n基线快照已重写: {repo_rel(BASELINE_FILE)} "
              f"({totals['sites']} 处 / {totals['items']} 项 / {totals['files']} 文件 "
              f"/ {totals['globals']} 个全局)")
        print('该文件需随债项变动一并提交, 并在 plan 的变更历史里说明增减原因.')
        return 0

    baseline, err_msg = load_baseline()
    if baseline is None:
        print(f"\n[FAIL-CLOSED] {err_msg}", file=sys.stderr)
        return 1
    print(f"基线快照: {sum(baseline.values())} 处 / {len(baseline)} 项 "
          f"({BASELINE_FILE.name})")

    over, under = diff_baseline(actual, baseline)

    if over:
        print(f"\n[FAIL] 超出基线 {len(over)} 项 (新增隐式依赖):")
        for (fpath, name), got, want in over:
            print(f'  {fpath}: {name} 实际 {got} 处 / 基线 {want} 处')
            for h in sorted(
                (h for h in prod if relative(h['file']) == fpath and h['global'] == name),
                key=lambda h: h['line'],
            ):
                print(f"    L{h['line']}: {h['code'][:72]}")
    if under:
        print(f"\n[FAIL] 基线过期 {len(under)} 项 (债项已减少, 快照未同步):")
        for (fpath, name), got, want in under:
            print(f"  {fpath}: {name} 实际 {got} 处 / 基线 {want} 处")

    if over or under:
        print("-" * 60)
        print(f">>> functions 层 privileged 全局引用与基线不符: "
              f"实际 {sum(actual.values())} 处 / 基线 {sum(baseline.values())} 处 <<<")
        print('确属新增债 ⇒ 按 §7 trait 化改造消除或另行裁定; 确属债项消解 ⇒ '
              '跑 `--update-baseline` 并把增减写进 plan 变更历史.')
        return 1

    print("\nPASS: 生产站点与冻结基线逐项一致 (存量债未增加)")
    return 0


if __name__ == '__main__':
    sys.exit(main())
