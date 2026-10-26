//! 环境探测命令（环境管理）。
//!
//! 真实执行本机命令并解析结果。之所以放在 Rust 侧：
//! 前端（WebView）不允许直接执行系统命令，这是 Tauri 的安全边界，
//! 也不应该为了"方便"去放开 shell 权限。
//!
//! 所有探测都设超时，避免某个命令卡住导致整个体检无响应。

use std::process::Command;
use std::time::{Duration, Instant};

use serde::Serialize;

/// 单条命令的探测结果。
#[derive(Debug, Serialize)]
pub struct ProbeResult {
    pub available: bool,
    pub version: String,
    pub detail: String,
}

/// 显卡信息。
#[derive(Debug, Serialize)]
pub struct GpuInfo {
    pub name: String,
    pub vram_gb: f64,
    pub usable: bool,
}

/// 磁盘信息。
#[derive(Debug, Serialize)]
pub struct DiskInfo {
    pub path: String,
    pub free_gb: f64,
    pub ok: bool,
}

/// 完整环境报告。
#[derive(Debug, Serialize)]
pub struct EnvReport {
    pub python: ProbeResult,
    pub node: ProbeResult,
    pub uv: ProbeResult,
    pub gpus: Vec<GpuInfo>,
    pub disk: DiskInfo,
    pub problems: Vec<String>,
    pub healthy: bool,
}

/// 带超时地执行命令，返回 (是否成功, 合并后的输出)。
fn run_with_timeout(cmd: &str, args: &[&str], timeout: Duration) -> (bool, String) {
    let started = Instant::now();
    let mut child = match Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (false, e.to_string()),
    };

    // 轮询等待，避免 wait_with_output 永久阻塞
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    return (false, "命令执行超时".to_string());
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            Err(e) => return (false, e.to_string()),
        }
    }

    match child.wait_with_output() {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.success(), text)
        }
        Err(e) => (false, e.to_string()),
    }
}

/// 从任意输出里提取第一个 x.y 或 x.y.z 版本号。
fn extract_version(output: &str) -> String {
    let mut best = String::new();
    let bytes: Vec<char> = output.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut dots = 0;
            let mut j = i;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == '.') {
                if bytes[j] == '.' {
                    dots += 1;
                }
                j += 1;
            }
            if dots >= 1 {
                let candidate: String = bytes[start..j].iter().collect();
                if candidate.len() > best.len() {
                    best = candidate;
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    best
}

/// 判断版本是否满足下限。
fn meets_min(version: &str, want: (u32, u32, u32)) -> bool {
    let parts: Vec<u32> = version
        .split('.')
        .filter_map(|p| p.parse::<u32>().ok())
        .collect();
    if parts.is_empty() {
        return false;
    }
    let have = (
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    );
    have >= want
}

/// 探测一个运行时：按候选顺序依次尝试，第一个成功即采用。
///
/// 为什么需要候选列表：Windows 上的 `python.exe` 常常是
/// **App Execution Alias 存根（0 字节）**，子进程直接执行会返回 9009
/// "找不到命令"，但 `py` 启动器可用。只试 `python` 会误报"缺少 Python"，
/// 进而让用户去装一个其实已经装好的运行时。
fn probe_runtime(cmd: &str, candidates: &[(&str, &[&str])], min: (u32, u32, u32)) -> ProbeResult {
    let mut tried = Vec::new();

    for (name, args) in candidates {
        let (ok, output) = run_with_timeout(name, args, Duration::from_secs(8));
        let combined = output.trim();

        if ok && !combined.is_empty() {
            let version = extract_version(combined);
            let meets = meets_min(&version, min);
            return ProbeResult {
                available: true,
                version: version.clone(),
                detail: if meets {
                    format!("{name} {version}")
                } else {
                    format!("{name} {version}（低于要求 {}.{}.{}）", min.0, min.1, min.2)
                },
            };
        }
        tried.push(*name);
    }

    ProbeResult {
        available: false,
        version: String::new(),
        detail: format!("未找到 {cmd}（已尝试：{}）", tried.join("、")),
    }
}

fn probe_gpus() -> Vec<GpuInfo> {
    // AdapterRAM 在某些驱动下为 0/不可靠；仅作参考，不做硬性判断依据
    let (ok, out) = run_with_timeout(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_VideoController | ForEach-Object { \"$($_.Name)|$($_.AdapterRAM)\" }",
        ],
        Duration::from_secs(10),
    );
    if !ok {
        return Vec::new();
    }
    out.lines()
        .map(|l| l.trim())
        .filter(|l| l.contains('|'))
        .map(|l| {
            let mut it = l.splitn(2, '|');
            let name = it.next().unwrap_or("").trim().to_string();
            let ram: f64 = it.next().unwrap_or("0").trim().parse().unwrap_or(0.0);
            let vram_gb = (ram / 1024.0 / 1024.0 / 1024.0 * 10.0).round() / 10.0;
            GpuInfo {
                usable: vram_gb >= 2.0,
                name,
                vram_gb,
            }
        })
        .filter(|g| !g.name.is_empty())
        .collect()
}

fn probe_disk() -> DiskInfo {
    let (ok, out) = run_with_timeout(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            "$d=Get-PSDrive -Name D -ErrorAction SilentlyContinue; if($d){[math]::Round($d.Free/1GB,1)}",
        ],
        Duration::from_secs(8),
    );
    let free_gb: f64 = if ok {
        out.trim().parse().unwrap_or(0.0)
    } else {
        0.0
    };
    DiskInfo {
        path: "D:\\".into(),
        free_gb,
        // 至少留 2GB 给安装
        ok: free_gb >= 2.0,
    }
}

/// 汇总环境报告。
#[tauri::command]
pub fn probe_environment() -> EnvReport {
    // Python：优先 python，其次 py 启动器（Windows 常见），最后 python3
    let python = probe_runtime(
        "Python",
        &[
            ("python", &["--version"]),
            ("py", &["--version"]),
            ("python3", &["--version"]),
        ],
        (3, 11, 0),
    );
    let node = probe_runtime("Node.js", &[("node", &["--version"])], (20, 0, 0));
    let uv = probe_runtime("uv", &[("uv", &["--version"])], (0, 1, 0));
    let gpus = probe_gpus();
    let disk = probe_disk();

    let mut problems = Vec::new();
    // Python 与 Node 缺失会直接阻塞模块安装；uv 仅影响加速
    if !python.available {
        problems.push("缺少 Python（建议 3.11+）".to_string());
    }
    if !node.available {
        problems.push("缺少 Node.js（建议 20+）".to_string());
    }
    if !disk.ok {
        problems.push(format!("磁盘空间不足（D 盘剩余 {} GB）", disk.free_gb));
    }

    EnvReport {
        healthy: problems.is_empty(),
        problems,
        python,
        node,
        uv,
        gpus,
        disk,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_version_from_python_output() {
        assert_eq!(extract_version("Python 3.12.13"), "3.12.13");
    }

    #[test]
    fn extract_version_from_node_output() {
        assert_eq!(extract_version("v24.14.0"), "24.14.0");
    }

    #[test]
    fn extract_version_picks_longest_match() {
        // 输出里可能有多段数字，应取最长的版本形态
        assert_eq!(extract_version("uv 0.10.8 (abc 123)"), "0.10.8");
    }

    #[test]
    fn extract_version_returns_empty_when_none() {
        assert_eq!(extract_version("no version here"), "");
    }

    #[test]
    fn meets_min_compares_semver_like() {
        assert!(meets_min("3.12.0", (3, 11, 0)));
        assert!(meets_min("3.11.0", (3, 11, 0)));
        assert!(!meets_min("3.10.9", (3, 11, 0)));
        assert!(!meets_min("", (3, 11, 0)));
    }

    #[test]
    fn run_with_timeout_reports_missing_command() {
        let (ok, out) = run_with_timeout(
            "definitely-not-a-real-command-xyz",
            &[],
            Duration::from_secs(2),
        );
        assert!(!ok);
        assert!(!out.is_empty());
    }

    #[test]
    fn probe_runtime_falls_back_to_next_candidate() {
        // 第一个候选必然失败，第二个应成功
        // （模拟 Windows 上 python.exe 是 0 字节 alias 的真实情况）
        let r = probe_runtime(
            "Python",
            &[
                ("definitely-not-a-real-cmd-xyz", &["--version"]),
                ("cmd", &["/c", "echo", "Python 3.12.0"]),
            ],
            (3, 11, 0),
        );
        assert!(r.available, "应回退到第二个候选并成功：{r:?}");
        assert_eq!(r.version, "3.12.0");
    }

    #[test]
    fn probe_runtime_reports_all_tried_when_none_work() {
        let r = probe_runtime(
            "Python",
            &[
                ("definitely-not-a-real-cmd-a", &[]),
                ("definitely-not-a-real-cmd-b", &[]),
            ],
            (3, 11, 0),
        );
        assert!(!r.available);
        assert!(
            r.detail.contains("已尝试"),
            "应说明尝试过哪些命令：{}",
            r.detail
        );
        assert!(r.detail.contains("definitely-not-a-real-cmd-a"));
    }

    #[test]
    fn probe_runtime_rejects_version_below_minimum() {
        let r = probe_runtime(
            "Python",
            &[("cmd", &["/c", "echo", "Python 3.9.0"])],
            (3, 11, 0),
        );
        assert!(r.available, "能执行就算可用");
        assert!(
            r.detail.contains("低于要求"),
            "应提示版本过低：{}",
            r.detail
        );
    }

    #[test]
    fn probe_environment_returns_report_without_panic() {
        // 在无特定运行时的机器上也必须返回结构完整的报告
        let r = probe_environment();
        assert!(!r.problems.is_empty() || r.healthy);
        assert!(r.disk.path.contains("D"));
    }
}
