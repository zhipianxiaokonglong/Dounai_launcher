//! 定位并校验随包自带的 uv。系统 PATH 里的 uv 不接管，版本失控会让离线验收失去意义。
//!
//! 程序根缺文件时按固定 URL 下载 release 资产，解出 uv.exe 后用 install.json 的哈希复核。
//! 单一固定 URL 不引入 catalog，M2 的双源不受影响。

use std::fs;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::process::Command;

use super::error::{EnvError, EnvErrorKind};
use super::runner::{UvRequest, UvRunner};
use super::EnvOptions;
use crate::install::InstallState;
use crate::paths::Paths;
use crate::state;

pub const UV_VERSION: &str = "0.13.7";
pub const UV_MIN_VERSION: &str = "0.11.8";

/// 随包自带的 `bin/uv.exe` 的锚。`install.json` 的 `uv_sha256` 由它初始化，
/// 启动器每次启动都按这个值校正，所以升 uv 必须同时改这三行。
///
/// 取值来源：阿里云 PyPI 的 `uv-0.13.7-py3-none-win_amd64.whl`（wheel 本身
/// `sha256:2835e2ab397d0db86323b9975dce75ecd76aa9cfa0d6a2aea84734aae7f8c9b3`）
/// 内 `uv-0.13.7.data/scripts/uv.exe`，31 803 184 字节。
pub const UV_SHA256: &str = "sha256:1791d7a9ed5b6df13ccf80f102e025c333010d96326b4ff0262aa3b578835c5a";
pub const UV_BYTES: u64 = 31_803_184;

/// 首选：GitHub release 资产（官方、zip 里直接是 `uv.exe`）。
pub const UV_DOWNLOAD_URL: &str =
    "https://github.com/astral-sh/uv/releases/download/0.13.7/uv-x86_64-pc-windows-msvc.zip";

/// 备选：PyPI 镜像上的同名 wheel。
///
/// 目标机器上 github.com 不可达（实测超时），随包又缺 `bin/uv.exe` 时这条是唯一
/// 的补齐路径。wheel 里 `uv.exe` 在 `uv-<版本>.data/scripts/` 下，`extract_uv_exe`
/// 按「任意层级下的 uv.exe」匹配，两种布局共用一套解包。
pub const UV_MIRROR_URL: &str = "https://mirrors.aliyun.com/pypi/packages/8e/4f/041b59d2a38807a3e31ef41064cf3e45d2752fe0409d89040d52778049ae/uv-0.13.7-py3-none-win_amd64.whl";

const MAX_UV_EXE_BYTES: u64 = 100 * 1024 * 1024;

/// 补齐 uv 时的取数顺序：官方资产优先，镜像兜底。任一源成功即停。
pub const UV_DOWNLOAD_URLS: [&str; 2] = [UV_DOWNLOAD_URL, UV_MIRROR_URL];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UvBinary {
    pub path: PathBuf,
    pub version: String,
    pub sha256: String,
    /// 本次实际下载的 release 资产字节数；复用程序根里的 uv 时为 0。
    pub downloaded_bytes: u64,
}

pub trait UvFetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, EnvError>;
}

/// Windows 10 1803 起自带 curl.exe，用它做零依赖的 HTTPS 下载。完整性锚是最终 exe 的 sha256。
pub struct CurlUvFetcher;

impl UvFetcher for CurlUvFetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, EnvError> {
        let tmp = std::env::temp_dir().join(format!("dounai-uv-{}.zip", std::process::id()));
        let mut command = Command::new("curl");
        command
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--max-time",
                "180",
                "--output",
            ])
            .arg(&tmp)
            .arg(url);
        // 不弹控制台窗口（下载可能要跑一会儿，黑窗会让人以为卡住了）。
        super::console::hide_console(&mut command);
        let result = command.output();
        let output = result.map_err(|e| {
            EnvError::new(EnvErrorKind::UvMissing, "", format!("无法启动 curl：{e}"))
        })?;
        if !output.status.success() {
            let _ = fs::remove_file(&tmp);
            return Err(EnvError::new(
                EnvErrorKind::UvMissing,
                "",
                format!("下载 uv 失败：{}", String::from_utf8_lossy(&output.stderr)),
            ));
        }
        let bytes = fs::read(&tmp).map_err(|e| {
            EnvError::new(
                EnvErrorKind::UvMissing,
                "",
                format!("读取下载文件失败：{e}"),
            )
        });
        let _ = fs::remove_file(&tmp);
        bytes
    }
}

fn state_error(kind: EnvErrorKind, id: &str, message: impl Into<String>) -> EnvError {
    EnvError::new(kind, id, message)
}

/// 候选顺序：install.json 记录的位置、程序根 `bin/uv.exe`、数据根里的下载副本。
pub fn locate_uv(
    paths: &Paths,
    install: &InstallState,
    options: &EnvOptions,
    runner: &dyn UvRunner,
    fetcher: &dyn UvFetcher,
    instance_id: &str,
) -> Result<UvBinary, EnvError> {
    let expected = &install.uv_sha256;
    // 程序根是只读安装产物，优先于 install.json 里用户可写目录下记录的位置。
    let candidates = [
        paths.bundled_uv_path(),
        install.uv_path.clone(),
        paths.downloaded_uv_path(),
    ];
    let mut found: Option<PathBuf> = None;
    let mut mismatch: Option<(PathBuf, String)> = None;
    for path in &candidates {
        if !path.exists() {
            continue;
        }
        let actual = state::file_sha256_prefixed(path).map_err(|e| {
            state_error(
                EnvErrorKind::UvMissing,
                instance_id,
                format!("无法计算 uv 哈希：{e}"),
            )
        })?;
        if actual != *expected {
            // 单个候选被替换时先试下一个；全部不符再硬拒，不用下载掩盖篡改。
            mismatch = Some((path.clone(), actual));
            continue;
        }
        found = Some(path.clone());
        break;
    }
    if found.is_none() {
        if let Some((path, actual)) = mismatch {
            return Err(state_error(
                EnvErrorKind::UvMissing,
                instance_id,
                format!(
                    "所有 uv 候选的哈希都与 install.json 不符，例如 {}：期望 {expected}，实际 {actual}",
                    path.display()
                ),
            ));
        }
    }

    let (path, downloaded_bytes) = match found {
        Some(p) => (p, 0),
        None => {
            if options.offline || !options.allow_uv_download {
                return Err(state_error(
                    EnvErrorKind::UvMissing,
                    instance_id,
                    format!(
                        "程序根缺少 bin/uv.exe（期望 {expected}），当前配置不允许联网补齐"
                    ),
                ));
            }
            let mut last_error: Option<EnvError> = None;
            let mut fetched: Option<Vec<u8>> = None;
            for url in UV_DOWNLOAD_URLS {
                match fetcher.fetch(url) {
                    Ok(bytes) => {
                        fetched = Some(bytes);
                        break;
                    }
                    // 逐个源重试：GitHub 在部分网络下完全不可达，不能因为第一源失败就放弃。
                    Err(error) => last_error = Some(error),
                }
            }
            let bytes = match fetched {
                Some(bytes) => bytes,
                None => {
                    return Err(last_error.unwrap_or_else(|| {
                        state_error(
                            EnvErrorKind::UvMissing,
                            instance_id,
                            "程序根缺少 bin/uv.exe，且没有可用的下载源",
                        )
                    }))
                }
            };
            let downloaded_bytes = bytes.len() as u64;
            let exe = extract_uv_exe(&bytes, instance_id)?;
            let target = paths.downloaded_uv_path();
            state::write_bytes_atomic(&target, &exe).map_err(|e| {
                state_error(
                    EnvErrorKind::UvMissing,
                    instance_id,
                    format!("写入 {} 失败：{e}", target.display()),
                )
            })?;
            let actual = state::file_sha256_prefixed(&target).map_err(|e| {
                state_error(
                    EnvErrorKind::UvMissing,
                    instance_id,
                    format!("校验哈希失败：{e}"),
                )
            })?;
            if actual != *expected {
                let _ = fs::remove_file(&target);
                return Err(state_error(
                    EnvErrorKind::UvMissing,
                    instance_id,
                    format!("下载的 uv.exe 哈希不符：期望 {expected}，实际 {actual}"),
                ));
            }
            (target, downloaded_bytes)
        }
    };

    let mut request = UvRequest::new(&path, instance_id, &["--version"]);
    request.cwd = paths.data_root.clone();
    let output = runner
        .run(&request)
        .map_err(|e| e.reclassify_if_generic(EnvErrorKind::UvVersion))?;
    let version = parse_version(&output.stdout, instance_id)?;
    if version != UV_VERSION {
        return Err(EnvError::new(
            EnvErrorKind::UvVersion,
            instance_id,
            format!("uv 版本应为 {UV_VERSION}，实际 {version}"),
        )
        .with_command_line(&request.command_line())
        .with_output_text(&output.combined()));
    }
    Ok(UvBinary {
        path,
        version,
        sha256: expected.clone(),
        downloaded_bytes,
    })
}

/// 从 `uv --version` 的输出里取第一段 x.y.z。
pub fn parse_version(stdout: &str, instance_id: &str) -> Result<String, EnvError> {
    for token in stdout.split_whitespace() {
        let cleaned = token.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
        let parts: Vec<&str> = cleaned.split('.').collect();
        if parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        {
            return Ok(cleaned.to_string());
        }
    }
    Err(EnvError::new(
        EnvErrorKind::UvVersion,
        instance_id,
        "无法从 uv --version 解析版本",
    )
    .with_output_text(stdout))
}

/// 只解包 `uv.exe`，顺带挡住解压炸弹。
pub fn extract_uv_exe(bytes: &[u8], instance_id: &str) -> Result<Vec<u8>, EnvError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| {
        EnvError::new(
            EnvErrorKind::UvMissing,
            instance_id,
            format!("uv 资产不是合法 zip：{e}"),
        )
    })?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| {
            EnvError::new(
                EnvErrorKind::UvMissing,
                instance_id,
                format!("读取 zip 条目失败：{e}"),
            )
        })?;
        let name = entry
            .name()
            .map_err(|e| {
                EnvError::new(
                    EnvErrorKind::UvMissing,
                    instance_id,
                    format!("zip 条目名非法：{e}"),
                )
            })?
            .replace('\\', "/");
        if name == "uv.exe" || name.ends_with("/uv.exe") {
            if entry.size() > MAX_UV_EXE_BYTES {
                return Err(EnvError::new(
                    EnvErrorKind::UvMissing,
                    instance_id,
                    format!("uv.exe 解压后超过 {} 字节", MAX_UV_EXE_BYTES),
                ));
            }
            let mut out = Vec::new();
            entry.read_to_end(&mut out).map_err(|e| {
                EnvError::new(
                    EnvErrorKind::UvMissing,
                    instance_id,
                    format!("解包失败：{e}"),
                )
            })?;
            if out.is_empty() {
                return Err(EnvError::new(
                    EnvErrorKind::UvMissing,
                    instance_id,
                    "zip 内的 uv.exe 为空",
                ));
            }
            return Ok(out);
        }
    }
    Err(EnvError::new(
        EnvErrorKind::UvMissing,
        instance_id,
        "uv 资产里没有 uv.exe",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{InstallMode as Mode, Paths};
    use crate::security::sha256_hex;
    use std::io::Write;

    fn zip_with_uv(content: &[u8]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("uv-x86_64-pc-windows-msvc/uv.exe", opts)
                .unwrap();
            zip.write_all(content).unwrap();
            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    struct FakeFetcher(Vec<u8>);
    impl UvFetcher for FakeFetcher {
        fn fetch(&self, _url: &str) -> Result<Vec<u8>, EnvError> {
            Ok(self.0.clone())
        }
    }

    fn setup(exe: Option<&[u8]>, hash: &str) -> (tempfile::TempDir, Paths, InstallState) {
        let dir = tempfile::tempdir().unwrap();
        let install_root = dir.path().join("app");
        let data_root = dir.path().join("data");
        fs::create_dir_all(&install_root).unwrap();
        fs::create_dir_all(&data_root).unwrap();
        if let Some(bytes) = exe {
            fs::create_dir_all(install_root.join("bin")).unwrap();
            fs::write(install_root.join("bin").join("uv.exe"), bytes).unwrap();
        }
        let paths = Paths::new(Mode::CurrentUser, install_root, data_root).unwrap();
        let install = InstallState {
            schema_version: 1,
            paths: paths.clone(),
            uv_path: paths.bundled_uv_path(),
            uv_sha256: hash.into(),
            uv_version: UV_VERSION.into(),
            written_at: "2026-11-10T00:00:00Z".into(),
        };
        (dir, paths, install)
    }

    #[test]
    fn parses_uv_version_output() {
        assert_eq!(parse_version("uv 0.13.7 (mock)\n", "x").unwrap(), "0.13.7");
        assert_eq!(
            parse_version(
                "uv 0.11.7 (9d177269e 2026-04-15 x86_64-pc-windows-msvc)",
                "x"
            )
            .unwrap(),
            "0.11.7"
        );
        assert_eq!(
            parse_version("no version", "x").unwrap_err().kind,
            EnvErrorKind::UvVersion
        );
    }

    #[test]
    fn extracts_only_uv_exe_from_release_asset() {
        let zip = zip_with_uv(b"real uv bytes");
        assert_eq!(extract_uv_exe(&zip, "x").unwrap(), b"real uv bytes");
        assert_eq!(
            extract_uv_exe(b"not a zip", "x").unwrap_err().kind,
            EnvErrorKind::UvMissing
        );
    }

    #[test]
    fn locate_uses_bundled_binary_and_checks_version() {
        let exe = b"uv binary";
        let hash = format!("sha256:{}", sha256_hex(exe));
        let (_dir, paths, install) = setup(Some(exe), &hash);
        let runner = super::super::runner::MockUvRunner::default();
        let fetcher = FakeFetcher(Vec::new());
        let uv = locate_uv(
            &paths,
            &install,
            &EnvOptions::default(),
            &runner,
            &fetcher,
            "mod@a",
        )
        .unwrap();
        assert_eq!(uv.path, paths.bundled_uv_path());
        assert_eq!(uv.version, UV_VERSION);
        assert_eq!(uv.downloaded_bytes, 0);
        assert_eq!(runner.calls()[0].args, vec!["--version"]);
        assert_eq!(runner.calls()[0].cwd, paths.data_root);
    }

    #[test]
    fn locate_rejects_version_below_pinned() {
        let exe = b"uv binary";
        let hash = format!("sha256:{}", sha256_hex(exe));
        let (_dir, paths, install) = setup(Some(exe), &hash);
        let runner = super::super::runner::MockUvRunner::new(super::super::runner::MockConfig {
            version: "uv 0.11.7 (mock)".into(),
            ..Default::default()
        });
        let e = locate_uv(
            &paths,
            &install,
            &EnvOptions::default(),
            &runner,
            &FakeFetcher(Vec::new()),
            "mod@a",
        )
        .unwrap_err();
        assert_eq!(e.kind, EnvErrorKind::UvVersion);
    }

    #[test]
    fn locate_rejects_hash_mismatch() {
        let (_dir, paths, install) =
            setup(Some(b"uv binary"), &format!("sha256:{}", "a".repeat(64)));
        let runner = super::super::runner::MockUvRunner::default();
        let e = locate_uv(
            &paths,
            &install,
            &EnvOptions::default(),
            &runner,
            &FakeFetcher(Vec::new()),
            "mod@a",
        )
        .unwrap_err();
        assert_eq!(e.kind, EnvErrorKind::UvMissing);
        assert!(e.message.contains("哈希"));
    }

    #[test]
    fn locate_downloads_when_bundled_is_missing() {
        let exe = b"downloaded uv";
        let hash = format!("sha256:{}", sha256_hex(exe));
        let (_dir, paths, install) = setup(None, &hash);
        let runner = super::super::runner::MockUvRunner::default();
        let asset = zip_with_uv(exe);
        let asset_bytes = asset.len() as u64;
        let uv = locate_uv(
            &paths,
            &install,
            &EnvOptions::default(),
            &runner,
            &FakeFetcher(asset),
            "mod@a",
        )
        .unwrap();
        assert_eq!(uv.path, paths.downloaded_uv_path());
        assert_eq!(uv.downloaded_bytes, asset_bytes);
        assert_eq!(fs::read(&uv.path).unwrap(), exe);
    }

    #[test]
    fn offline_run_does_not_download() {
        let exe = b"uv binary";
        let hash = format!("sha256:{}", sha256_hex(exe));
        let (_dir, paths, install) = setup(None, &hash);
        let runner = super::super::runner::MockUvRunner::default();
        let options = EnvOptions {
            offline: true,
            ..EnvOptions::default()
        };
        let e = locate_uv(
            &paths,
            &install,
            &options,
            &runner,
            &FakeFetcher(zip_with_uv(exe)),
            "mod@a",
        )
        .unwrap_err();
        assert_eq!(e.kind, EnvErrorKind::UvMissing);
        assert!(!paths.downloaded_uv_path().exists());
    }
}
