#!/usr/bin/env python3
"""
M6.4 模块耦合度审计脚本 — 循环依赖/依赖深度/公开接口/跨子系统直接访问

检查规则:
  (1) 检测 privileged 与 functions 两层子模块间的双向依赖 (循环耦合)
  (2) 检测 privileged 子模块间的跨子系统内部访问 (绕过 api.rs)
  (3) 统计各模块的公开接口比例 (pub 项 / 总项)
  (4) 生成模块依赖矩阵 JSON 报告

F3 判据 (P-54 甲+丙a): 两层的禁止循环依赖全部与机器派生基线快照
`scripts/audit_coupling.baseline.json` 对账 —— 基线外的环、基线环的计数增加
(合计或任一方向) 一律阻断; 基线环减弱或消失而快照未同步同样阻断 (双向 ratchet,
与 audit_implicit_deps 的 P-49 基线同构). 基线只能由 `--update-baseline` 从同一份
扫描结果导出, 脚本内不保存任何手写环清单或阈值. functions 层没有「允许紧耦合」名单,
其任一环都进入对账.

边可见性口径 (P-54 实测, 见 §6 P-65): 依赖边只来自单行 `use ...;` 语句 —— 跨行
use (36 处 / 15 对)、`pub use` (5 处 / 5 对) 与非 use 行的限定调用 (privileged 827 处 /
145 对, functions 322 处 / 22 对) 都不成边. 三者全扩后禁止环 privileged 4 → 31、
functions 1 → 5, 属红墙, 故本轮不扩边, 另立 P-65.

用法: python3 scripts/audit_coupling.py [--update-baseline]
退出码: 0 = 通过, 1 = 有违规 / 基线不符 / 快照缺失或不自洽
"""

import argparse
import os
import re
import sys
import json
from collections import defaultdict
from pathlib import Path

PRIVILEGED_BASE = Path('src/kernel/privileged')
FUNCTIONS_BASE = Path('src/kernel/functions')
BASELINE_FILE = Path(__file__).resolve().parent / 'audit_coupling.baseline.json'

# privileged 子系统间允许的内部访问白名单
# 格式: (源模块, 目标模块内部子模块) → 允许
# 默认: 只允许通过 api.rs / types.rs / mod.rs 访问
ALLOWED_INTERNAL_ACCESS = {
    # proc 需要访问 mm 的 API 层 (通过 mm::api)
    # 但不应直接访问 mm::pmm, mm::vma 等内部
}

# 允许适度紧耦合的子系统对 (仅底层硬件抽象 + 调度核心通路)
# 这些对的双向依赖不会触发错误, 仅输出 info 级别提示
ALLOWED_TIGHT_COUPLING = {
    ('arch', 'sync'),      # 架构层需要自旋锁原语
    ('arch', 'klog'),      # 早期启动日志
    ('idt', 'mm'),         # 中断→页错误处理
    ('timer', 'idt'),      # 定时器中断注册
    ('driver', 'sync'),    # 硬件驱动需要锁
    ('driver', 'mm'),      # 硬件驱动需要 DMA
    ('driver', 'io'),      # 硬件驱动需要 I/O
    ('console', 'driver'), # 控制台需要显示驱动
    ('proc', 'sched'),     # 进程管理↔调度核心
    ('sgeg', 'proc'),     # 安全凭证↔进程管理 (PwmContext 绑定 Process)
    ('proc', 'tests'),     # 测试框架↔被测模块 (#[cfg(test)] 内嵌测试)
    ('fs', 'tests'),       # 测试框架↔被测模块
    ('mm', 'tests'),       # 测试框架↔被测模块
}

# privileged 子系统间禁止直接访问的内部子模块模式
# 如果 A 直接 use B::internal_submodule (而非 B::api / B::types / B::mod),
# 则视为违规
INTERNAL_PATTERNS = [
    # mm 内部 — api/vma/swap/kpti/page_fault/pressure/copy_user/pmm 已在 mm/mod.rs re-export
    r'privileged::mm::vmm_x86_64',
    r'privileged::mm::vmm_aarch64',
    r'privileged::mm::slab',
    r'privileged::mm::frame',
    r'privileged::mm::cow',
    r'privileged::mm::pcache',
    r'privileged::mm::kpti_aarch64',
    r'privileged::mm::numa',
    r'privileged::mm::arch',
    # proc 内部 — api/signal/rlimit/fd_alloc/seccomp/namespace/cgroup/elf/madvise_mlock/cpu_queue/session/thread/scheduler/scheduler_ex/process/canary/posix_timer/user_proc 已 re-export
    r'privileged::proc::oomd',
    r'privileged::proc::cfs',
    # syscall 内部 — types/epoll/api 已 re-export
    r'privileged::syscall::futex',
    r'privileged::syscall::eventfd',
    r'privileged::syscall::timerfd',
    r'privileged::syscall::signalfd',
    r'privileged::syscall::sendfile',
    r'privileged::syscall::io',
    r'privileged::syscall::mmap',
    r'privileged::syscall::mprotect',
    r'privileged::syscall::madvise_mlock',
    r'privileged::syscall::brk',
    r'privileged::syscall::clone',
    r'privileged::syscall::posix_timer',
    r'privileged::syscall::canary',
    r'privileged::syscall::raw',
    # fs 内部 — vfs 是 fs 的公共子模块入口, 不标记为内部
    r'privileged::fs::ramfs',
    r'privileged::fs::devfs',
    # driver 内部 — net/virtio/display/storage/char/input/power/kexec/uefi 已在 driver/mod.rs glob re-export
    # sync 内部 — spinlock/irq_spinlock/lockdep 已 re-export
    r'privileged::sync::raw',
    r'privileged::sync::arch',
    r'privileged::sync::seqlock::raw',
    r'privileged::sync::rcu::raw',
    # net 内部 — init 已 re-export (poll_network)
    r'privileged::net::smoltcp_impl',
    r'privileged::net::smoltcp',
    r'privileged::net::save',
    # timer 内部 — tick/calibration/tickless/time_sync/hrtimer/sleep/pit 已 glob re-export
    # idt 内部
    r'privileged::idt::statistics',
    r'privileged::idt::handlers',
    r'privileged::idt::safety',
    r'privileged::idt::types',
    # arch 内部 — apic/ioapic/gdt/tss/uart/exception/mmu/gic/timer/X8664/Aarch64/shadow_stack 已 re-export
    # sgeg 内部 — api/session/engine/secure_boot 已 glob re-export
    # debug 内部 — api/ebpf/ftrace/kgdb 已 glob re-export
    # egdf 内部 — composite/firmware/proto_net/devtree 已 glob re-export
    # cpu 内部 — tsc 已 re-export (read_tsc/read_tsc_serialized)
    # io 内部 — iouring 已 glob re-export
]


def get_submodules(base):
    """获取 base 目录下所有子模块名."""
    modules = []
    if not base.exists():
        return modules
    for d in sorted(base.iterdir()):
        if d.is_dir() and (d / 'mod.rs').exists():
            modules.append(d.name)
    return modules


def scan_cross_module_deps(base, layer_name):
    """扫描子模块间的交叉依赖."""
    modules = get_submodules(base)
    dep_matrix = defaultdict(lambda: defaultdict(int))
    dep_details = defaultdict(lambda: defaultdict(list))

    for mod in modules:
        mod_dir = base / mod
        for rs_file in sorted(mod_dir.rglob('*.rs')):
            try:
                with open(rs_file, 'r', encoding='utf-8', errors='replace') as f:
                    for lineno, line in enumerate(f, 1):
                        stripped = line.strip()
                        if stripped.startswith('//') or stripped.startswith('/*'):
                            continue
                        m = re.match(r'^\s*use\s+(.*?);', line)
                        if not m:
                            continue
                        import_path = m.group(1)
                        for other_mod in modules:
                            if other_mod == mod:
                                continue
                            pattern = f'{layer_name}::{other_mod}'
                            if pattern in import_path:
                                dep_matrix[mod][other_mod] += 1
                                dep_details[mod][other_mod].append({
                                    'file': str(rs_file.relative_to(Path('src/kernel'))),
                                    'line': lineno,
                                    'import': import_path,
                                })
            except Exception:
                continue

    return dep_matrix, dep_details


def detect_circular_deps(dep_matrix, layer):
    """检测双向依赖 (A→B 且 B→A), 区分允许/禁止的紧耦合.

    ALLOWED_TIGHT_COUPLING 只描述 privileged 子系统对; functions 层不适用,
    其任一双向依赖都按禁止环处理, 交给基线对账.
    """
    circular = []
    allowed = []
    modules = sorted(dep_matrix.keys())
    for i, a in enumerate(modules):
        for b in modules[i+1:]:
            if dep_matrix[a].get(b, 0) > 0 and dep_matrix[b].get(a, 0) > 0:
                pair = (a, b) if a < b else (b, a)
                entry = {
                    'layer': layer,
                    'modules': (a, b),
                    'a_to_b': dep_matrix[a][b],
                    'b_to_a': dep_matrix[b][a],
                    'total': dep_matrix[a][b] + dep_matrix[b][a],
                }
                if layer == 'privileged' and pair in ALLOWED_TIGHT_COUPLING:
                    allowed.append(entry)
                else:
                    circular.append(entry)
    return sorted(circular, key=lambda x: x['total'], reverse=True), \
           sorted(allowed, key=lambda x: x['total'], reverse=True)


def cycles_to_entries(cycles):
    """把循环依赖条目压成基线可对账的键值表: (层, 模块A, 模块B) → (A→B, B→A)."""
    return {(c['layer'], c['modules'][0], c['modules'][1]):
            (c['a_to_b'], c['b_to_a']) for c in cycles}


def totals_of(entries):
    """由条目自身求和得出的自洽校验位 (手改条目而不重跑 --update-baseline 即不自洽)."""
    return {
        'pairs': len(entries),
        'refs': sum(a + b for a, b in entries.values()),
        'privileged': sum(1 for layer, _, _ in entries if layer == 'privileged'),
        'functions': sum(1 for layer, _, _ in entries if layer == 'functions'),
    }


def dump_baseline(entries):
    """确定性序列化 (排序 + 固定缩进), 保证 --update-baseline 幂等到字节级."""
    payload = {
        '_meta': {
            'generated_by': 'python3 scripts/audit_coupling.py --update-baseline',
            '口径': 'privileged + functions 两层禁止循环依赖的冻结清单; 判定按 (层, 模块对) '
                  '集合与双向 use 边计数, 计数增加/减少同样阻断',
            'disposition': 'P-54 冻结存量环, 解耦归 F3 后续批次',
        },
        'totals': totals_of(entries),
        'entries': [[layer, a, b, ab, ba]
                    for (layer, a, b), (ab, ba) in sorted(entries.items())],
    }
    return json.dumps(payload, ensure_ascii=False, indent=2) + '\n'


def load_baseline():
    """读快照; 返回 (条目, 错误说明). 读不到/不自洽都 fail-closed."""
    if not BASELINE_FILE.is_file():
        return None, (f'基线快照缺失: {repo_rel(BASELINE_FILE)} '
                      f'(须显式跑 --update-baseline 生成并提交, 不接受自动放行)')
    try:
        payload = json.loads(BASELINE_FILE.read_text(encoding='utf-8'))
        entries = {(e[0], e[1], e[2]): (int(e[3]), int(e[4]))
                   for e in payload['entries']}
    except (OSError, json.JSONDecodeError, KeyError, TypeError, ValueError) as exc:
        return None, f'基线快照无法解析: {exc}'
    declared = payload.get('totals', {})
    actual = totals_of(entries)
    if declared != actual:
        return None, (f'基线快照 totals 与 entries 不自洽: 声明 {declared} / 实算 {actual} '
                      f'—— 手改过快照, 请重跑 --update-baseline')
    return entries, ''


def diff_baseline(actual, baseline):
    """返回 (超出基线的项, 落后基线的项), 按方向逐项比较.

    超出 = 新增环, 或已有环任一方向的 edge 数变大;
    落后 = 已有环任一方向变小, 或基线中的环完全消失 —— 环消失是好事, 但快照必须
    同步, 否则同一位置的死条目会掩盖将来重新引入的环.
    """
    over = []
    under = []
    for key in sorted(actual):
        got = actual[key]
        want = baseline.get(key)
        if want is None:
            over.append((key, got, None))
            continue
        if got[0] > want[0] or got[1] > want[1]:
            over.append((key, got, want))
        if got[0] < want[0] or got[1] < want[1]:
            under.append((key, got, want))
    for key in sorted(baseline):
        if key not in actual:
            under.append((key, (0, 0), baseline[key]))
    return over, under


def repo_rel(path):
    """仓库内路径显示为相对路径; 仓库外 (自测用临时快照) 原样显示."""
    try:
        return str(path.relative_to(Path(__file__).resolve().parent.parent))
    except ValueError:
        return str(path)


def check_internal_access(base, layer_name):
    """检查跨子系统直接访问内部子模块."""
    issues = []
    modules = get_submodules(base)

    for mod in modules:
        mod_dir = base / mod
        for rs_file in sorted(mod_dir.rglob('*.rs')):
            try:
                with open(rs_file, 'r', encoding='utf-8', errors='replace') as f:
                    for lineno, line in enumerate(f, 1):
                        stripped = line.strip()
                        if stripped.startswith('//') or stripped.startswith('/*'):
                            continue
                        m = re.match(r'^\s*use\s+(.*?);', line)
                        if not m:
                            continue
                        import_path = m.group(1)

                        for pattern in INTERNAL_PATTERNS:
                            # 精确匹配: pattern 必须是完整路径段, 避免 privileged::mm::vma 误匹配
                            # privileged::mm::vma_get_current_mm (vma 是 vma_get 的前缀但不是路径段)
                            # 在 pattern 后追加 :: 或匹配到路径末尾
                            if pattern in import_path:
                                # 验证 pattern 后面要么是 :: 要么是路径结束
                                idx = import_path.find(pattern)
                                end = idx + len(pattern)
                                if end < len(import_path) and import_path[end:end+2] != '::':
                                    continue
                                # 排除自身模块的内部访问
                                # e.g. privileged::mm::pmm 被 privileged/mm/ 内部使用是允许的
                                # pattern 格式: privileged::proc::process → 子系统是 proc (index 1)
                                target_mod = pattern.split('::')[1] if '::' in pattern else ''
                                if target_mod == mod:
                                    continue
                                # 排除 tests 目录 — 白盒测试允许直接访问内部实现
                                if mod == 'tests':
                                    continue
                                issues.append({
                                    'file': str(rs_file.relative_to(Path('src/kernel'))),
                                    'line': lineno,
                                    'severity': 'HIGH',
                                    'type': 'INTERNAL_ACCESS',
                                    'source': mod,
                                    'target': pattern,
                                    'import': import_path,
                                    'message': f'{mod} 直接访问 {pattern} 内部, 应通过公开 API',
                                })
            except Exception:
                continue

    return issues


def count_pub_surface(base):
    """统计各模块的公开接口比例."""
    result = {}
    modules = get_submodules(base)

    for mod in modules:
        mod_dir = base / mod
        pub_count = 0
        total_count = 0
        for rs_file in sorted(mod_dir.rglob('*.rs')):
            try:
                with open(rs_file, 'r', encoding='utf-8', errors='replace') as f:
                    for line in f:
                        stripped = line.strip()
                        if stripped.startswith('//') or stripped.startswith('/*') or stripped.startswith('#'):
                            continue
                        # 统计 fn, struct, enum, trait, const, static, type 定义
                        if re.match(r'^\s*pub\s+(?:async\s+)?fn\s+', line):
                            pub_count += 1
                            total_count += 1
                        elif re.match(r'^\s*fn\s+', line):
                            total_count += 1
                        elif re.match(r'^\s*pub\s+struct\s+', line):
                            pub_count += 1
                            total_count += 1
                        elif re.match(r'^\s*struct\s+', line):
                            total_count += 1
                        elif re.match(r'^\s*pub\s+enum\s+', line):
                            pub_count += 1
                            total_count += 1
                        elif re.match(r'^\s*enum\s+', line):
                            total_count += 1
                        elif re.match(r'^\s*pub\s+trait\s+', line):
                            pub_count += 1
                            total_count += 1
                        elif re.match(r'^\s*trait\s+', line):
                            total_count += 1
                        elif re.match(r'^\s*pub\s+const\s+', line):
                            pub_count += 1
                            total_count += 1
                        elif re.match(r'^\s*const\s+', line):
                            total_count += 1
                        elif re.match(r'^\s*pub\s+static\s+', line):
                            pub_count += 1
                            total_count += 1
                        elif re.match(r'^\s*static\s+', line):
                            total_count += 1
            except Exception:
                continue

        ratio = (pub_count / total_count * 100) if total_count > 0 else 0
        result[mod] = {
            'pub': pub_count,
            'total': total_count,
            'ratio': round(ratio, 1),
        }

    return result


def generate_dependency_matrix_json(fw_matrix, svc_matrix, circular, internal_issues,
                                    pub_surface, reconciliation):
    """生成依赖矩阵 JSON 报告."""
    report = {
        'timestamp': '2026-06-16',
        'privileged_deps': {},
        'functions_deps': {},
        'circular_deps': circular,
        'internal_access_issues': len(internal_issues),
        'pub_surface': pub_surface,
        'baseline_reconciliation': reconciliation,
    }

    for mod in sorted(fw_matrix.keys()):
        report['privileged_deps'][mod] = dict(fw_matrix[mod])

    for mod in sorted(svc_matrix.keys()):
        report['functions_deps'][mod] = dict(svc_matrix[mod])

    return report


def main():
    parser = argparse.ArgumentParser(description='模块耦合度审计 (F3)')
    parser.add_argument('--update-baseline', action='store_true',
                        help='用当前扫描结果重写基线快照 (环消解/重新引入后显式执行)')
    args = parser.parse_args()

    print('=' * 78)
    print('M6.4 模块耦合度审计报告')
    print('=' * 78)
    print()

    for base, label in ((PRIVILEGED_BASE, 'privileged'), (FUNCTIONS_BASE, 'functions')):
        if not base.is_dir():
            print(f'[FAIL-CLOSED] {label} 扫描根不存在: {base} '
                  f'(须在仓库根执行, 否则空矩阵会伪装成全绿)', file=sys.stderr)
            sys.exit(1)
        if not get_submodules(base):
            print(f'[FAIL-CLOSED] {label} 扫描根下没有子模块: {base}', file=sys.stderr)
            sys.exit(1)

    # 1. privileged 子模块间交叉依赖
    print('[1] privileged 子模块间交叉依赖')
    print('-' * 78)
    fw_matrix, fw_details = scan_cross_module_deps(PRIVILEGED_BASE, 'privileged')
    fw_total = sum(sum(targets.values()) for targets in fw_matrix.values())
    print(f'总交叉引用数: {fw_total}')

    # 按引用数排序输出
    for mod in sorted(fw_matrix.keys()):
        deps = fw_matrix[mod]
        if deps:
            dep_str = ', '.join(f'{k}:{v}' for k, v in sorted(deps.items(), key=lambda x: -x[1]))
            print(f'  {mod} → {dep_str}')
    print()

    # 2. functions 子模块间交叉依赖
    print('[2] functions 子模块间交叉依赖')
    print('-' * 78)
    svc_matrix, svc_details = scan_cross_module_deps(FUNCTIONS_BASE, 'functions')
    svc_total = sum(sum(targets.values()) for targets in svc_matrix.values())
    print(f'总交叉引用数: {svc_total}')

    for mod in sorted(svc_matrix.keys()):
        deps = svc_matrix[mod]
        if deps:
            dep_str = ', '.join(f'{k}:{v}' for k, v in sorted(deps.items(), key=lambda x: -x[1]))
            print(f'  {mod} → {dep_str}')
    print()

    # 3. 循环依赖检测 — 两层都判定
    print('[3] 双向依赖 (循环耦合) 检测 — privileged + functions')
    print('-' * 78)
    fw_circular, fw_allowed = detect_circular_deps(fw_matrix, 'privileged')
    svc_circular, svc_allowed = detect_circular_deps(svc_matrix, 'functions')
    circular = fw_circular + svc_circular
    allowed_coupling = fw_allowed + svc_allowed
    print(f'  privileged: 禁止 {len(fw_circular)} 对 / 允许 {len(fw_allowed)} 对')
    print(f'  functions:  禁止 {len(svc_circular)} 对 / 允许 {len(svc_allowed)} 对')
    for c in circular:
        a, b = c['modules']
        print(f'  ⚠ [{c["layer"]}] {a} ↔ {b}: {c["a_to_b"]}/{c["b_to_a"]} '
              f'(合计 {c["total"]})')
    if allowed_coupling:
        print()
        print('  [允许的紧耦合] (privileged 硬件抽象/调度核心通路):')
        for c in allowed_coupling:
            a, b = c['modules']
            print(f'    ✓ {a} ↔ {b}: {c["a_to_b"]}/{c["b_to_a"]} (合计 {c["total"]})')
    print()

    # 4. 与机器派生基线对账 (P-54: 不再使用 >20 阈值)
    print('[4] 禁止循环依赖与基线快照对账')
    print('-' * 78)
    actual_entries = cycles_to_entries(circular)

    if args.update_baseline:
        BASELINE_FILE.write_text(dump_baseline(actual_entries), encoding='utf-8')
        totals = totals_of(actual_entries)
        print(f'基线快照已重写: {repo_rel(BASELINE_FILE)} '
              f'({totals["pairs"]} 对 / {totals["refs"]} 处引用; '
              f'privileged {totals["privileged"]} / functions {totals["functions"]})')
        print('该文件需随环增减一并提交, 并在 plan 的变更历史里说明增减原因.')
        sys.exit(0)

    baseline, err_msg = load_baseline()
    if baseline is None:
        print(f'[FAIL-CLOSED] {err_msg}', file=sys.stderr)
        sys.exit(1)
    print(f'基线快照: {len(baseline)} 对禁止环 '
          f'({sum(a + b for a, b in baseline.values())} 处引用; {BASELINE_FILE.name})')

    over, under = diff_baseline(actual_entries, baseline)
    if over:
        print()
        print(f'[FAIL] 超出基线 {len(over)} 项 (新增或加剧的循环依赖):')
        for (layer, a, b), got, want in over:
            want_str = '基线中不存在' if want is None else f'{want[0]}/{want[1]}'
            print(f'  [{layer}] {a} ↔ {b}: 实际 {got[0]}/{got[1]} / 基线 {want_str}')
    if under:
        print()
        print(f'[FAIL] 基线过期 {len(under)} 项 (环已减弱或消失, 快照未同步):')
        for (layer, a, b), got, want in under:
            print(f'  [{layer}] {a} ↔ {b}: 实际 {got[0]}/{got[1]} / 基线 {want[0]}/{want[1]}')
    print()

    reconciliation = {
        'baseline_pairs': len(baseline),
        'over': [{'layer': k[0], 'modules': [k[1], k[2]],
                  'actual': list(got),
                  'baseline': list(want) if want is not None else None}
                 for k, got, want in over],
        'under': [{'layer': k[0], 'modules': [k[1], k[2]],
                   'actual': list(got), 'baseline': list(want)}
                  for k, got, want in under],
    }

    # 5. 跨子系统内部访问检查
    print('[5] 跨子系统内部访问检查')
    print('-' * 78)
    internal_issues = check_internal_access(PRIVILEGED_BASE, 'privileged')
    if internal_issues:
        # 按源模块分组
        by_source = defaultdict(list)
        for issue in internal_issues:
            by_source[issue['source']].append(issue)

        for source in sorted(by_source.keys()):
            issues = by_source[source]
            print(f'  {source}: {len(issues)} 处内部访问')
            for issue in issues[:5]:  # 每个模块最多显示 5 条
                print(f'    {issue["file"]}:L{issue["line"]} → {issue["target"]}')
            if len(issues) > 5:
                print(f'    ... 还有 {len(issues) - 5} 处')
    else:
        print('  无违规')
    print()

    # 6. 公开接口比例
    print('[6] privileged 公开接口比例')
    print('-' * 78)
    pub_surface = count_pub_surface(PRIVILEGED_BASE)
    for mod in sorted(pub_surface.keys()):
        info = pub_surface[mod]
        print(f'  {mod}: {info["pub"]}/{info["total"]} ({info["ratio"]}%)')
    print()

    # 7. 生成 JSON 报告
    json_path = Path('other/target/audit/dependency-matrix.json')
    json_path.parent.mkdir(parents=True, exist_ok=True)
    report = generate_dependency_matrix_json(fw_matrix, svc_matrix, circular,
                                             internal_issues, pub_surface,
                                             reconciliation)
    with open(json_path, 'w', encoding='utf-8') as f:
        json.dump(report, f, ensure_ascii=False, indent=2)
    print(f'JSON 报告保存至: {json_path}')
    print()

    # 8. 结果判定
    high_severity = [i for i in internal_issues if i['severity'] == 'HIGH']

    if high_severity or over or under:
        print('=' * 78)
        if high_severity:
            print(f'⚠  {len(high_severity)} 处跨子系统内部访问违规')
        if over:
            print(f'⚠  {len(over)} 项超出循环依赖基线')
        if under:
            print(f'⚠  {len(under)} 项循环依赖基线过期')
        print('>>> 耦合审计未通过 <<<')
        sys.exit(1)
    else:
        print('>>> 耦合审计通过 (两层禁止环与冻结基线逐项一致) <<<')
        sys.exit(0)


if __name__ == '__main__':
    main()
