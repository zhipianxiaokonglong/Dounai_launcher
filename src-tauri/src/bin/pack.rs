//! zip 发布包工具：生成密钥、打包签名、发布前复核。
//!
//! 用法：
//! ```text
//! dounai-pack keygen --out-private priv.b64 --out-public pub.b64
//! dounai-pack pack   --dir <源目录> --key <K1 私钥> --out <zip> [--key-id K1]
//! dounai-pack verify --pubkey <公钥文件> --zip <zip>
//! ```
//!
//! 源目录里必须有一份 `manifest.json`，包内文件名固定为 `manifest.json`。
//! 工具不改写源目录，签名后的 manifest 只写进 zip。

use std::path::Path;
use std::process::ExitCode;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use dounai_launcher::release::{
    check_manifest_basics, generate_keypair, load_signing_key, scan_files, sign_manifest,
    verify_zip, write_zip, ReleaseError,
};
use dounai_launcher::signature::SUPPORTED_ALGO;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        return ExitCode::from(2);
    }
    let result = match args[0].as_str() {
        "keygen" => cmd_keygen(&args[1..]),
        "pack" => cmd_pack(&args[1..]),
        "verify" => cmd_verify(&args[1..]),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => Err(ReleaseError::BadManifest(format!("未知子命令：{other}"))),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("错误：{e}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "Dounai 模块发布包工具\n\
         \n\
         用法:\n\
         \x20 dounai-pack keygen --out-private <路径> --out-public <路径>\n\
         \x20 dounai-pack pack   --dir <源目录> --key <K1 私钥> --out <zip> [--key-id K1]\n\
         \x20 dounai-pack verify --pubkey <公钥文件> --zip <zip>\n\
         \n\
         说明:\n\
         \x20 pack 输出 catalog 条目需要的 sha256、size 与 publisher_fp。\n\
         \x20 私钥不进仓库，也不要写进 CI 变量。"
    );
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            return args.get(i + 1).cloned();
        }
        i += 1;
    }
    None
}

fn require(args: &[String], name: &str) -> Result<String, ReleaseError> {
    flag(args, name).ok_or_else(|| ReleaseError::BadManifest(format!("缺少参数 {name}")))
}

fn read_json(path: impl AsRef<Path>) -> Result<Value, ReleaseError> {
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

fn pubkey_fingerprint(pubkey_b64: &str) -> Result<String, ReleaseError> {
    let raw = B64
        .decode(pubkey_b64.as_bytes())
        .map_err(|e| ReleaseError::BadKey(e.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(&raw);
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn cmd_keygen(args: &[String]) -> Result<(), ReleaseError> {
    let priv_path = require(args, "--out-private")?;
    let pub_path = require(args, "--out-public")?;
    let (sk, pk) = generate_keypair()?;
    std::fs::write(&priv_path, B64.encode(sk.to_bytes()))?;
    std::fs::write(&pub_path, &pk)?;
    println!("已生成密钥对");
    println!("  私钥：{priv_path}（切勿提交或外传）");
    println!("  公钥：{pub_path}");
    println!("  公钥值：{pk}");
    println!("  指纹：{}", pubkey_fingerprint(&pk)?);
    Ok(())
}

fn cmd_pack(args: &[String]) -> Result<(), ReleaseError> {
    let dir = Path::new(&require(args, "--dir")?).to_path_buf();
    let key_path = Path::new(&require(args, "--key")?).to_path_buf();
    let out = Path::new(&require(args, "--out")?).to_path_buf();
    let key_id = flag(args, "--key-id").unwrap_or_else(|| "K1".to_string());

    let manifest_path = dir.join("manifest.json");
    if !manifest_path.is_file() {
        return Err(ReleaseError::BadManifest(format!(
            "源目录缺少 manifest.json：{}",
            dir.display()
        )));
    }
    let mut manifest = read_json(&manifest_path)?;
    check_manifest_basics(&manifest)?;
    let sk = load_signing_key(&key_path)?;
    let files = scan_files(&dir)?;
    if files.is_empty() {
        return Err(ReleaseError::BadManifest(
            "源目录除 manifest.json 外没有文件".into(),
        ));
    }
    manifest["files"] = json!(files);
    sign_manifest(&mut manifest, &sk, &key_id)?;
    write_zip(
        &out,
        &dir,
        &serde_json::to_string_pretty(&manifest)?,
        &files,
    )?;

    // 发布机自检：刚打出的包必须能通过完整验签与逐文件哈希。
    let pk = B64.encode(sk.verifying_key().to_bytes());
    let report = verify_zip(&out, &pk)?;
    println!("已打包：{}", out.display());
    println!("  module_id      = {}", report.module_id);
    println!("  version        = {}", report.version);
    println!(
        "  role           = {}",
        manifest["role"].as_str().unwrap_or("")
    );
    println!("  文件数         = {}", report.file_count);
    println!("  zip sha256     = {}", report.sha256);
    println!("  zip size       = {}", report.size);
    println!("  key_id         = {key_id}");
    println!("  algo           = {SUPPORTED_ALGO}");
    println!("  pubkey         = {pk}");
    println!("  publisher_fp   = {}", pubkey_fingerprint(&pk)?);
    Ok(())
}

fn cmd_verify(args: &[String]) -> Result<(), ReleaseError> {
    let pk = std::fs::read_to_string(require(args, "--pubkey")?)?
        .trim()
        .to_string();
    let zip_path = Path::new(&require(args, "--zip")?).to_path_buf();
    let report = verify_zip(&zip_path, &pk)?;
    println!("验签通过：{}", zip_path.display());
    println!("  module_id  = {}", report.module_id);
    println!("  version    = {}", report.version);
    println!("  文件数     = {}", report.file_count);
    println!("  zip sha256 = {}", report.sha256);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_sha256_of_public_key_bytes() {
        let raw = [7u8; 32];
        let pk = B64.encode(raw);
        let got = pubkey_fingerprint(&pk).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(raw);
        assert_eq!(got, format!("sha256:{}", hex::encode(hasher.finalize())));
    }

    #[test]
    fn require_reports_missing_flag() {
        let args: Vec<String> = vec!["--dir".into()];
        assert!(require(&args, "--key").is_err());
    }

    #[test]
    fn flag_parser_reads_value() {
        let args: Vec<String> = ["--dir", "src", "--key", "k.b64"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(flag(&args, "--dir").as_deref(), Some("src"));
        assert_eq!(flag(&args, "--key-id"), None);
    }
}
