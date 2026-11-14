//! catalog 签名工具。每个源用自己的 catalog 签名钥，官方默认 K5。
//!
//! 用法：
//! ```text
//! dounai-sign-catalog sign   --key <私钥> --catalog <catalog.json> [--key-id K5] [--out <路径>]
//! dounai-sign-catalog verify --pubkey <公钥> --catalog <catalog.json>
//! ```
//!
//! `sign` 会把 `signature.signer_fp` 写成该私钥对应公钥的完整指纹；
//! 启动器拿 sources.json 里该源登记的公钥验签，并比对这个指纹。
//! 工具会检查有效期不超过 7 天。签名钥泄露时靠短过期窗口加带外公告与
//! 强制升级启动器处置，catalog 的 `revoked` 不能单独撤销签名钥。

use std::path::Path;
use std::process::ExitCode;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use dounai_launcher::release::{load_signing_key, preflight_catalog, sign_catalog_doc, ReleaseError};
use dounai_launcher::signature::verify_manifest;
use launcher_core::m2::sources::BUILTIN_SOURCE_IDS;
use serde_json::Value;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        return ExitCode::from(2);
    }
    let result = match args[0].as_str() {
        "sign" => cmd_sign(&args[1..]),
        "verify" => cmd_verify(&args[1..]),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => Err(ReleaseError::BadCatalog(format!("未知子命令：{other}"))),
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
        "Dounai catalog 签名工具\n\
         \n\
         用法:\n\
         \x20 dounai-sign-catalog sign   --key <私钥> --catalog <catalog.json> [--key-id K5] [--out <路径>]\n\
         \x20 dounai-sign-catalog verify --pubkey <公钥> --catalog <catalog.json>\n\
         \n\
         说明:\n\
         \x20 sign 默认覆盖原文件，指定 --out 则写到新文件。\n\
         \x20 --key-id 只是展示用标识，社区源可写自己的；判定只认 signer_fp 与公钥指纹。\n\
         \x20 有效期上限 7 天；catalog 签名钥与包签名钥不得使用同一把密钥。"
    );
}

/// 收集可重复出现的旗标值（如 `--allow-source a --allow-source b`）。
fn flags(args: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
            }
        }
        i += 1;
    }
    out
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
    flag(args, name).ok_or_else(|| ReleaseError::BadCatalog(format!("缺少参数 {name}")))
}

fn read_json(path: impl AsRef<Path>) -> Result<Value, ReleaseError> {
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

fn cmd_sign(args: &[String]) -> Result<(), ReleaseError> {
    let catalog_path = require(args, "--catalog")?;
    let sk = load_signing_key(Path::new(&require(args, "--key")?))?;
    let mut catalog = read_json(&catalog_path)?;
    let extras = flags(args, "--allow-source");
    let mut allowed: Vec<&str> = BUILTIN_SOURCE_IDS.to_vec();
    allowed.extend(extras.iter().map(String::as_str));
    preflight_catalog(&catalog, &allowed)?;
    let key_id = flag(args, "--key-id").unwrap_or_else(|| "K5".into());
    sign_catalog_doc(&mut catalog, &sk, &key_id)?;

    // 签完立即用同一把公钥验一遍，避免写出验不过的索引。
    let pk = B64.encode(sk.verifying_key().to_bytes());
    verify_manifest(&catalog, &pk)?;

    let out = flag(args, "--out").unwrap_or_else(|| catalog_path.clone());
    let text = serde_json::to_string_pretty(&catalog)?;
    std::fs::write(&out, format!("{text}\n"))?;
    println!("已签名：{out}");
    println!("  sequence      = {}", catalog["sequence"]);
    println!("  generated_at  = {}", catalog["generated_at"]);
    println!("  expires       = {}", catalog["expires"]);
    println!("  key_id        = {key_id}");
    println!("  signer_fp     = {}", catalog["signature"]["signer_fp"]);
    println!("  pubkey        = {pk}");
    Ok(())
}

fn cmd_verify(args: &[String]) -> Result<(), ReleaseError> {
    let pk = std::fs::read_to_string(require(args, "--pubkey")?)?
        .trim()
        .to_string();
    let catalog_path = require(args, "--catalog")?;
    let catalog = read_json(&catalog_path)?;
    verify_manifest(&catalog, &pk)?;
    let extras = flags(args, "--allow-source");
    let mut allowed: Vec<&str> = BUILTIN_SOURCE_IDS.to_vec();
    allowed.extend(extras.iter().map(String::as_str));
    preflight_catalog(&catalog, &allowed)?;
    let fp = launcher_core::m2::fingerprint(&pk)
        .map_err(|e| ReleaseError::BadCatalog(e.to_string()))?;
    let signer_fp = catalog["signature"]["signer_fp"]
        .as_str()
        .ok_or_else(|| ReleaseError::BadCatalog("catalog 缺少 signer_fp".into()))?;
    if signer_fp != fp {
        return Err(ReleaseError::BadCatalog(format!(
            "signer_fp 与公钥指纹不符：文档 {signer_fp}，公钥 {fp}"
        )));
    }
    println!("验签通过：{catalog_path}");
    println!("  sequence  = {}", catalog["sequence"]);
    println!("  expires   = {}", catalog["expires"]);
    println!("  signer_fp = {fp}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_parser_reads_value() {
        let args: Vec<String> = ["--key", "k.b64", "--catalog", "c.json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(flag(&args, "--key").as_deref(), Some("k.b64"));
        assert_eq!(flag(&args, "--out"), None);
    }

    #[test]
    fn flags_collects_repeated_values() {
        let args: Vec<String> = ["--allow-source", "a", "--allow-source", "b"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(flags(&args, "--allow-source"), vec!["a", "b"]);
        assert!(flags(&args, "--key").is_empty());
    }

    #[test]
    fn require_reports_missing_flag() {
        let args: Vec<String> = vec!["--key".into()];
        assert!(require(&args, "--catalog").is_err());
    }
}
