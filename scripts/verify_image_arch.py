#!/usr/bin/env python3
"""内核镜像内嵌用户态 ELF 的架构自检 (S-6 防线③).

背景: 内核在编译期用 `include_bytes!` 把 `other/build/<arch>/user/init.bin` 原样
嵌进镜像. 该产物曾跨架构共享同一路径并原地覆盖, 于是出现过"嵌 aarch64 用户态的
x86_64 镜像" — 全程 0 error / 0 warning / rc=0, 直到进入 Ring 3 执行非法指令才
#PF. 本脚本把这类问题拦在启动之前, 判定不符即 fail (不降级为 warn).

判据说明: 只用两类可靠信号 ——
  1. 产物 ELF 头部 (`e_ident` 魔数/class/端序 + `e_machine`) 的架构身份;
  2. 镜像是否**逐字节包含**本架构产物的完整内容 (嵌入是否真的发生).
`kernel.flat` 尺寸不参与判定: 同一合法构型在全量/增量构建下尺寸可差数十 KB,
尺寸只能提示"产物变动过", 不是架构判据.

用法:
    python3 scripts/verify_image_arch.py --arch x86_64 \\
        --image other/build/kernel.flat \\
        --init  other/build/x86_64/user/init.bin

    # 只校验单个 ELF 的架构身份 (Makefile/CI 可用)
    python3 scripts/verify_image_arch.py --arch aarch64 --init other/build/aarch64/user/eash.bin

退出码: 0 = 通过, 1 = 不符或缺失.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

# 架构名 -> ELF e_machine 期望值 (与内核 elf::verify 的 EM_X86_64 / EM_AARCH64 同步)
EXPECTED_E_MACHINE = {
    "x86_64": 0x3E,
    "aarch64": 0xB7,
}

ELF_MAGIC = b"\x7fELF"
ELFCLASS64 = 2
# e_ident[EI_DATA]: 1 = LSB (两目标架构均为小端)
ELFDATA2LSB = 1
# e_machine 字段在 ELF64 头内的偏移
OFF_E_MACHINE = 18


class ElfError(Exception):
    """ELF 头不可解析或字段不符."""


def read_e_machine(path: Path) -> int:
    """读取 ELF64 文件头, 返回 `e_machine`.

    Raises:
        ElfError: 文件过短 / 魔数不符 / 非 ELFCLASS64 / 非小端.
    """
    data = path.read_bytes()
    if len(data) < OFF_E_MACHINE + 2:
        raise ElfError(f"{path}: 不足 ELF 头部长度 ({len(data)} 字节)")
    if data[:4] != ELF_MAGIC:
        raise ElfError(f"{path}: ELF 魔数不匹配")
    if data[4] != ELFCLASS64:
        raise ElfError(f"{path}: 非 ELFCLASS64 (e_ident[4]={data[4]})")
    if data[5] != ELFDATA2LSB:
        raise ElfError(f"{path}: 非小端 (e_ident[5]={data[5]})")
    return int.from_bytes(data[OFF_E_MACHINE : OFF_E_MACHINE + 2], "little")


def check_elf(path: Path, arch: str) -> tuple[bool, str]:
    """校验单个 ELF 文件的架构身份."""
    expected = EXPECTED_E_MACHINE[arch]
    if not path.is_file():
        return False, f"产物缺失: {path}"
    try:
        actual = read_e_machine(path)
    except ElfError as exc:
        return False, f"ELF 头非法: {exc}"
    if actual != expected:
        return False, (
            f"{path}: e_machine=0x{actual:02X} 与 ARCH={arch} 期望 0x{expected:02X} 不符"
        )
    return True, f"{path}: e_machine=0x{actual:02X} (ARCH={arch}) OK"


def check_image_contains(image: Path, init_bin: Path) -> tuple[bool, str]:
    """校验镜像逐字节包含本架构 init.bin (证明嵌入的就是该产物)."""
    if not image.is_file():
        return False, f"镜像缺失: {image}"
    if not init_bin.is_file():
        return False, f"产物缺失: {init_bin}"
    blob = init_bin.read_bytes()
    if len(blob) == 0:
        return False, f"{init_bin}: 内容为空, 未真正生成用户态产物"
    data = image.read_bytes()
    if len(data) < len(blob):
        return False, f"{image} ({len(data)}B) 比 {init_bin} ({len(blob)}B) 还小, 不可能已嵌入"
    off = data.find(blob)
    if off < 0:
        return False, (
            f"{image} 未包含 {init_bin} 的完整字节 (镜像陈旧或嵌入了另一份产物; "
            "请确认 make 依赖与 build.rs rerun-if-changed 生效)"
        )
    return True, f"{image} @0x{off:x} 内含 {init_bin} ({len(blob)}B) OK"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--arch", required=True, choices=sorted(EXPECTED_E_MACHINE))
    parser.add_argument("--image", type=Path, help="内核镜像 (kernel.flat / kernel-aarch64.img)")
    parser.add_argument("--init", type=Path, required=True, help="待校验的用户态 ELF")
    args = parser.parse_args(argv)

    failures: list[str] = []
    ok, msg = check_elf(args.init, args.arch)
    print(f"{'[OK]' if ok else '[FAIL]'} {msg}")
    if not ok:
        failures.append(msg)

    if args.image is not None:
        ok, msg = check_image_contains(args.image, args.init)
        print(f"{'[OK]' if ok else '[FAIL]'} {msg}")
        if not ok:
            failures.append(msg)

    if failures:
        print(
            f"verify_image_arch: 不通过 ({args.arch}) — 请勿在产物不符的镜像上跑门禁/调试, "
            "先 ./ci/build.sh <arch> 或 make ARCH=<arch> all 重建",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
