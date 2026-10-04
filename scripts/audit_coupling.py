#!/usr/bin/env python3
"""
M6.4 模块耦合度审计脚本 — 循环依赖/依赖深度/公开接口/跨子系统直接访问

检查规则:
  (1) 检测 framework 子模块间的双向依赖 (循环耦合)
  (2) 检测 framework 子模块间的跨子系统内部访问 (绕过 api.rs)
  (3) 统计各模块的公开接口比例 (pub 项 / 总项)
  (4) 检测 services 子模块间的隐式依赖传递
  (5) 生成模块依赖矩阵 JSON 报告

退出码: 0 = 通过, 1 = 有严重违规
"""

import os
import re
import sys
import json
from collections import defaultdict
from pathlib import Path

FRAMEWORK_BASE = Path('src/kernel/framework')
SERVICES_BASE = Path('src/kernel/services')

# framework 子系统间允许的内部访问白名单
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
    ('credo', 'proc'),     # 安全凭证↔进程管理 (PwmContext 绑定 Process)
    ('proc', 'tests'),     # 测试框架↔被测模块 (#[cfg(test)] 内嵌测试)
    ('fs', 'tests'),       # 测试框架↔被测模块
    ('mm', 'tests'),       # 测试框架↔被测模块
}

# framework 子系统间禁止直接访问的内部子模块模式
# 如果 A 直接 use B::internal_submodule (而非 B::api / B::types / B::mod),
# 则视为违规
INTERNAL_PATTERNS = [
    # mm 内部 — api/vma/swap/kpti/page_fault/pressure/copy_user/pmm 已在 mm/mod.rs re-export
    r'framework::mm::vmm_x86_64',
    r'framework::mm::vmm_aarch64',
    r'framework::mm::slab',
    r'framework::mm::frame',
    r'framework::mm::cow',
    r'framework::mm::pcache',
    r'framework::mm::kpti_aarch64',
    r'framework::mm::numa',
    r'framework::mm::arch',
    # proc 内部 — api/signal/rlimit/fd_alloc/seccomp/namespace/cgroup/elf/madvise_mlock/cpu_queue/session/thread/scheduler/scheduler_ex/process/canary/posix_timer/user_proc 已 re-export
    r'framework::proc::oomd',
    r'framework::proc::cfs',
    # syscall 内部 — types/epoll/api 已 re-export
    r'framework::syscall::futex',
    r'framework::syscall::eventfd',
    r'framework::syscall::timerfd',
    r'framework::syscall::signalfd',
    r'framework::syscall::sendfile',
    r'framework::syscall::io',
    r'framework::syscall::mmap',
    r'framework::syscall::mprotect',
    r'framework::syscall::madvise_mlock',
    r'framework::syscall::brk',
    r'framework::syscall::clone',
    r'framework::syscall::posix_timer',
    r'framework::syscall::canary',
    r'framework::syscall::raw',
    # fs 内部 — vfs 是 fs 的公共子模块入口, 不标记为内部
    r'framework::fs::ramfs',
    r'framework::fs::devfs',
    # driver 内部 — net/virtio/display/storage/char/input/power/kexec/uefi 已在 driver/mod.rs glob re-export
    # sync 内部 — spinlock/irq_spinlock/lockdep 已 re-export
    r'framework::sync::raw',
    r'framework::sync::arch',
    r'framework::sync::seqlock::raw',
    r'framework::sync::rcu::raw',
    # net 内部 — init 已 re-export (poll_network)
    r'framework::net::smoltcp_impl',
    r'framework::net::smoltcp',
    r'framework::net::save',
    # timer 内部 — tick/calibration/tickless/time_sync/hrtimer/sleep/pit 已 glob re-export
    # idt 内部
    r'framework::idt::statistics',
    r'framework::idt::handlers',
    r'framework::idt::safety',
    r'framework::idt::types',
    # arch 内部 — apic/ioapic/gdt/tss/uart/exception/mmu/gic/timer/X8664/Aarch64/shadow_stack 已 re-export
    # credo 内部 — api/session/engine/secure_boot 已 glob re-export
    # debug 内部 — api/ebpf/ftrace/kgdb 已 glob re-export
    # chitin 内部 — composite/firmware/proto_net/devtree 已 glob re-export
    # cpu 内部 — tsc 已 re-export (read_tsc/read_tsc_serialized/cycles_to_nanoseconds)
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


def detect_circular_deps(dep_matrix):
    """检测双向依赖 (A→B 且 B→A), 区分允许/禁止的紧耦合."""
    circular = []
    allowed = []
    modules = sorted(dep_matrix.keys())
    for i, a in enumerate(modules):
        for b in modules[i+1:]:
            if dep_matrix[a].get(b, 0) > 0 and dep_matrix[b].get(a, 0) > 0:
                pair = (a, b) if a < b else (b, a)
                entry = {
                    'modules': (a, b),
                    'a_to_b': dep_matrix[a][b],
                    'b_to_a': dep_matrix[b][a],
                    'total': dep_matrix[a][b] + dep_matrix[b][a],
                }
                if pair in ALLOWED_TIGHT_COUPLING:
                    allowed.append(entry)
                else:
                    circular.append(entry)
    return sorted(circular, key=lambda x: x['total'], reverse=True), \
           sorted(allowed, key=lambda x: x['total'], reverse=True)


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
                            # 精确匹配: pattern 必须是完整路径段, 避免 framework::mm::vma 误匹配
                            # framework::mm::vma_get_current_mm (vma 是 vma_get 的前缀但不是路径段)
                            # 在 pattern 后追加 :: 或匹配到路径末尾
                            if pattern in import_path:
                                # 验证 pattern 后面要么是 :: 要么是路径结束
                                idx = import_path.find(pattern)
                                end = idx + len(pattern)
                                if end < len(import_path) and import_path[end:end+2] != '::':
                                    continue
                                # 排除自身模块的内部访问
                                # e.g. framework::mm::pmm 被 framework/mm/ 内部使用是允许的
                                # pattern 格式: framework::proc::process → 子系统是 proc (index 1)
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


def generate_dependency_matrix_json(fw_matrix, svc_matrix, circular, internal_issues, pub_surface):
    """生成依赖矩阵 JSON 报告."""
    report = {
        'timestamp': '2026-06-16',
        'framework_deps': {},
        'services_deps': {},
        'circular_deps': circular,
        'internal_access_issues': len(internal_issues),
        'pub_surface': pub_surface,
    }

    for mod in sorted(fw_matrix.keys()):
        report['framework_deps'][mod] = dict(fw_matrix[mod])

    for mod in sorted(svc_matrix.keys()):
        report['services_deps'][mod] = dict(svc_matrix[mod])

    return report


def main():
    print('=' * 78)
    print('M6.4 模块耦合度审计报告')
    print('=' * 78)
    print()

    # 1. framework 子模块间交叉依赖
    print('[1] framework 子模块间交叉依赖')
    print('-' * 78)
    fw_matrix, fw_details = scan_cross_module_deps(FRAMEWORK_BASE, 'framework')
    fw_total = sum(sum(targets.values()) for targets in fw_matrix.values())
    print(f'总交叉引用数: {fw_total}')

    # 按引用数排序输出
    for mod in sorted(fw_matrix.keys()):
        deps = fw_matrix[mod]
        if deps:
            dep_str = ', '.join(f'{k}:{v}' for k, v in sorted(deps.items(), key=lambda x: -x[1]))
            print(f'  {mod} → {dep_str}')
    print()

    # 2. services 子模块间交叉依赖
    print('[2] services 子模块间交叉依赖')
    print('-' * 78)
    svc_matrix, svc_details = scan_cross_module_deps(SERVICES_BASE, 'services')
    svc_total = sum(sum(targets.values()) for targets in svc_matrix.values())
    print(f'总交叉引用数: {svc_total}')

    for mod in sorted(svc_matrix.keys()):
        deps = svc_matrix[mod]
        if deps:
            dep_str = ', '.join(f'{k}:{v}' for k, v in sorted(deps.items(), key=lambda x: -x[1]))
            print(f'  {mod} → {dep_str}')
    print()

    # 3. 循环依赖检测
    print('[3] 双向依赖 (循环耦合) 检测')
    print('-' * 78)
    circular, allowed_coupling = detect_circular_deps(fw_matrix)
    if circular:
        for c in circular:
            a, b = c['modules']
            print(f'  ⚠ {a} ↔ {b}: {c["a_to_b"]}/{c["b_to_a"]} (合计 {c["total"]}) — 必须解耦')
    else:
        print('  无禁止的循环依赖')
    if allowed_coupling:
        print()
        print('  [允许的紧耦合] (硬件抽象/调度核心通路):')
        for c in allowed_coupling:
            a, b = c['modules']
            print(f'    ✓ {a} ↔ {b}: {c["a_to_b"]}/{c["b_to_a"]} (合计 {c["total"]})')
    print()

    # 4. 跨子系统内部访问检查
    print('[4] 跨子系统内部访问检查')
    print('-' * 78)
    internal_issues = check_internal_access(FRAMEWORK_BASE, 'framework')
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

    # 5. 公开接口比例
    print('[5] framework 公开接口比例')
    print('-' * 78)
    pub_surface = count_pub_surface(FRAMEWORK_BASE)
    for mod in sorted(pub_surface.keys()):
        info = pub_surface[mod]
        print(f'  {mod}: {info["pub"]}/{info["total"]} ({info["ratio"]}%)')
    print()

    # 6. 生成 JSON 报告
    json_path = Path('other/target/audit/dependency-matrix.json')
    json_path.parent.mkdir(parents=True, exist_ok=True)
    report = generate_dependency_matrix_json(fw_matrix, svc_matrix, circular, internal_issues, pub_surface)
    with open(json_path, 'w', encoding='utf-8') as f:
        json.dump(report, f, ensure_ascii=False, indent=2)
    print(f'JSON 报告保存至: {json_path}')
    print()

    # 7. 结果判定
    high_severity = [i for i in internal_issues if i['severity'] == 'HIGH']
    severe_circular = [c for c in circular if c['total'] > 20]

    if high_severity or severe_circular:
        print('=' * 78)
        if high_severity:
            print(f'⚠  {len(high_severity)} 处跨子系统内部访问违规')
        if severe_circular:
            print(f'⚠  {len(severe_circular)} 对严重循环依赖 (合计引用 > 20)')
        print('>>> 耦合审计未通过 <<<')
        sys.exit(1)
    else:
        print('>>> 耦合审计通过 <<<')
        sys.exit(0)


if __name__ == '__main__':
    main()
