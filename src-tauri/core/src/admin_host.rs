//! 管理页宿主契约。桌面壳的 unstable API 调用留在壳内，本模块只定接口与不变式。

use std::sync::Mutex;

/// 子 WebView 的位置与尺寸，由 UI 一处计算、core 一处下发。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// 挂载请求。`label` 由 instance_id 派生，`url` 只能来自 `admin_url_for`。
#[derive(Debug, Clone, PartialEq)]
pub struct MountRequest {
    pub label: String,
    pub url: String,
    pub bounds: Bounds,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AdminHostError {
    #[error("URL 不是回环地址：{0}")]
    NotLoopback(String),
    #[error("URL 缺少路径：{0}")]
    MissingPath(String),
    #[error("URL 路径含越界组件：{0}")]
    TraversalPath(String),
    #[error("端口非法：{0}")]
    BadPort(String),
    #[error("label 非法：{0}")]
    BadLabel(String),
    #[error("尺寸非法")]
    BadBounds,
    #[error("未挂载的 label：{0}")]
    NotMounted(String),
    #[error("宿主不可用：{0}")]
    Unavailable(String),
}

/// 校验挂载请求。桌面壳调用 unstable API 前先跑这里，CI 也能直接测。
pub fn validate_mount_request(req: &MountRequest) -> Result<u16, AdminHostError> {
    if req.label.is_empty()
        || req.label.len() > 128
        || !req
            .label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(AdminHostError::BadLabel(req.label.clone()));
    }

    let b = req.bounds;
    if !b.x.is_finite()
        || !b.y.is_finite()
        || !b.width.is_finite()
        || !b.height.is_finite()
        || b.x < 0.0
        || b.y < 0.0
        || b.width <= 0.0
        || b.height <= 0.0
    {
        return Err(AdminHostError::BadBounds);
    }

    let rest = req
        .url
        .strip_prefix("http://127.0.0.1:")
        .ok_or_else(|| AdminHostError::NotLoopback(req.url.clone()))?;
    let (port_text, path) = rest
        .split_once('/')
        .ok_or_else(|| AdminHostError::MissingPath(req.url.clone()))?;
    let port = port_text
        .parse::<u16>()
        .map_err(|_| AdminHostError::BadPort(port_text.to_string()))?;
    if port == 0 {
        return Err(AdminHostError::BadPort(port_text.to_string()));
    }
    // 根路径 `/` 必须放行：contracts/examples 里 dounai-input 与 moodvoice 的
    // `admin_ui.web_ui.path` 就是 "/"，拼出来是 http://127.0.0.1:8101/。
    // 这条校验在 M4 之前没有真正的消费者，第一次内嵌时把两个官方模块误判成
    // 「缺少路径」并降级成独立窗口。host 与 port 已经锁死在回环地址上，指向
    // 服务根不构成越界；这里只挡路径里的 `..`（query / fragment 一并检查，
    // 保守拒绝好过放行）。
    if path.split('/').any(|part| part == "..") {
        return Err(AdminHostError::TraversalPath(req.url.clone()));
    }
    Ok(port)
}

/// 宿主能力面。子 WebView 与独立窗口 fallback 都按这三个方法对接。
pub trait AdminHost: Send + Sync {
    fn mount(&self, req: &MountRequest) -> Result<(), AdminHostError>;
    fn unmount(&self, label: &str) -> Result<(), AdminHostError>;
    fn reload(&self, label: &str) -> Result<(), AdminHostError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    Mount(String),
    Unmount(String),
    Reload(String),
}

/// 记录调用的宿主，供无窗口测试与 CI 断言。不创建任何窗口。
#[derive(Debug, Default)]
pub struct RecordingAdminHost {
    events: Mutex<Vec<HostEvent>>,
}

impl RecordingAdminHost {
    pub fn events(&self) -> Vec<HostEvent> {
        self.events.lock().unwrap().clone()
    }

    fn is_mounted(&self, label: &str) -> bool {
        let mut mounted = false;
        for event in self.events.lock().unwrap().iter() {
            match event {
                HostEvent::Mount(l) if l == label => mounted = true,
                HostEvent::Unmount(l) if l == label => mounted = false,
                _ => {}
            }
        }
        mounted
    }
}

impl AdminHost for RecordingAdminHost {
    fn mount(&self, req: &MountRequest) -> Result<(), AdminHostError> {
        validate_mount_request(req)?;
        self.events
            .lock()
            .unwrap()
            .push(HostEvent::Mount(req.label.clone()));
        Ok(())
    }

    fn unmount(&self, label: &str) -> Result<(), AdminHostError> {
        if !self.is_mounted(label) {
            return Err(AdminHostError::NotMounted(label.to_string()));
        }
        self.events
            .lock()
            .unwrap()
            .push(HostEvent::Unmount(label.to_string()));
        Ok(())
    }

    fn reload(&self, label: &str) -> Result<(), AdminHostError> {
        if !self.is_mounted(label) {
            return Err(AdminHostError::NotMounted(label.to_string()));
        }
        self.events
            .lock()
            .unwrap()
            .push(HostEvent::Reload(label.to_string()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(label: &str, url: &str) -> MountRequest {
        MountRequest {
            label: label.into(),
            url: url.into(),
            bounds: Bounds {
                x: 10.0,
                y: 20.0,
                width: 800.0,
                height: 600.0,
            },
        }
    }

    #[test]
    fn accepts_loopback_with_path_and_returns_port() {
        let r = request("omninexus_stable_main", "http://127.0.0.1:8102/settings");
        assert_eq!(validate_mount_request(&r), Ok(8102));
        let spare = request("probe", "http://127.0.0.1:18102/settings?tab=logs");
        assert_eq!(validate_mount_request(&spare), Ok(18102));
        // 服务根也是合法入口，不只是「/后面还得有字」。
        assert_eq!(
            validate_mount_request(&request("probe", "http://127.0.0.1:8101/")),
            Ok(8101)
        );
    }

    /// 回归：宿主校验必须能接受所有官方样例 manifest 声明的管理页路径。
    ///
    /// 之前只拿 `/settings` 写用例，结果 `web_ui.path = "/"` 的两个样例在真机上
    /// 全部降级成独立窗口。样例改了这里会跟着变，避免契约与宿主再次分叉。
    #[test]
    fn accepts_every_official_manifest_web_ui_path() {
        const EXAMPLES: &[&str] = &[
            include_str!("../../../contracts/examples/dounai-input.v2.json"),
            include_str!("../../../contracts/examples/moodvoice.v2.json"),
            include_str!("../../../contracts/examples/omninexus.v2.json"),
        ];
        for raw in EXAMPLES {
            let manifest: serde_json::Value = serde_json::from_str(raw).unwrap();
            let path = manifest["admin_ui"]["web_ui"]["path"]
                .as_str()
                .expect("样例必须声明 web_ui.path");
            let url = format!("http://127.0.0.1:8101{path}");
            assert_eq!(
                validate_mount_request(&request("probe", &url)),
                Ok(8101),
                "官方样例路径被宿主拒绝：{url}"
            );
        }
    }

    #[test]
    fn rejects_any_non_loopback_or_broken_url() {
        for url in [
            "http://localhost:8102/settings",
            "https://127.0.0.1:8102/settings",
            "http://0.0.0.0:8102/settings",
            "http://127.0.0.1.evil.com:8102/settings",
            "http://127.0.0.1:0/settings",
            "http://127.0.0.1:99999/settings",
            "http://127.0.0.1:8102",
            "http://127.0.0.1:8102/../admin",
            "file:///C:/evil.html",
        ] {
            let r = request("probe", url);
            assert!(validate_mount_request(&r).is_err(), "未被拒绝：{url}");
        }
        // 「没有斜杠」与「路径越界」是两种不同的失败，别再混成 MissingPath。
        assert_eq!(
            validate_mount_request(&request("probe", "http://127.0.0.1:8102")),
            Err(AdminHostError::MissingPath(
                "http://127.0.0.1:8102".to_string()
            ))
        );
        assert_eq!(
            validate_mount_request(&request("probe", "http://127.0.0.1:8102/../admin")),
            Err(AdminHostError::TraversalPath(
                "http://127.0.0.1:8102/../admin".to_string()
            ))
        );
    }

    #[test]
    fn rejects_unsafe_label_and_bounds() {
        for label in ["", "a/b", "has space", "omninexus@stable", &"x".repeat(129)] {
            let r = request(label, "http://127.0.0.1:8102/settings");
            assert_eq!(
                validate_mount_request(&r),
                Err(AdminHostError::BadLabel(label.to_string()))
            );
        }
        let mut r = request("probe", "http://127.0.0.1:8102/settings");
        r.bounds.width = 0.0;
        assert_eq!(validate_mount_request(&r), Err(AdminHostError::BadBounds));
        r.bounds.width = f64::NAN;
        assert_eq!(validate_mount_request(&r), Err(AdminHostError::BadBounds));
    }

    #[test]
    fn host_tracks_lifecycle_and_rejects_bad_requests() {
        let host = RecordingAdminHost::default();
        let ok = request("probe", "http://127.0.0.1:8102/settings");
        let bad = request("probe", "http://evil.example/settings");
        assert!(host.mount(&bad).is_err());
        assert!(host.events().is_empty());
        assert!(host.reload("probe").is_err());
        host.mount(&ok).unwrap();
        host.reload("probe").unwrap();
        host.unmount("probe").unwrap();
        assert_eq!(
            host.events(),
            vec![
                HostEvent::Mount("probe".into()),
                HostEvent::Reload("probe".into()),
                HostEvent::Unmount("probe".into()),
            ]
        );
        assert!(host.reload("probe").is_err());
    }
}
