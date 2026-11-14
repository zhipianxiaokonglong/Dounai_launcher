#!/usr/bin/env python3
"""官方 catalog 一键重签脚本（backlog：签发到上传未脚本化）。

2026-11-14 现网事故的直接诱因是哈希核对、sequence 递增、7 天时间窗全靠手工，
每 7 天重复一次必然再出错。本脚本把这些步骤固化：

  1. 拉现网 catalog（或 --from 本地文件）作为底稿；
  2. sequence 取现网值 +1，generated_at 取当前 UTC，expires 取 +7 天（上限即 7 天）；
  3. 逐条目下载模块包（或 --modules-dir 用本地副本），重算 sha256 与大小，
     与目录声明比对——不一致即中止，除非 --update-hashes 显式要求改写；
  4. 调 sign_catalog 签名（工具内含 source_id 预检 + 签后自验）；
  5. 用 K5 公钥再跑一次 verify 复验；
  6. 打印 sftp 上传命令。上传仍由人工执行——脚本不掌握也不该掌握私钥之外的凭据。

用法示例：
  python scripts/release-catalog.py --key E:/Dounai-ai/Dounai_launcher-keys/K5-2026-11-13/k5.private.key
  python scripts/release-catalog.py --key <私钥> --modules-dir D:/pkgs   # 用本地包副本
  python scripts/release-catalog.py --key <私钥> --from catalog.json     # 用本地底稿
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import tempfile
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
LIVE_CATALOG_URL = (
    "https://downloads.sourceforge.net/project/dounai-releases/catalog.json"
)
UPLOAD_TARGET = "tonyjh07@frs.sourceforge.net:/home/frs/project/dounai-releases/catalog.json"

FAIL = "\033[31m"
OK = "\033[32m"
RESET = "\033[0m"


def die(message: str) -> None:
    print(f"{FAIL}中止：{message}{RESET}", file=sys.stderr)
    sys.exit(1)


def fetch(url: str, dest: Path, host_suffixes_note: str = "") -> None:
    request = urllib.request.Request(url, headers={"User-Agent": "dounai-release-catalog/1"})
    with urllib.request.urlopen(request, timeout=120) as response, dest.open("wb") as out:
        while chunk := response.read(1 << 16):
            out.write(chunk)


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def signer_binary(explicit: str | None) -> Path:
    if explicit:
        candidate = Path(explicit)
        if not candidate.is_file():
            die(f"指定的签发工具不存在：{candidate}")
        return candidate
    for relative in ("src-tauri/target/release/sign_catalog.exe", "src-tauri/target/debug/sign_catalog.exe"):
        candidate = REPO / relative
        if candidate.is_file():
            return candidate
    die(
        "找不到 sign_catalog.exe。先在 src-tauri 下执行 "
        "cargo build --release --bin sign_catalog，或用 --signer 指定路径。"
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--key", required=True, help="K5 私钥文件路径")
    parser.add_argument("--key-id", default="K5")
    parser.add_argument("--from", dest="from_file", help="用本地 catalog 作底稿（默认拉现网）")
    parser.add_argument("--modules-dir", help="模块包本地目录；缺省时逐条目从现网下载")
    parser.add_argument("--days", type=int, default=7, help="有效期天数，上限 7")
    parser.add_argument("--sequence", type=int, help="显式指定 sequence（默认现网值 +1）")
    parser.add_argument("--update-hashes", action="store_true", help="包哈希/大小与目录不符时改写目录而不是中止")
    parser.add_argument("--signer", help="sign_catalog 可执行文件路径")
    parser.add_argument("--out", default="catalog.json", help="签名产物输出路径")
    parser.add_argument("--pubkey", help="K5 公钥文件（复验用；缺省自动生成临时文件）")
    parser.add_argument("--skip-download-check", action="store_true", help="跳过包哈希核对（不推荐，仅限底稿刚核对过）")
    args = parser.parse_args()

    if args.days > 7:
        die("catalog 有效期上限 7 天，--days 不能更大")

    with tempfile.TemporaryDirectory(prefix="dounai-release-") as tmp:
        tmpdir = Path(tmp)

        # 1) 底稿
        if args.from_file:
            base = Path(args.from_file)
            if not base.is_file():
                die(f"底稿不存在：{base}")
            doc = json.loads(base.read_text(encoding="utf-8"))
            print(f"底稿：本地 {base}")
        else:
            base = tmpdir / "catalog-old.json"
            try:
                fetch(LIVE_CATALOG_URL, base)
            except Exception as error:  # noqa: BLE001
                die(f"拉取现网 catalog 失败：{error}")
            doc = json.loads(base.read_text(encoding="utf-8"))
            print(f"底稿：现网 sequence={doc.get('sequence')} expires={doc.get('expires')}")

        # 2) 时间窗与序号
        now = datetime.now(timezone.utc).replace(microsecond=0)
        generated_at = now.strftime("%Y-%m-%dT%H:%M:%SZ")
        expires = (now + timedelta(days=args.days)).strftime("%Y-%m-%dT%H:%M:%SZ")
        old_sequence = int(doc.get("sequence") or 0)
        doc["sequence"] = args.sequence if args.sequence is not None else old_sequence + 1
        doc["generated_at"] = generated_at
        doc["expires"] = expires
        print(f"序号：{old_sequence} -> {doc['sequence']}；窗口 {generated_at} -> {expires}（{args.days} 天）")

        # 3) 包哈希核对
        if not args.skip_download_check:
            modules_dir = Path(args.modules_dir) if args.modules_dir else None
            if modules_dir is not None and not modules_dir.is_dir():
                die(f"--modules-dir 不存在：{modules_dir}")
            changed = 0
            for entry in doc.get("modules", []):
                module_id = entry.get("module_id", "?")
                urls = [s.get("url") for s in entry.get("sources", []) if s.get("url")]
                if not urls:
                    die(f"{module_id} 没有可用的下载地址")
                if modules_dir is not None:
                    local = modules_dir / Path(urls[0]).name
                    if not local.is_file():
                        die(f"{module_id} 的本地副本缺失：{local}")
                    package = local
                else:
                    package = tmpdir / Path(urls[0]).name
                    if not package.is_file():
                        print(f"下载 {module_id} ...")
                        try:
                            fetch(urls[0], package)
                        except Exception as error:  # noqa: BLE001
                            die(f"{module_id} 下载失败：{error}")
                actual_hash = sha256_of(package)
                actual_size = package.stat().st_size
                declared_hash = str(entry.get("sha256", "")).removeprefix("sha256:")
                declared_size = int(entry.get("size") or 0)
                if actual_hash != declared_hash or actual_size != declared_size:
                    if not args.update_hashes:
                        die(
                            f"{module_id} 与目录声明不符：哈希 {declared_hash[:12]}…/{declared_size}B "
                            f"vs 实际 {actual_hash[:12]}…/{actual_size}B。"
                            "包若已更新，用 --update-hashes 改写目录并重新签发。"
                        )
                    entry["sha256"] = f"sha256:{actual_hash}"
                    entry["size"] = actual_size
                    changed += 1
                    print(f"{OK}改写 {module_id}：sha256:{actual_hash[:12]}… size={actual_size}{RESET}")
                else:
                    print(f"{OK}核对 {module_id}：哈希与大小一致{RESET}")
            if changed:
                print(f"注意：{changed} 个条目的哈希被改写，请确认这是有意为之的包更新。")

        # 4) 签名（工具内含 source_id 预检与签后自验）
        unsigned = tmpdir / "catalog-unsigned.json"
        unsigned.write_text(json.dumps(doc, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        signer = signer_binary(args.signer)
        result = subprocess.run(
            [str(signer), "sign", "--key", args.key, "--catalog", str(unsigned), "--key-id", args.key_id],
            capture_output=True,
            text=True,
        )
        sys.stdout.write(result.stdout)
        if result.returncode != 0:
            sys.stderr.write(result.stderr)
            die("sign_catalog 预检或签名未通过（见上方错误）")

        # 5) K5 公钥复验
        pubkey_path = Path(args.pubkey) if args.pubkey else tmpdir / "k5.pubkey.b64"
        if not args.pubkey:
            signer_fp = json.loads(unsigned.read_text(encoding="utf-8"))["signature"]["signer_fp"]
            import base64

            # sign_catalog verify 需要公钥文件；从 keys/K5.json 取官方公钥。
            keys_file = REPO / "keys" / "K5.json"
            if keys_file.is_file():
                pubkey_b64 = json.loads(keys_file.read_text(encoding="utf-8"))["pubkey"]
                pubkey_path.write_text(pubkey_b64 + "\n", encoding="utf-8")
            else:
                die(f"找不到 {keys_file}，请用 --pubkey 显式指定 K5 公钥文件")
        result = subprocess.run(
            [str(signer), "verify", "--pubkey", str(pubkey_path), "--catalog", str(unsigned)],
            capture_output=True,
            text=True,
        )
        sys.stdout.write(result.stdout)
        if result.returncode != 0:
            sys.stderr.write(result.stderr)
            die("复验未通过")

        # 6) 产物与上传命令
        out_path = Path(args.out).resolve()
        out_path.write_text(unsigned.read_text(encoding="utf-8"), encoding="utf-8")
        print(f"{OK}签名产物：{out_path}{RESET}")
        print("上传（人工执行）：")
        print(f"  sftp {UPLOAD_TARGET.rsplit(':')[0]} <<'EOF'")
        print(f"put {out_path.as_posix()} /home/frs/project/dounai-releases/catalog.json")
        print("EOF")


if __name__ == "__main__":
    main()
