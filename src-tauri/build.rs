use std::path::{Path, PathBuf};

fn main() {
    tauri_build::build();
    stage_bundled_uv();
}

/// 把 `bin/uv.exe` 复制到与可执行文件同级的 `bin/` 下。
///
/// `Paths::bundled_uv_path()` 只认 `<exe 目录>/bin/uv.exe`：NSIS 安装后由
/// `bundle.resources` 落到 `$INSTDIR/bin/`，而 `tauri dev` 与直接跑
/// `target/<profile>/*.exe` 时没有任何东西负责这一步，于是「随包自带 uv」在开发
/// 和本地验收下等于不存在。这里补上，让两种运行方式走同一条候选路径。
///
/// 大小没变就不重复拷贝，省掉每次构建 31 MB 的写盘。
fn stage_bundled_uv() {
    let source = PathBuf::from("bin").join("uv.exe");
    println!("cargo:rerun-if-changed=bin/uv.exe");
    if !source.exists() {
        println!(
            "cargo:warning=缺少 {}，本次构建不带 uv；运行时会退到联网补齐",
            source.display()
        );
        return;
    }
    let Some(profile_dir) = profile_dir() else {
        println!("cargo:warning=无法定位 target/<profile>，跳过 uv 暂存");
        return;
    };
    let target_dir = profile_dir.join("bin");
    let target = target_dir.join("uv.exe");
    if same_size(&source, &target) {
        return;
    }
    if let Err(error) =
        std::fs::create_dir_all(&target_dir).and_then(|_| std::fs::copy(&source, &target).map(|_| ()))
    {
        println!(
            "cargo:warning=暂存 uv 到 {} 失败：{error}",
            target.display()
        );
    }
}

/// `OUT_DIR` 形如 `<target>/<profile>/build/<pkg>-<hash>/out`，上溯三级即 profile 目录。
fn profile_dir() -> Option<PathBuf> {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR")?);
    out_dir.ancestors().nth(3).map(Path::to_path_buf)
}

fn same_size(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.len() == b.len(),
        _ => false,
    }
}
