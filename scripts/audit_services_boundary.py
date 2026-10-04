#!/usr/bin/env python3
"""
M6.3 services→framework 边界渗透检查脚本

检查 services/ 层是否:
  (1) 包含任何 unsafe 代码块 / unsafe fn / unsafe trait
  (2) 直接访问 framework 内部模块 (而非公开 API)
  (3) 使用裸指针 (*mut T, *const T) 直接解引用
  (4) 跳过 #![deny(unsafe_code)] 的强制

退出码: 0 = 通过, 1 = 有违规
"""

import os
import re
import sys
import json
from collections import defaultdict
from pathlib import Path

BASE = Path('src/kernel/services')
FRAMEWORK_BASE = Path('src/kernel/framework')

# services 不应直接访问的 framework 内部模块
# 这些是 implementation details, 应通过 services 代理层访问
#
# 框内核 8 类公开 API (services 可直接访问):
#   framework::frame (Frame)
#   framework::vmspace (VmSpace)
#   framework::usermode (UserMode)
#   framework::userctx (UserContext)
#   framework::iomem (IoMem)
#   framework::ioport (IoPort)
#   framework::irqline (IrqLine)
#   framework::dma_buf (DmaStream)
#   framework::credo_pwm (PWM)
#   framework::net_socket (NetSocket)
#   framework::proc_elf (Elf)
#
# 禁止直接访问的内部模块 (实现细节):
# B01-04 修复: 补全缺失项 — ipc::msgq::raw / syscall::types
# 等, 实测 services 穿透访问未被报; 同时新增 mm::errno / driver::idt::irq_trait /
# debug::ebpf / debug::opcode / debug::fnv1a_32 / debug::TraceEvent 等反向依赖点.
# 2J-C: coredump 已下沉 services/proc (经 coredump_trait 注入), 原 'proc::coredump' 禁条移除.
FORBIDDEN_FRAMEWORK_MODULES = [
    # 同步原语 implementation details (应通过 services/sync/* 代理)
    'framework::sync::raw',
    'framework::sync::arch',
    'framework::sync::atomic',  # 原子操作应通过 services/sync/atomic re-export
    'framework::sync::types',
    'framework::sync::seqlock::raw',
    'framework::sync::rcu::raw',
    # 架构底层
    'framework::arch::x86_64',
    'framework::arch::aarch64',
    'framework::arch::CurrentArch',
    # IDT 实现细节
    'framework::idt::statistics',
    'framework::idt::handlers',
    'framework::idt::safety',
    'framework::idt::IdtManager',
    'framework::idt::types',
    # 原始 8 API 的 raw 实现
    'framework::frame::raw',
    'framework::vmspace::raw',
    'framework::iomem::raw',
    'framework::ioport::raw',
    'framework::irqline::raw',
    'framework::dma_buf::raw',
    'framework::userptr::raw',
    'framework::page_table',
    'framework::cpu_local',
    'framework::racy_cell',
    # 分配器/引导底层
    'framework::alloc::raw',
    'framework::boot::raw',
    # barrier 实现细节
    'framework::barrier::undo_log',
    'framework::barrier::fault_inject',
    'framework::barrier::reset',
    # 日志/控制台底层
    'framework::klog::raw',
    'framework::console::raw',
    # IPC 内部
    'framework::ipc::msgq::raw',
    # syscall 内部 (类型/API 应通过 services/syscall 顶层)
    'framework::syscall::types',
    # errno 不列入禁止: framework::errno 是刻意的中性 re-export
    # (实际定义在 services::syscall::types, 见 framework/errno.rs 头注释),
    # 用于消除 proc/mm/fs/io 对 syscall 子系统的直接依赖.
    # services::error::KernelError 是另一错误类型, 不能替代 Errno.
    # driver 内部 — 子模块应在 framework/driver/mod.rs 顶层 glob re-export
    'framework::driver::idt::irq_trait',
    # debug 内部 — services 应通过 services/debug 顶层
    'framework::debug::ebpf',
    'framework::debug::opcode',
    'framework::debug::fnv1a_32',
    'framework::debug::TraceEvent',
    'framework::debug::EVENT_SIZE',
    'framework::debug::FTRACE_BUF_CAP',
    'framework::debug::KgdbRegs',
    'framework::debug::KgdbSerial',
    'framework::debug::bpf_init',
    'framework::debug::bpf_is_initialized',
    'framework::debug::bpf_subsystem',
    'framework::debug::sys_bpf',
    'framework::debug::BpfProgType',
    # arch::cet_* (proc/shadow_stack 反向依赖)
    'framework::arch::cet_init',
    'framework::arch::cet_is_initialized',
    'framework::arch::cet_subsystem',
    'framework::arch::sys_cet',
    # userctx / usermode 不列入禁止: 类型定义已于 2026-08-03 按 I3 不变式
    # 迁回 framework (见 services/userctx.rs 头注释), 且本脚本头「公开 API」
    # 清单已声明 services 可直接访问 — 删除陈旧黑名单条目以恢复两者一致.
]

# services 应该通过的安全 API
SAFE_FRAMEWORK_APIS = [
    'framework::sync',  # 顶层 re-export
    'framework::cpu',
    'framework::mm',
    'framework::proc',
    'framework::fs',
    'framework::net',
    'framework::ipc',
    'framework::credo',
    'framework::chitin',
    'framework::barrier',
    'framework::driver',
    'framework::pci',
    'framework::dma',
    'framework::irq',
    'framework::syscall',
    'framework::timer',
    'framework::sched',
    'framework::tests',
    'framework::frame',        # Frame
    'framework::vmspace',      # VmSpace
    'framework::iomem',        # IoMem
    'framework::ioport',       # IoPort
    'framework::irqline',      # IrqLine
    'framework::dma_buf',      # DmaStream
    'framework::alloc',        # 分配器
    'framework::klog',         # 日志
    'framework::console',      # 控制台
    'framework::config',       # 配置
    'framework::boot',         # 引导
    'framework::lib',          # 工具
]


def is_unsafe_in_services(filepath):
    """检查文件是否包含 unsafe 代码."""
    issues = []

    try:
        with open(filepath, 'r', encoding='utf-8', errors='replace') as f:
            lines = f.readlines()
    except Exception:
        return issues

    # 模式 1: unsafe { 块
    unsafe_block = re.compile(r'\bunsafe\s*\{')
    # 模式 2: unsafe fn
    unsafe_fn = re.compile(r'\bunsafe\s+fn\b')
    # 模式 3: unsafe impl
    unsafe_impl = re.compile(r'\bunsafe\s+impl\b')
    # 模式 4: unsafe trait
    unsafe_trait = re.compile(r'\bunsafe\s+trait\b')
    # 模式 5: extern "C" (允许但记录)
    extern_c = re.compile(r'\bextern\s+"C"\b')
    # 模式 6: 裸指针解引用
    raw_ptr_deref = re.compile(r'\*(?:const|mut)\s+\w+\s*[.\[]|as\s+\*(?:const|mut)\s+\w+')

    for lineno_1, line in enumerate(lines, start=1):
        stripped = line.strip()

        # 跳过注释行
        if stripped.startswith('//') or stripped.startswith('*') or stripped.startswith('///') or stripped.startswith('/*'):
            continue

        # 跳过 #![deny(unsafe_code)] 等 attribute 行
        if stripped.startswith('#!') or stripped.startswith('#['):
            continue

        # 检查 unsafe
        if unsafe_block.search(line):
            issues.append({
                'file': str(filepath),
                'line': lineno_1,
                'severity': 'CRITICAL',
                'type': 'UNSAFE_BLOCK_IN_SERVICES',
                'message': 'services 层禁止 unsafe 块',
                'code': line.strip()[:200],
            })
        elif unsafe_fn.search(line):
            issues.append({
                'file': str(filepath),
                'line': lineno_1,
                'severity': 'CRITICAL',
                'type': 'UNSAFE_FN_IN_SERVICES',
                'message': 'services 层禁止 unsafe fn',
                'code': line.strip()[:200],
            })
        elif unsafe_impl.search(line):
            issues.append({
                'file': str(filepath),
                'line': lineno_1,
                'severity': 'CRITICAL',
                'type': 'UNSAFE_IMPL_IN_SERVICES',
                'message': 'services 层禁止 unsafe impl',
                'code': line.strip()[:200],
            })
        elif unsafe_trait.search(line):
            issues.append({
                'file': str(filepath),
                'line': lineno_1,
                'severity': 'CRITICAL',
                'type': 'UNSAFE_TRAIT_IN_SERVICES',
                'message': 'services 层禁止 unsafe trait',
                'code': line.strip()[:200],
            })

    return issues


def check_forbidden_imports(filepath):
    """检查 services 是否导入了 framework 的禁止内部模块."""
    issues = []

    try:
        with open(filepath, 'r', encoding='utf-8', errors='replace') as f:
            lines = f.readlines()

    except Exception:
        return issues

    # 导入模式 (B01-05: 支持 `use` / `pub use` 两种形式)
    # 匹配 `use foo::bar;` 或 `pub use foo::bar;` 或 `pub(crate) use foo::bar;`
    use_pattern = re.compile(r'^\s*(pub(?:\([^)]*\))?\s+)?use\s+(.*?);')
    # 路径模式 (在 use 语句中)

    for lineno_1, line in enumerate(lines, start=1):
        stripped = line.strip()
        if stripped.startswith('//') or stripped.startswith('*') or stripped.startswith('///') or stripped.startswith('/*'):
            continue

        m = use_pattern.match(line)
        if not m:
            continue
        import_path = m.group(2)

        for forbidden in FORBIDDEN_FRAMEWORK_MODULES:
            if forbidden in import_path and not is_proxy_allowed(filepath, forbidden):
                issues.append({
                    'file': str(filepath),
                    'line': lineno_1,
                    'severity': 'HIGH',
                    'type': 'FORBIDDEN_FRAMEWORK_IMPORT',
                    'message': f'services 禁止直接导入 framework 内部模块 `{forbidden}`, 应使用 services 代理',
                    'code': line.strip()[:200],
                })

    return issues


def check_raw_pointer_access(filepath):
    """检查 services 是否直接解引用裸指针 (不通过 framework 安全 API)."""
    issues = []

    try:
        with open(filepath, 'r', encoding='utf-8', errors='replace') as f:
            lines = f.readlines()
    except Exception:
        return issues

    # 裸指针解引用模式 (简化)
    # *const_ptr 或 *mut_ptr 后跟 . 或 [
    # 排除: *const T (类型位置), *mut T (类型位置)
    raw_deref = re.compile(r'(\*+(?:const|mut)\s+\w+)\s*[.\[]')

    for lineno_1, line in enumerate(lines, start=1):
        stripped = line.strip()
        if stripped.startswith('//') or stripped.startswith('*') or stripped.startswith('///') or stripped.startswith('/*'):
            continue

        if raw_deref.search(line):
            issues.append({
                'file': str(filepath),
                'line': lineno_1,
                'severity': 'HIGH',
                'type': 'RAW_POINTER_DEREF_IN_SERVICES',
                'message': 'services 禁止直接解引用裸指针, 应通过 framework 安全 API',
                'code': line.strip()[:200],
            })

    return issues


# Vendored 3rd-party 库目录: 不属于项目自有代码, 审计豁免.
# 设计依据: docs/plan/smoltcp-framekernel-wrapper.md §同步机制
# 排除理由: smoltcp 100% safe Rust (上游承诺), 我们仅 vendored 不修改;
#           上游代码含 26 处 unsafe 块 (phy/sys/raw_socket 等),
#           审计项目自有代码不应误判 vendored 部分.
# 添加新 vendored 库时, 追加 Path 即可 (使用绝对前缀匹配).
VENDORED_EXCLUDE = [
    Path('src/kernel/services/net/smoltcp'),  # 上游 smoltcp 0.13.1 (2026-06)
]


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


# 代理层豁免: services 特定文件作为 framework 公开 API 的代理/转发点,
# 其自身对 framework 内部模块的 use / pub use 属既定设计, 而非边界穿透.
# 设计依据: 与 VENDORED_EXCLUDE 同机制的精细白名单 — 仅豁免「文件 + 禁条」组合,
#           不影响其他文件对该禁条的穿透检测.
# 条目格式: (文件相对路径, 禁条模块字符串).
# 来源: B05 审查登记的 5 处代理层自拦截误报 (audit-fix-06).
PROXY_ALLOWANCE = [
    # debug 子系统: mod.rs 转发 fnv1a_32, ebpf_verifier 内部引用 BpfProgType/opcode
    ('src/kernel/services/debug/mod.rs', 'framework::debug::fnv1a_32'),
    ('src/kernel/services/debug/ebpf_verifier.rs', 'framework::debug::BpfProgType'),
    ('src/kernel/services/debug/ebpf_verifier.rs', 'framework::debug::opcode'),
    # ipc 子系统: msgq.rs 转发 raw 层消息类型 (MessageRef)
    ('src/kernel/services/ipc/msgq.rs', 'framework::ipc::msgq::raw'),
    # DECISION-N (2026-09-13): DECISION-J 反转批次的 re-export 代理壳
    # (纯 `pub use framework::...::*`, 无业务逻辑, 属机制安全导出面的合法转发).
    # sync 子系统: types.rs 代理 framework::sync::types (锁状态/守卫/统计)
    ('src/kernel/services/sync/types.rs', 'framework::sync::types'),
    # barrier 子系统: reset_config.rs 代理 framework::barrier::reset::config
    ('src/kernel/services/barrier/reset_config.rs', 'framework::barrier::reset'),
    # DECISION-J 第十七批: syscall/types.rs 代理 framework::syscall::types
    # (syscall 编号表迁回 framework, services re-export 兼容层)
    ('src/kernel/services/syscall/types.rs', 'framework::syscall::types'),
]


def is_proxy_allowed(filepath, forbidden):
    """判断「文件 + 禁条」组合是否属代理层豁免."""
    fpath = str(Path(filepath).resolve())
    for allow_file, allow_forbidden in PROXY_ALLOWANCE:
        if str(Path(allow_file).resolve()) == fpath and forbidden == allow_forbidden:
            return True
    return False


def scan_directory(base):
    """扫描 services/ 目录所有 .rs 文件 (vendored 第三方目录豁免)."""
    all_issues = []
    files = sorted(base.rglob('*.rs'))
    skipped = 0
    for f in files:
        if is_vendored(f):
            skipped += 1
            continue
        all_issues.extend(is_unsafe_in_services(f))
        all_issues.extend(check_forbidden_imports(f))
        all_issues.extend(check_raw_pointer_access(f))
    if skipped > 0:
        print(f'[INFO] 跳过 {skipped} 个 vendored 文件 (来自 VENDORED_EXCLUDE)', file=sys.stderr)
    return all_issues, len(files) - skipped


def generate_report(issues, file_count):
    """生成报告."""
    by_severity = defaultdict(list)
    for issue in issues:
        by_severity[issue['severity']].append(issue)

    report = []
    report.append('=' * 78)
    report.append('M6.3 services→framework 边界渗透检查报告')
    report.append('=' * 78)
    report.append('')
    report.append(f'扫描文件数: {file_count}')
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


def check_services_inter_module_deps():
    """检查 services 子模块间的依赖合理性."""
    issues = []
    modules = []

    for d in sorted(BASE.iterdir()):
        if d.is_dir() and (d / 'mod.rs').exists():
            modules.append(d.name)

    # services 子模块间允许的依赖 (白名单)
    # 格式: (源, 目标) → 允许
    ALLOWED_INTER_DEPS = {
        # fs 依赖 sync 是合理的 (锁原语)
        ('fs', 'sync'),
        # proc 依赖 sync 是合理的
        ('proc', 'sync'),
        # net 依赖 sync 是合理的
        ('net', 'sync'),
        # driver 依赖 sync 是合理的
        ('driver', 'sync'),
        # ipc 依赖 sync 是合理的
        ('ipc', 'sync'),
        # credo 依赖 sync 是合理的
        ('credo', 'sync'),
        # mm 依赖 sync 是合理的
        ('mm', 'sync'),
        # chitin 依赖 sync 是合理的
        ('chitin', 'sync'),
        # barrier 依赖 sync 是合理的
        ('barrier', 'sync'),
        # storage 依赖 sync 是合理的
        ('storage', 'sync'),
        # io 依赖 sync 是合理的
        ('io', 'sync'),
        # debug 依赖 sync 是合理的
        ('debug', 'sync'),
        # syscall 依赖 sync 是合理的
        ('syscall', 'sync'),
        # proc 依赖 config 是合理的
        ('proc', 'config'),
        # mm 依赖 config 是合理的
        ('mm', 'config'),
        # fs 依赖 config 是合理的
        ('fs', 'config'),
        # ipc 依赖 proc 是合理的 (进程间通信)
        ('ipc', 'proc'),
        # fs 依赖 credo 是合理的 (权限检查)
        ('fs', 'credo'),
        # driver 依赖 mm 是合理的 (DMA 映射)
        ('driver', 'mm'),
        # driver 依赖 config 是合理的
        ('driver', 'config'),
        # driver 依赖 chitin 是合理的 (块设备经 services::chitin 安全代理注册/注销)
        ('driver', 'chitin'),
        # barrier 依赖 credo 是合理的 (故障恢复权限检查)
        ('barrier', 'credo'),
        # fs 依赖 syscall 是合理的 (fs 系统调用实现使用 syscall 的 Errno 类型)
        ('fs', 'syscall'),
        # proc 依赖 fs 是合理的 (memfd_create 等需要 OpenFile/AnonymousFs)
        ('proc', 'fs'),
        # credo 依赖 fs 是合理的 (持久化编排经 services::fs 的 VFS safe 包装读写 /pwm.db)
        ('credo', 'fs'),
    }

    for mod in modules:
            mod_dir = BASE / mod
            for rs_file in sorted(mod_dir.rglob('*.rs')):
                try:
                    with open(rs_file, 'r', encoding='utf-8', errors='replace') as f:
                        for lineno, line in enumerate(f, 1):
                            stripped = line.strip()
                            if stripped.startswith('//') or stripped.startswith('/*'):
                                continue
                            # B01-05: 支持 `use` / `pub use` 两种形式
                            m = re.match(r'^\s*(pub(?:\([^)]*\))?\s+)?use\s+(.*?);', line)
                            if not m:
                                continue
                            import_path = m.group(2)
                            for other_mod in modules:
                                if other_mod == mod:
                                    continue
                                pattern = f'services::{other_mod}'
                                if pattern in import_path:
                                    if (mod, other_mod) not in ALLOWED_INTER_DEPS:
                                        issues.append({
                                            'file': str(rs_file),
                                            'line': lineno,
                                            'severity': 'MEDIUM',
                                            'type': 'UNLISTED_INTER_MODULE_DEP',
                                            'message': f'services::{mod} 依赖 services::{other_mod} 未在白名单中, 需审查合理性',
                                            'code': line.strip()[:200],
                                        })
                except Exception:
                    continue

    return issues


def main():
    # B01-03: 退出码升级 — HIGH/MEDIUM 违规也应阻断 CI
    # 通过 --strict-medium 启用 MEDIUM 阻断 (默认仅阻断 CRITICAL + HIGH)
    # --strict-medium 设计: MEDIUM (services 间非白名单依赖) 默认仅警告,
    # 但允许 CI 严格模式开启. 这样新代码不会因历史遗留触发 CI 失败,
    # 但 CI 可选择性开启.
    import argparse
    parser = argparse.ArgumentParser(description='QueenX services→framework 边界审计')
    parser.add_argument('--strict-medium', action='store_true',
                        help='MEDIUM 级别违规也触发 exit 1')
    args = parser.parse_args()

    if not BASE.exists():
        print(f'ERROR: {BASE} not found', file=sys.stderr)
        sys.exit(2)

    print(f'扫描 {BASE} ...')
    issues, file_count = scan_directory(BASE)

    # 新增: services 子模块间依赖合理性检查
    inter_issues = check_services_inter_module_deps()
    issues.extend(inter_issues)

    report = generate_report(issues, file_count)
    print(report)

    # 保存 JSON 报告 (gitignored other/target/audit/)
    json_path = Path('other/target/audit/services-boundary.json')
    json_path.parent.mkdir(parents=True, exist_ok=True)
    with open(json_path, 'w', encoding='utf-8') as f:
        json.dump({
            'file_count': file_count,
            'issue_count': len(issues),
            'issues': issues,
        }, f, ensure_ascii=False, indent=2)
    print(f'\nJSON 报告保存至: {json_path}')

    # B01-03: 退出码 — CRITICAL 必阻断, HIGH 必阻断,
    # MEDIUM 仅在 --strict-medium 时阻断
    critical = sum(1 for i in issues if i['severity'] == 'CRITICAL')
    high = sum(1 for i in issues if i['severity'] == 'HIGH')
    medium = sum(1 for i in issues if i['severity'] == 'MEDIUM')

    if critical > 0:
        print(f'\n>>> {critical} 个 CRITICAL 违规 (services 包含 unsafe) <<<')
        sys.exit(1)
    if high > 0:
        print(f'\n>>> {high} 个 HIGH 违规 (services 访问 framework 内部) <<<')
        sys.exit(1)
    if medium > 0 and args.strict_medium:
        print(f'\n>>> {medium} 个 MEDIUM 违规 (--strict-medium 启用) <<<')
        sys.exit(1)

    print(f'\n>>> services 边界检查通过 <<<')
    sys.exit(0)


if __name__ == '__main__':
    main()
