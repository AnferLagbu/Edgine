#!/usr/bin/env python3
"""
E-03 feature 语义拆分审计脚本

背景: kernel_test 单 feature 混有两种语义 —
  (1) 硬件路径切换 (裸机专用桩/缩减常量, 如 net 桩、IPC_MAX_* 缩减)
  (2) 纯逻辑测试注册辅助 (供 host-tests 同源引用内核真实源码)

E-03 拆分后:
  - 纯逻辑测试辅助 → `#[cfg(any(feature = "kernel_test", feature = "host-test"))]`
  - 硬件路径门控    → `#[cfg(feature = "kernel_test")]`

约定扩展 (T1 G7 host 符号桩化, 2026-09-17):
  kt/ht 同属"测试环境", 真机路径门控语义 = `not(any(kernel_test, host-test))`.
  framework 侧真机路径 (如 lib.rs kernel_init 引导链) 由 `not(kernel_test)` 推广为
  `not(any(kernel_test, host-test))`; 本脚本检查面仍为 services/ + framework/tests/,
  framework 侧的推广属既有约定表述扩展, 不改变本脚本的判定规则.

本脚本检查:
  (1) services/ 下 kernel_test 门控出现位置 — 纯逻辑门控文件 (A 类已改 any)
      不得残留"仅 kernel_test"门控; B/C 类 (net 桩 smoltcp_impl.rs / net/mod.rs /
      config/caps.rs) 列入白名单允许
  (2) framework/tests/mod.rs — 纯逻辑 mod 用 any(kernel_test, host-test) 门控,
      driver/net/idt/reset 用 kernel_test 门控, 两语义分离正确
  (3) framework/tests/ 门控外 test_* 模块内部禁止 kernel_test 门控;
      any 门控的纯逻辑 mod 文件内部也禁止 kernel_test 门控 (host-test 下语义翻转)

退出码: 0 = 通过, 1 = 有违规 (CRITICAL/HIGH 阻断), 2 = 扫描路径不存在
"""

import json
import re
import sys
from collections import defaultdict
from pathlib import Path

BASE_SERVICES = Path('src/kernel/services')
BASE_TESTS = Path('src/kernel/framework/tests')

# kernel_test 门控匹配: 覆盖
#   #[cfg(feature = "kernel_test")] / #[cfg(not(feature = "kernel_test"))]
#   #[cfg(all(..., not(feature = "kernel_test")))]
#   cfg!(...) 运行时宏
KT_GATE = re.compile(r'feature\s*=\s*"kernel_test"')

# any(kernel_test, host-test) 门控 (顺序允许两种)
ANY_GATE = re.compile(
    r'#\[cfg\(any\(\s*feature\s*=\s*"(?:kernel_test|host-test)"\s*,\s*'
    r'feature\s*=\s*"(?:kernel_test|host-test)"\s*\)\)\]'
)
# 纯 kernel_test 门控 attribute (不含 host-test)
KT_CFG_ATTR = re.compile(r'#\[cfg\((?![^)]*host-test)[^)]*feature\s*=\s*"kernel_test"[^)]*\)\]')

# ============================================================================
# services 白名单 (B/C 类, 允许 kernel_test 门控)
# ============================================================================
# B 类: net 构建桩 — smoltcp_impl.rs 的 fw_init 别名切换 + net/mod.rs 的
#       kernel_test stub 模块/From 转换/state() 分支, 均属硬件路径语义, 保持单端.
# C 类: config/caps.rs — kpti 的 cfg!() 运行时宏 (裸机测试时禁用 KPTI), 属硬件路径.
SERVICES_WHITELIST = [
    'src/kernel/services/net/smoltcp_impl.rs',
    'src/kernel/services/net/mod.rs',
    'src/kernel/services/config/caps.rs',
]

# Vendored 3rd-party 目录: 审计豁免 (同 audit_services_boundary.py)
VENDORED_EXCLUDE = [
    Path('src/kernel/services/net/smoltcp'),
]

# framework/tests 未门控模块内的运行时 cfg!() 断言豁免 (精确「文件 + 模式」组合).
# 设计依据: 与 audit_services_boundary.py 的 PROXY_ALLOWANCE 同机制 —
# 仅豁免运行时断言, 不豁免编译期 #[cfg(...)] 门控 attribute.
# 条目: test_config.rs L162 — 镜像 C 类 config/caps.rs 的 kpti 门控规则
# (kpti = cfg!(all(x86_64, not(kernel_test))), 硬件路径 KPTI 能力报告, E-03 明确
# 保持 kernel_test 单端). 该断言在 host-test 下 cfg!(kernel_test)=false 与 caps.rs
# C 类规则自洽 (host-test: kpti=true, 断言 expect_kpti=true, 通过), 不参与编译门控,
# 故豁免并记录 INFO.
TESTS_RUNTIME_CFG_ALLOWANCE = [
    ('src/kernel/framework/tests/test_config.rs', 'cfg!(feature = "kernel_test")'),
]


def is_runtime_cfg_allowed(filepath, line):
    """判断「未门控测试模块 + 运行时 cfg!() 断言」是否属豁免组合."""
    fpath = str(Path(filepath).resolve())
    for allow_file, pattern in TESTS_RUNTIME_CFG_ALLOWANCE:
        if str(Path(allow_file).resolve()) == fpath and pattern in line:
            return True
    return False


def is_vendored(filepath):
    """检查文件是否在 vendored 第三方目录中 (审计豁免)."""
    try:
        fpath = Path(filepath).resolve()
        for excl in VENDORED_EXCLUDE:
            if str(fpath).startswith(str(excl.resolve())):
                return True
    except Exception:
        pass
    return False


def is_comment_or_attr_line(line):
    """跳过纯注释行 / doc 注释行 (不跳过 #[cfg] attribute 行本身)."""
    stripped = line.strip()
    return (
        stripped.startswith('//')
        or stripped.startswith('*')
        or stripped.startswith('/*')
    )


def classify_cfg_attr(line):
    """分类一行 cfg attribute.
    返回: 'any' (含 kernel_test + host-test) / 'kt' (仅 kernel_test) / None.
    注: 仅匹配 attribute 位置 (#[cfg(...)]), 不匹配 cfg!(...) 运行时宏.
    """
    stripped = line.strip()
    if not stripped.startswith('#[cfg('):
        return None
    if 'host-test' in stripped and 'kernel_test' in stripped and 'any(' in stripped:
        return 'any'
    if 'kernel_test' in stripped and 'host-test' not in stripped:
        return 'kt'
    return None


# ============================================================================
# 检查 1: services/ kernel_test 门控扫描
# ============================================================================

def scan_services_kernel_test_gates():
    """services 层 kernel_test 门控扫描.
    白名单文件允许 (记录 INFO); 其余文件出现"仅 kernel_test"门控即违规 (HIGH).
    """
    issues = []
    files = sorted(BASE_SERVICES.rglob('*.rs'))
    skipped = 0
    for f in files:
        if is_vendored(f):
            skipped += 1
            continue
        rel = str(f)
        whitelisted = rel in SERVICES_WHITELIST
        try:
            with open(f, 'r', encoding='utf-8', errors='replace') as fh:
                lines = fh.readlines()
        except Exception:
            continue
        for lineno, line in enumerate(lines, start=1):
            if not KT_GATE.search(line):
                continue
            # 注释行 (如 "// kernel_test 模式下...") 不审计
            if is_comment_or_attr_line(line):
                continue
            # any 门控行 (已含 host-test) 不视为"仅 kernel_test"
            if 'host-test' in line:
                continue
            if whitelisted:
                issues.append({
                    'file': rel,
                    'line': lineno,
                    'severity': 'INFO',
                    'type': 'SERVICES_KERNEL_TEST_GATE_WHITELISTED',
                    'message': 'B/C 类硬件路径门控白名单文件, 允许 kernel_test 门控 (E-03 登记)',
                    'code': line.strip()[:200],
                })
            else:
                issues.append({
                    'file': rel,
                    'line': lineno,
                    'severity': 'HIGH',
                    'type': 'SERVICES_KERNEL_TEST_ONLY_GATE',
                    'message': 'services 纯逻辑门控文件残留"仅 kernel_test"门控, '
                               '应按 E-03 改 any(kernel_test, host-test)',
                    'code': line.strip()[:200],
                })
    if skipped > 0:
        print(f'[INFO] 跳过 {skipped} 个 vendored 文件 (来自 VENDORED_EXCLUDE)', file=sys.stderr)
    return issues


# ============================================================================
# 检查 2: framework/tests/mod.rs 门控分离
# ============================================================================

# E-03 语义拆分后的分类 (数据驱动, 新增 gated mod 需在此登记)
GATED_PURE_LOGICAL = {'arch', 'string', 'sched', 'sync', 'sys'}
GATED_KERNEL_ONLY = {'driver', 'net', 'idt', 'reset'}

# mod.rs 内注册调用 → 应归属的块
REGISTER_CALL_BLOCK = {
    'driver::register_tests();': 'kt',
    'net::register_tests();': 'kt',
    'reset::register_tests();': 'kt',
    'idt::register_tests();': 'kt',
    'arch::register_tests();': 'any',
    'sys::register_tests();': 'any',
    'string::register_tests();': 'any',
    'sched::register_tests();': 'any',
    'sync::register_tests();': 'any',
}


def check_mod_rs(mod_rs):
    """检查 mod.rs: (a) mod 声明门控分类; (b) test_runner_init 注册块归属."""
    issues = []
    try:
        with open(mod_rs, 'r', encoding='utf-8', errors='replace') as fh:
            lines = fh.readlines()
    except Exception as e:
        issues.append({
            'file': str(mod_rs),
            'line': 1,
            'severity': 'CRITICAL',
            'type': 'MOD_RS_READ_FAILED',
            'message': f'mod.rs 读取失败: {e}',
            'code': '',
        })
        return issues

    # --- (a) mod 声明分类 ---
    for lineno, line in enumerate(lines, start=1):
        m = re.match(r'^pub mod (\w+);', line.strip())
        if not m:
            continue
        mod_name = m.group(1)
        # 找最近一条前置 cfg attribute (跳过注释/空行)
        cfg_kind = None
        for prev in range(lineno - 2, -1, -1):
            p = lines[prev].strip()
            if p == '' or p.startswith('//') or p.startswith('/*'):
                continue
            cfg_kind = classify_cfg_attr(p)
            break
        if mod_name in GATED_PURE_LOGICAL:
            if cfg_kind != 'any':
                issues.append({
                    'file': str(mod_rs),
                    'line': lineno,
                    'severity': 'CRITICAL',
                    'type': 'PURE_LOGICAL_MOD_WRONG_GATE',
                    'message': f'纯逻辑 mod `{mod_name}` 应为 '
                               f'#[cfg(any(feature = "kernel_test", feature = "host-test"))]'
                               f' (实际: {cfg_kind or "无 cfg"})',
                    'code': line.strip()[:200],
                })
        elif mod_name in GATED_KERNEL_ONLY:
            if cfg_kind != 'kt':
                issues.append({
                    'file': str(mod_rs),
                    'line': lineno,
                    'severity': 'CRITICAL',
                    'type': 'KERNEL_ONLY_MOD_WRONG_GATE',
                    'message': f'硬件路径 mod `{mod_name}` 应为 '
                               f'#[cfg(feature = "kernel_test")] (实际: {cfg_kind or "无 cfg"})',
                    'code': line.strip()[:200],
                })
        else:
            # 未分类的 gated mod (如有前置 cfg) → 提示登记
            if cfg_kind is not None:
                issues.append({
                    'file': str(mod_rs),
                    'line': lineno,
                    'severity': 'MEDIUM',
                    'type': 'UNCLASSIFIED_GATED_MOD',
                    'message': f'gated mod `{mod_name}` 未在 E-03 分类表中登记, 需人工确认归属',
                    'code': line.strip()[:200],
                })

    # --- (b) test_runner_init 注册块归属 ---
    region_start = None
    for lineno, line in enumerate(lines, start=1):
        if 'pub fn test_runner_init' in line:
            region_start = lineno
            break
    if region_start is None:
        issues.append({
            'file': str(mod_rs),
            'line': 1,
            'severity': 'CRITICAL',
            'type': 'TEST_RUNNER_INIT_MISSING',
            'message': 'mod.rs 未找到 pub fn test_runner_init',
            'code': '',
        })
        return issues

    # 逐行记录 feature cfg 上下文 + 关注调用
    cur_cfg = None  # 'any' / 'kt' / None
    for lineno, line in enumerate(lines, start=region_start):
        stripped = line.strip()
        kind = classify_cfg_attr(stripped)
        if kind is not None:
            cur_cfg = kind
            continue
        if stripped == '' or stripped.startswith('//'):
            continue
        call = stripped.rstrip(',')
        if call in REGISTER_CALL_BLOCK:
            expect = REGISTER_CALL_BLOCK[call]
            if cur_cfg != expect:
                issues.append({
                    'file': str(mod_rs),
                    'line': lineno,
                    'severity': 'HIGH',
                    'type': 'REGISTER_CALL_WRONG_BLOCK',
                    'message': f'注册调用 `{call}` 应位于 {"any(纯逻辑)" if expect == "any" else "kernel_test(硬件路径)"} '
                               f'块内 (当前上下文: {cur_cfg or "无 feature cfg"})',
                    'code': stripped[:200],
                })
    return issues


# ============================================================================
# 检查 3: framework/tests/ 其他文件 — 门控外/any 门控文件内部禁止 kernel_test 门控
# ============================================================================

def scan_tests_files():
    """framework/tests/ 非 mod.rs 文件扫描.
    - any 门控纯逻辑 mod 文件 (arch/string/sched/sync/sys): 内部 kernel_test 门控
      会在 host-test 下语义翻转 → 违规 (HIGH)
    - 未门控 test_* 模块文件: kernel_test 门控 → 违规 (HIGH)
    - kernel_test 单端门控 mod 文件 (driver/net/idt/reset): 内部门控允许 (INFO 记录)
    """
    issues = []
    for f in sorted(BASE_TESTS.glob('*.rs')):
        name = f.stem
        if name == 'mod':
            continue
        with open(f, 'r', encoding='utf-8', errors='replace') as fh:
            lines = fh.readlines()
        if name in GATED_PURE_LOGICAL:
            role = 'any_pure'
        elif name in GATED_KERNEL_ONLY:
            role = 'kt_kernel'
        else:
            role = 'ungated'
        for lineno, line in enumerate(lines, start=1):
            if not KT_GATE.search(line):
                continue
            if is_comment_or_attr_line(line):
                continue
            if 'host-test' in line:
                continue
            if role == 'any_pure':
                issues.append({
                    'file': str(f),
                    'line': lineno,
                    'severity': 'HIGH',
                    'type': 'PURE_LOGICAL_MOD_INTERNAL_KT_GATE',
                    'message': f'any 门控纯逻辑 mod `{name}` 内部含 kernel_test 门控, '
                               'host-test 下语义翻转, 需移除或改 any 门控',
                    'code': line.strip()[:200],
                })
            elif role == 'ungated':
                if is_runtime_cfg_allowed(f, line):
                    issues.append({
                        'file': str(f),
                        'line': lineno,
                        'severity': 'INFO',
                        'type': 'UNGATED_TEST_MOD_KT_RUNTIME_ASSERT_ALLOWED',
                        'message': f'未门控测试模块 `{name}` 内运行时 cfg!() 断言镜像 '
                                   'C 类 caps.rs 硬件路径规则 (E-03 登记豁免)',
                        'code': line.strip()[:200],
                    })
                else:
                    issues.append({
                        'file': str(f),
                        'line': lineno,
                        'severity': 'HIGH',
                        'type': 'UNGATED_TEST_MOD_KT_GATE',
                        'message': f'未门控测试模块 `{name}` 内部含 kernel_test 门控, '
                                   '该模块在 host-test 下也会编译, 语义不一致',
                        'code': line.strip()[:200],
                    })
            else:  # kt_kernel
                issues.append({
                    'file': str(f),
                    'line': lineno,
                    'severity': 'INFO',
                    'type': 'KERNEL_ONLY_MOD_INTERNAL_KT_GATE',
                    'message': f'kernel_test 单端门控 mod `{name}` 内部 kernel_test 门控 '
                               '(E-03 允许, 模块整体仅 kernel_test 编译)',
                    'code': line.strip()[:200],
                })
    return issues


# ============================================================================
# 报告
# ============================================================================

def generate_report(issues):
    """生成报告 (仿 audit_services_boundary.py)."""
    by_severity = defaultdict(list)
    for issue in issues:
        by_severity[issue['severity']].append(issue)

    report = []
    report.append('=' * 78)
    report.append('E-03 feature 语义拆分审计报告')
    report.append('  纯逻辑测试辅助 = any(kernel_test, host-test)')
    report.append('  硬件路径门控   = feature = "kernel_test"')
    report.append('=' * 78)
    report.append('')
    report.append(f'问题总数: {len(issues)}')
    report.append('')

    for sev in ['CRITICAL', 'HIGH', 'MEDIUM', 'LOW', 'INFO']:
        count = len(by_severity[sev])
        if count == 0:
            continue
        report.append(f'[{sev}] {count} 项')
        report.append('-' * 78)
        by_file = defaultdict(list)
        for issue in by_severity[sev]:
            by_file[issue['file']].append(issue)
        for filepath, file_issues in sorted(by_file.items()):
            report.append(f'\n  {filepath}:')
            for issue in sorted(file_issues, key=lambda x: x['line']):
                report.append(f'    L{issue["line"]}: {issue["type"]}')
                report.append(f'      {issue["message"]}')
                report.append(f'      代码: {issue["code"]}')
        report.append('')

    return '\n'.join(report)


def main():
    if not BASE_SERVICES.exists() or not BASE_TESTS.exists():
        print(f'ERROR: {BASE_SERVICES} 或 {BASE_TESTS} not found', file=sys.stderr)
        sys.exit(2)

    issues = []
    issues.extend(scan_services_kernel_test_gates())
    issues.extend(check_mod_rs(BASE_TESTS / 'mod.rs'))
    issues.extend(scan_tests_files())

    report = generate_report(issues)
    print(report)

    json_path = Path('other/target/audit/feature-semantics.json')
    json_path.parent.mkdir(parents=True, exist_ok=True)
    with open(json_path, 'w', encoding='utf-8') as fh:
        json.dump({'issue_count': len(issues), 'issues': issues}, fh,
                  ensure_ascii=False, indent=2)
    print(f'\nJSON 报告保存至: {json_path}')

    # 退出码: CRITICAL/HIGH 阻断 (MEDIUM 仅提示)
    critical = sum(1 for i in issues if i['severity'] == 'CRITICAL')
    high = sum(1 for i in issues if i['severity'] == 'HIGH')
    if critical > 0:
        print(f'\n>>> {critical} 个 CRITICAL 违规 (E-03 门控分类破坏) <<<')
        sys.exit(1)
    if high > 0:
        print(f'\n>>> {high} 个 HIGH 违规 (kernel_test 门控语义混淆) <<<')
        sys.exit(1)

    print('\n>>> E-03 feature 语义拆分审计通过 <<<')
    sys.exit(0)


if __name__ == '__main__':
    main()
