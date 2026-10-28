#!/bin/bash
# Edgine 内核代码审计脚本
# 工具栈: cargo check (双架构) + clippy (pedantic, 含 feature 维) + rustfmt
#         + audit_*.py 审计腿群 + Lockbud 死锁/数据竞争扫描 + QEMU 真实启动 (full 档)
# 用法: ./ci/audit.sh [quick|full]   (缺省档 = quick)
#
# - quick: 步骤 0–0.5n 审计腿群 + 1 双架构 check + 2/2b clippy (2 双架构) + 2c rustfmt, 到 2c 后即退出
# - full:  quick + 2d 内核测试 release 维 compile-only + 2e 存量审计腿群 (P-49) + 3 Lockbud + 4 QEMU
#
# P-56 甲: 原「3 SAFETY 注释覆盖率统计」与「6 模块级 SAFETY 不变式」两条统计腿已删除 ——
# 前者与 0.5 组的 audit_safety_coverage.py 同名不同尺 (实测 131% 的「覆盖率」), 后者统计
# 注释文案出现频次且在删除前每次都稳定打黄字 (实测仅 1 个文件命中、阈值 >=5). 两者都不
# 承重, 只训练读者忽略黄字; 剩余步骤由此重编号 (Lockbud 4→3, QEMU 7→4).
#
# P-48 甲订正两处自述失真: 原工具栈行宣称含一个已于 2026-06-26 退役的 UB 检测工具 (退役
# 记录见下方退役注记, 那里是单源), 且原档位行把 quick 称作「CI 默认」—— 验收判据由 AGENTS
# §2.3 第 3 条定为 **full 全绿** (quick 不含 2d / Lockbud / QEMU, 不构成验收依据), 而远程
# 仅 Gitee、无流水线, 「CI 默认」本无对应物. 到位腿数**不写进本块**, 由 0.5n 腿单点断言,
# 免得这里成为第二个会漂移的计数源; 本块的出现过的工具名一律以实际腿为准.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

MODE="${1:-quick}"

# 构建模式显式化 (方案 B): 裸机构建显式注入 build-std (src/rust/.cargo/config.toml
# 已删全局 [unstable] build-std). 避免 cwd 隐式加载 — src/rust 目录内 host-target
# 构建会触发 E0152 双 alloc 冲突 (DECISION-021 同族). 详见主计划文档 §10.
BUILD_STD_CFG=(--config 'unstable.build-std=["core","compiler_builtins","alloc"]' --config 'unstable.build-std-features=["compiler-builtins-mem"]')

step() { echo -e "\n${YELLOW}━━━ $1 ━━━${NC}"; }
ok()   { echo -e "${GREEN}✓ $1${NC}"; }
err()  { echo -e "${RED}✗ $1${NC}"; exit 1; }

# ── P-46 甲: 审计腿存在性包装的 fail-closed 出口 ─────────────────
# 本脚本用 `if command -v python3 && [ -f 脚本 ]` 包住 13 处审计腿, 且原先**全部没有
# else**; step 头又写在包装内部 ⇒ 条件为假时该腿连一行日志都不留, 退出码仍是 0
# (负控制实测: 把 audit_repr_c.py 改名 ⇒ quick 里 repr_c 出现 0 次且 EXIT=0).
# 即「audit 全绿」与「少跑了几条腿」在日志上不可区分. 现每处补 else 走本函数,
# 按用户裁定「缺 python3 与缺脚本一律硬失败」, 只在文案上区分成因.
# 与 0.5k2 (P-45 coupling 腿) 同形态, 后者是本修法在仓内已有的先例.
audit_leg_missing() {
    local leg_name="$1" leg_path="$2"
    if ! command -v python3 >/dev/null 2>&1; then
        err "审计腿未执行: ${leg_name} —— 环境无 python3 (fail-closed, 不接受假绿)"
    fi
    err "审计腿未执行: ${leg_name} —— ${leg_path} 不存在 (fail-closed, 不接受假绿)"
}

# ── P-46 乙: 到位清单单点断言 ──────────────────────────────────
# 甲把「静默消失」变成硬失败, 但 13 个维护点上新增第 14 条腿仍可能忘写 else.
# 故再加一道: 每条腿调起前 mark_leg 登记, 0.5n 处单点比对「期望清单 == 实际到位」.
# 期望清单是手写字面 ⇒ 由 host-tests 结构锁 `test_audit_sh_audit_legs_are_fail_closed`
# 与本清单互校 (AGENTS.md §2.2 的强制项必须逐项出现在此), 防它自身漂移.
# 19 项 = 18 条被存在性包装的腿 (含两个 for 循环各展开 3 条) + 0.5k2 的 coupling.
AUDIT_REQUIRED_LEGS="tcb_ratio invariants unsafe block_registration once_cell c_naming comment_language tlb_receive_order aarch64_kernel_fp_free repr_c volatile_access static_mut functions_boundary safety_coverage deadlock_matrix coupling orphaned_init unwired_pub_fn dead_code_comment"
# ── P-49: 仅 full 档执行的存量审计腿清单 ──────────────────────
# 六条脚本此前在仓内**零调用者** (P-49 测绘), §2.2 与 plan 里点名它们却没有任何本地执法
# 路径. 本轮接入 step 2e —— 位置在 quick 早退分支**之后** ⇒ 不进 AUDIT_REQUIRED_LEGS
# (0.5n 在两档都断言, 放进去会让 quick 恒红), 改由 2e 段的 full 档到位断言单点比对.
# 清单形态 = 「腿名:scripts/ 下相对路径」成对, 路径不套 audit_* 命名约定 (废弃命名腿那条实为
# scan_obsolete_naming.py, 自测那条在 tests/ 子目录), 由 host-tests 结构锁逐项核验真实存在.
# 第 7 条 implicit_deps_selftest 是 P-49 甲・自测锁的执行点 —— 一个只有人手工跑的自测
# 与"零调用者审计脚本"是同一个病灶, 接线时若不给它调用点, 本轮修法当场失效.
# 第 8 条 audit_selftest 是 P-50 ③ 的执行点, 同型但更隐蔽: 它的四个用例此前在自测文件里
# "手动模拟"检测算法 (检测器退化时自测照样全绿). 真调化后若仍只留在从不触发的
# .github/workflows/ci-lint.yml 里, 等于把 P-49 的病灶换个位置复现.
# 第 9 条 missing_docs_debt 是 P-59 裁定 ① 的执行点: F8 的存在性由 rustc `missing_docs`
# 作准 (AGENTS §5 原写的 `clippy::missing_docs_in_crate_items` 是不存在的 lint 名, 故编译器
# 执法长期为零), 本腿取该口径读数与存量债册双向对账, 只拦新增不豁免存量 —— 每批补写完
# 就把该模块转 `#![deny(missing_docs)]` 硬执法 (清零工程 P-66).
AUDIT_REQUIRED_LEGS_FULL="implicit_deps:audit_implicit_deps.py implicit_deps_selftest:tests/audit_implicit_deps_selftest.py audit_selftest:tests/audit_selftest.py feature_semantics:audit_feature_semantics.py reverse_deps:audit_reverse_deps.py public_api_docs:audit_public_api_docs.py missing_docs_debt:audit_missing_docs_debt.py smoltcp_purity:audit_smoltcp_purity.py obsolete_naming:scan_obsolete_naming.py"
AUDIT_RUN_LEGS_FULL=""
mark_leg_full() { AUDIT_RUN_LEGS_FULL="${AUDIT_RUN_LEGS_FULL} $1"; }
AUDIT_RUN_LEGS=""
mark_leg() { AUDIT_RUN_LEGS="${AUDIT_RUN_LEGS} $1"; }

# ── P-69 乙: 腿清单与 AGENTS §2.2 反向对账 ──────────────────────
# 病灶 (P-59 负控制 C5a 实测): 两处到位断言的期望值与实测值同源于清单变量本身,
# 把整条腿从清单摘掉时 runtime 仍打「到位」全绿 (9/9 → 8/8), 只有 host-tests 会红.
# 本段把「文档说强制」作为外部期望: §2.2 里以反引号点名过的每个 audit_*/scan_* 脚本,
# 其腿名必须出现在两档清单之一 —— 文档有而清单无, 即摘腿 / 新增脚本未接线, 硬失败.
# 反向 (清单腿未在文档里点名) 不判: §2.2 自述「本表为常用核心门禁项, 非穷举」,
# 如 quick 档的 unsafe 腿映射 tools/audit_unsafe.py, 全量清单脚注才有它.
# 若 python3 缺失也走 else 分支硬失败, 与 13 条 python 审计腿的 fail-closed 同语义.
if python3 - "$PROJECT_ROOT/AGENTS.md" "$AUDIT_REQUIRED_LEGS" "$AUDIT_REQUIRED_LEGS_FULL" <<'PY'
import re
import sys
from pathlib import Path

doc_path, quick_line, full_line = sys.argv[1:4]
doc = Path(doc_path).read_text(encoding="utf-8")
sec = doc[doc.index("### 2.2"):doc.index("### 2.3")]

# §2.2 内反引号点名的审计脚本 (可带 .py): 含表格「脚本」列与两条脚注的腿清单.
declared = set(re.findall(r"`((?:audit|scan)_[a-z_]+)(?:\.py)?`", sec))
wired = {s.split(":", 1)[0] for s in (quick_line.split() + full_line.split())}


def leg_names(ident):
    # 腿名约定: 多数去掉 audit_ / scan_ 前缀即为腿名 (scan_obsolete_naming 对应
    # obsolete_naming), 少数保留全名 (audit_selftest 登记的就是 audit_selftest),
    # 故同时给出两个候选与清单求交.
    out = {ident}
    for prefix in ("audit_", "scan_"):
        if ident.startswith(prefix):
            out.add(ident[len(prefix):])
    return out


missing = sorted(i for i in declared if not leg_names(i) & wired)
if missing:
    for ident in missing:
        print(f"AGENTS §2.2 点名 {ident}, 但两档清单都没有对应腿", file=sys.stderr)
    sys.exit(1)
print(f"✓ {len(declared)} 条文档点名腿全部在册")
PY
then
    ok "P-69 腿清单与 AGENTS §2.2 反向对账: 通过 (文档点名腿无摘除)"
else
    err "P-69: §2.2 点名的审计腿不在两档清单里 —— 摘腿或未接线 (fail-closed, 不接受假绿)"
fi

# ── P-47 甲: 「本腿未执行」台账 ────────────────────────────────
# 外部工具型腿 (Lockbud) 在工具缺失时根本不产生读数, 原先这种状态与「工具发现了
# 问题」共用同一句不阻断黄字 ⇒ 「audit 全绿」里混进了「这条腿从未跑过」.
# 现拆两态并单列一张 skip 台账, 在 quick / full 两个出口都播报未执行腿数,
# 让「绿」不再能被读成「所有腿都给出过验证」.
# 只登记「腿已进入但因环境缺失而未执行」的腿; 档位本身不执行的 full-only 腿不在此列
# (那属 P-48 的 quick-vs-full 口径, 由 P-57 甲的 report_mode_boundary 在日志里播报).
AUDIT_SKIPPED_COUNT=0
AUDIT_SKIPPED_LEGS=""
skip_leg() {
    local leg_name="$1" leg_reason="$2"
    AUDIT_SKIPPED_COUNT=$(( AUDIT_SKIPPED_COUNT + 1 ))
    AUDIT_SKIPPED_LEGS="${AUDIT_SKIPPED_LEGS}  - ${leg_name}: ${leg_reason}"$'\n'
    echo -e "${YELLOW}⚠ ${leg_name} 本腿未执行: ${leg_reason}${NC}"
}

report_unexecuted_legs() {
    if [ "$AUDIT_SKIPPED_COUNT" -eq 0 ]; then
        ok "本轮因环境缺失未执行腿数: 0"
    else
        echo -e "${YELLOW}━━━ 本轮因环境缺失未执行腿数: ${AUDIT_SKIPPED_COUNT} —— 以下腿零读数, 不可当作已验证 ━━━${NC}"
        printf '%s' "$AUDIT_SKIPPED_LEGS"
    fi
}

# ── P-57 甲: 档位覆盖边界进日志 ────────────────────────────────
# P-48 把 quick 的覆盖边界写进了头部自述与 AGENTS §2.3, 但日志收尾只播报 skip 台账;
# 旧收尾的「未执行腿数: 0」按 P-47 语义只代表「没有因环境缺失而中断的腿」, 不代表
# full-only 腿跑过 —— 只看日志的人会得到反向结论. 两个出口各播报一次档位与边界,
# 与 skip 台账各说各事; 结构锁 `test_p57_log_self_reports_mode_and_environment_ledger`
# 锁住 quick 行必须点名 full 与不含的步骤.
# 条件里用 ${MODE} 而非 $MODE: 三条既有结构锁按「MODE 等于 quick」的字面定位早退
# 分支, 在本函数里复写同一字面会让它们把函数定义误认成早退位置.
report_mode_boundary() {
    if [ "${MODE}" = "quick" ]; then
        echo -e "${BLUE}→ 档位: quick —— 覆盖步骤 0–2c; 不含 2d/2e/Lockbud/QEMU; 不构成 §2.3 第 3 条验收依据, 验收请用 bash ci/audit.sh full${NC}"
    else
        echo -e "${BLUE}→ 档位: full —— 覆盖步骤 0–4 全部腿 (含 2d/2e/Lockbud/QEMU)${NC}"
    fi
}

# ── 0. TCB 安全边界门禁 (M2 里程碑硬约束) ────────────────────
# 服务层 (functions/) 不允许任何 unsafe 代码。这是框内核架构的核心契约。
# check_tcb.sh 是 fail-fast 门禁: 一旦发现立即 exit 1, 整个 audit 终止。
# 历史教训: 2026-06-03 审计发现 check_tcb.sh 正则有 bug (变长 lookbehind),
# 导致 functions/ 出现 8 处 unsafe 仍报 PASS。已修复, 见 tools/check_tcb.sh 顶部注释。
step "0/6 TCB 安全边界门禁 (functions/ 零 unsafe)"
if [ -x "$PROJECT_ROOT/tools/check_tcb.sh" ]; then
    if "$PROJECT_ROOT/tools/check_tcb.sh"; then
        ok "TCB 边界: functions/ 零 unsafe, privileged/ 收敛"
    else
        err "TCB 边界被破坏! 见上方 FAIL 输出"
    fi
else
    err "tools/check_tcb.sh 不存在或不可执行"
fi

# ── 0.5a 产物目录卫生门禁 ────────────────────────────────────────
# 临时/构建产物统一收敛到 other/ 下 (整个 other/ 被 .gitignore 覆盖).
# fail-closed: 若已跟踪文件中混入被忽略产物 (git ls-files -i -c --exclude-standard
# 非空), 说明产物被误提交 (历史上 .arch/target 等曾漏出), 阻断 CI 并提示移除.
step "0.5a/6 产物目录卫生 (tracked ∩ ignored = ∅)"
STALE_TRACKED=$(git ls-files -i -c --exclude-standard)
if [ -z "$STALE_TRACKED" ]; then
    ok "产物目录卫生: 无被跟踪的忽略产物 (全部收敛到 other/)"
else
    echo "$STALE_TRACKED" | head -20
    err "有被跟踪的忽略产物混入仓库 (见上), 需从 git 移除 (git rm --cached <path>)"
fi

# ── 0.5b TCB 度量报告 (E10) ──────────────────────────────────────
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_tcb_ratio.py" ]; then
    mark_leg "tcb_ratio"
    step "0.5b/6 TCB 度量报告 (E10)"
    "$PROJECT_ROOT/scripts/audit_tcb_ratio.py" 2>&1 | tail -20
else
    audit_leg_missing "tcb_ratio" "$PROJECT_ROOT/scripts/audit_tcb_ratio.py"
fi

# ── 0.5c 6 安全不变式审计 (E9) ────────────────────────────────────
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_invariants.py" ]; then
    mark_leg "invariants"
    step "0.5c/6 6 安全不变式审计 (E9)"
    if "$PROJECT_ROOT/scripts/audit_invariants.py" 2>&1 | tail -12; then
        ok "6 安全不变式: 全部满足"
    else
        err "6 安全不变式: 有违反! 见上方输出"
    fi
else
    audit_leg_missing "invariants" "$PROJECT_ROOT/scripts/audit_invariants.py"
fi

# privileged 全量 SAFETY 注释覆盖审计
# quick 模式也会跑 (核心 fail-fast 门禁, 不需要 Lockbud 等 full 档重工具)
# B01-27 修复: audit_unsafe.py 窗口逻辑缺陷 (属性闭合行/多行属性误报) 已修复,
# 原 127 处"缺失"中 108 处实为工具误报 (已有 SAFETY 注释但未识别),
# 真实缺失 20 处已全部补齐, 基线归零 (0 容忍, 任何缺失即 CI 失败).
EXPECTED_MAX_SAFETY_MISSING=0
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/tools/audit_unsafe.py" ]; then
    mark_leg "unsafe"
    step "0.5/6 Privileged SAFETY 注释全量审计 (B01-27 工具修复, 基线 ≤${EXPECTED_MAX_SAFETY_MISSING})"
    AUDIT_RESULT=$("$PROJECT_ROOT/tools/audit_unsafe.py" --summary 2>&1 || true)
    echo "$AUDIT_RESULT" | tail -25
    MISSING=$(echo "$AUDIT_RESULT" | grep -E "缺 SAFETY:" | head -1 | awk '{print $NF}')
    # B01-16 修复: 当脚本输出为空或缺 "缺 SAFETY:" 行时, 视为异常 (fail-closed).
    # 原脚本静默通过 (无 ok 也无 err) 隐藏门禁失效.
    if [ -z "$AUDIT_RESULT" ]; then
        err "audit_unsafe.py 输出为空 (脚本异常或 privileged/ 为空)"
    elif [ -n "$MISSING" ] && [ "$MISSING" -le "$EXPECTED_MAX_SAFETY_MISSING" ]; then
        ok "privileged SAFETY 覆盖 (缺漏 $MISSING ≤ ${EXPECTED_MAX_SAFETY_MISSING} 基线)"
    elif [ -n "$MISSING" ] && [ "$MISSING" -gt "$EXPECTED_MAX_SAFETY_MISSING" ]; then
        err "privileged SAFETY 缺漏 $MISSING > 基线 ${EXPECTED_MAX_SAFETY_MISSING}, 需审查"
    else
        # 输出非空但未匹配 "缺 SAFETY:" 行 — 视为脚本异常
        err "audit_unsafe.py 输出格式异常 (未找到 缺 SAFETY: 行)"
    fi
else
    audit_leg_missing "unsafe" "$PROJECT_ROOT/tools/audit_unsafe.py"
fi

# I-43: 块设备抽象统一性 audit — 防驱动绕过 proto_block 桥接
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_block_registration.py" ]; then
    mark_leg "block_registration"
    step "0.5d/6 块设备单一桥接入口 (I-43)"
    if "$PROJECT_ROOT/scripts/audit_block_registration.py" 2>&1 | tail -10; then
        ok "I-43: 块设备驱动统一通过 proto_block::register_block_device"
    else
        err "I-43: 有块设备驱动绕过 proto_block 桥接! 见上方输出"
    fi
else
    audit_leg_missing "block_registration" "$PROJECT_ROOT/scripts/audit_block_registration.py"
fi

# I-16: functions 层 OnceCell 抽象统一性 audit — 防 functions 绕过 OnceCell 用 spin::Once
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_once_cell.py" ]; then
    mark_leg "once_cell"
    step "0.5e/6 functions OnceCell 单一抽象 (I-16)"
    if "$PROJECT_ROOT/scripts/audit_once_cell.py" 2>&1 | tail -8; then
        ok "I-16: functions 统一通过 sync::once::OnceCell"
    else
        err "I-16: 有 functions 模块绕过 OnceCell 抽象用 spin::Once! 见上方输出"
    fi
else
    audit_leg_missing "once_cell" "$PROJECT_ROOT/scripts/audit_once_cell.py"
fi

# I-07: C 风格命名残留 audit — 防 C 类型后缀/C 函数名混入 Rust 代码
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_c_naming.py" ]; then
    mark_leg "c_naming"
    step "0.5f/6 C 风格命名残留 (I-07)"
    if "$PROJECT_ROOT/scripts/audit_c_naming.py" 2>&1 | tail -8; then
        ok "I-07: 0 C 风格类型后缀, kmalloc/kfree 仅限 extern \"C\""
    else
        err "I-07: 有 C 风格命名残留! 见上方输出"
    fi
else
    audit_leg_missing "c_naming" "$PROJECT_ROOT/scripts/audit_c_naming.py"
fi

# TD-22: 注释语言一致性 audit — 硬阈值门禁 (违规 > 0 即 CI 失败)
# 已完成全部清理: 1983→0 (2026-06-15). 新代码引入英文段落注释将被阻断.
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_comment_language.py" ]; then
    mark_leg "comment_language"
    step "0.5g/6 注释语言一致性 (TD-22, 硬阈值)"
    AUDIT_OUT=$("$PROJECT_ROOT/scripts/audit_comment_language.py" 2>&1 || true)
    if echo "$AUDIT_OUT" | grep -q "PASSED"; then
        ok "TD-22: 注释中文化 100% 完成 (0 违规)"
    else
        VIOLATION_COUNT=$(echo "$AUDIT_OUT" | grep -E "FAILED:" | head -1 | awk '{print $5}')
        FILE_COUNT=$(echo "$AUDIT_OUT" | grep -E "FAILED:" | head -1 | awk '{print $NF}' | tr -d '()')
        err "TD-22: 英文段落注释残留 ${VIOLATION_COUNT} 处, 涉及 ${FILE_COUNT} 文件 (硬阈值, 阻断 CI)"
    fi
else
    audit_leg_missing "comment_language" "$PROJECT_ROOT/scripts/audit_comment_language.py"
fi

# S-14: TLB 失效接收侧三段次序 audit — 防"先 flush 后读代"的假追平
# (结构性次序问题, 运行期不可观测; 以静态 fail-closed 审计确定性覆盖,
#  见 docs/plan/tlb-shootdown-epoch.md §5 注入 2)
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_tlb_receive_order.py" ]; then
    mark_leg "tlb_receive_order"
    step "0.5h/6 TLB 失效接收侧三段次序 (S-14)"
    if "$PROJECT_ROOT/scripts/audit_tlb_receive_order.py" 2>&1 | tail -8; then
        ok "S-14: 接收侧次序正确 (读代 → 全量失效 → 声明追平)"
    else
        err "S-14: 接收侧三段次序被破坏 (假追平风险)! 见上方输出"
    fi
else
    audit_leg_missing "tlb_receive_order" "$PROJECT_ROOT/scripts/audit_tlb_receive_order.py"
fi

# FP-06: aarch64 内核「零 FP/SIMD」反汇编白名单审计 — 防 EL1 出现浮点/NEON
# (软浮点 target 只保证编译期不生成 FP 指令, 无法拦截源码/汇编层的回退;
#  以反汇编白名单确定性覆盖, 见 docs/plan/aarch64-kernel-fp-free.md FP-06)
# 产物: other/build/aarch64/kernel.bin —— 内核产物已按架构分目录 (P-10), 与 x86_64
# 不共用输出路径, 因此本步骤不再要求"先跑 build.sh aarch64": ./ci/build.sh all 现
# 已两架构各自链接, 两种产物同时存在. 旧共用路径曾使本审计在 build all 之后必读到
# x86_64 镜像而 fail-closed (假阳性, 非真实 FP 回退).
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_aarch64_kernel_fp_free.py" ]; then
    mark_leg "aarch64_kernel_fp_free"
    step "0.5i/6 aarch64 内核零 FP/SIMD (FP-06)"
    FP_AUDIT_OUT=$("$PROJECT_ROOT/scripts/audit_aarch64_kernel_fp_free.py" 2>&1) && FP_AUDIT_RC=0 || FP_AUDIT_RC=$?
    echo "$FP_AUDIT_OUT" | tail -20
    if [ "$FP_AUDIT_RC" -eq 0 ]; then
        ok "FP-06: aarch64 内核白名单外 FP/SIMD = 0"
    else
        err "FP-06: aarch64 内核存在白名单外 FP/SIMD 或不可检查 (fail-closed)! 见上方输出"
    fi
else
    audit_leg_missing "aarch64_kernel_fp_free" "$PROJECT_ROOT/scripts/audit_aarch64_kernel_fp_free.py"
fi

# ── 0.5j 内存安全静态防线 audit (repr(C) / volatile / static mut) ──
# 对应 I2/I5/I6 不变式与 LTO 字段错位防线; 源码级 fail-closed 确定性覆盖.
# 接线说明: 此前三脚本仅本地可跑、未接入 CI; 现纳入 quick 门禁 (见 AGENTS.md §2.2).
# 依赖 set -o pipefail 使 `cmd | tail` 管道正确反映脚本退出码 (B01-16 同源).
for MEMSAFETY_AUDIT in repr_c volatile_access static_mut; do
    if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_${MEMSAFETY_AUDIT}.py" ]; then
        mark_leg "$MEMSAFETY_AUDIT"
        step "0.5j/6 内存安全防线 audit (${MEMSAFETY_AUDIT})"
        if "$PROJECT_ROOT/scripts/audit_${MEMSAFETY_AUDIT}.py" 2>&1 | tail -8; then
            ok "${MEMSAFETY_AUDIT}: 内存安全防线通过"
        else
            err "${MEMSAFETY_AUDIT}: 内存安全防线被破坏! 见上方输出"
        fi
    else
        audit_leg_missing "$MEMSAFETY_AUDIT" "$PROJECT_ROOT/scripts/audit_${MEMSAFETY_AUDIT}.py"
    fi
done

# ── 0.5k 硬规则 F1+F2 / F4 + 锁顺序专项核心审计 ────────────────
# 这三项在 AGENTS.md §2.2 里被列为「强制」门禁, 但此前只接入了 GitHub
# workflow (ci-lint.yml), 本地权威入口本脚本反而没跑 —— 本地绿而 CI 红 (或反之)
# 的根源. 现在归位: 本地一键门禁 = CI 同一判据, 且都是 fail-closed
# (三脚本均在违规时 exit 1 / exit 2).
# 调起方式 = `python3 脚本`: 三脚本在仓库里是 mode 644 (不可执行), 而 workflow
# 也正是 `python3 scripts/...` 调起 —— 直接执行会因权限不够而假失败,
# 依赖可执行位也不能保证跨 checkout 稳定 (与 workflow 保持同一调起形式).
for HARD_RULE_AUDIT in functions_boundary safety_coverage deadlock_matrix; do
    if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_${HARD_RULE_AUDIT}.py" ]; then
        mark_leg "$HARD_RULE_AUDIT"
        step "0.5k/6 硬规则核心 audit (${HARD_RULE_AUDIT})"
        if python3 "$PROJECT_ROOT/scripts/audit_${HARD_RULE_AUDIT}.py" 2>&1 | tail -10; then
            ok "${HARD_RULE_AUDIT}: 硬规则门禁通过"
        else
            err "${HARD_RULE_AUDIT}: 硬规则被破坏 (F1/F2/F4 或锁顺序专项)! 见上方输出"
        fi
    else
        audit_leg_missing "$HARD_RULE_AUDIT" "$PROJECT_ROOT/scripts/audit_${HARD_RULE_AUDIT}.py"
    fi
done

# ── 0.5k2 硬规则 F3 模块耦合审计 (P-45 接线) ────────────────────
# AGENTS.md §5 F3 的「检查方式」一栏写的就是 audit_coupling.py, §2.2 还把它的 CI
# 门禁列标为「强制」——但此前本脚本里它没有任何调用点, 仓内唯一一处在从不触发的
# GitHub workflow (P-37 结案事实: 本仓只有 Gitee remote, 无流水线配置). 即 F3 长期
# 处于「文档强制、无执法路径」状态. 本段把执法路径补上, 不改动脚本判据本身.
# 接线点刻意选在 quick 早退分支 **之前** ⇒ quick 与 full 两个档位都覆盖
# (脚本自测耗时 ≈0.8s, 不构成档位成本负担; 行号随本脚本改动位移, 定位请 grep 段名).
# 退出码取自脚本本身: 用 `OUT=$(cmd) && RC=0 || RC=$?` 形态 (与 0.5i 的 FP-06 腿同
# 写法), 不走管道 ⇒ 不依赖 pipefail, 也不会把退出码取自 tail/grep.
# 刻意**不加** `if command -v python3 && [ -f … ]` 存在性包装 (P-46 的 fail-open 族):
# 脚本缺失或 python3 不可用时本腿必须硬失败, 而不是静默跳过后仍 EXIT=0.
# 判据 (P-54 收口, 取代旧的「合计引用 > 20 才阻断」): (a) INTERNAL_PATTERNS 清单内
# 的跨子系统内部访问 (severity 恒 HIGH ⇒ 阻断); (b) privileged + functions 两层的禁止
# 循环依赖与机器派生基线 scripts/audit_coupling.baseline.json 双向对账 —— 新环、任一
# 方向计数增加、或快照过期都 exit 1. 存量 5 对环 (privileged 4 + functions 1) 冻结在
# 侧车里, 那是快照不是放行阈值.
# 仍不覆盖 (P-65 立案): 依赖边只取单行 `use ...;` —— 跨行 use / `pub use` / 非 use 行
# 的限定调用都不成边; 三者全扩会新增 27 对 privileged 禁止环 (4 → 31), 属红墙.
step "0.5k2/6 硬规则 F3 模块耦合审计 (coupling)"
mark_leg "coupling"
COUPLING_OUT=$(python3 "$PROJECT_ROOT/scripts/audit_coupling.py" 2>&1) && COUPLING_RC=0 || COUPLING_RC=$?
echo "$COUPLING_OUT" | grep -E '^ *⚠|^>>>' || true
if [ "$COUPLING_RC" -eq 0 ]; then
    ok "coupling: F3 门禁通过 (两层循环依赖与基线一致, 完整矩阵见脚本 [1]/[2] 段输出)"
else
    echo "$COUPLING_OUT" | tail -30
    err "coupling: F3 被破坏 (跨子系统内部访问, 或循环依赖与基线不符)! 见上方输出"
fi

# ── 0.5l P-8 初始化编排孤儿入口审计 ────────────────────────
# O1 门禁: `#[unsafe(no_mangle)]` + 初始化语义 + 全仓 0 调用者 + 体内嵌套 ≥2 项 init
# ⇒「孤儿编排入口」. scheduler_init 曾以该形态挂住 10 项子系统 init, 生产环境从未
# 执行; `audit_unwired_pub_fn.py` 显式豁免 no_mangle (FFI 边界), 本项是那个盲区.
# 先跑 --self-test: 检测逻辑退化会让门禁静默全绿, 脚本自身必须先自证.
# O2 (退化 FFI 包裹, 存量 11 项) 仅报告不阻塞 —— 登记见
# docs/plan/net-e2e-tcp-echo-flakiness.md §6 P-12.
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_orphaned_init.py" ]; then
    mark_leg "orphaned_init"
    step "0.5l/6 孤儿初始化编排入口 audit (P-8) — 自检"
    if python3 "$PROJECT_ROOT/scripts/audit_orphaned_init.py" --self-test 2>&1 | tail -6; then
        ok "orphaned_init: 检测器自检通过"
    else
        err "orphaned_init: 检测器退化, 门禁不可信 (见上方夹具用例)"
    fi
    step "0.5l/6 孤儿初始化编排入口 audit (P-8) — 全仓扫描"
    if python3 "$PROJECT_ROOT/scripts/audit_orphaned_init.py" 2>&1 | tail -20; then
        ok "orphaned_init: 无孤儿编排入口 (P-8 不变式成立)"
    else
        err "orphaned_init: 发现孤儿编排入口! 子系统初始化从未执行"
    fi
else
    audit_leg_missing "orphaned_init" "$PROJECT_ROOT/scripts/audit_orphaned_init.py"
fi

# ── 0.5m P-13 零引用 pub fn / 未接线 syscall 编号审计 ──────────
# R2 (CRITICAL): `syscall/types.rs` 声明的 SYS_*/EG_* 编号未被 dispatch 分发 ⇒
# 用户态拿到编号却得 ENOSYS, 属接口欺骗, 无条件阻断. R1 零引用 pub fn 由用户裁定
# **常开 `--strict` 阻断**: 脚本把「未分类 R1」并入 warn 聚合 (`warn =
# r1_unclassified + R3_unused_module`), 故 `--strict` 对 HIGH>0 确实退 1. 门禁
# 强度与 §5 F9「零死代码无豁免」同构 —— 本轮 `apic::get_deadline` (本轮自身新增的
# 零消费者 API) 是靠下一轮审计才发现的, 常开后这类自伤在提交前即被拦.
# 前置事实: 存量 R1 已逐项甄别完毕 (未分类 HIGH 45 → 0, 其中 25 项经独立 oracle
# 对跑证实为工具假阳性, 19 项预存已分组登记) ⇒ 开启零摩擦. 台账见
# docs/plan/syscall-followup.md B-6, 过程记录见 net-e2e-tcp-echo-flakiness.md §6 P-13.
# 性能前置: 本机无 ripgrep 时走脚本内降级路径; 该路径原为 O(待查名 × 全仓字节)
# (实测 13 分钟不收敛), 现已改为单遍切词建索引 → O(全仓字节 + 待查名).
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_unwired_pub_fn.py" ]; then
    mark_leg "unwired_pub_fn"
    step "0.5m/6 未接线公共 API audit (P-13, --strict)"
    if python3 "$PROJECT_ROOT/scripts/audit_unwired_pub_fn.py" --strict 2>&1 | tail -24; then
        ok "audit_unwired: 无未接线 syscall 编号, 且无未分类零引用 pub fn"
    else
        err "audit_unwired: R2 未分发 syscall 编号 (CRITICAL), 或未分类零引用 pub fn / 零引用 pub mod (--strict 阻断)"
    fi
else
    audit_leg_missing "unwired_pub_fn" "$PROJECT_ROOT/scripts/audit_unwired_pub_fn.py"
fi

# ── 0.5m2 P-53 F9 死代码注释审计 (全仓) ─────────────────────────
# F9 原文的检查方式只有「Rust 编译器 dead_code lint」, 而 rustc 只覆盖被编译到的路径
# (feature 关闭 / cfg 掩码下的区域无人可见); 仓内原有兜底仅是 4 份 host-tests 里按
# 手抄文件清单跑的 grep. 本腿补上全仓执法路径:
#   · dead_code / unused / unused_* 抑制 = 硬零, 不进基线 (唯一两处真实存量已消除);
#   · clippy 抑制族同属 F9 射程 (用户 2026-10-23 裁定), 存量 1661 处冻结在机器派生
#     侧车 audit_dead_code_comment.baseline.json (逻辑属性行口径; 单行口径的 292 会
#     漏计仓内既有的折行 `#[expect(clippy::…, reason = "…")]` 形态), 清零归 P-64;
#     新增/减少均双向阻断.
# 判定按**属性位置**锚定, 注释与字符串字面量里的同形文本不误报 (自测 12 例覆盖).
if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/audit_dead_code_comment.py" ]; then
    mark_leg "dead_code_comment"
    step "0.5m2/6 F9 死代码注释 audit (P-53)"
    set +e
    DC_OUT=$(python3 "$PROJECT_ROOT/scripts/audit_dead_code_comment.py" --self-test 2>&1)
    DC_RC=$?
    set -e
    printf '%s\n' "$DC_OUT" | tail -3
    if [ "$DC_RC" -ne 0 ]; then
        err "dead_code_comment: 检测器自检失败 (exit=$DC_RC), 门禁不可信"
    fi
    set +e
    DC_OUT=$(python3 "$PROJECT_ROOT/scripts/audit_dead_code_comment.py" 2>&1)
    DC_RC=$?
    set -e
    printf '%s\n' "$DC_OUT" | tail -24
    if [ "$DC_RC" -eq 0 ]; then
        ok "dead_code_comment: 死代码抑制 0 处, clippy 抑制与冻结基线逐项一致"
    else
        err "dead_code_comment: 死代码抑制存在, 或 clippy 抑制与基线不符 (exit=$DC_RC)"
    fi
else
    audit_leg_missing "dead_code_comment" "$PROJECT_ROOT/scripts/audit_dead_code_comment.py"
fi

# ── 0.5n 审计腿到位性单点断言 (P-46 乙) ─────────────────────────
# 甲把「腿静默消失」变成该腿位置的硬失败; 本段是第二道: 期望清单与实际到位清单
# 逐项比对. 位置在 quick 早退分支之前 ⇒ 两档都判. 期望清单是手写字面, 由
# host-tests 的 test_audit_sh_audit_legs_are_fail_closed 与 AGENTS.md §2.2 强制
# 名单互校 (删 else / 删 mark_leg / 删本断言, 三者任一都转红).
step "0.5n/6 审计腿到位性断言 (P-46)"
AUDIT_MISSING_LEGS=""
for want_leg in $AUDIT_REQUIRED_LEGS; do
    case " $AUDIT_RUN_LEGS " in
        *" $want_leg "*) ;;
        *) AUDIT_MISSING_LEGS="${AUDIT_MISSING_LEGS} $want_leg" ;;
    esac
done
AUDIT_LEG_TOTAL=$(set -- $AUDIT_REQUIRED_LEGS; echo $#)
if [ -z "$AUDIT_MISSING_LEGS" ]; then
    ok "审计腿到位: ${AUDIT_LEG_TOTAL}/${AUDIT_LEG_TOTAL} (无静默跳过)"
else
    err "审计腿未到位:${AUDIT_MISSING_LEGS} —— 共 $(echo $AUDIT_MISSING_LEGS | wc -w) 条 (fail-closed, 不接受假绿)"
fi

# ── 1. 双架构 check ─────────────────────────────────────────────
step "1/6 双架构 cargo check (x86_64 + aarch64)"
pushd src/rust > /dev/null
unset RUSTC_WRAPPER
for target in x86_64-unknown-none aarch64-unknown-none-softfloat; do
    echo -e "${BLUE}[audit] target=${target}${NC}"
    # 方案 D: kernel 独立 crate, 裸机 check 指向 kernel manifest (edgine 壳仅 host).
    if cargo check --manifest-path ../kernel/Cargo.toml --target "${target}" --target-dir ../../other/target "${BUILD_STD_CFG[@]}" 2>&1 | tail -3; then
        ok "${target}: check passed"
    else
        err "${target}: check FAILED"
    fi
done
popd > /dev/null

# ── 2. Clippy pedantic (x86_64 + aarch64) ──────────────────────
step "2/6 Clippy pedantic (x86_64 + aarch64, lib only)"
pushd src/rust > /dev/null
unset RUSTC_WRAPPER
# B01-28 修复: clippy 命令与 CI (ci-x86.yml clippy-pedantic job) 对齐:
#   + --release: dev 模式会编译诊断代码 (nvme similar_names 假阳性等), 与 CI 一致用 release
#   + 无 cast_* 豁免: P-64 清零工程逐族撤除 (7a `cast_possible_wrap` /
#     7b `cast_sign_loss` / 7c `cast_possible_truncation` / 7d `cast_precision_loss`),
#     源码零 `as` 回绕 / 丢符号 / 窄化 / 精度损失写法, 数值换算一律走
#     `privileged::lib::intcast` 或 `functions::intcast` 的位级助手
#   + 去掉 -W clippy::cargo: 依赖多版本 bitflags 会误报, CI 未启用
#   + -D warnings 保留更强门禁 (任何 warning 阻断, 见 B01-16)
# B01-16 修复: 加 -D warnings 让任何 warning 阻断 CI, 失败走 err 而非仅警告.
# 原代码 `if cmd | tail; then ok; else warn; fi` 中 `tail` 退出 0 总是成功,
# 即使 cargo clippy 失败也被掩盖 (P0-05 类问题).
# P-52 甲: 双架构循环 —— x86_64 单架构时 src/kernel 里 147 处
# `target_arch = "aarch64"` 门控代码完全不经 clippy. 远端 ci-x86.yml 本有一条
# aarch64 clippy 步骤, 但本仓仅 Gitee、workflow 从不执行 ⇒ 不能代替本地门禁.
# 成本实测: aarch64 维 RC=0 (0 诊断), 增量 CPU ≈14s; build-std 由步骤 1 的
# 双架构 check 先行构建, 本维只重编 kernel crate. 与步骤 1 同形 (两档都跑).
for target in x86_64-unknown-none aarch64-unknown-none-softfloat; do
    # 捕获式退出码 (B01-25 惯用法): `if cmd | tail` 在 pipefail 下失败即走 else,
    # 于是 "失败 (exit=…)" 分支不可达、真因被「执行异常」吞掉 (P-55 同族).
    set +e
    CLIPPY_OUT=$(cargo clippy --manifest-path ../kernel/Cargo.toml --workspace --release --lib --bins --examples --target "${target}" --target-dir ../../other/target \
        "${BUILD_STD_CFG[@]}" \
        -- -D warnings -D clippy::pedantic 2>&1)
    CLIPPY_RC=$?
    set -e
    printf '%s\n' "$CLIPPY_OUT" | tail -10
    if [ "$CLIPPY_RC" -eq 0 ]; then
        ok "clippy pedantic (${target}): passed"
    else
        err "clippy pedantic (${target}) 失败 (exit=$CLIPPY_RC, 见上方输出)"
    fi
done
popd > /dev/null

# ── 2b. Clippy feature 维 (kernel_test + host-test, J-01/G-02/G-18) ──
# J-01 (2026-09-08): E-03 同源双编译后 feature 门控代码 lint 纳入门槛.
# host target 运行 (避开裸机 build.rs 产物检查), 与 ci-x86.yml clippy-pedantic
# job 对齐. 两维清理后零 unfulfilled 作为门槛 (G-02 合并).
step "2b/6 Clippy feature 维 (kernel_test + host-test, host target)"
for FEATURE in kernel_test host-test; do
    # 方案 D: feature 维 clippy 指向 kernel manifest (host target, 门控代码 lint)
    # 捕获式退出码 (B01-25 惯用法, P-55 ②): `if cmd | tail` 在 pipefail 下失败即走
    # else, 于是 "失败 (exit=…)" 分支不可达、真因被「执行异常」吞掉.
    set +e
    FEATURE_CLIPPY_OUT=$(cargo clippy --manifest-path "$PROJECT_ROOT/src/kernel/Cargo.toml" --workspace --features "$FEATURE" --lib \
        --target-dir "$PROJECT_ROOT/other/target" \
        -- -D warnings -D clippy::pedantic 2>&1)
    FEATURE_CLIPPY_RC=$?
    set -e
    printf '%s\n' "$FEATURE_CLIPPY_OUT" | tail -10
    if [ "$FEATURE_CLIPPY_RC" -eq 0 ]; then
        ok "clippy ${FEATURE} 维: passed"
    else
        err "clippy ${FEATURE} 维失败 (exit=$FEATURE_CLIPPY_RC, 见上方输出)"
    fi
done

# ── 2c. rustfmt 风格门禁 (全仓四 crate) ────────────────────────
# 长期修复 (2026-10-03): 此前本地 audit 不跑 fmt, 导致 kernel 39 hunks 漂移长期
# 不可见 (仅 CI 暴露且已被忽略). 现与 ci-x86.yml clippy-pedantic job 对齐, 覆盖
# kernel / host-tests / edgine 壳; rustfmt.toml 与 rust-toolchain.toml 均在仓库根.
# 补入 src/user (2026-10-09): 用户态七 crate 此前不受任何 fmt 门禁, 漂移静默积累.
step "2c/6 rustfmt 风格门禁 (kernel + host-tests + edgine 壳 + user 态)"
# P-71: src/kernel 已是 workspace, 用 `-p` 显式点名 kernel + kernel-privileged +
# kernel-functions 三个成员.
# 不用 `--all`: 该开关会连带格式化 path 依赖 (vendored smoltcp 虽已 exclude 仍被
# `--all` 的「本地路径依赖」语义波及), 破坏「smoltcp 锁定, 仅升级时动」的口径.
if cargo fmt --manifest-path src/kernel/Cargo.toml -p kernel -p kernel-privileged -p kernel-functions -- --check; then
    ok "fmt: src/kernel/Cargo.toml (kernel + kernel-privileged + kernel-functions) 通过"
else
    err "fmt: src/kernel/Cargo.toml 存在格式漂移 — 运行 cargo fmt --manifest-path src/kernel/Cargo.toml -p kernel -p kernel-privileged -p kernel-functions 修复"
fi
for FMT_MANIFEST in host-tests/Cargo.toml src/rust/Cargo.toml src/user/Cargo.toml; do
    if cargo fmt --manifest-path "$FMT_MANIFEST" -- --check; then
        ok "fmt: ${FMT_MANIFEST} 通过"
    else
        err "fmt: ${FMT_MANIFEST} 存在格式漂移 — 运行 cargo fmt --manifest-path ${FMT_MANIFEST} 修复"
    fi
done

if [ "$MODE" = "quick" ]; then
    report_unexecuted_legs
    report_mode_boundary
    echo -e "\n${GREEN}━━━ audit (quick) 完成 ━━━${NC}"
    exit 0
fi

# ── 2d. 内核 release 维 compile-only (P-44 测试维 + P-51 feature 维) ──
# 门槛 6 的判据是 debug 配方 (Makefile 的 test-kernel-host, 不带 --release). release 测试维
# 自 P-43 起「可编译、可运行、零警告」, 但**此前不由任何门禁编译** —— 于是谁把测试侧 cfg
# 改回与被测代码掩码不同形, 六门槛照样全绿: 修好的病灶没有回归锁.
# P-51 同族缺口: lockdep 的 cfg 全形如 any(debug_assertions, feature = "lockdep"), 常规
# release 构建从不点亮它; e1000-real-hw 的真实硬件分支也只在该 feature 下编译.
# 判据与实现分别见 scripts/check_kernel_test_release_dim.sh 与
# scripts/check_kernel_feature_release_dims.sh 顶部 (两条: 编译退出码 0 + 本 crate 零告警;
# 只编译不跑用例 ⇒ 不产出覆盖读数, 不得读成覆盖达成).
# 置于 quick 早退分支之后 = 仅 full 执行 (增量成本: kernel crate 按 feature 组合重编).
step "2d/6 内核 release 维 compile-only (P-44 测试维 + P-51 feature 维)"
# 捕获式退出码 (B01-25 惯用法, P-55 ②): `if cmd | tail` 在 pipefail 下失败即走 else,
# 「失败 (exit=…)」分支不可达、真因被「执行异常」吞掉.
set +e
RELEASE_DIM_OUT=$("$PROJECT_ROOT/scripts/check_kernel_test_release_dim.sh" 2>&1)
RELEASE_DIM_RC=$?
set -e
printf '%s\n' "$RELEASE_DIM_OUT" | tail -40
if [ "$RELEASE_DIM_RC" -eq 0 ]; then
    ok "内核测试 release 维: 编译通过 + 本 crate 零告警"
else
    err "内核测试 release 维检查失败 (exit=$RELEASE_DIM_RC, 见 scripts/check_kernel_test_release_dim.sh 输出)"
fi

set +e
FEATURE_DIM_OUT=$("$PROJECT_ROOT/scripts/check_kernel_feature_release_dims.sh" 2>&1)
FEATURE_DIM_RC=$?
set -e
printf '%s\n' "$FEATURE_DIM_OUT" | tail -40
if [ "$FEATURE_DIM_RC" -eq 0 ]; then
    ok "内核 feature release 维 (lockdep + e1000-real-hw): 编译通过 + 本 crate 零告警"
else
    err "内核 feature release 维检查失败 (exit=$FEATURE_DIM_RC, 见 scripts/check_kernel_feature_release_dims.sh 输出)"
fi

# ── 2e. 存量审计腿接入 (P-49) ─────────────────────────────────
# 用户裁定「丙・六脚本全接入 full」. P-49 时六条腿分两种形态; P-58 收口后八条腿全部阻断,
# P-59 追加第 9 条 (missing_docs_debt) 亦阻断 —— 现九条腿无一有豁免分支:
#   P-49 的报告腿 obsolete_naming 当时用的是旧扫描脚本 (P-58 后更名为 `scan_obsolete_naming.py`) —— 默认无条件
#   退 0, 只有 --actionable-only 才判退出码, 而那口径 53 处里 46 处是扫描器自身源文本、
#   另有大量 plan/report 历史正文, 读数不收敛 ⇒ 只能作报告腿. P-58 裁定「校正 + 升阻断」:
#   改为受控文本全量扫描 (git ls-files, 含无扩展名的 Makefile 与 .asm)、docs/plan 与
#   docs/report 历史面 KEEP、自命中豁免、二值退出且读取失败 fail-closed; 检测器自检是
#   每次扫描的前置断言 (自检失败退 2, 不给「零违规」). 该腿由此无豁免分支: 非零即 err.
# 存在性包装沿用 P-46 甲形态 (缺 python3 / 缺脚本一律 audit_leg_missing 硬失败), 包装位
# 点数与 audit_leg_missing 调用数由 host-tests 结构锁互校, 新增裸包装即红.
# 每条腿在 `cd "$PROJECT_ROOT"` 的子 shell 里调起, 不借外层 cwd: 本组腿的扫描根与 JSON
# 落点都按脚本自身口径解析 (audit_feature_semantics.py:37-38 用相对路径, :521 写
# other/target/audit/), 而本文件在 2e 前后有 pushd/popd (步骤 1/2 与 Lockbud) —— 一旦
# 有人在别处补一对 pushd 而忘记 popd, 借 cwd 的腿会读错根 (实测 cwd=/ 时该腿 RC=2).
# 置于 quick 早退分支之后 = 仅 full 执行. 耗时逐腿实测 (本轮九腿连跑合计 37.0s; 单跑
# implicit_deps 1.8 / implicit_deps_selftest 6.6 / audit_selftest 0.1 / feature_semantics 0.1
# / reverse_deps 0.4 / public_api_docs 7.8 / missing_docs_debt 15.8 / smoltcp_purity 3.7 /
# obsolete_naming 1.0; 实测时本机 load avg ~17, 绝对值随负载浮动, 只有量级可比 —— P-49 当年
# 空闲机的八腿合计口径是 ~14.5s).
# 只有 smoltcp_purity 会浅克隆上游 + ls-remote ⇒ 受网络影响; 该腿的联网检查在脚本内降级为
# WARN (audit_smoltcp_purity.py:230-273), 断网只改耗时不改判据, 不会因环境转红.
# public_api_docs 在 P-59 裁定 ② 后每次裸调都要逐 pub fn 判 impl 归属 (默认面硬零 + impl 面
# 对账同源于一次扫描): 两面若靠 --strict 分流, 2e 的无参调起永远只判得到默认面, 533 处存量
# 便没有执法点.
# missing_docs_debt 是九腿里唯一真编译的腿 (其余各腿都是纯文本扫描, 无改动时不会更贵), 故它
# 也是唯一受前序改动影响的腿: 刚动过 src/kernel 时实测 15.8s, 无改动热跑实测 7.7s, 首跑冷启
# 30.2s. 它跑 `cargo clippy … -- -Dmissing_docs` 取编译器读数, 需工具链在位 (缺 cargo ⇒ RC=2
# 转红, 与本文件步骤 2d 同语义), 并用独立 target 目录 other/target/audit-missing-docs
# (实测 143M) 而非主目录: 尾旗标参与 cargo 的 unit fingerprint, 借主目录会把门槛 1/2 的全部
# 产物判脏 (实测主目录门槛形 clippy 被搅脏后重编 kernel lib), 隔离目录只付自己的冷启动.
step "2e/6 存量审计腿接入 (P-49)"
for P49_SPEC in $AUDIT_REQUIRED_LEGS_FULL; do
    P49_LEG="${P49_SPEC%%:*}"
    P49_SCRIPT="${P49_SPEC##*:}"
    mark_leg_full "$P49_LEG"
    if command -v python3 >/dev/null 2>&1 && [ -f "$PROJECT_ROOT/scripts/$P49_SCRIPT" ]; then
        if (cd "$PROJECT_ROOT" && python3 "scripts/$P49_SCRIPT") 2>&1 | tail -25; then
            ok "审计腿 ${P49_LEG}: 通过"
        else
            err "审计腿 ${P49_LEG} 失败 (scripts/${P49_SCRIPT} 非零退出)"
        fi
    else
        audit_leg_missing "$P49_LEG" "$PROJECT_ROOT/scripts/$P49_SCRIPT"
    fi
done

# 到位断言 (full 档): 清单点名的腿必须逐条 mark 过, 否则该腿是被摘掉的而非"跑输了".
AUDIT_MISSING_LEGS_FULL=""
for want_leg_full in $AUDIT_REQUIRED_LEGS_FULL; do
    case " $AUDIT_RUN_LEGS_FULL " in
        *" ${want_leg_full%%:*} "*) ;;
        *) AUDIT_MISSING_LEGS_FULL="${AUDIT_MISSING_LEGS_FULL} ${want_leg_full%%:*}" ;;
    esac
done
AUDIT_LEG_FULL_TOTAL=$(set -- $AUDIT_REQUIRED_LEGS_FULL; echo $#)
if [ -z "$AUDIT_MISSING_LEGS_FULL" ]; then
    ok "P-49 存量审计腿到位: ${AUDIT_LEG_FULL_TOTAL}/${AUDIT_LEG_FULL_TOTAL} (无静默跳过)"
else
    err "P-49 存量审计腿未到位:${AUDIT_MISSING_LEGS_FULL} (fail-closed, 不接受假绿)"
fi

# ── 3. Lockbud 死锁/数据竞争扫描 ────────────────────────────────
# P-47 甲「状态分离」: 原写法 `cargo lockbud … || LOCKBUD_RESULT=$?` 的非零结果只打成
# 一句「可能发现潜在问题或工具未安装」的黄字且不改退出码. 本机实测 `command -v
# cargo-lockbud` 为空 ⇒ full 每次都走该合并分支, §2.2 宣称的死锁/数据竞争扫描从未
# 产生过一次读数, 而日志看起来像执行过. 现拆三态:
#   工具缺失 → skip_leg 记入「因环境缺失未执行腿数」(不改退出码, 与 P-55 乙 对 QEMU 的裁定同语义);
#   真跑非零 → err 阻断, 与 §2.2 把该项列为「强制」一致.
# P-56 丙 (2026-10-23 按国内镜像真装一次) 的三条实测订正:
#   ① 装机名两个: `cargo install --path .` 同时产出 `lockbud` (rustc wrapper) 与
#      `cargo-lockbud` (cargo 子命令派发). 本仓调用 `cargo lockbud` 派发的是后者, 故存在性
#      探 `cargo-lockbud`; 但缺 `lockbud` 时 wrapper 起不来, 两个都探. requirements.sh 原注
#      「只装 lockbud」不成立, 已按实测改写.
#   ② 上游硬前提: 项目必须用与 lockbud **同版本**的 nightly 编译 (上游 README). 本机装出的
#      lockbud 内建 rustc = 1.95.0-nightly (efc9e1b50 2026-02-06), 本仓钉 nightly-2026-10-02;
#      用 2026-02-07 编译本树实测失败: kernel 5 处 E0658 (`uint_bit_width` 未稳定) + sha2 0.11
#      codegen `rustc-LLVM ERROR: Do not know how to split the result of this operator!`.
#      ⇒ 「已安装」不等于「可执行」. 匹配判据 = 用仓库工具链加载一次 wrapper: 能加载 ⇒ 版本
#      匹配可跑; `librustc_driver-*.so: cannot open shared object file` ⇒ skip 并点名实测原因.
#   ③ 调用形态两处缺陷: 派发器把第一个 `--` **之前**的参数塞进 LOCKBUD_FLAGS (lockbud 的 clap
#      解析, 无法识别的 flag 被 unwrap_or_default 静默丢弃), **之后**才原样交给 `cargo build`;
#      原写法没有 `--`, `--manifest-path` 等全被丢弃 ⇒ 退化为在 src/rust 裸跑 `cargo build`,
#      扫不到 kernel. 现补 `--` 分界; 另加 `--exit-code-on-bug` (默认 false, 有发现也退 0,
#      「零发现」会假绿) 与 `-l kernel` (上游建议只分析本仓 crate, 避免依赖噪声).
step "3/6 Lockbud 死锁/数据竞争扫描"
pushd src/rust > /dev/null
unset RUSTC_WRAPPER
REPO_TOOLCHAIN="$(sed -n 's/^channel *= *"\([^"]*\)"/\1/p' "$PROJECT_ROOT/rust-toolchain.toml")"
if ! command -v cargo-lockbud >/dev/null 2>&1 || ! command -v lockbud >/dev/null 2>&1; then
    skip_leg "lockbud" "工具缺失: lockbud / cargo-lockbud 不在 PATH (两者由同一次 cargo install 产出, 安装步骤见 scripts/requirements.sh)"
elif ! LOCKBUD_RUSTC="$(timeout 5 rustup run "$REPO_TOOLCHAIN" lockbud --version 2>&1)"; then
    skip_leg "lockbud" "工具与仓库工具链不匹配: lockbud 内建 rustc 无法在 ${REPO_TOOLCHAIN} 下加载 (上游要求项目与 lockbud 同版本 nightly; 实测内建 1.95.0-nightly 2026-02-06, 用该 nightly 编译本树失败 E0658 + sha2 LLVM error, 见 P-56)"
else
    LOCKBUD_OUT=$(cargo lockbud --exit-code-on-bug -l kernel -- --manifest-path ../kernel/Cargo.toml --target x86_64-unknown-none --target-dir ../../other/target "${BUILD_STD_CFG[@]}" 2>&1) && LOCKBUD_RC=0 || LOCKBUD_RC=$?
    printf '%s\n' "$LOCKBUD_OUT" | tail -25
    if [ "$LOCKBUD_RC" -eq 0 ]; then
        ok "lockbud: 扫描通过 (零发现; 内建 ${LOCKBUD_RUSTC})"
    else
        err "lockbud: 扫描返回非零 (exit=${LOCKBUD_RC}) —— --exit-code-on-bug 下非零即发现问题, 按 §2.2「强制」阻断 (P-47 甲 / P-56 ③)"
    fi
fi
popd > /dev/null

# ── 已退役: miri (2026-06-26 弃用) ──
# 原 Miri 严格 provenance 配置验证已删除, UB 检测由 Rust 编译期 + 7 个审计脚本覆盖

# ── 4. QEMU 真实启动测试 (full 模式) ──────────────────────────
# 双架构内核必须在 QEMU 中真实启动通过
# 跳过条件: QEMU 二进制不可用 ⇒ skip_leg 记账 (P-55 乙: 日志点名「本腿未执行」，退出码不变)
step "4/6 QEMU 双架构真实启动测试"
if [ "$MODE" = "full" ]; then
    if command -v qemu-system-x86_64 >/dev/null 2>&1 && command -v qemu-system-aarch64 >/dev/null 2>&1; then
        # B01-16 修复: 设置 FAIL_OK=0 让 QEMU 测试失败立即阻断.
        # 原脚本默认 FAIL_OK=1, 启动失败时仍返 0 (虚假通过).
        # 捕获式退出码 (B01-25 惯用法, P-55 ②): `if cmd | tail` 在 pipefail 下失败即走
        # else, 「失败 (exit=…)」分支不可达、真因被「执行异常」吞掉.
        set +e
        QEMU_OUT=$(FAIL_OK=0 "$PROJECT_ROOT/scripts/qemu_boot_test.sh" all 2>&1)
        QEMU_RC=$?
        set -e
        printf '%s\n' "$QEMU_OUT" | tail -6
        if [ "$QEMU_RC" -eq 0 ]; then
            ok "QEMU 双架构启动测试: 2/2 通过"
        else
            err "QEMU 双架构启动测试失败 (exit=$QEMU_RC, 见 scripts/qemu_boot_test.sh 输出)"
        fi
    else
        skip_leg "qemu" "工具缺失: qemu-system-x86_64 / qemu-system-aarch64 不在 PATH (安装后本腿即执行)"
    fi
else
    echo -e "${BLUE}-> 跳过 (仅 full 模式执行; quick 模式跑 SA bootstrap 即可)${NC}"
fi

report_unexecuted_legs
report_mode_boundary
echo -e "\n${GREEN}━━━ audit (full) 完成 ━━━${NC}"
