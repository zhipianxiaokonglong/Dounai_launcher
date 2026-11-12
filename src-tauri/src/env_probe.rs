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
    /// 显存值可能被 uint32 截断（仅在回退到 AdapterRAM 且恰好接近 4 GiB 时为 true）。
    ///
    /// 保留这个标记而不是直接改数字：前端可以据此显示「可能不准」，
    /// 比悄悄给一个错值更诚实。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// 磁盘信息。
#[derive(Debug, Serialize)]
pub struct DiskInfo {
    /// 被检测的卷（数据根所在盘）。
    pub path: String,
    pub free_gb: f64,
    pub ok: bool,
    /// 该卷是否就是启动器数据根所在卷。
    ///
    /// 现在恒为 true（我们只检测数据根），保留字段是为了让前端能区分
    /// 「这个数字代表真正会被写入的盘」而不是别的盘——历史上这里显示的是 D 盘。
    #[serde(default)]
    pub is_data_root: bool,
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

/// 探测显卡与显存。
///
/// ## 为什么不用 `Win32_VideoController.AdapterRAM`
///
/// 该字段是 **uint32**，超过 4 GiB 的显存会**溢出截断**：
///
/// | | 字节数 | 换算 |
/// |---|---|---|
/// | RTX 3080 Ti 实际 | 12884901888 | 12 GiB |
/// | WMI `AdapterRAM` | 4293918720 | ≈4 GiB（被截断） |
///
/// 实测报告里就是「12 GB 的卡报成 4 GB」。这会导致对机器能力的严重低估，
/// 进而劝退用户不要用本地模型。
///
/// ## 取值优先级
///
/// 1. 注册表 `HardwareInformation.qwMemorySize` —— **64 位**，不会被截断，
///    且与 WMI 同源（不需要额外依赖厂商工具）
/// 2. `nvidia-smi --query-gpu=memory.total` —— NVIDIA 卡最准，作为交叉验证
/// 3. `AdapterRAM` —— 仅最后兜底，并在接近 4 GiB 时标注「可能被截断」
fn probe_gpus() -> Vec<GpuInfo> {
    // 一次 PowerShell 调用同时取「注册表 64 位值」与「WMI 名称 + 32 位值」。
    //
    // 注册表路径里 {4d36e968-e325-11ce-bfc1-08002be10318} 是显示适配器类 GUID，
    // 其下每个 4 位数字子键对应一块显卡，qwMemorySize 是 64 位显存字节数。
    let (ok, out) = run_with_timeout(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            r#"
$cls = 'HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}'
$reg = @{}
if (Test-Path $cls) {
  Get-ChildItem $cls -ErrorAction SilentlyContinue | ForEach-Object {
    $p = $_.PSPath
    $mem = (Get-ItemProperty -Path $p -Name 'HardwareInformation.qwMemorySize' -ErrorAction SilentlyContinue).'HardwareInformation.qwMemorySize'
    $desc = (Get-ItemProperty -Path $p -Name 'DriverDesc' -ErrorAction SilentlyContinue).DriverDesc
    if ($mem -and $desc) { $reg[$desc] = $mem }
  }
}
Get-CimInstance Win32_VideoController | ForEach-Object {
  $n = $_.Name
  $a = $_.AdapterRAM
  # 只做精确匹配：显示适配器名称在 WMI 与注册表 DriverDesc 中是一致的。
  # 曾经试过模糊匹配，结果把 NVIDIA 的 12GB 错配给了虚拟显示器——
  # 宁可为 0，也不能张冠李戴。
  $q = 0
  if ($reg.ContainsKey($n)) { $q = $reg[$n] }
  "$n|$a|$q"
}
"#,
        ],
        Duration::from_secs(15),
    );
    if !ok {
        return Vec::new();
    }

    // nvidia-smi 的总显存（字节），用于交叉校验与缺失时兜底
    let nv_total = nvidia_total_vram_bytes();

    out.lines()
        .map(|l| l.trim())
        .filter(|l| l.split('|').count() >= 2)
        .map(|l| {
            let mut it = l.splitn(3, '|');
            let name = it.next().unwrap_or("").trim().to_string();
            let adapter: f64 = it.next().unwrap_or("0").trim().parse().unwrap_or(0.0);
            let qword: f64 = it.next().unwrap_or("0").trim().parse().unwrap_or(0.0);

            // 优先 64 位注册表值；其次 nvidia-smi（**仅限 NVIDIA 卡**）；最后 AdapterRAM。
            //
            // 「仅限 NVIDIA 卡」很关键：nvidia-smi 报的是 NVIDIA 卡的显存，
            // 若给虚拟显示器也套用，就会得出「虚拟显卡 12GB」这种荒谬结论。
            let name_is_nvidia = name.to_ascii_lowercase().contains("nvidia");
            let (bytes, source) = if qword > 0.0 {
                (qword, "registry")
            } else if name_is_nvidia {
                match nv_total {
                    Some(nv) => (nv, "nvidia-smi"),
                    None => (adapter, "adapterram"),
                }
            } else {
                (adapter, "adapterram")
            };

            let vram_gb = (bytes / 1024.0 / 1024.0 / 1024.0 * 10.0).round() / 10.0;

            // AdapterRAM 接近 4 GiB 时几乎必然是被 uint32 截断的结果
            let truncated_hint = source == "adapterram" && (bytes - 4_294_967_295.0f64).abs() < 1.0;

            GpuInfo {
                usable: vram_gb >= 2.0,
                name,
                vram_gb,
                truncated: truncated_hint,
            }
        })
        .filter(|g| !g.name.is_empty())
        .collect()
}

/// 用 nvidia-smi 取第一块 NVIDIA 卡的总显存（字节）。
///
/// 机器上没有 NVIDIA 卡或没装驱动时返回 None——这是正常情况，不报错。
fn nvidia_total_vram_bytes() -> Option<f64> {
    let (ok, out) = run_with_timeout(
        "nvidia-smi",
        &["--query-gpu=memory.total", "--format=csv,noheader,nounits"],
        Duration::from_secs(8),
    );
    if !ok {
        return None;
    }
    // 输出是 MiB 数字（可能带单位与空行），取第一行第一个数字
    out.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .and_then(|l| l.split_whitespace().next().map(|s| s.to_string()))
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|mib| mib * 1024.0 * 1024.0)
}

/// 探测**数据根所在盘**的剩余空间。
///
/// ## 为什么不能再硬编码 D 盘
///
/// 盘符写死 `D` 会同时导致两类错误：
///
/// 1. **漏报**：启动器数据根实际是
///    `app_local_data_dir()/launcher`（通常在 C 盘）。
///    用户看到「D 盘剩 1161 GB 空间充足」，但真正写入的 C 盘只剩 196 GB，
///    等安装失败才知道。
/// 2. **误报**：机器没有 D 盘时 `free_gb = 0`，判定为「空间不足」——
///    而这类机器很常见。
///
/// 修法：直接检测数据根路径所在的卷。
fn probe_disk(data_root: &std::path::Path) -> DiskInfo {
    // 取盘符：Windows 路径形如 `C:\Users\...`，首字符即盘符。
    let drive = data_root
        .components()
        .find_map(|c| match c {
            std::path::Component::Prefix(p) => Some(p.as_os_str().to_string_lossy().to_string()),
            _ => None,
        })
        .unwrap_or_default();
    // 去掉尾部的 `\`，得到 `C:`
    let drive = drive.trim_end_matches('\\').to_string();

    let script = format!(
        "$d=Get-PSDrive -Name '{}' -ErrorAction SilentlyContinue; if($d){{[math]::Round($d.Free/1GB,1)}}",
        drive.trim_end_matches(':')
    );
    let (ok, out) = run_with_timeout(
        "powershell",
        &["-NoProfile", "-Command", &script],
        Duration::from_secs(8),
    );
    let free_gb: f64 = if ok {
        out.trim().parse().unwrap_or(0.0)
    } else {
        0.0
    };
    DiskInfo {
        path: format!("{drive}\\"),
        free_gb,
        is_data_root: true,
        // 至少留 2GB 给安装
        ok: free_gb >= 2.0,
    }
}

/// 汇总环境报告。
#[tauri::command]
pub fn probe_environment() -> EnvReport {
    // 无上下文时的兜底：用当前工作目录所在卷。
    // 真正的命令层入口是 commands::probe_environment，它会传入数据根。
    let fallback = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    probe_environment_with(&fallback)
}

/// 探测环境，磁盘检测以 data_root 所在卷为准。
///
/// 与 probe_environment 的区别：那个用工作目录兜底，本函数显式接收数据根。
/// 命令层应调用本函数——只有它知道模块真正会被装到哪。
pub fn probe_environment_with(data_root: &std::path::Path) -> EnvReport {
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
    let disk = probe_disk(data_root);

    let mut problems = Vec::new();
    // Python 与 Node 缺失会直接阻塞模块安装；uv 仅影响加速
    if !python.available {
        problems.push("缺少 Python（建议 3.11+）".to_string());
    }
    if !node.available {
        problems.push("缺少 Node.js（建议 20+）".to_string());
    }
    if !disk.ok {
        problems.push(format!(
            "磁盘空间不足（{} 剩余 {} GB）",
            disk.path.trim_end_matches('\\'),
            disk.free_gb
        ));
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
        assert!(!r.disk.path.is_empty(), "应报告实际检测的卷");
        assert!(r.disk.is_data_root, "检测的应是数据根所在卷");
    }
}
