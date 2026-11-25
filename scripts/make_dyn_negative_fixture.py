#!/usr/bin/env python3
"""从可用的动态 ELF 生成两条失败路径夹具（构建期派生，不入库二进制副本）。

- bad-interp: 把 `PT_INTERP` 路径首字节 `/` 改成 `x`，变成相对路径；
  内核必须在解析阶段返回 `ENOEXEC`，而不是退化成静态装载。
"""

from __future__ import annotations

import argparse
import struct
from pathlib import Path

PT_INTERP = 3


def parse_elf64(data: bytes) -> tuple[int, int, int]:
    if len(data) < 64 or data[:4] != b"\x7fELF":
        raise ValueError("不是 ELF 文件")
    if data[4] != 2:
        raise ValueError("只支持 ELF64")
    header = struct.unpack_from("<16sHHIQQQIHHHHHH", data, 0)
    phoff = header[5]
    phentsize = header[9]
    phnum = header[10]
    if phentsize != 56 or phoff == 0 or phnum == 0:
        raise ValueError("ELF64 程序头表不合法")
    if phoff + phnum * phentsize > len(data):
        raise ValueError("程序头表越出文件")
    return phoff, phentsize, phnum


def phdr_at(data: bytes, phoff: int, phentsize: int, index: int) -> tuple[int, int, int]:
    off = phoff + index * phentsize
    p_type, _p_flags, p_offset, _vaddr, _paddr, p_filesz, _memsz, _align = struct.unpack_from(
        "<IIQQQQQQ", data, off
    )
    return p_type, p_offset, p_filesz


def patch_bad_interp(data: bytes) -> bytes:
    phoff, phentsize, phnum = parse_elf64(data)
    for i in range(phnum):
        p_type, p_offset, p_filesz = phdr_at(data, phoff, phentsize, i)
        if p_type != PT_INTERP:
            continue
        if p_filesz == 0 or p_offset + p_filesz > len(data):
            raise ValueError("PT_INTERP 段越界或为空")
        if data[p_offset:p_offset + 1] != b"/":
            raise ValueError("PT_INTERP 不是绝对路径，无法派生相对路径夹具")
        out = bytearray(data)
        out[p_offset] = ord("x")
        return bytes(out)
    raise ValueError("输入没有 PT_INTERP")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()

    data = args.base.read_bytes()
    out = patch_bad_interp(data)

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_bytes(out)
    print(f"bad-interp: {args.base} -> {args.out} ({len(out)} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
