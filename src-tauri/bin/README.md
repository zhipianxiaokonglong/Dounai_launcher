# 随包 uv 不入库，由脚本取

31.8 MB 的第三方二进制不进版本库，请在下一次构建前运行：

```bash
python scripts/fetch-uv.py
```

脚本从阿里云 PyPI 镜像取 `uv-0.13.7-py3-none-win_amd64.whl`，校验**两层**哈希
（wheel 整体、解出来的 `uv.exe`），然后原子写入本目录的 `uv.exe`。取值必须与
`core/src/env/bootstrap.rs` 里的 `UV_SHA256` / `UV_BYTES` / `UV_VERSION` 三个常量一致，
否则启动器会把它判成「随包 uv 被篡改」。

| 项 | 值 |
|---|---|
| 版本 | `uv 0.13.7 (316c827e0 2026-11-03 x86_64-pc-windows-msvc)` |
| 字节数 | 31 803 184 |
| `uv.exe` sha256 | `1791d7a9ed5b6df13ccf80f102e025c333010d96326b4ff0262aa3b578835c5a` |
| 来源 wheel | `uv-0.13.7-py3-none-win_amd64.whl` |
| wheel sha256 | `2835e2ab397d0db86323b9975dce75ecd76aa9cfa0d6a2aea84734aae7f8c9b3` |
| wheel 内路径 | `uv-0.13.7.data/scripts/uv.exe` |

为什么不用 GitHub release 资产：目标机器上 `github.com` 不可达（实测超时 21 秒），
而运行时补齐的默认顺序仍是 GitHub 优先、PyPI 镜像兜底（`UV_DOWNLOAD_URLS`）。
随包这条路走通了，运行时那条就只是兜底。

uv 以 MIT 或 Apache-2.0 双协议发布，两种许可的全文都在上面那个 wheel 的
`uv-0.13.7.dist-info/licenses/` 下；随包分发时需一并附上。

本目录只有 `.gitignore` 与这份说明入库；`uv.exe` 由 `cargo build` 的
`build.rs` 复制到 `target/<profile>/bin/`，安装包由 `bundle.resources` 落到
`$INSTDIR/bin/`。
