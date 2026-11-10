//! catalog 签名工具。签名钥是 K5，与包签名钥 K1 分离。
//!
//! 用法：
//! ```text
//! dounai-sign-catalog sign   --key <K5 私钥> --catalog <catalog.json> [--out <路径>]
//! dounai-sign-catalog verify --pubkey <K5 公钥> --catalog <catalog.json>
//! ```
//!
//! 工具会检查有效期不超过 7 天。K5 泄露时靠短过期窗口加带外公告与强制升级
//! 启动器处置，catalog 的 `revoked` 不能单独撤销 K5。

use std::path::Path;
use std::process::ExitCode;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use dounai_launcher::release::{check_catalog, load_signing_key, sign_catalog_doc, ReleaseError};
use dounai_launcher::signature::verify_manifest;
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
         \x20 dounai-sign-catalog sign   --key <K5 私钥> --catalog <catalog.json> [--out <路径>]\n\
         \x20 dounai-sign-catalog verify --pubkey <K5 公钥> --catalog <catalog.json>\n\
         \n\
         说明:\n\
         \x20 sign 默认覆盖原文件，指定 --out 则写到新文件。\n\
         \x20 有效期上限 7 天；K1 与 K5 不得使用同一把密钥。"
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
    check_catalog(&catalog)?;
    sign_catalog_doc(&mut catalog, &sk)?;

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
    println!("  key_id        = K5");
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
    check_catalog(&catalog)?;
    println!("验签通过：{catalog_path}");
    println!("  sequence = {}", catalog["sequence"]);
    println!("  expires  = {}", catalog["expires"]);
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
    fn require_reports_missing_flag() {
        let args: Vec<String> = vec!["--key".into()];
        assert!(require(&args, "--catalog").is_err());
    }
}
