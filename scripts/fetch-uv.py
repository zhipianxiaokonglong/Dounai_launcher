#!/usr/bin/env python3
"""把随包 uv 取到 ``src-tauri/bin/uv.exe``。

仓库不存这个 31.8 MB 的第三方二进制，构建前跑一次本脚本即可。取值必须与
``src-tauri/core/src/env/bootstrap.rs`` 的 ``UV_SHA256`` / ``UV_BYTES`` /
``UV_VERSION`` 三个常量一致，否则启动器会把文件判成「随包 uv 被篡改」。

默认从阿里云 PyPI 镜像取 wheel：官方 GitHub release 资产在这台机器上不可达
（实测超时 21 秒），而运行时补齐的兜底顺序仍是 GitHub 优先、镜像其次。

用法::

    python scripts/fetch-uv.py            # 缺就取，已存在且哈希对得上就跳过
    python scripts/fetch-uv.py --force    # 无论是否存在都重新取
    python scripts/fetch-uv.py --check    # 只校验现有文件，不联网
    python scripts/fetch-uv.py --url URL  # 换一个 wheel 地址

只用标准库，Windows / Linux / macOS 的 Python 3.9+ 都能跑。
"""

from __future__ import annotations

import argparse
import hashlib
import io
import os
import sys
import tempfile
import urllib.request
import zipfile
from pathlib import Path

UV_VERSION = "0.13.7"
UV_SHA256 = "1791d7a9ed5b6df13ccf80f102e025c333010d96326b4ff0262aa3b578835c5a"
UV_BYTES = 31_803_184
WHEEL_SHA256 = "2835e2ab397d0db86323b9975dce75ecd76aa9cfa0d6a2aea84734aae7f8c9b3"
WHEEL_MEMBER = "uv-0.13.7.data/scripts/uv.exe"
WHEEL_URL = (
    "https://mirrors.aliyun.com/pypi/packages/"
    "8e/4f/041b59d2a38807a3e31ef41064cf3e45d2752fe0409d89040d52778049ae/"
    "uv-0.13.7-py3-none-win_amd64.whl"
)

TARGET = Path(__file__).resolve().parent.parent / "src-tauri" / "bin" / "uv.exe"


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def looks_right(path: Path) -> bool:
    if not path.is_file() or path.stat().st_size != UV_BYTES:
        return False
    return sha256_hex(path.read_bytes()) == UV_SHA256


def download(url: str) -> bytes:
    print(f"取 {url}")
    request = urllib.request.Request(url, headers={"User-Agent": "dounai-fetch-uv/1"})
    with urllib.request.urlopen(request, timeout=180) as response:
        return response.read()


def extract(wheel: bytes) -> bytes:
    actual = sha256_hex(wheel)
    if actual != WHEEL_SHA256:
        raise SystemExit(
            f"wheel 哈希不符：期望 {WHEEL_SHA256}，实际 {actual}。"
            "镜像可能换了文件，先核对来源再重试。"
        )
    with zipfile.ZipFile(io.BytesIO(wheel)) as archive:
        try:
            data = archive.read(WHEEL_MEMBER)
        except KeyError:
            raise SystemExit(f"wheel 里没有 {WHEEL_MEMBER}，可能不是 win_amd64 版本")
    actual = sha256_hex(data)
    if actual != UV_SHA256 or len(data) != UV_BYTES:
        raise SystemExit(
            f"uv.exe 与内置常量不符：期望 {UV_SHA256}（{UV_BYTES} 字节），"
            f"实际 {actual}（{len(data)} 字节）。"
            "如果是有意升级 uv，请同步改 core/src/env/bootstrap.rs 与 bin/README.md。"
        )
    return data


def write_atomic(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=".uv-", suffix=".tmp")
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
        os.replace(tmp, path)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description="取随包 uv 到 src-tauri/bin/uv.exe")
    parser.add_argument("--force", action="store_true", help="已存在也重新取")
    parser.add_argument("--check", action="store_true", help="只校验，不联网")
    parser.add_argument("--url", default=WHEEL_URL, help="wheel 下载地址")
    args = parser.parse_args()

    if looks_right(TARGET):
        print(f"已就绪：{TARGET}（sha256:{UV_SHA256}）")
        return 0
    if args.check:
        print(f"缺失或哈希不符：{TARGET}", file=sys.stderr)
        return 1
    if TARGET.exists() and not args.force:
        print(f"{TARGET} 已存在但与内置常量不符，重新取：{TARGET}")

    write_atomic(TARGET, extract(download(args.url)))
    print(f"已写入 {TARGET}（{UV_BYTES} 字节，sha256:{UV_SHA256}）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
