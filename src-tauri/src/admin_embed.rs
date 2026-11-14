//! 内嵌管理页宿主：把 M0 冻结的 `admin_host::AdminHost` 接到 Tauri 子 WebView。
//!
//! `admin_host.rs` 只定接口与不变式，不碰桌面 API；创建、隐藏、销毁在这里。
//! 主窗口保留自己的 WebView，管理页作为 `Window::add_child` 挂上来的原生子视图，
//! 覆盖内容区里 `#admin-slot` 的位置。这样不用把主界面也改成无 WebView 的壳，
//! 而管理页仍是顶层 WebView——三个上游的 `X-Frame-Options: DENY` 不受影响。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::mpsc::channel;
use std::time::Duration;

use launcher_core::admin_host::{
    validate_mount_request, AdminHost, AdminHostError, Bounds, MountRequest,
};
use tauri::{
    webview::WebviewBuilder, AppHandle, LogicalPosition, LogicalSize, Manager, Url, Webview,
    WebviewUrl,
};

pub const MAIN_WINDOW: &str = "main";
pub const LABEL_PREFIX: &str = "admin-embed-";
const MOUNT_TIMEOUT: Duration = Duration::from_secs(15);

/// 外部 URL 的 WebView 会继承 Tauri 的全局注入。
///
/// capability 已改成按 webview label 授权（见 `capabilities/default.json`），
/// 内嵌页没有任何命令权限；这里仍要清掉注入对象：页面不该知道自己跑在
/// Tauri 里，也不该留下可枚举的 IPC 入口。`for_all_frames` 是必须的——
/// 上游页面可能用 iframe 加载子页面，只清顶层会留后门。
///
/// 不能全局关 `withGlobalTauri`：主窗口的 `bridge.ts` 依赖 `window.__TAURI__`。
/// 独立窗口的 fallback 用同一份脚本，避免两条路径的清理规则分叉。
pub const ADMIN_SANITIZE_SCRIPT: &str = r#"(function(){ try { window.__DOUNAI_ADMIN_SANITIZED__ = true; } catch (e) {}
  var kill = function (name) {
    try { delete window[name]; } catch (e) {}
    if (typeof window[name] !== 'undefined') {
      try {
        Object.defineProperty(window, name, { value: undefined, configurable: false, writable: false });
      } catch (e) {
        try { window[name] = undefined; } catch (e2) {}
      }
    }
  };
  kill('__TAURI__');
  kill('__TAURI_IPC__');
  kill('__TAURI_METADATA__');
  try {
    Object.defineProperty(window, '__TAURI_INTERNALS__', {
      value: Object.freeze({}),
      configurable: false,
      writable: false,
    });
  } catch (e) {
    try { window.__TAURI_INTERNALS__ = Object.freeze({}); } catch (e2) {}
  }
})();"#;

/// label 由 `instance_id` 派生，重复点击同一个实例复用同一个子 WebView。
///
/// `validate_mount_request` 只收 ASCII 字母数字与 `_` `-`，而 instance_id 可能带
/// `/`、`@`、空格甚至中文。全部替换成 `_` 会碰撞（两个不同 id 洗成同一个串），
/// 所以尾部再挂一份原始串的哈希。长度按 schema 的 128 上限反推。
pub fn derive_label(instance_id: &str) -> String {
    let mut hasher = DefaultHasher::new();
    instance_id.hash(&mut hasher);
    let tail = format!("{:016x}", hasher.finish());
    let keep = 128usize
        .saturating_sub(LABEL_PREFIX.len())
        .saturating_sub(tail.len())
        .saturating_sub(1);
    let head: String = instance_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(keep)
        .collect();
    format!("{LABEL_PREFIX}{head}_{tail}")
}

fn find(app: &AppHandle, label: &str) -> Option<Webview> {
    app.get_window(MAIN_WINDOW)?
        .webviews()
        .into_iter()
        .find(|w| w.label() == label)
}

fn unavailable(label: &str) -> AdminHostError {
    AdminHostError::Unavailable(label.to_string())
}

fn valid_bounds(b: Bounds) -> bool {
    b.x.is_finite()
        && b.y.is_finite()
        && b.width.is_finite()
        && b.height.is_finite()
        && b.x >= 0.0
        && b.y >= 0.0
        && b.width > 0.0
        && b.height > 0.0
}

#[derive(Clone)]
pub struct TauriAdminHost {
    app: AppHandle,
}

impl TauriAdminHost {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }

    /// 窗口缩放时重下发尺寸。不创建 WebView，无需挪出命令线程。
    ///
    /// 不进 `AdminHost` trait：那是 M0 冻结契约，只有 mount/unmount/reload 三个方法，
    /// 而「尺寸变化」没有生命周期语义，不该为了它改契约。
    pub fn resize(&self, label: &str, bounds: Bounds) -> Result<(), AdminHostError> {
        if !valid_bounds(bounds) {
            return Err(AdminHostError::BadBounds);
        }
        let webview = find(&self.app, label).ok_or_else(|| unavailable("子 WebView 未挂载"))?;
        webview
            .set_position(LogicalPosition::new(bounds.x, bounds.y))
            .map_err(|e| unavailable(&e.to_string()))?;
        webview
            .set_size(LogicalSize::new(bounds.width, bounds.height))
            .map_err(|e| unavailable(&e.to_string()))?;
        Ok(())
    }

    /// 切到别的视图时隐藏而不是销毁：管理页里填了一半的表单不该因为看一眼日志就没了。
    pub fn set_visible(&self, label: &str, visible: bool) -> Result<(), AdminHostError> {
        let webview = find(&self.app, label).ok_or_else(|| unavailable("子 WebView 未挂载"))?;
        let result = if visible {
            webview.show()
        } else {
            webview.hide()
        };
        result.map_err(|e| unavailable(&e.to_string()))
    }
}

impl AdminHost for TauriAdminHost {
    fn mount(&self, req: &MountRequest) -> Result<(), AdminHostError> {
        let port = validate_mount_request(req)?;
        let app = self.app.clone();
        let req = req.clone();
        let (tx, rx) = channel();
        // Tauri 2 在 Windows 上建 WebView 必须挪出命令线程，否则 Webview2 的 COM
        // 初始化会死锁在 about:blank。连续切换多个实例时也不能在命令线程里排队。
        std::thread::spawn(move || {
            let result = mount_sync(&app, &req, port);
            let _ = tx.send(result);
        });
        rx.recv_timeout(MOUNT_TIMEOUT)
            .unwrap_or_else(|_| Err(unavailable("创建子 WebView 超时")))
    }

    fn unmount(&self, label: &str) -> Result<(), AdminHostError> {
        let webview = find(&self.app, label)
            .ok_or_else(|| AdminHostError::NotMounted(label.to_string()))?;
        webview
            .close()
            .map_err(|e| unavailable(&e.to_string()))
    }

    fn reload(&self, label: &str) -> Result<(), AdminHostError> {
        let webview = find(&self.app, label)
            .ok_or_else(|| AdminHostError::NotMounted(label.to_string()))?;
        webview
            .reload()
            .map_err(|e| unavailable(&e.to_string()))
    }
}

fn mount_sync(app: &AppHandle, req: &MountRequest, port: u16) -> Result<(), AdminHostError> {
    // 复用：URL 变了就 navigate，否则只更新位置并显示。重复点「管理」不该
    // 建出第二个同 label 的 WebView（label 撞了会直接报错）。
    if let Some(existing) = find(app, &req.label) {
        let current = existing.url().ok().map(|u| u.to_string());
        if current.as_deref() != Some(req.url.as_str()) {
            let parsed = req
                .url
                .parse::<Url>()
                .map_err(|e| unavailable(&e.to_string()))?;
            existing
                .navigate(parsed)
                .map_err(|e| unavailable(&e.to_string()))?;
        }
        existing
            .set_position(LogicalPosition::new(req.bounds.x, req.bounds.y))
            .map_err(|e| unavailable(&e.to_string()))?;
        existing
            .set_size(LogicalSize::new(req.bounds.width, req.bounds.height))
            .map_err(|e| unavailable(&e.to_string()))?;
        existing.show().map_err(|e| unavailable(&e.to_string()))?;
        return Ok(());
    }

    let window = app
        .get_window(MAIN_WINDOW)
        .ok_or_else(|| unavailable("主窗口不存在"))?;
    let parsed = req
        .url
        .parse::<Url>()
        .map_err(|e| unavailable(&e.to_string()))?;
    let builder = WebviewBuilder::new(req.label.clone(), WebviewUrl::External(parsed))
        .initialization_script_for_all_frames(ADMIN_SANITIZE_SCRIPT)
        // 管理页可以站内跳转，但不许跳到别的 host 或端口：后者会捅到
        // 同一台机器上别的模块服务，等于用管理页当跳板。
        .on_navigation(move |url| {
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port_or_known_default() == Some(port)
        });
    window
        .add_child(
            builder,
            LogicalPosition::new(req.bounds.x, req.bounds.y),
            LogicalSize::new(req.bounds.width, req.bounds.height),
        )
        .map_err(|e| unavailable(&format!("创建子 WebView 失败：{e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(label: &str) -> MountRequest {
        MountRequest {
            label: label.into(),
            url: "http://127.0.0.1:8102/settings".into(),
            bounds: Bounds {
                x: 240.0,
                y: 90.0,
                width: 900.0,
                height: 600.0,
            },
        }
    }

    #[test]
    fn derived_labels_are_valid_and_stable() {
        for id in [
            "omninexus/stable",
            "dounai/input@main",
            "带 空格 的 实例",
            &"x".repeat(300),
        ] {
            let label = derive_label(id);
            assert!(label.len() <= 128, "label 过长：{}", label.len());
            assert_eq!(validate_mount_request(&request(&label)), Ok(8102));
            assert_eq!(label, derive_label(id), "同一实例必须复用同一个 label");
        }
    }

    #[test]
    fn distinct_instances_do_not_collide() {
        let a = derive_label("dounai/input@a b");
        let b = derive_label("dounai/input_a_b");
        assert_ne!(a, b, "非法字符被替换后不能靠可见部分区分");
    }

    #[test]
    fn root_path_from_input_module_is_mountable() {
        // dounai-input / moodvoice 的 web_ui.path 是 "/"，曾在这里被判
        // 「缺少路径」而降级成独立窗口。宿主是真正的调用点，再钉一次。
        let mut r = request(&derive_label("dounai/input@stable"));
        r.url = "http://127.0.0.1:8101/".into();
        assert_eq!(validate_mount_request(&r), Ok(8101));
    }

    #[test]
    fn resize_rejects_bad_bounds() {
        for bad in [
            Bounds { x: -1.0, y: 0.0, width: 10.0, height: 10.0 },
            Bounds { x: 0.0, y: 0.0, width: 0.0, height: 10.0 },
            Bounds { x: f64::NAN, y: 0.0, width: 10.0, height: 10.0 },
            Bounds { x: f64::INFINITY, y: 0.0, width: 10.0, height: 10.0 },
        ] {
            assert!(!valid_bounds(bad), "非法尺寸必须挡住：{bad:?}");
        }
        assert!(valid_bounds(Bounds { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }));
    }
}
