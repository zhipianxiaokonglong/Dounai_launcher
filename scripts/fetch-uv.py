#!/usr/bin/env python3
"""把随包 uv 取到 ``src-tauri/bin/uv.exe``。

仓库不存这个 31.8 MB 的第三方二进制，构建前跑一次本脚本即可。取值必须与
``src-tauri/core/src/env/bootstrap.rs`` 的 ``UV_SHA256`` / ``UV_BYTES`` /
``UV_VERSION`` 三个常量一致，否则启动器会把文件判成「随包 uv 被篡改」。

默认从 PyPI 官方源取 wheel，失败时回退到阿里云镜像：GitHub Actions runner
在境外，官方源可达而阿里云镜像不一定；目标机器反过来（github.com 不可达）。
两个源指向同一个 wheel，都按 wheel / uv.exe 两层哈希校验。
运行时补齐的兜底顺序仍是 GitHub 优先、镜像其次。

用法::

    python scripts/fetch-uv.py            # 缺就取，已存在且哈希对得上就跳过
    python scripts/fetch-uv.py --force    # 无论是否存在都重新取
    python scripts/fetch-uv.py --check    # 只校验现有文件，不联网
    python scripts/fetch-uv.py --url URL  # 只用指定的 wheel 地址（不回退）

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

# Windows runner 的 stdout 默认 cp1252，中文提示会在 print 时就抛
# UnicodeEncodeError（CI 实测）。脚本自己切到 UTF-8，不依赖调用方设环境变量。
for _stream in (sys.stdout, sys.stderr):
    if hasattr(_stream, "reconfigure"):
        _stream.reconfigure(encoding="utf-8", errors="replace")

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
PYPI_WHEEL_URL = (
    "https://files.pythonhosted.org/packages/"
    "8e/4f/041b59d2a38807a3e31ef41064cf3e45d2752fe0409d89040d52778049ae/"
    "uv-0.13.7-py3-none-win_amd64.whl"
)
# 官方源放首位：CI runner 在境外，阿里云镜像超时会让每台 runner 白等 180 秒。
WHEEL_URLS = (PYPI_WHEEL_URL, WHEEL_URL)

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


def download_any(urls: tuple[str, ...]) -> bytes:
    errors: list[str] = []
    for url in urls:
        try:
            return download(url)
        except OSError as exc:
            print(f"取不到 {url}：{exc}", file=sys.stderr)
            errors.append(f"{url}: {exc}")
    raise SystemExit("所有 wheel 源都不可用：" + "；".join(errors))


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
    parser.add_argument(
        "--url", default=None, help="只用指定的 wheel 地址（不做源回退）"
    )
    args = parser.parse_args()

    if looks_right(TARGET) and not args.force:
        print(f"已就绪：{TARGET}（sha256:{UV_SHA256}）")
        return 0
    if args.check:
        print(f"缺失或哈希不符：{TARGET}", file=sys.stderr)
        return 1
    if TARGET.exists() and not args.force:
        print(f"{TARGET} 已存在但与内置常量不符，重新取：{TARGET}")

    urls = (args.url,) if args.url else WHEEL_URLS
    write_atomic(TARGET, extract(download_any(urls)))
    print(f"已写入 {TARGET}（{UV_BYTES} 字节，sha256:{UV_SHA256}）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
