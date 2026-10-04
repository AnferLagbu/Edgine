#!/usr/bin/env python3
"""
audit_tlb_receive_order.py — TLB 失效接收侧三段次序防线 (S-14)

检查 `privileged::smp::tlb_catch_up_local` 函数体内三条语句的**次序**:
    1. `tlb_gen_now()`      — 先读当代
    2. `tlb_flush_all()`    — 再全量失效本核 TLB
    3. `tlb_gen_set_self()` — 最后声明本核已追平该代

次序不可颠倒: 若先 flush 后读代, 则读到的是 flush **之后**新发布的代, 会把本次
flush 未覆盖的批次误判为"已追平" (假追平), 导致延迟释放的帧被提前归还 → UAF.
该缺陷是**结构性次序问题**, 在运行期不可观测 (需构造特定交错), 故以静态
fail-closed 审计确定性覆盖 (见 docs/plan/tlb-shootdown-epoch.md §5 注入 2).

fail-closed: 函数未找到 / 出现多次 / 任一语句缺失 / 次序错 ⇒ 一律判违规.
用法: python3 scripts/audit_tlb_receive_order.py
退出码: 0=通过, 1=有违规
"""
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TARGET = os.path.join(ROOT, "src/kernel/privileged/smp/mod.rs")

FN_SIG = "fn tlb_catch_up_local("
# 期望次序 (语句指纹 → 人类可读名)
EXPECTED = [
    ("tlb_gen_now", "先读当代 tlb_gen_now"),
    ("tlb_flush_all", "再全量失效 tlb_flush_all"),
    ("tlb_gen_set_self", "最后声明追平 tlb_gen_set_self"),
]


def extract_body(content, fn_sig):
    """定位函数体并返回函数体源码; 找不到 / 不唯一 ⇒ None."""
    if content.count(fn_sig) != 1:
        return None
    sig_pos = content.index(fn_sig)
    brace = content.find("{", sig_pos)
    if brace < 0:
        return None
    depth = 0
    for i in range(brace, len(content)):
        ch = content[i]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return content[brace + 1 : i]
    return None  # 括号不闭合


def main():
    violations = []
    if not os.path.exists(TARGET):
        print(f"  ⚠ {TARGET} 不存在 (fail-closed: 视为违规)")
        violations.append("目标文件不存在")

    body = None
    if not violations:
        with open(TARGET, "r", encoding="utf-8", errors="replace") as f:
            content = f.read()
        body = extract_body(content, FN_SIG)
        if body is None:
            print(f"  ⚠ 未找到或找到多个 `{FN_SIG}` (fail-closed: 视为违规)")
            violations.append("收尾函数 tlb_catch_up_local 缺失或不唯一")

    positions = []
    if body is not None:
        for token, human in EXPECTED:
            idx = body.find(token)
            if idx < 0:
                print(f"  ✗ 函数体内缺少 `{token}` ({human})")
                violations.append(f"缺语句 {token}")
            positions.append((idx, token, human))

    # 次序校验 (仅在三条齐全时才有意义)
    if body is not None and not violations:
        idxs = [p[0] for p in positions]
        if idxs != sorted(idxs):
            print("  ✗ 三段次序颠倒 (期望: 读代 → 失效 → 声明)")
            for idx, token, human in positions:
                print(f"      pos={idx:>4}  {token}  ({human})")
            violations.append("接收侧三段次序颠倒")

    print("=== audit_tlb_receive_order: 检查 tlb_catch_up_local 三段次序 ===")
    if violations:
        print(f"  ✗ 违规: {len(violations)}")
        for v in violations:
            print(f"    ✗ {v}")
        print("\n⚠ TLB 失效接收侧次序防线被破坏 (fail-closed)")
        sys.exit(1)
    print("  ✓ 次序正确: 读代 → 全量失效 → 声明追平")
    print("\n✓ audit_tlb_receive_order 通过")
    sys.exit(0)


if __name__ == "__main__":
    main()