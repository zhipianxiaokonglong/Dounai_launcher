//! 下载器：断点续传、镜像加速、哈希校验、失败重试（依赖安装）。
//!
//! 设计要点：
//! - **断点续传**：用 `.part` 文件记录已下载字节，重试从断点继续，不从头再来；
//! - **镜像加速**：按顺序尝试多个源，任一成功即停；同一源失败会切到下一个；
//! - **哈希校验**：下载完成后校验，不匹配则删除 `.part` 并报错（不留下坏文件）；
//! - **重试**：可配置次数与退避，只对网络类错误重试，哈希不匹配**不重试**
//!   （重试一个哈希不匹配的源只会浪费时间，应直接换源）。
//!
//! HTTP 层通过 `HttpFetcher` 抽象注入，因此本模块可在无网络环境下完整测试。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 下载错误。
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("IO 失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("全部下载源均失败：{0}")]
    AllSourcesFailed(String),
    #[error("哈希校验失败：期望 {expected}，实际 {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("服务器不支持断点续传（返回 {0}，期望 206）")]
    RangeNotSupported(u16),
    #[error("下载内容为空")]
    Empty,
}

/// HTTP 响应抽象，便于注入测试实现。
pub struct FetchResponse {
    /// 状态码：200 表示全量返回，206 表示按 Range 返回
    pub status: u16,
    /// 本次返回的数据
    pub body: Vec<u8>,
}

/// HTTP 抓取抽象。
///
/// `range_from` 为 `Some(n)` 时应返回自第 n 字节起的数据与状态码 206；
/// 若服务端不支持 Range，应返回 200 与完整内容（调用方据此重置进度）。
pub trait HttpFetcher {
    fn fetch(&self, url: &str, range_from: Option<u64>) -> Result<FetchResponse, String>;
}

/// 下载配置。
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// 每个源的重试次数
    pub retries: usize,
    /// 重试退避基数（毫秒），实际等待为 base * 2^attempt
    pub backoff_ms: u64,
    /// 期望的 SHA-256（小写 hex），`None` 表示不校验
    pub expected_sha256: Option<String>,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            retries: 2,
            backoff_ms: 120,
            expected_sha256: None,
        }
    }
}

/// 下载结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DownloadReport {
    /// 最终落盘路径
    pub path: PathBuf,
    /// 文件大小
    pub bytes: u64,
    /// 实际使用的源（镜像加速结果）
    pub source: String,
    /// 断点续传复用的字节数（0 表示全新下载）
    pub resumed_from: u64,
    /// 总尝试次数（含失败）
    pub attempts: usize,
}

/// 下载到 `dest`，按顺序尝试每个源。
///
/// 成功时 `dest` 存在且哈希（若指定）已通过；失败时**不留下目标文件**，
/// 只可能残留 `.part`（供下次续传），不会留下看起来完整的坏文件。
pub fn download<F: HttpFetcher>(
    fetcher: &F,
    sources: &[String],
    dest: &Path,
    options: &DownloadOptions,
) -> Result<DownloadReport, DownloadError> {
    if sources.is_empty() {
        return Err(DownloadError::AllSourcesFailed("没有可用的下载源".into()));
    }

    let part = part_path(dest);
    let mut last_error = String::new();
    let mut attempts = 0usize;

    for source in sources {
        for attempt in 0..=options.retries {
            attempts += 1;

            // 每轮重新读取进度：上一轮失败可能已留下部分数据
            // 读取已有进度（断点续传）
            let existing = if part.exists() {
                std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0)
            } else {
                0
            };
            let range = if existing > 0 { Some(existing) } else { None };

            match fetcher.fetch(source, range) {
                Ok(response) => {
                    let resumed_from = if response.status == 200 || response.status == 206 {
                        let from = if response.status == 206 { existing } else { 0 };
                        write_part(&part, &response.body, response.status == 206)?;
                        from
                    } else {
                        last_error = format!("{source} 返回状态 {}", response.status);
                        continue;
                    };

                    let bytes = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
                    if bytes == 0 {
                        last_error = format!("{source} 返回空内容");
                        continue;
                    }

                    // 校验：不匹配直接删除 .part，换源（不重试同一个坏源）
                    if let Some(expected) = &options.expected_sha256 {
                        let data = std::fs::read(&part)?;
                        let actual = crate::security::sha256_hex(&data);
                        if !crate::security::verify_sha256(&data, expected) {
                            let _ = std::fs::remove_file(&part);
                            last_error =
                                format!("{source} 哈希不匹配（期望 {expected}，实际 {actual}）");
                            break; // 换源
                        }
                    }

                    // 落到最终位置
                    if let Some(dir) = dest.parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    std::fs::rename(&part, dest)?;

                    return Ok(DownloadReport {
                        path: dest.to_path_buf(),
                        bytes,
                        source: source.clone(),
                        resumed_from,
                        attempts,
                    });
                }
                Err(err) => {
                    last_error = format!("{source}: {err}");
                    if attempt < options.retries {
                        std::thread::sleep(std::time::Duration::from_millis(
                            options.backoff_ms * (1u64 << attempt),
                        ));
                    }
                }
            }
        }
    }

    Err(DownloadError::AllSourcesFailed(last_error))
}

fn part_path(dest: &Path) -> PathBuf {
    let mut p = dest.to_path_buf();
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());
    p.set_file_name(format!("{name}.part"));
    p
}

/// 写 `.part` 文件。`append=true` 表示续写。
fn write_part(part: &Path, data: &[u8], append: bool) -> Result<(), DownloadError> {
    if let Some(dir) = part.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(part)?;
    file.write_all(data)?;
    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// 脚本返回：状态码 + 响应体，或错误信息。
    type ScriptResult = Result<(u16, Vec<u8>), String>;
    /// 可编排的假抓取器脚本签名：(url, range_from) -> 结果。
    type FakeScript = Box<dyn Fn(&str, Option<u64>) -> ScriptResult>;

    /// 可编排的假抓取器：按 (url, range) 返回预设响应。
    struct FakeFetcher {
        calls: RefCell<Vec<(String, Option<u64>)>>,
        script: FakeScript,
    }

    impl HttpFetcher for FakeFetcher {
        fn fetch(&self, url: &str, range_from: Option<u64>) -> Result<FetchResponse, String> {
            self.calls.borrow_mut().push((url.to_string(), range_from));
            let (status, body) = (self.script)(url, range_from)?;
            Ok(FetchResponse { status, body })
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dounai-dl-{}-{}",
            name,
            std::process::id() as u128 * 1000 + now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn now() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }

    #[test]
    fn downloads_and_verifies_hash() {
        let dir = tmp("ok");
        let dest = dir.join("a.bin");
        let payload = b"hello world".to_vec();
        let hash = crate::security::sha256_hex(&payload);
        let body = payload.clone();
        let fetcher = FakeFetcher {
            calls: RefCell::new(vec![]),
            // 闭包需 'static，这里 move 进去
            script: Box::new(move |_, _| Ok((200, body.clone()))),
        };
        let opts = DownloadOptions {
            expected_sha256: Some(hash),
            ..Default::default()
        };
        let r = download(&fetcher, &["http://m/x".into()], &dest, &opts).unwrap();
        assert_eq!(r.bytes, 11);
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello world");
        assert_eq!(r.attempts, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resumes_from_existing_part() {
        let dir = tmp("resume");
        let dest = dir.join("b.bin");
        let part = dir.join("b.bin.part");
        std::fs::write(&part, b"AAAA").unwrap(); // 已有 4 字节

        let fetcher = FakeFetcher {
            calls: RefCell::new(vec![]),
            script: Box::new(|_, range| {
                assert_eq!(range, Some(4), "应从第 4 字节续传");
                Ok((206, b"BBBB".to_vec()))
            }),
        };
        let r = download(
            &fetcher,
            &["http://m/b".into()],
            &dest,
            &DownloadOptions::default(),
        )
        .unwrap();
        assert_eq!(r.resumed_from, 4);
        assert_eq!(r.bytes, 8);
        assert_eq!(std::fs::read(&dest).unwrap(), b"AAAABBBB");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn falls_back_to_full_download_when_range_unsupported() {
        let dir = tmp("norange");
        let dest = dir.join("c.bin");
        let part = dir.join("c.bin.part");
        std::fs::write(&part, b"OLD").unwrap();

        let fetcher = FakeFetcher {
            calls: RefCell::new(vec![]),
            script: Box::new(|_, _| Ok((200, b"FULLCONTENT".to_vec()))),
        };
        let r = download(
            &fetcher,
            &["http://m/c".into()],
            &dest,
            &DownloadOptions::default(),
        )
        .unwrap();
        assert_eq!(r.resumed_from, 0, "不支持续传时应报告从头下载");
        assert_eq!(std::fs::read(&dest).unwrap(), b"FULLCONTENT");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tries_next_mirror_on_failure() {
        let dir = tmp("mirror");
        let dest = dir.join("d.bin");
        let fetcher = FakeFetcher {
            calls: RefCell::new(vec![]),
            script: Box::new(|url, _| {
                if url.contains("bad") {
                    Err("连接超时".into())
                } else {
                    Ok((200, b"FROM-GOOD-MIRROR".to_vec()))
                }
            }),
        };
        let sources = vec!["http://bad/d".to_string(), "http://good/d".to_string()];
        let r = download(&fetcher, &sources, &dest, &DownloadOptions::default()).unwrap();
        assert_eq!(r.source, "http://good/d");
        assert_eq!(std::fs::read(&dest).unwrap(), b"FROM-GOOD-MIRROR");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hash_mismatch_deletes_part_and_moves_to_next_source() {
        let dir = tmp("badhash");
        let dest = dir.join("e.bin");
        let fetcher = FakeFetcher {
            calls: RefCell::new(vec![]),
            script: Box::new(|url, _| {
                if url.contains("corrupt") {
                    Ok((200, b"EVIL".to_vec()))
                } else {
                    Ok((200, b"GOOD".to_vec()))
                }
            }),
        };
        let expected = crate::security::sha256_hex(b"GOOD");
        let opts = DownloadOptions {
            expected_sha256: Some(expected),
            ..Default::default()
        };
        let sources = vec!["http://corrupt/e".to_string(), "http://good/e".to_string()];
        let r = download(&fetcher, &sources, &dest, &opts).unwrap();
        assert_eq!(r.source, "http://good/e");
        assert_eq!(std::fs::read(&dest).unwrap(), b"GOOD");
        assert!(
            !dir.join("e.bin.part").exists(),
            "坏源留下的 part 必须被删除"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn all_sources_fail_reports_error() {
        let dir = tmp("allfail");
        let dest = dir.join("f.bin");
        let fetcher = FakeFetcher {
            calls: RefCell::new(vec![]),
            script: Box::new(|_, _| Err("拒绝连接".into())),
        };
        let err = download(
            &fetcher,
            &["http://x/f".into()],
            &dest,
            &DownloadOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, DownloadError::AllSourcesFailed(_)));
        assert!(!dest.exists(), "失败时不应留下目标文件");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_body_is_rejected() {
        let dir = tmp("empty");
        let dest = dir.join("g.bin");
        let fetcher = FakeFetcher {
            calls: RefCell::new(vec![]),
            script: Box::new(|_, _| Ok((200, vec![]))),
        };
        let err = download(
            &fetcher,
            &["http://x/g".into()],
            &dest,
            &DownloadOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, DownloadError::AllSourcesFailed(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn retries_before_giving_up_on_source() {
        let dir = tmp("retry");
        let dest = dir.join("h.bin");
        let attempts = RefCell::new(0);
        let fetcher = FakeFetcher {
            calls: RefCell::new(vec![]),
            script: Box::new(move |_, _| {
                let mut a = attempts.borrow_mut();
                *a += 1;
                if *a < 2 {
                    Err("临时失败".into())
                } else {
                    Ok((200, b"DONE".to_vec()))
                }
            }),
        };
        let opts = DownloadOptions {
            retries: 2,
            backoff_ms: 1,
            expected_sha256: None,
        };
        let r = download(&fetcher, &["http://x/h".into()], &dest, &opts).unwrap();
        assert_eq!(r.attempts, 2, "第一次失败后应重试成功");
        assert_eq!(std::fs::read(&dest).unwrap(), b"DONE");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_source_list_is_error() {
        let dir = tmp("nosource");
        let err = download(
            &FakeFetcher {
                calls: RefCell::new(vec![]),
                script: Box::new(|_, _| Ok((200, b"x".to_vec()))),
            },
            &[],
            &dir.join("i.bin"),
            &DownloadOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, DownloadError::AllSourcesFailed(_)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
