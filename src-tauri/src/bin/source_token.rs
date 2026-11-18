//! 源邀请码工具：把 catalog 地址 + 公钥生成一行可复制的邀请码，或反向解析校验。
//!
//! 用法：
//! ```text
//! dounai-source-token issue  --url <catalog 地址> --key <公钥 base64> [--name <名称>]
//! dounai-source-token issue  --url <catalog 地址> --key-file <K5.json 或 pub.b64> [--name <名称>]
//! dounai-source-token parse  --token <邀请码>
//! ```
//!
//! 编解码与运行时完全共用 `launcher_core::m2::source_token`，杜绝「工具生成的
//! 邀请码运行时解析不了」。工具只产出字符串，不写任何状态文件、不接触私钥。
//! 私钥不参与——邀请码里只有公钥，信任仍来自用户带外核对指纹后确认。

use std::path::Path;
use std::process::ExitCode;

use launcher_core::m2::source_token::{issue, parse};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_usage();
        return ExitCode::from(2);
    }
    let result = match args[0].as_str() {
        "issue" => cmd_issue(&args[1..]),
        "parse" => cmd_parse(&args[1..]),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => Err(format!("未知子命令：{other}")),
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
        "Dounai 源邀请码工具\n\
         \n\
         用法:\n\
         \x20 dounai-source-token issue --url <catalog 地址> --key <公钥 base64> [--name <名称>]\n\
         \x20 dounai-source-token issue --url <catalog 地址> --key-file <K5.json 或 pub.b64> [--name <名称>]\n\
         \x20 dounai-source-token parse --token <邀请码>\n\
         \n\
         说明:\n\
         \x20 issue 只输出一行邀请码，不写文件；公钥来自 --key 或 --key-file。\n\
         \x20 parse 打印解码结果与现算指纹，供发布前自检。\n\
         \x20 邀请码里的指纹与白名单一律不采信：运行时从公钥现算指纹、从地址派生白名单。"
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

/// 从 --key（裸 base64）或 --key-file（K5.json 的 pubkey 字段，或裸 base64 文本）取公钥。
fn resolve_pubkey(args: &[String]) -> Result<String, String> {
    if let Some(key) = flag(args, "--key") {
        return Ok(key.trim().to_string());
    }
    let path = flag(args, "--key-file").ok_or_else(|| "需要 --key 或 --key-file".to_string())?;
    let text =
        std::fs::read_to_string(Path::new(&path)).map_err(|e| format!("读取公钥文件失败：{e}"))?;
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        let json: serde_json::Value =
            serde_json::from_str(trimmed).map_err(|e| format!("公钥文件不是合法 JSON：{e}"))?;
        json.get("pubkey")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| "公钥 JSON 缺少 pubkey 字段".to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

fn cmd_issue(args: &[String]) -> Result<(), String> {
    let url = flag(args, "--url").ok_or_else(|| "缺少 --url".to_string())?;
    let pubkey = resolve_pubkey(args)?;
    let name = flag(args, "--name");
    let token = issue(&url, &pubkey, name.as_deref()).map_err(|e| e.to_string())?;
    println!("{token}");
    Ok(())
}

fn cmd_parse(args: &[String]) -> Result<(), String> {
    let token = flag(args, "--token").ok_or_else(|| "缺少 --token".to_string())?;
    let decoded = parse(&token).map_err(|e| e.to_string())?;
    let fingerprint =
        launcher_core::m2::fingerprint(&decoded.catalog_pubkey).map_err(|e| e.to_string())?;
    println!("地址：{}", decoded.base_url);
    println!("名称：{}", decoded.name.as_deref().unwrap_or("(未命名)"));
    println!("公钥：{}", decoded.catalog_pubkey);
    println!("现算指纹：{fingerprint}");
    Ok(())
}
