//! 环境准备的过程事件。只上报，不参与任何判定。
//!
//! core 不认识 Tauri，所以这里只维护一个线程本的接收器：`prepare_instance_with`
//! 在同一个线程里同步跑完，组装层在调用前用 `with_sink` 挂上回调，由它转成前端
//! 事件。没有接收器时全部是空操作，单测与 `env_smoke` 行为不变。
//!
//! 为什么不做成 `EnvContext` 的字段：`EnvContext` 是 8 处结构体字面量的公开构造函数，
//! 加字段会把「怎么显示进度」这件事扩散到每个测试夹具。进度是旁路，旁路就该能缺省。

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;

/// 阶段取值进前端，命名与界面上的阶段一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvPhase {
    /// 定位 uv 可执行文件（可能下载）。
    Uv,
    /// 准备受管 Python（可能下载）。
    Python,
    /// 解析依赖，算出快照哈希。
    Resolve,
    /// 把依赖同步进 venv。
    Sync,
    /// 复核快照与安装根。
    Verify,
    /// 全部完成。
    Done,
}

impl EnvPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            EnvPhase::Uv => "uv",
            EnvPhase::Python => "python",
            EnvPhase::Resolve => "resolve",
            EnvPhase::Sync => "sync",
            EnvPhase::Verify => "verify",
            EnvPhase::Done => "done",
        }
    }
}

/// 一次阶段上报。字段都是给人看的，不含判定依据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvProgress {
    pub phase: EnvPhase,
    /// 一句话说明当前在做什么。
    pub title: String,
    /// 更细的一行：文件名、包名、uv 的原始输出行。没有就不显示。
    pub detail: Option<String>,
    /// 依赖同步进度：已装 / 总数。不知道时为 None。
    pub current: Option<u32>,
    pub total: Option<u32>,
}

impl EnvProgress {
    pub fn new(phase: EnvPhase, title: impl Into<String>) -> Self {
        Self {
            phase,
            title: title.into(),
            detail: None,
            current: None,
            total: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        if !detail.trim().is_empty() {
            self.detail = Some(detail);
        }
        self
    }

    pub fn with_total(mut self, total: u32) -> Self {
        if total > 0 {
            self.total = Some(total);
        }
        self
    }
}

pub type Sink = Arc<dyn Fn(EnvProgress) + Send + Sync>;

struct Slot {
    sink: Sink,
    last: Option<EnvProgress>,
}

thread_local! {
    static SLOT: RefCell<Option<Slot>> = const { RefCell::new(None) };
}

struct Reset;

impl Drop for Reset {
    fn drop(&mut self) {
        SLOT.with(|slot| *slot.borrow_mut() = None);
    }
}

/// 在 `f` 执行期间挂上接收器。返回后（含 panic）一定摘掉，不跨调用泄漏。
pub fn with_sink<T>(sink: Option<Sink>, f: impl FnOnce() -> T) -> T {
    let Some(sink) = sink else {
        return f();
    };
    SLOT.with(|slot| {
        *slot.borrow_mut() = Some(Slot { sink, last: None });
    });
    let _reset = Reset;
    f()
}

/// 当前线程有没有接收器。runner 据此决定要不要开流式读取与 `-v`。
pub fn active() -> bool {
    SLOT.with(|slot| slot.borrow().is_some())
}

/// 上报一次阶段变化。没有接收器时是空操作。
pub fn emit(progress: EnvProgress) {
    SLOT.with(|slot| {
        let mut borrowed = slot.borrow_mut();
        let Some(state) = borrowed.as_mut() else {
            return;
        };
        state.last = Some(progress.clone());
        (state.sink)(progress);
    });
}

/// 当前阶段的快照。子进程读线程看不到调用线程的线程本状态，只能从这里拿一份走。
pub fn last() -> Option<EnvProgress> {
    SLOT.with(|slot| slot.borrow().as_ref().and_then(|s| s.last.clone()))
}

pub fn sink() -> Option<Sink> {
    SLOT.with(|slot| slot.borrow().as_ref().map(|s| Arc::clone(&s.sink)))
}

/// 子进程输出行的上报器。
///
/// 只认带 `==` 的安装/下载行：uv 的 `-v` 输出里，包粒度的事件都长这样。
/// 认不出就丢弃——进度是给人看的，宁可少一行，也不要把 DEBUG 噪音铺到界面上。
pub struct LineReporter {
    sink: Sink,
    base: EnvProgress,
    last: Mutex<Option<String>>,
    started: Instant,
    sent: Mutex<u32>,
}

/// 一次准备最多上报这么多行，防止异常输出把 webview 刷爆。
const MAX_LINES: u32 = 400;

impl LineReporter {
    /// 没有接收器、或当前还没有阶段快照时返回 None。
    pub fn new() -> Option<Self> {
        let sink = sink()?;
        let base = last()?;
        Some(Self {
            sink,
            base,
            last: Mutex::new(None),
            started: Instant::now(),
            sent: Mutex::new(0),
        })
    }

    pub fn line(&self, raw: &str) {
        let Some(text) = package_line(raw) else {
            return;
        };
        {
            let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
            if *sent >= MAX_LINES {
                return;
            }
            *sent += 1;
        }
        {
            let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            if last.as_deref() == Some(text.as_str()) {
                return;
            }
            *last = Some(text.clone());
        }
        let elapsed = self.started.elapsed().as_secs();
        let detail = if elapsed > 0 {
            format!("{text} · 已用 {elapsed}s")
        } else {
            text
        };
        (self.sink)(EnvProgress {
            detail: Some(detail),
            ..self.base.clone()
        });
    }
}

/// 从 uv 的一行输出里认出「在装哪个包」。认不出返回 None。
pub fn package_line(raw: &str) -> Option<String> {
    let text = raw.trim();
    if text.is_empty() || text.len() > 400 {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let verb = if lower.contains("installing") || lower.contains("installed") {
        "安装"
    } else if lower.contains("downloading") {
        "下载"
    } else if lower.contains("prepared") {
        "准备"
    } else if lower.contains("uninstalling") {
        "卸载"
    } else {
        return None;
    };
    let token = text.split_whitespace().find(|part| part.contains("=="))?;
    let token = token.trim_matches(|c: char| {
        !(c.is_ascii_alphanumeric() || c == '=' || c == '.' || c == '-' || c == '_')
    });
    if !token.contains("==") {
        return None;
    }
    Some(format!("{verb} {}", token.replace("==", " ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(sink: &Arc<Mutex<Vec<EnvProgress>>>, p: EnvProgress) {
        sink.lock().unwrap().push(p);
    }

    #[test]
    fn without_sink_nothing_happens() {
        assert!(!active());
        emit(EnvProgress::new(EnvPhase::Uv, "定位 uv"));
        assert!(last().is_none());
        assert!(LineReporter::new().is_none());
    }

    #[test]
    fn sink_receives_phases_and_last_tracks_them() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let sink: Sink = Arc::new(move |p| collect(&sink_seen, p));
        with_sink(Some(sink), || {
            assert!(active());
            emit(EnvProgress::new(EnvPhase::Uv, "定位 uv").with_detail("uv 0.13.7"));
            emit(EnvProgress::new(EnvPhase::Sync, "同步依赖").with_total(3));
            let last = last().unwrap();
            assert_eq!(last.phase, EnvPhase::Sync);
            assert_eq!(last.total, Some(3));
        });
        assert!(!active(), "离开 with_sink 后必须摘掉接收器");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].detail.as_deref(), Some("uv 0.13.7"));
    }

    #[test]
    fn line_reporter_only_keeps_package_lines() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let sink: Sink = Arc::new(move |p| collect(&sink_seen, p));
        with_sink(Some(sink), || {
            emit(EnvProgress::new(EnvPhase::Sync, "同步依赖"));
            let reporter = LineReporter::new().expect("有接收器就该有上报器");
            reporter.line("DEBUG uv_installer: Installing uvicorn==0.34.0");
            reporter.line("DEBUG uv_installer: Installing uvicorn==0.34.0");
            reporter.line("Resolved 42 packages in 1.2s");
            reporter.line("   Compiling something");
            // 同一条包行只报一次；噪音行全部丢掉。
            let seen = seen.lock().unwrap();
            let details: Vec<_> = seen.iter().filter_map(|p| p.detail.clone()).collect();
            assert!(details.iter().any(|d| d.starts_with("安装 uvicorn 0.34.0")));
            assert_eq!(
                details.iter().filter(|d| d.contains("uvicorn")).count(),
                1,
                "重复行应被去重"
            );
        });
    }

    #[test]
    fn package_line_ignores_non_package_noise() {
        assert!(package_line("Resolved 42 packages in 1.2s").is_none());
        assert!(package_line("").is_none());
        assert!(package_line("Installing a==1.0;").is_some());
    }
}
