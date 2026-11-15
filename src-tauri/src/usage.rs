//! 实例资源占用：2 秒一轮，只留内存里的 60 点，不落盘。
//!
//! 数据源是每个实例自己的 Job Object（M3 强杀兜底用的那个），不是 parent-pid
//! 猜出来的进程树：孙子进程也在同一个 Job 里，父子关系断了也不影响统计。
//!
//! 这里只做三件事：记读数、算差分、存环形缓冲。Win32 调用全在 `process::win`。

use std::collections::{BTreeMap, VecDeque};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::process::win::UsageRaw;

/// 采样周期。改这里要同步改 DESIGN 里那节与界面上的「N 秒前」文案。
pub const INTERVAL_MS: u64 = 2_000;
/// 每实例保留的点数：60 × 2 秒 = 2 分钟。
pub const POINTS: usize = 60;
/// 差分基准的最小间隔。太短的一次差分只是噪声，沿用上一次的百分比。
const MIN_WINDOW_SECS: f32 = 0.2;

#[derive(Debug, Clone, Serialize)]
pub struct UsagePoint {
    pub cpu_percent: Option<f32>,
    pub working_set_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstanceUsageView {
    pub instance_id: String,
    /// 首轮没有差分基准，或本轮读不到进程时为 None。界面据此写「—」，不写 0。
    pub cpu_percent: Option<f32>,
    pub working_set_bytes: Option<u64>,
    pub process_count: Option<u32>,
    /// 进程树里的 pid。界面不逐条显示，留给实例详情。
    pub pids: Vec<u32>,
    /// 旧 → 新，最多 `POINTS` 个。
    pub points: Vec<UsagePoint>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageTotals {
    pub instances: usize,
    pub processes: u32,
    pub working_set_bytes: u64,
    /// 全部有读数的实例之和；一个都读不到时为 None。
    pub cpu_percent: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageSnapshot {
    pub at_ms: u64,
    pub totals: UsageTotals,
    pub items: Vec<InstanceUsageView>,
}

#[derive(Debug, Clone, Default)]
struct Track {
    last_ticks: Option<u64>,
    last_at: Option<Instant>,
    cpu_percent: Option<f32>,
    working_set_bytes: Option<u64>,
    pids: Vec<u32>,
    points: VecDeque<UsagePoint>,
}

/// 全部实例的占用状态。一把锁保护，读快照与记一轮都是短临界区。
pub struct UsageBoard {
    tracks: std::sync::Mutex<BTreeMap<String, Track>>,
    cores: f32,
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Default for UsageBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageBoard {
    pub fn new() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f32;
        Self {
            tracks: std::sync::Mutex::new(BTreeMap::new()),
            cores: cores.max(1.0),
        }
    }

    /// 记一轮读数。键必须是**当前托管且活着**的实例；不在表里的连历史一起丢掉。
    ///
    /// 停止实例后界面要立刻回到「—」，留着上一轮的数值等于把旧读数冒充当前占用。
    pub fn record(&self, alive: &BTreeMap<String, Option<UsageRaw>>, now: Instant) {
        let mut tracks = lock(&self.tracks);
        tracks.retain(|id, _| alive.contains_key(id));
        for (id, raw) in alive {
            let entry = tracks.entry(id.clone()).or_default();
            let Some(raw) = raw else {
                // 读不到（权限不够或刚退出）：当前值置空，历史保留。
                // 刚退出的实例下一轮就被 retain 清掉，不会留下一条空线。
                entry.cpu_percent = None;
                entry.working_set_bytes = None;
                entry.pids.clear();
                entry.last_ticks = None;
                entry.last_at = None;
                continue;
            };

            let cpu = match (entry.last_ticks, entry.last_at) {
                (Some(ticks), Some(at)) => {
                    let wall = now.saturating_duration_since(at).as_secs_f32();
                    if wall < MIN_WINDOW_SECS {
                        entry.cpu_percent
                    } else {
                        // 100ns 计数除以 1e7 得到秒；再除以「墙钟 × 核数」就是整机占比。
                        let used = raw.cpu_ticks.saturating_sub(ticks) as f32 / 10_000_000.0;
                        Some((used / (wall * self.cores) * 100.0).clamp(0.0, 100.0))
                    }
                }
                // 首轮没有基准：只记账不算百分比。
                _ => None,
            };

            entry.last_ticks = Some(raw.cpu_ticks);
            entry.last_at = Some(now);
            entry.cpu_percent = cpu;
            entry.working_set_bytes = Some(raw.working_set_bytes);
            entry.pids = raw.pids.clone();
            entry.points.push_back(UsagePoint {
                cpu_percent: cpu,
                working_set_bytes: raw.working_set_bytes,
            });
            while entry.points.len() > POINTS {
                entry.points.pop_front();
            }
        }
    }

    /// 停止或卸载后立刻摘掉，不必等下一轮采样。
    pub fn forget(&self, instance_id: &str) {
        lock(&self.tracks).remove(instance_id);
    }

    pub fn is_empty(&self) -> bool {
        lock(&self.tracks).is_empty()
    }

    pub fn snapshot(&self) -> UsageSnapshot {
        let tracks = lock(&self.tracks);
        let mut items = Vec::with_capacity(tracks.len());
        let mut totals = UsageTotals::default();
        let mut cpu_sum = 0f32;
        let mut cpu_known = false;

        for (id, track) in tracks.iter() {
            let readable = track.working_set_bytes.is_some();
            if let Some(bytes) = track.working_set_bytes {
                totals.working_set_bytes += bytes;
            }
            totals.processes += track.pids.len() as u32;
            if let Some(cpu) = track.cpu_percent {
                cpu_sum += cpu;
                cpu_known = true;
            }
            items.push(InstanceUsageView {
                instance_id: id.clone(),
                cpu_percent: track.cpu_percent,
                working_set_bytes: track.working_set_bytes,
                // 读不到进程时进程数同样未知，给 0 会被当成「一棵都没有」。
                process_count: readable.then_some(track.pids.len() as u32),
                pids: track.pids.clone(),
                points: track.points.iter().cloned().collect(),
            });
        }

        totals.instances = items.len();
        totals.cpu_percent = cpu_known.then_some(cpu_sum);
        UsageSnapshot {
            at_ms: now_ms(),
            totals,
            items,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(ticks: u64, bytes: u64, pids: Vec<u32>) -> UsageRaw {
        UsageRaw {
            pids,
            cpu_ticks: ticks,
            working_set_bytes: bytes,
        }
    }

    fn board_with_cores(cores: f32) -> UsageBoard {
        let mut board = UsageBoard::new();
        board.cores = cores;
        board
    }

    #[test]
    fn first_round_reports_memory_but_no_cpu() {
        let board = board_with_cores(1.0);
        let now = Instant::now();
        let mut alive = BTreeMap::new();
        alive.insert("a".to_string(), Some(raw(1_000, 2048, vec![10])));
        board.record(&alive, now);

        let snap = board.snapshot();
        assert_eq!(snap.items.len(), 1);
        assert_eq!(snap.items[0].working_set_bytes, Some(2048));
        assert_eq!(snap.items[0].process_count, Some(1));
        assert_eq!(snap.items[0].cpu_percent, None);
        assert_eq!(snap.totals.cpu_percent, None);
    }

    #[test]
    fn cpu_is_averaged_over_the_core_count() {
        // 1 秒墙钟、1 亿 ticks（=10 秒 CPU 时间）、4 核 → 10 / (1 × 4) = 250% 会被 clamp 到 100。
        // 用 4 千万 ticks（4 秒）→ 4 / (1 × 4) = 100%。
        let board = board_with_cores(4.0);
        let start = Instant::now();
        let mut alive = BTreeMap::new();
        alive.insert("a".to_string(), Some(raw(0, 1024, vec![10])));
        board.record(&alive, start);

        alive.insert("a".to_string(), Some(raw(40_000_000, 1024, vec![10])));
        board.record(&alive, start + std::time::Duration::from_secs(1));

        let snap = board.snapshot();
        let cpu = snap.items[0].cpu_percent.expect("第二轮应有百分比");
        assert!((cpu - 100.0).abs() < 0.5, "实际 {cpu}");
    }

    #[test]
    fn short_window_keeps_the_previous_percentage() {
        let board = board_with_cores(1.0);
        let start = Instant::now();
        let mut alive = BTreeMap::new();
        alive.insert("a".to_string(), Some(raw(0, 1024, vec![10])));
        board.record(&alive, start);
        alive.insert("a".to_string(), Some(raw(10_000_000, 1024, vec![10])));
        board.record(&alive, start + std::time::Duration::from_secs(1));
        let steady = board.snapshot().items[0].cpu_percent;

        // 50ms 后的差分只是噪声：沿用上一次的百分比，不给出一个抖动值。
        alive.insert("a".to_string(), Some(raw(99_000_000, 1024, vec![10])));
        board.record(&alive, start + std::time::Duration::from_millis(1_050));
        assert_eq!(board.snapshot().items[0].cpu_percent, steady);
    }

    #[test]
    fn points_stop_at_the_window_size() {
        let board = board_with_cores(1.0);
        let start = Instant::now();
        for i in 0..(POINTS + 10) {
            let mut alive = BTreeMap::new();
            alive.insert("a".to_string(), Some(raw(i as u64 * 1_000, 1024, vec![10])));
            board.record(
                &alive,
                start + std::time::Duration::from_millis(2_000 * i as u64),
            );
        }
        let snap = board.snapshot();
        assert_eq!(snap.items[0].points.len(), POINTS);
        // 最新一点在末尾：工作集随轮次不变，这里用长度与容量对齐即可。
        assert!(snap.items[0].points.last().is_some());
    }

    #[test]
    fn stopping_an_instance_drops_its_history() {
        let board = board_with_cores(1.0);
        let now = Instant::now();
        let mut alive = BTreeMap::new();
        alive.insert("a".to_string(), Some(raw(0, 1024, vec![10])));
        alive.insert("b".to_string(), Some(raw(0, 2048, vec![20])));
        board.record(&alive, now);
        assert_eq!(board.snapshot().items.len(), 2);

        alive.remove("a");
        board.record(&alive, now + std::time::Duration::from_secs(2));
        let snap = board.snapshot();
        assert_eq!(snap.items.len(), 1);
        assert_eq!(snap.items[0].instance_id, "b");
        assert_eq!(snap.totals.working_set_bytes, 2048);

        board.forget("b");
        assert!(board.is_empty());
        assert_eq!(board.snapshot().totals.instances, 0);
    }

    #[test]
    fn unreadable_instance_reports_nothing_instead_of_zero() {
        let board = board_with_cores(1.0);
        let now = Instant::now();
        let mut alive = BTreeMap::new();
        alive.insert("a".to_string(), None);
        board.record(&alive, now);

        let snap = board.snapshot();
        assert_eq!(snap.items[0].cpu_percent, None);
        assert_eq!(snap.items[0].working_set_bytes, None);
        assert_eq!(snap.items[0].process_count, None);
        assert_eq!(snap.totals.processes, 0);
        assert_eq!(snap.totals.working_set_bytes, 0);
    }
}
