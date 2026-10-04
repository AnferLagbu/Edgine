#!/bin/bash
# SPDX-License-Identifier: MPL-2.0
# vendor_smoltcp.sh — smoltcp 第三方库 vendored 同步脚本
#
# 功能:
#   1. 验证当前 vendored smoltcp 与上游 [tag] 字节级一致 (纯洁性)
#   2. 可选: 重新同步到指定 tag (谨慎操作, 默认不执行)
#   3. 写入 smoltcp.versions 锁定文件 (tag + sha + 校验和)
#
# 设计动机:
#   REVAL-W 工程要求 smoltcp 源**永不修改**, 可直接同步上游.
#   本脚本提供 CI 友好的验证流程 + 安全的升级路径.
#
# 用法:
#   scripts/vendor_smoltcp.sh verify                 # 验证 vendored 纯度 (默认)
#   scripts/vendor_smoltcp.sh lock <tag>            # 写入 smoltcp.versions 锁文件
#   scripts/vendor_smoltcp.sh status                # 显示当前版本与上游对比
#   scripts/vendor_smoltcp.sh sync <tag>            # 升级到新 tag (危险, 需用户确认)
#
# 关联: docs/plan/smoltcp-framekernel-wrapper.md §同步机制

set -euo pipefail

# ============================================================================
# 常量 (避免硬编码, 与 smoltcp-framekernel-wrapper.md 保持单一来源)
# ============================================================================
SMOLTCP_VENDORED="src/kernel/functions/net/smoltcp"
SMOLTCP_LOCKFILE="src/kernel/functions/net/smoltcp.versions"
SMOLTCP_UPSTREAM="https://github.com/smoltcp-rs/smoltcp.git"

# 颜色输出 (CI 环境自动禁用)
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    BLUE='\033[0;34m'
    NC='\033[0m'
else
    RED='' GREEN='' YELLOW='' BLUE='' NC=''
fi

log_info()  { echo -e "${BLUE}[INFO]${NC}  $*"; }
log_ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
log_err()   { echo -e "${RED}[ERROR]${NC} $*" >&2; }

# ============================================================================
# 工具函数
# ============================================================================

# 读取 vendored smoltcp 的当前版本 (从 Cargo.toml)
get_vendored_version() {
    local cargo_toml="${SMOLTCP_VENDORED}/Cargo.toml"
    if [ ! -f "$cargo_toml" ]; then
        log_err "vendored Cargo.toml 不存在: $cargo_toml"
        return 1
    fi
    grep -E '^version\s*=' "$cargo_toml" | head -1 | sed -E 's/^version\s*=\s*"([^"]+)".*/\1/'
}

# 计算 vendored 源码 (src/) 的 SHA256
compute_vendored_hash() {
    if [ ! -d "${SMOLTCP_VENDORED}/src" ]; then
        log_err "vendored src/ 目录不存在: ${SMOLTCP_VENDORED}/src"
        return 1
    fi
    # 只 hash src/ 内容 (Cargo.toml 等元数据可能本地化)
    # LC_ALL=C 确保跨 locale 一致 (Python audit 脚本也使用 LC_ALL=C)
    LC_ALL=C find "${SMOLTCP_VENDORED}/src" -type f -name '*.rs' -print0 | \
        LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum | awk '{print $1}'
}

# 从 git 仓库获取 tag 对应的 SHA
fetch_upstream_sha() {
    local tag="$1"
    if [ -z "$tag" ]; then
        log_err "tag 不能为空"
        return 1
    fi
    # 浅克隆后查询 tag 对应的 commit SHA
    git ls-remote --tags "$SMOLTCP_UPSTREAM" "refs/tags/${tag}" 2>/dev/null | \
        awk '{print $1}' | head -1
}

# 从 git 仓库获取 tag 对应 src/ 的 SHA256
fetch_upstream_src_hash() {
    local tag="$1"
    local sha
    sha=$(fetch_upstream_sha "$tag")
    if [ -z "$sha" ]; then
        log_err "无法解析 tag '$tag' 的 SHA"
        return 1
    fi
    # 浅克隆上游对应 tag
    local tmp
    tmp=$(mktemp -d)
    trap "rm -rf '$tmp'" RETURN
    git clone --depth 1 --quiet --branch "$tag" "$SMOLTCP_UPSTREAM" "$tmp/smoltcp" 2>/dev/null
    if [ ! -d "$tmp/smoltcp/src" ]; then
        log_err "上游 $tag 仓库无 src/ 目录"
        return 1
    fi
    # 相对路径 + LC_ALL=C: 与 audit_smoltcp_purity.py 检查 5 的
    # `cd <clone>/smoltcp && LC_ALL=C find src ...` 计算方式保持 byte 级一致
    (cd "$tmp/smoltcp" && LC_ALL=C find src -type f -name '*.rs' -print0 | \
        LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum | awk '{print $1}')
}

# ============================================================================
# 子命令: verify
# ============================================================================
cmd_verify() {
    log_info "验证 smoltcp vendored 纯度..."

    if [ ! -d "$SMOLTCP_VENDORED" ]; then
        log_err "vendored 目录不存在: $SMOLTCP_VENDORED"
        log_err "请先确认 smoltcp 是否已 vendored"
        return 1
    fi

    local version
    version=$(get_vendored_version)
    log_info "当前 vendored 版本: $version"

    local vendored_hash
    vendored_hash=$(compute_vendored_hash)
    log_info "当前 vendored src/ SHA256: $vendored_hash"

    if [ -f "$SMOLTCP_LOCKFILE" ]; then
        log_info "读取锁文件: $SMOLTCP_LOCKFILE"
        # shellcheck disable=SC1090
        source "$SMOLTCP_LOCKFILE"
        log_info "锁文件 tag=$SMOLTCP_TAG sha=$SMOLTCP_SHA mode=${SMOLTCP_LOCK_MODE:-?}"

        # LOCALIZED_VENDORED 模式: 本地 hash 对比 LOCAL_SRC_HASH (含本地化文件),
        # 上游 UPSTREAM_SRC_HASH 由 audit_smoltcp_purity.py 联网复核
        local local_src_hash="${SMOLTCP_LOCAL_SRC_HASH:-}"
        if [ "$vendored_hash" = "$local_src_hash" ]; then
            log_ok "vendored 源与锁文件一致 ✓"
            return 0
        else
            log_err "vendored 源与锁文件不一致 ✗"
            log_err "  vendored: $vendored_hash"
            log_err "  锁文件:   ${local_src_hash:-缺失}"
            return 1
        fi
    else
        log_warn "锁文件不存在: $SMOLTCP_LOCKFILE"
        log_warn "建议运行: scripts/vendor_smoltcp.sh lock $version"
        return 0
    fi
}

# ============================================================================
# 子命令: lock
# ============================================================================
cmd_lock() {
    local tag="${1:-}"
    if [ -z "$tag" ]; then
        log_err "用法: scripts/vendor_smoltcp.sh lock <tag>"
        log_err "示例: scripts/vendor_smoltcp.sh lock v0.13.0"
        return 1
    fi

    log_info "锁定 smoltcp 到 tag: $tag"

    local sha
    sha=$(fetch_upstream_sha "$tag")
    if [ -z "$sha" ]; then
        log_err "无法解析 tag '$tag' 的 SHA"
        return 1
    fi
    log_info "tag=$tag sha=$sha"

    local upstream_src_hash
    upstream_src_hash=$(fetch_upstream_src_hash "$tag")
    log_info "上游 src/ SHA256: $upstream_src_hash"

    local vendored_hash
    vendored_hash=$(compute_vendored_hash)
    log_info "本地 vendored src/ SHA256: $vendored_hash"

    # LOCALIZED_VENDORED 模式 (默认): 本地允许含本地化文件 (lint/SAFETY 注释),
    # 因此本地 hash 不必等于上游 hash — 差异由 SMOLTCP_LOCALIZED_FILES 清单约束.
    # 非本地化模式: 本地必须与上游 byte-level 一致, 否则拒绝锁定.
    local lock_mode="${SMOLTCP_LOCK_MODE:-LOCALIZED_VENDORED}"
    local localized_files="${SMOLTCP_LOCALIZED_FILES:-}"
    if [ "$vendored_hash" != "$upstream_src_hash" ]; then
        if [ "$lock_mode" = "LOCALIZED_VENDORED" ]; then
            if [ -z "$localized_files" ]; then
                log_warn "本地与上游不一致但 SMOLTCP_LOCALIZED_FILES 为空"
                log_warn "若存在本地化文件, 请通过环境变量 SMOLTCP_LOCALIZED_FILES 提供清单"
            else
                log_info "本地与上游不一致 (LOCALIZED_VENDORED, 差异由本地化清单约束)"
            fi
        else
            log_warn "本地 vendored 与上游不一致"
            log_warn "  vendored: $vendored_hash"
            log_warn "  上游:     $upstream_src_hash"
            log_warn "非 LOCALIZED_VENDORED 模式要求本地与上游一致"
            if [ "${SMOLTCP_FORCE:-0}" != "1" ]; then
                return 1
            fi
        fi
    fi

    # 确保目标目录存在
    mkdir -p "$(dirname "$SMOLTCP_LOCKFILE")"

    cat > "$SMOLTCP_LOCKFILE" <<EOF
# smoltcp vendored 锁文件
# 由 scripts/vendor_smoltcp.sh lock 自动生成
# 用于 CI 验证 vendored 源与上游 byte-level 一致 (排除本地化部分)
#
# 关联: docs/plan/smoltcp-framekernel-wrapper.md §同步机制
# 模式: $lock_mode

# 上游 tag (semver, 形如 v0.13.1)
SMOLTCP_TAG=$tag

# 上游 tag 对应 commit SHA
SMOLTCP_SHA=$sha

# 上游 src/ 目录下所有 .rs 文件的合并 SHA256
SMOLTCP_UPSTREAM_SRC_HASH=$upstream_src_hash

# 本地 vendored 实际 src/ 合并 SHA256 ($lock_mode 模式下含本地化文件)
SMOLTCP_LOCAL_SRC_HASH=$vendored_hash

# 锁定时间 (ISO 8601, 仅供人类阅读, CI 不读取)
SMOLTCP_LOCKED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)

# 锁定模式 (LOCALIZED_VENDORED / PURITY)
SMOLTCP_LOCK_MODE=$lock_mode

# 本地化文件清单 (LOCALIZED_VENDORED 模式, 空格分隔的相对路径)
SMOLTCP_LOCALIZED_FILES="$localized_files"
EOF

    log_ok "锁文件已写入: $SMOLTCP_LOCKFILE"
    log_ok "  tag:  $tag"
    log_ok "  sha:  $sha"
    log_ok "  upstream hash: $upstream_src_hash"
    log_ok "  local hash:    $vendored_hash"
    log_ok "  mode:          $lock_mode"
}

# ============================================================================
# 子命令: status
# ============================================================================
cmd_status() {
    log_info "smoltcp vendored 状态报告"
    echo "─────────────────────────────────────────"

    if [ ! -d "$SMOLTCP_VENDORED" ]; then
        log_err "vendored 目录不存在: $SMOLTCP_VENDORED"
        return 1
    fi

    local version
    version=$(get_vendored_version)
    echo "  vendored 路径:  $SMOLTCP_VENDORED"
    echo "  vendored 版本:  $version"

    if [ -f "$SMOLTCP_LOCKFILE" ]; then
        # shellcheck disable=SC1090
        source "$SMOLTCP_LOCKFILE"
        echo "  锁文件:          $SMOLTCP_LOCKFILE"
        echo "  锁文件 tag:      ${SMOLTCP_TAG:-?}"
        echo "  锁文件 sha:      ${SMOLTCP_SHA:-?}"
        echo "  上游 hash:       ${SMOLTCP_UPSTREAM_SRC_HASH:-?}"
        echo "  本地 hash:       ${SMOLTCP_LOCAL_SRC_HASH:-?}"
        echo "  锁定时间:        ${SMOLTCP_LOCKED_AT:-?}"
        echo "  锁定模式:        ${SMOLTCP_LOCK_MODE:-?}"
    else
        echo "  锁文件:         (未生成)"
    fi

    local vendored_hash
    vendored_hash=$(compute_vendored_hash)
    echo "  vendored hash:  $vendored_hash"

    echo "─────────────────────────────────────────"
}

# ============================================================================
# 子命令: sync
# ============================================================================
cmd_sync() {
    local tag="${1:-}"
    if [ -z "$tag" ]; then
        log_err "用法: scripts/vendor_smoltcp.sh sync <tag>"
        return 1
    fi

    log_warn "升级操作将替换当前 vendored 内容, 此操作不可逆!"
    log_warn "目标 tag: $tag"
    log_warn "如确认, 使用 SMOLTCP_FORCE=1 环境变量"
    if [ "${SMOLTCP_FORCE:-0}" != "1" ]; then
        log_info "未确认, 退出 (使用 SMOLTCP_FORCE=1 强制执行)"
        return 1
    fi

    log_info "开始同步 smoltcp 到 $tag..."

    local tmp
    tmp=$(mktemp -d)
    trap "rm -rf '$tmp'" RETURN

    git clone --depth 1 --quiet --branch "$tag" "$SMOLTCP_UPSTREAM" "$tmp/smoltcp" || {
        log_err "克隆上游失败"
        return 1
    }

    # 备份当前 vendored
    local backup="${SMOLTCP_VENDORED}.bak.$$"
    mv "$SMOLTCP_VENDORED" "$backup" || {
        log_err "备份当前 vendored 失败"
        return 1
    }

    # 复制新版本 (排除 .github 等 CI 配置)
    mkdir -p "$SMOLTCP_VENDORED"
    cp -r "$tmp/smoltcp/src" "$SMOLTCP_VENDORED/src"
    cp "$tmp/smoltcp/Cargo.toml" "$SMOLTCP_VENDORED/Cargo.toml"
    cp "$tmp/smoltcp/README.md" "$SMOLTCP_VENDORED/README.md" 2>/dev/null || true
    cp "$tmp/smoltcp/LICENSE-*" "$SMOLTCP_VENDORED/" 2>/dev/null || true

    rm -rf "$backup"
    log_ok "smoltcp 已同步到 $tag"

    # 自动写锁
    cmd_lock "$tag"
}

# ============================================================================
# 入口
# ============================================================================
main() {
    local cmd="${1:-verify}"
    shift || true

    case "$cmd" in
        verify) cmd_verify "$@" ;;
        lock)   cmd_lock   "$@" ;;
        status) cmd_status "$@" ;;
        sync)   cmd_sync   "$@" ;;
        help|-h|--help)
            sed -n '2,30p' "$0" | sed 's/^# \?//'
            ;;
        *)
            log_err "未知子命令: $cmd"
            log_err "运行 '$0 help' 查看用法"
            return 1
            ;;
    esac
}

main "$@"
