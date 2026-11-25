#!/bin/sh
# 重新生成 hello_dyn（需要宿主 gcc 与同目录的 musl 对象；不参与 CI 默认构建）。
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
gcc -fPIE -pie -nostdlib -nostartfiles \
    -Wl,-e,main -Wl,--dynamic-linker=/lib/ld-musl-x86_64.so.1 \
    -L"$here" -l:libc.musl-x86_64.so.1 \
    "$here/hello.c" -o "$here/hello_dyn"
