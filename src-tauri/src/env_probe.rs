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
    /// 虚拟显示器（远控、投屏、串流软件创建）。
    ///
    /// 它不是显卡，显存数字也没有参考价值，界面上不计入「能跑多大模型」的判断。
    /// 这里只做标记，不在后端过滤：前端需要知道自己隐去了几个，才好写那句
    /// 「另有 N 个虚拟显示器未计入」，而不是让列表莫名比系统看到少。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub virtual_adapter: bool,
}

/// 物理内存。
#[derive(Debug, Serialize)]
pub struct MemoryInfo {
    pub total_gb: f64,
    pub free_gb: f64,
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
    /// 读不到内存时缺省，界面据此显示「读不到」而不是 0 GB。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryInfo>,
    pub disk: DiskInfo,
    pub problems: Vec<String>,
    pub healthy: bool,
}

/// 构造一个**不弹控制台窗口**的命令。
///
/// Python、Node、nvidia-smi、PowerShell 全是控制台程序。从 GUI 进程直接 spawn
/// 时，Windows 会给每个子进程新开一个控制台窗口——体检一次就是四五个黑窗
/// 接连闪一下，看起来像出故障。`CREATE_NO_WINDOW` 让子进程不分配控制台，
/// 输出仍可通过管道读到。
#[cfg(windows)]
fn hidden_command(cmd: &str) -> Command {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
    let mut command = Command::new(cmd);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(not(windows))]
fn hidden_command(cmd: &str) -> Command {
    Command::new(cmd)
}

/// 带超时地执行命令，返回 (是否成功, 合并后的输出)。
fn run_with_timeout(cmd: &str, args: &[&str], timeout: Duration) -> (bool, String) {
    let started = Instant::now();
    let mut child = match hidden_command(cmd)
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
  "$n|$a|$q|$($_.PNPDeviceID)"
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
            let mut it = l.splitn(4, '|');
            let name = it.next().unwrap_or("").trim().to_string();
            let adapter: f64 = it.next().unwrap_or("0").trim().parse().unwrap_or(0.0);
            let qword: f64 = it.next().unwrap_or("0").trim().parse().unwrap_or(0.0);
            let pnp_id = it.next().unwrap_or("").trim().to_string();

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
            let virtual_adapter = is_virtual_adapter(&name, &pnp_id);

            GpuInfo {
                // 虚拟显示器不参与「够不够跑模型」的判断：它的显存是宿主分配的一小段，
                // 既不能用来推理，也不代表机器能力。
                usable: !virtual_adapter && vram_gb >= 2.0,
                name,
                vram_gb,
                truncated: truncated_hint,
                virtual_adapter,
            }
        })
        .filter(|g| !g.name.is_empty())
        .collect()
}

/// 判断一块显示适配器是不是虚拟显示器。
///
/// 远控（ToDesk、向日葵、RustDesk）、串流（Parsec）与投屏（spacedesk、
/// DisplayLink）都会注册显示适配器，它们在 `Win32_VideoController` 里与真显卡
/// 平级出现，名字也常带「显示适配器」四个字，很容易被当成一块低端显卡。
///
/// 两条判据都要看，缺一条就会漏：
///
/// 1. **设备枚举方式**：`ROOT\` 与 `SWD\` 开头的都是软件枚举设备，
///    虚拟显示器几乎都落在这两类里。
/// 2. **名称关键字**：部分串流驱动挂成 PCI 设备，设备路径看不出来，
///    只能靠名字里的 Virtual / Indirect / 厂商名。
///
/// 只看名称会漏掉英文驱动名的国产远控，只看设备路径会漏掉 PCI 串流卡。
fn is_virtual_adapter(name: &str, pnp_id: &str) -> bool {
    let id = pnp_id.trim().to_ascii_uppercase();
    if id.starts_with("ROOT\\") || id.starts_with("SWD\\") {
        return true;
    }

    let lower = name.to_ascii_lowercase();
    const HINTS: [&str; 12] = [
        "virtual",
        "indirect",
        "idd",
        "mirror",
        "displaylink",
        "spacedesk",
        "parsec",
        "todesk",
        "sunlogin",
        "rustdesk",
        "anydesk",
        "teamviewer",
    ];
    if HINTS.iter().any(|h| lower.contains(h)) {
        return true;
    }
    // 中文驱动名不参与 to_ascii_lowercase，单独匹配。
    ["虚拟", "远控", "投屏", "串流", "镜像"]
        .iter()
        .any(|h| name.contains(h))
}

/// 读物理内存（总容量与当前可用）。
///
/// `TotalVisibleMemorySize` / `FreePhysicalMemory` 的单位都是 KB，
/// 是任务管理器「性能 → 内存」那两个数字的同源值。
fn probe_memory() -> Option<MemoryInfo> {
    let (ok, out) = run_with_timeout(
        "powershell",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$os=Get-CimInstance Win32_OperatingSystem; if($os){\"$($os.TotalVisibleMemorySize)|$($os.FreePhysicalMemory)\"}",
        ],
        Duration::from_secs(8),
    );
    if !ok {
        return None;
    }
    parse_memory_kb(out.trim())
}

/// 解析 `总KB|可用KB`。读不到或格式不对时返回 None，界面据此显示「读不到」。
fn parse_memory_kb(line: &str) -> Option<MemoryInfo> {
    let mut it = line.splitn(2, '|');
    let total: f64 = it.next()?.trim().parse().ok()?;
    let free: f64 = it.next().unwrap_or("0").trim().parse().unwrap_or(0.0);
    if total <= 0.0 {
        return None;
    }
    let to_gb = |kb: f64| (kb / 1024.0 / 1024.0 * 10.0).round() / 10.0;
    Some(MemoryInfo {
        total_gb: to_gb(total),
        free_gb: to_gb(free),
    })
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
/// 从数据根取出 `X:` 形态的盘符。
///
/// 只接受单个 ASCII 字母加冒号：UNC 路径、非 Windows 路径都返回空串，
/// 免得把任意字符串拼进 PowerShell 脚本。检测目标是数据根，不是当前盘。
fn data_root_drive(data_root: &std::path::Path) -> String {
    let prefix = data_root
        .components()
        .find_map(|c| match c {
            std::path::Component::Prefix(p) => Some(p.as_os_str().to_string_lossy().to_string()),
            _ => None,
        })
        .unwrap_or_default()
        .trim_end_matches('\\')
        .to_string();
    let mut chars = prefix.chars();
    match (chars.next(), chars.next()) {
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic() => {
            format!("{}:", letter.to_ascii_uppercase())
        }
        _ => String::new(),
    }
}

fn probe_disk(data_root: &std::path::Path) -> DiskInfo {
    let drive = data_root_drive(data_root);
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
        // 拿不到盘符时不编造一个 `\`：如实写成「数据根所在卷」，
        // 让提示文字仍说明检测对象，而不是给一个看似真实的错误路径。
        path: if drive.is_empty() {
            "数据根所在卷".into()
        } else {
            format!("{drive}\\")
        },
        free_gb,
        is_data_root: true,
        // 至少留 2GB 给安装
        ok: free_gb >= 2.0,
    }
}

/// 汇总环境报告。
///
/// ## 为什么要 `async` + `spawn_blocking`
///
/// Tauri 的**同步命令跑在主线程**（事件循环所在线程）。这个函数要做六次子进程
/// 探测、最长可等几十秒，同步执行会让窗口整段时间点不动、拖不动、画不出——
/// 用户看到的就是「检测硬件时界面卡死」。放到阻塞线程池里，主线程继续处理
/// 绘制与输入，前端可以自行显示「检测中」。
#[tauri::command]
pub async fn probe_environment(
    state: tauri::State<'_, crate::state_v2::AppStateV2>,
) -> Result<EnvReport, String> {
    let data_root = state.paths.data_root.clone();
    tauri::async_runtime::spawn_blocking(move || probe_environment_with(&data_root))
        .await
        .map_err(|error| format!("环境检测没有跑完：{error}"))
}

/// 探测环境，磁盘检测以 data_root 所在卷为准。
///
/// 六项探测互不依赖，**并行跑**：串行时是「Python 三个候选 × 8s + Node + uv +
/// 显卡 15s + 内存 8s + 磁盘 8s」的累加，最坏情况接近一分钟；并行后总耗时
/// 约等于最慢的那一项。任一探测 panic 只让该项退化为「读不到」，不影响其它项。
pub fn probe_environment_with(data_root: &std::path::Path) -> EnvReport {
    let (python, node, uv, gpus, memory, disk) = std::thread::scope(|scope| {
        let python = scope.spawn(|| {
            probe_runtime(
                "Python",
                &[
                    ("python", &["--version"]),
                    ("py", &["--version"]),
                    ("python3", &["--version"]),
                ],
                (3, 11, 0),
            )
        });
        let node =
            scope.spawn(|| probe_runtime("Node.js", &[("node", &["--version"])], (20, 0, 0)));
        let uv = scope.spawn(|| probe_runtime("uv", &[("uv", &["--version"])], (0, 1, 0)));
        let gpus = scope.spawn(probe_gpus);
        let memory = scope.spawn(probe_memory);
        let disk = scope.spawn(|| probe_disk(data_root));

        (
            python.join().unwrap_or_else(|_| missing_runtime("Python")),
            node.join().unwrap_or_else(|_| missing_runtime("Node.js")),
            uv.join().unwrap_or_else(|_| missing_runtime("uv")),
            gpus.join().unwrap_or_default(),
            memory.join().unwrap_or(None),
            disk.join().unwrap_or_else(|_| DiskInfo {
                path: "数据根所在卷".into(),
                free_gb: 0.0,
                is_data_root: true,
                ok: false,
            }),
        )
    });

    let mut problems = Vec::new();
    // Python 与 Node 缺失会直接阻塞模块安装；uv 仅影响加速
    if !python.available {
        problems.push("缺少 Python（建议 3.11+）".to_string());
    }
    if !node.available {
        problems.push("缺少 Node.js（建议 20+）".to_string());
    }
    if let Some(mem) = &memory {
        if mem.total_gb < 8.0 {
            problems.push(format!(
                "内存偏少（共 {} GB，建议 8 GB 以上）",
                mem.total_gb
            ));
        }
    }
    // 显存偏紧只对真显卡说：虚拟显示器的显存不代表机器能力，不该触发建议。
    for gpu in gpus.iter().filter(|g| !g.virtual_adapter && !g.usable) {
        problems.push(format!(
            "显存偏紧（{} GB）：在模块里选更小的量化版本，或改用云端模型",
            gpu.vram_gb
        ));
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
        memory,
        disk,
    }
}

/// 探测线程异常退出时的退化结果：如实写「没有跑完」，不冒充「未安装」。
fn missing_runtime(name: &str) -> ProbeResult {
    ProbeResult {
        available: false,
        version: String::new(),
        detail: format!("{name} 检测没有跑完，请重新检测"),
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
    fn virtual_adapter_is_detected_by_device_path() {
        // 软件枚举设备就是虚拟显示器，与名字无关。
        assert!(is_virtual_adapter(
            "Dounai Remote Display",
            r"ROOT\DISPLAY\0000"
        ));
        assert!(is_virtual_adapter("Any Display", r"SWD\DEV\0001"));
        // 真卡走 PCI，不能误判。
        assert!(!is_virtual_adapter(
            "NVIDIA GeForce RTX 3080 Ti",
            r"PCI\VEN_10DE&DEV_2208&SUBSYS_00000000"
        ));
    }

    #[test]
    fn virtual_adapter_is_detected_by_name() {
        assert!(is_virtual_adapter("ToDesk Virtual Display", ""));
        assert!(is_virtual_adapter("远控程序的虚拟显示适配器", ""));
        assert!(is_virtual_adapter("Microsoft Indirect Display", ""));
        assert!(is_virtual_adapter("Parsec Virtual Display Adapter", ""));
        assert!(!is_virtual_adapter("Intel(R) UHD Graphics 630", ""));
        assert!(!is_virtual_adapter("AMD Radeon RX 7900 XTX", ""));
    }

    #[test]
    fn memory_kb_is_converted_to_gb() {
        let m = parse_memory_kb("33554432|8388608").expect("应解析成功");
        assert_eq!(m.total_gb, 32.0);
        assert_eq!(m.free_gb, 8.0);
        // 读不到时不能当成 0 GB 报给用户。
        assert!(parse_memory_kb("0|0").is_none());
        assert!(parse_memory_kb("not-a-number|0").is_none());
        assert!(parse_memory_kb("").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn probe_environment_returns_report_without_panic() {
        // 在无特定运行时的机器上也必须返回结构完整的报告。
        // 显式给一个 C 盘数据根，验证 D-3 不再写死 D 盘。
        let r = probe_environment_with(std::path::Path::new(
            r"C:\Users\test\AppData\Local\launcher",
        ));
        assert!(!r.problems.is_empty() || r.healthy);
        assert!(!r.disk.path.is_empty(), "应报告实际检测的卷");
        assert!(r.disk.is_data_root, "检测的应是数据根所在卷");
        assert!(r.disk.path.starts_with("C:"), "应检测数据根所在卷");
    }

    #[cfg(windows)]
    #[test]
    fn data_root_drive_follows_the_given_root() {
        assert_eq!(
            data_root_drive(std::path::Path::new(r"E:\Dounai-ai\launcher")),
            "E:"
        );
        assert_eq!(
            data_root_drive(std::path::Path::new(r"C:\Users\test")),
            "C:"
        );
        assert_eq!(
            data_root_drive(std::path::Path::new(r"\\server\share\launcher")),
            ""
        );
    }
}
