#!/usr/bin/env python3
"""
audit_aarch64_kernel_fp_free.py — aarch64 内核「零 FP/SIMD」反汇编审计 (FP-06)

背景: 内核 aarch64 target 切到 `aarch64-unknown-none-softfloat` (含 -neon) 后,
EL1 不应再执行任何浮点/NEON 指令. 但「零 FP/SIMD」若只依赖编译 flag 就不可验证:
任何源码或汇编层面的回退 (新增 asm 块 / 手写 `.arch_extension fp` 等) 都没有
拦截点. 本脚本把该判据变为可 CI 强制的确定性检查
(见 docs/plan/aarch64-kernel-fp-free.md 的 FP-06).

检查对象: `other/build/kernel.bin` (aarch64 内核 ELF, 由 `make ARCH=aarch64` 链接产出).

判据: 白名单**外** FP/SIMD 指令计数 == 0.

白名单 `context_switch_asm` 符号范围 (上下文 FP 状态保存/恢复序列), 仅允许:
  - 16 × `stp qN, qN'` + 16 × `ldp qN, qN'` (共 32 条 q 寄存器搬运)
  - 4 × `mrs/msr fpcr|fpsr` (FPCR/FPSR 系统寄存器搬运)
该区间内的涉及 FP 的指令必须严格匹配上述形态之一, 且计数等于预期值 —
形态或计数不符同样判违规 (白名单收紧, 任何改动都必须显式更新本脚本).

FP/SIMD 识别方式: 操作数中出现 q/v/s/h/d/b 寄存器, 或 mrs/msr 访问 fpcr/fpsr.
匹配前先剥离 objdump 的 `<符号>` 注解与 `//` 行尾注释, 避免符号名/立即数误匹配.

fail-closed (不可检查 = 违规):
  产物缺失 / 非 AArch64 ELF / objdump 不可用或执行失败或输出为空 /
  `context_switch_asm` 符号缺失或不唯一 ⇒ 一律判违规 (exit 1).

前置条件: 本脚本读取当前 `other/build/kernel.bin`, 因此要求该产物是**最近一次 aarch64
  链接**的结果. 注意 `./ci/build.sh all` 最后链接的是 x86_64 (双架构共用该输出
  路径), 需先单独运行 `./ci/build.sh aarch64`.

用法: python3 scripts/audit_aarch64_kernel_fp_free.py
退出码: 0 = 通过, 1 = 违规 / 不可检查
"""

import os
import re
import shutil
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ARTIFACT = os.path.join(ROOT, "other", "build", "kernel.bin")

# 白名单符号 (context.rs 的 global_asm! 块内唯一手写 FP 序列)
WHITELIST_SYMBOL = "context_switch_asm"
# 白名单预期计数
EXPECTED_SV_INSNS = 32  # 16 × stp q + 16 × ldp q (覆盖 32 个 q 寄存器的存/取)
EXPECTED_SYSREG_MOVES = 4  # mrs/msr fpcr, fpsr
# 白名单内允许的指令形态: (助记符集合, 形态名)
ALLOWED_SV = {"stp", "ldp"}
ALLOWED_SYSREG = {"mrs", "msr"}

EM_AARCH64 = 0xB7
# objdump 候选 (优先 aarch64 专用 binutils, 其次 llvm, 最后 host binutils)
OBJDUMP_CANDIDATES = ("aarch64-linux-gnu-objdump", "llvm-objdump", "objdump")

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
SYMBOL_ANNOT_RE = re.compile(r"<[^>]*>")
# FP/SIMD 通用寄存器 qN / vN / sN / hN / dN / bN (b = SIMD 字节寄存器)
FP_REG_RE = re.compile(r"\b[qvshdb]\d{1,2}\b")
Q_REG_RE = re.compile(r"\bq\d{1,2}\b")
SYSREG_RE = re.compile(r"\b(?:fpcr|fpsr)\b")

SYM_HEADER_RE = re.compile(r"^([0-9a-f]+) <(.+)>:$")
# 指令行: 地址后有冒号; 允许前导空白 (不同 objdump 变体缩进不一致)
INSN_RE = re.compile(r"^\s*([0-9a-f]+):\s+(\S+)\s*(.*)$")


def check_elf_aarch64(path):
    """校验产物是 AArch64 64 位 ELF. 返回 (是否通过, 原因)."""
    try:
        with open(path, "rb") as f:
            header = f.read(20)
    except OSError as exc:
        return False, f"产物不可读 ({exc})"

    if len(header) < 20 or header[:4] != b"\x7fELF":
        return False, "不是 ELF 文件"
    if header[4] != 2:
        return False, "不是 64 位 ELF"
    e_machine = int.from_bytes(header[18:20], "little")
    if e_machine != EM_AARCH64:
        return (
            False,
            f"e_machine=0x{e_machine:x} 非 AArch64 — 当前产物可能是 x86_64 镜像, "
            f"请先运行 ./ci/build.sh aarch64",
        )
    return True, ""


def disassemble(path):
    """反汇编产物. 返回 (工具名, 反汇编文本, 失败原因)."""
    errors = []
    for tool in OBJDUMP_CANDIDATES:
        if shutil.which(tool) is None:
            continue
        try:
            proc = subprocess.run(
                [tool, "-d", "--no-show-raw-insn", path],
                capture_output=True,
                text=True,
                timeout=600,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            errors.append(f"{tool}: {exc}")
            continue
        text = ANSI_RE.sub("", proc.stdout)
        if proc.returncode == 0 and "Disassembly of section" in text:
            return tool, text, ""
        errors.append(f"{tool}: exit={proc.returncode}, 输出无 'Disassembly of section'")
    detail = "; ".join(errors) if errors else "无可用 objdump"
    return None, "", f"反汇编失败 ({detail})"


def parse_instructions(text):
    """解析反汇编文本. 返回 (指令记录列表, 出现的符号名列表).

    指令记录 = (地址, 所属符号名或 None, 助记符, 操作数).
    操作数已剥离 `<符号>` 注解与 `//` 行尾注释.
    """
    records = []
    symbols = []
    current_sym = None
    for raw in text.splitlines():
        line = raw.rstrip()
        m = SYM_HEADER_RE.match(line)
        if m:
            current_sym = m.group(2)
            symbols.append(current_sym)
            continue
        m = INSN_RE.match(line)
        if not m:
            continue
        operands = SYMBOL_ANNOT_RE.sub("", m.group(3).split("//")[0])
        mnemonic = m.group(2).split("//")[0].strip()
        records.append((m.group(1), current_sym, mnemonic, operands))
    return records, symbols


def classify(operands):
    """识别一行操作数中的 FP/SIMD 成分. 返回 (FP 寄存器列表, 是否访问 fpcr/fpsr)."""
    return FP_REG_RE.findall(operands), bool(SYSREG_RE.search(operands))


def main():
    print("=== audit_aarch64_kernel_fp_free: aarch64 内核零 FP/SIMD 审计 (FP-06) ===")

    # 1. 产物可用性 (fail-closed)
    if not os.path.exists(ARTIFACT):
        print(f"  ✗ 产物缺失: {ARTIFACT} (fail-closed: 视为违规)")
        print("     → 先运行 ./ci/build.sh aarch64 生成 aarch64 内核 ELF")
        sys.exit(1)
    elf_ok, elf_why = check_elf_aarch64(ARTIFACT)
    if not elf_ok:
        print(f"  ✗ 产物不可用: {elf_why} (fail-closed: 视为违规)")
        sys.exit(1)

    # 2. 反汇编 (fail-closed)
    tool, text, why = disassemble(ARTIFACT)
    if tool is None:
        print(f"  ✗ {why} (fail-closed: 视为违规)")
        print("     → 安装 aarch64 binutils (aarch64-linux-gnu-objdump) 或 llvm")
        sys.exit(1)

    # 3. 解析 (fail-closed)
    records, symbols = parse_instructions(text)
    if not records:
        print("  ✗ 反汇编结果为空 (fail-closed: 视为违规)")
        sys.exit(1)
    sym_count = symbols.count(WHITELIST_SYMBOL)
    if sym_count != 1:
        print(
            f"  ✗ 白名单符号 `{WHITELIST_SYMBOL}` 出现 {sym_count} 次 (期望 1) "
            f"(fail-closed: 视为违规)"
        )
        sys.exit(1)

    # 4. 分类扫描
    outside = []  # 白名单外 FP/SIMD
    zone_bad = []  # 白名单内非白名单形态
    zone_sv_insns = 0
    zone_sysreg_moves = 0
    zone_insns = 0

    for addr, sym, mnemonic, operands in records:
        fp_regs, has_sysreg = classify(operands)
        if sym == WHITELIST_SYMBOL:
            zone_insns += 1
            if not fp_regs and not has_sysreg:
                continue  # 通用寄存器/系统指令, 不涉及 FP 状态
            q_regs = Q_REG_RE.findall(operands)
            non_q = [r for r in fp_regs if not r.startswith("q")]
            if mnemonic in ALLOWED_SV and len(q_regs) == 2 and not non_q and not has_sysreg:
                zone_sv_insns += 1
                continue
            if mnemonic in ALLOWED_SYSREG and has_sysreg and not q_regs and not non_q:
                zone_sysreg_moves += 1
                continue
            zone_bad.append(f"{addr}  {mnemonic} {operands}".strip())
        elif fp_regs or has_sysreg:
            owner = sym if sym else "<未知符号>"
            outside.append(f"{addr}  [{owner}]  {mnemonic} {operands}".strip())

    # 5. 白名单计数校验 (形态收紧: 任何改动都必须显式更新本脚本)
    count_bad = []
    if zone_sv_insns != EXPECTED_SV_INSNS:
        count_bad.append(
            f"白名单内 stp/ldp q 搬运 = {zone_sv_insns}, 预期 {EXPECTED_SV_INSNS} (FP 上下文序列被改动)"
        )
    if zone_sysreg_moves != EXPECTED_SYSREG_MOVES:
        count_bad.append(
            f"白名单内 fpcr/fpsr 搬运 = {zone_sysreg_moves}, 预期 {EXPECTED_SYSREG_MOVES}"
        )

    violations = []
    if outside:
        violations.append(f"白名单外 FP/SIMD 指令 {len(outside)} 条")
    if zone_bad:
        violations.append(f"白名单内非白名单形态 {len(zone_bad)} 条")
    violations.extend(count_bad)

    # 6. 报告
    print(f"  产物: {os.path.relpath(ARTIFACT, ROOT)} (AArch64 ELF)")
    print(f"  反汇编工具: {tool}")
    print(f"  指令总数: {len(records)} (白名单区间 {zone_insns})")
    print(
        f"  白名单内: stp/ldp q 搬运 {zone_sv_insns}/{EXPECTED_SV_INSNS}, "
        f"fpcr/fpsr 搬运 {zone_sysreg_moves}/{EXPECTED_SYSREG_MOVES}"
    )
    print(f"  白名单外 FP/SIMD: {len(outside)}")

    if outside:
        print("  --- 白名单外 FP/SIMD 明细 (最多 20 条) ---")
        for line in outside[:20]:
            print(f"    ✗ {line}")
        if len(outside) > 20:
            print(f"    ... (共 {len(outside)} 条)")
    if zone_bad:
        print("  --- 白名单内异常形态明细 (最多 20 条) ---")
        for line in zone_bad[:20]:
            print(f"    ✗ {line}")
        if len(zone_bad) > 20:
            print(f"    ... (共 {len(zone_bad)} 条)")
    for line in count_bad:
        print(f"  ✗ {line}")

    if violations:
        print(f"\n⚠ aarch64 内核零 FP/SIMD 判据被破坏 (违规 {len(violations)} 类, fail-closed)")
        sys.exit(1)

    print("\n✓ audit_aarch64_kernel_fp_free 通过: 白名单外 FP/SIMD = 0")
    sys.exit(0)


if __name__ == "__main__":
    main()