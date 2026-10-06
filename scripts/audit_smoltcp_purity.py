#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""
M6.7 smoltcp vendored 纯度审计脚本

REVAL-W 工程要求 smoltcp 源**永不修改**, 可直接同步上游.
本脚本读取 smoltcp.versions 锁文件, 验证 vendored 源与上游 byte-level 一致.

检查内容:
  (1) smoltcp.versions 锁文件存在
  (2) vendored src/ SHA256 与锁文件记录一致
  (3) 锁文件不是 PENDING_* 占位值 (offline 模式除外)
  (4) (可选) 与上游 SHA 比对: 要求 git 可达, 否则降级为本地校验

退出码: 0 = 通过, 1 = 有违规, 2 = 锁文件缺失 (warn)

关联: docs/plan/smoltcp-framekernel-wrapper.md §CI 防污染机制
"""

import hashlib
import re
import subprocess
import sys
from pathlib import Path

# ============================================================================
# 常量
# ============================================================================
BASE = Path("src/kernel")
# W3.1 (2026-06-24): smoltcp 从 privileged/ 迁到 functions/ (决策 3-B)
# 原因: smoltcp 100% safe Rust, 应在 functions 层 (FK 合规)
VENDORED_SMOLTCP = BASE / "functions" / "net" / "smoltcp"
LOCK_FILE = BASE / "functions" / "net" / "smoltcp.versions"
UPSTREAM_REPO = "https://github.com/smoltcp-rs/smoltcp.git"


# ============================================================================
# 工具函数
# ============================================================================

def log(level: str, msg: str) -> None:
    """彩色日志输出, 适配 CI (无 TTY 时禁用颜色)."""
    colors = {
        "INFO":  "\033[0;34m",
        "OK":    "\033[0;32m",
        "WARN":  "\033[0;33m",
        "ERROR": "\033[0;31m",
    }
    nc = "\033[0m" if sys.stdout.isatty() else ""
    c = colors.get(level, "")
    prefix = f"{c}[{level}]{nc}"
    stream = sys.stderr if level in ("WARN", "ERROR") else sys.stdout
    print(f"{prefix} {msg}", file=stream)


def parse_lock_file() -> dict:
    """解析 smoltcp.versions 锁文件 (key=value 格式)."""
    if not LOCK_FILE.exists():
        return {}

    config = {}
    with open(LOCK_FILE, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            m = re.match(r"^([A-Z_][A-Z_0-9]*)\s*=\s*(.+?)\s*$", line)
            if m:
                config[m.group(1)] = m.group(2)
    return config


def compute_vendored_hash() -> str:
    """计算 vendored smoltcp src/ 目录所有 .rs 文件的合并 SHA256.

    算法: 与 scripts/vendor_smoltcp.sh 保持一致, 即
          sha256(sha256sum(file1) || sha256sum(file2) || ...).
          优先委托给 shell `find -print0 | sort -z | xargs -0 sha256sum`
          以保证 byte-level 一致 (locale-dependent sort 难以在 Python 复现).

    关键: 文件按 find + sort -z 排序, 每行格式为 `<hash>  <filename>`.
    """
    if not VENDORED_SMOLTCP.exists():
        return ""
    src_dir = VENDORED_SMOLTCP / "src"
    if not src_dir.exists():
        return ""

    import shutil
    import subprocess as sp
    if shutil.which("find") and shutil.which("sort") and shutil.which("xargs") and shutil.which("sha256sum"):
        try:
            # 与 shell 脚本完全一致: find -print0 | sort -z | xargs -0 sha256sum
            # LC_ALL=C 确保字节序可重现 (避免 locale 差异)
            env = {"LC_ALL": "C", "PATH": "/usr/bin:/bin:/usr/local/bin"}
            result = sp.run(
                f"find {VENDORED_SMOLTCP}/src -type f -name '*.rs' -print0"
                " | LC_ALL=C sort -z"
                " | xargs -0 sha256sum",
                shell=True, capture_output=True, text=True, check=True, env=env,
            )
            return hashlib.sha256(result.stdout.encode("utf-8")).hexdigest()
        except (sp.CalledProcessError, FileNotFoundError):
            pass

    # Python 回退路径 (排序可能与 shell 不一致, 仅作占位)
    files = sorted(src_dir.rglob("*.rs"))
    lines = []
    for f in files:
        file_hash = hashlib.sha256(f.read_bytes()).hexdigest()
        lines.append(f"{file_hash}  {f}")
    combined = "\n".join(lines) + "\n"
    return hashlib.sha256(combined.encode("utf-8")).hexdigest()


def check_upstream_reachable() -> bool:
    """检查 git 上游是否可达 (用于深度验证)."""
    try:
        result = subprocess.run(
            ["git", "ls-remote", "--tags", UPSTREAM_REPO, "HEAD"],
            capture_output=True, timeout=10, text=True,
        )
        return result.returncode == 0
    except (subprocess.TimeoutExpired, FileNotFoundError, Exception):
        return False


def fetch_upstream_sha(tag: str) -> str:
    """从上游获取 tag 对应 SHA (需网络)."""
    try:
        result = subprocess.run(
            ["git", "ls-remote", "--tags", UPSTREAM_REPO, f"refs/tags/{tag}"],
            capture_output=True, timeout=30, text=True,
        )
        if result.returncode != 0:
            return ""
        for line in result.stdout.splitlines():
            sha, ref = line.split(maxsplit=1)
            if ref == f"refs/tags/{tag}":
                return sha
    except Exception:
        pass
    return ""


# ============================================================================
# 主审计逻辑
# ============================================================================

def main() -> int:
    log("INFO", "=" * 70)
    log("INFO", "M6.7 smoltcp vendored 纯度审计")
    log("INFO", "=" * 70)
    log("INFO", f"vendored 路径: {VENDORED_SMOLTCP}")
    log("INFO", f"锁文件路径:   {LOCK_FILE}")
    log("INFO", "")

    issues = []

    # ---- 检查 1: vendored 目录存在 ----
    if not VENDORED_SMOLTCP.exists():
        log("ERROR", f"vendored 目录不存在: {VENDORED_SMOLTCP}")
        return 1

    # ---- 检查 2: 锁文件存在 ----
    if not LOCK_FILE.exists():
        log("WARN", f"锁文件不存在: {LOCK_FILE}")
        log("WARN", "建议运行: scripts/vendor_smoltcp.sh lock v0.13.0")
        return 2

    config = parse_lock_file()
    log("INFO", "锁文件内容:")
    for key in ("SMOLTCP_TAG", "SMOLTCP_SHA", "SMOLTCP_UPSTREAM_SRC_HASH", "SMOLTCP_LOCAL_SRC_HASH", "SMOLTCP_LOCK_MODE"):
        if key in config:
            log("INFO", f"  {key} = {config[key]}")
    log("INFO", "")

    # ---- 检查 3: SHA 字段不能是 PENDING 占位 ----
    # 联网探测只做一次, 供检查 3/5/6 复用 (离线下每次调用都要等超时, 避免重复).
    online = check_upstream_reachable()

    # 语义 (与 docstring 检查 3 的 "offline 模式除外" 一致):
    #   - 网络相关字段 (SMOLTCP_SHA / SMOLTCP_UPSTREAM_SRC_HASH): 离线无法获取/复核上游值,
    #     占位降级为 WARN (与检查 5/6 上游不可达时的降级行为保持一致); 联网时仍是硬违规.
    #   - 本地可验证字段 (SMOLTCP_LOCAL_SRC_HASH): 始终硬违规 — 其占位会使检查 4 的
    #     本地纯度比对静默跳过, 门禁失效.
    sha = config.get("SMOLTCP_SHA", "")
    if "PENDING" in sha:
        if online:
            log("WARN", f"SMOLTCP_SHA 是占位值: {sha}")
            log("WARN", "在联网环境运行 scripts/vendor_smoltcp.sh lock 重写")
            issues.append("SMOLTCP_SHA 占位")
        else:
            log("INFO", f"SMOLTCP_SHA 是占位值 (离线无法获取上游 SHA, 跳过): {sha}")

    lock_mode = config.get("SMOLTCP_LOCK_MODE", "")
    upstream_src_hash = config.get("SMOLTCP_UPSTREAM_SRC_HASH", "")
    local_src_hash = config.get("SMOLTCP_LOCAL_SRC_HASH", "")

    if "PENDING" in upstream_src_hash:
        if online:
            log("WARN", f"SMOLTCP_UPSTREAM_SRC_HASH 是占位值: {upstream_src_hash}")
            issues.append("SMOLTCP_UPSTREAM_SRC_HASH 占位")
        else:
            log("INFO", f"SMOLTCP_UPSTREAM_SRC_HASH 是占位值 (离线, 跳过): {upstream_src_hash}")
    if "PENDING" in local_src_hash:
        log("WARN", f"SMOLTCP_LOCAL_SRC_HASH 是占位值: {local_src_hash}")
        issues.append("SMOLTCP_LOCAL_SRC_HASH 占位")

    # ---- 检查 4: vendored 源 hash 与锁文件一致 ----
    actual_local_hash = compute_vendored_hash()
    if not actual_local_hash:
        log("ERROR", "无法计算 vendored src/ hash")
        return 1
    log("INFO", f"本地 vendored src/ SHA256:  {actual_local_hash}")
    log("INFO", f"锁文件 SMOLTCP_LOCAL_SRC_HASH:  {local_src_hash}")

    if local_src_hash and "PENDING" not in local_src_hash:
        if actual_local_hash != local_src_hash:
            log("ERROR", "✗ 本地 vendored 源 hash 与锁文件不一致")
            log("ERROR", f"  本地:    {actual_local_hash}")
            log("ERROR", f"  锁文件:  {local_src_hash}")
            log("ERROR", "可能原因: 本地修改过 vendored 代码, 或锁文件过期")
            issues.append("本地 vendored hash 失配")
        else:
            log("OK", "✓ 本地 vendored 源 hash 与锁文件一致")
    else:
        log("WARN", "跳过 hash 比对 (SMOLTCP_LOCAL_SRC_HASH 是占位值)")

    # ---- 检查 5: 上游 src/ hash 一致性 (可选, 需联网) ----
    tag = config.get("SMOLTCP_TAG", "")
    if online and upstream_src_hash and "PENDING" not in upstream_src_hash:
        log("INFO", "")
        log("INFO", f"上游可达, 正在验证 tag {tag} 的 src/ hash...")
        # 浅克隆上游, 计算 hash
        try:
            import tempfile
            with tempfile.TemporaryDirectory() as tmp:
                subprocess.run(
                    ["git", "clone", "--depth", "1", "--quiet", "--branch", tag, UPSTREAM_REPO, f"{tmp}/smoltcp"],
                    check=True, timeout=60,
                )
                upstream_actual_hash = subprocess.run(
                    "cd {}/smoltcp && LC_ALL=C find src -type f -name '*.rs' -print0 | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum".format(tmp),
                    shell=True, capture_output=True, text=True, check=True,
                ).stdout.split()[0]
                if upstream_actual_hash == upstream_src_hash:
                    log("OK", f"✓ 上游 tag {tag} src/ hash 与锁文件一致")
                else:
                    log("ERROR", f"✗ 上游 tag {tag} src/ hash 不一致")
                    log("ERROR", f"  锁文件:    {upstream_src_hash}")
                    log("ERROR", f"  upstream:  {upstream_actual_hash}")
                    issues.append("上游 src/ hash 失配")
        except Exception as e:
            log("WARN", f"无法验证上游 src/ hash: {e}")
    else:
        log("WARN", f"上游不可达或 hash 占位, 跳过 src/ hash 验证")

    # ---- 检查 6: (可选) 与上游 SHA 比对 ----
    if tag and "PENDING" not in sha:
        if online:
            log("INFO", f"上游可达, 正在验证 tag {tag} 的 commit SHA...")
            upstream_sha = fetch_upstream_sha(tag)
            if upstream_sha:
                if upstream_sha == sha:
                    log("OK", f"✓ 锁文件 SHA 与上游 tag {tag} 一致")
                else:
                    log("ERROR", f"✗ 锁文件 SHA 与上游 tag {tag} 不一致")
                    log("ERROR", f"  锁文件:    {sha}")
                    log("ERROR", f"  upstream:  {upstream_sha}")
                    issues.append("上游 commit SHA 失配")
            else:
                log("WARN", f"无法解析上游 tag {tag} 的 commit SHA")
        else:
            log("WARN", f"上游不可达, 跳过 commit SHA 比对 (需 git 网络访问 {UPSTREAM_REPO})")
    elif not tag:
        log("WARN", "锁文件无 SMOLTCP_TAG 字段")

    # ---- 检查 7: LOCALIZED_VENDORED 模式特殊校验 ----
    if lock_mode == "LOCALIZED_VENDORED":
        localized = config.get("SMOLTCP_LOCALIZED_FILES", "")
        if not localized:
            log("WARN", "LOCALIZED_VENDORED 模式但缺 SMOLTCP_LOCALIZED_FILES 清单")
        else:
            file_list = localized.split()
            log("OK", f"LOCALIZED_VENDORED 模式, {len(file_list)} 个本地化文件已在清单")

    # ---- 总结 ----
    log("INFO", "")
    log("INFO", "=" * 70)
    if issues:
        log("ERROR", f"✗ 审计未通过 ({len(issues)} 项问题)")
        for i, issue in enumerate(issues, 1):
            log("ERROR", f"  {i}. {issue}")
        return 1
    elif not config:
        log("WARN", "⚠ 锁文件为空, 建议生成")
        return 2
    else:
        log("OK", "✓ M6.7 通过: smoltcp vendored 纯度审计")
        return 0


if __name__ == "__main__":
    sys.exit(main())
