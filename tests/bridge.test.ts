/**
 * 桥接层测试。
 *
 * 重点守护一件事：**Tauri 2 的 invoke 在 `__TAURI__.core.invoke`**。
 * Dounai 悬浮窗仓库曾因只读 `__TAURI__.invoke` 导致桌面端整体失效
 * （打包后静默退回浏览器直连，所有上游请求不可达）。
 * 这里用测试把两种位置都覆盖住，防止再次回归。
 */

import { describe, expect, it, beforeEach, afterEach } from "vitest";
import {
  isDesktop,
  listModuleStates,
  setModuleEnabled,
  uninstallModule,
  installModule,
  refreshCatalog,
  downloadPackage,
  trustInfo,
  probeEnvironmentCmd,
  purgeLocalData,
  BridgeUnavailableError,
} from "../src/bridge.js";

type Call = { cmd: string; args?: Record<string, unknown> };

function installTauri(shape: "core" | "legacy", calls: Call[], result: unknown = []) {
  const invoke = async (cmd: string, args?: Record<string, unknown>) => {
    calls.push({ cmd, args });
    return result;
  };
  if (shape === "core") {
    (globalThis as Record<string, unknown>).__TAURI__ = { core: { invoke } };
  } else {
    (globalThis as Record<string, unknown>).__TAURI__ = { invoke };
  }
}

function clearTauri() {
  delete (globalThis as Record<string, unknown>).__TAURI__;
}

describe("isDesktop", () => {
  afterEach(clearTauri);

  it("无 __TAURI__ 时为 false", () => {
    expect(isDesktop()).toBe(false);
  });

  it("Tauri 2 的 core.invoke 形式被识别", () => {
    installTauri("core", []);
    expect(isDesktop()).toBe(true);
  });

  it("兼容旧式顶层 invoke", () => {
    installTauri("legacy", []);
    expect(isDesktop()).toBe(true);
  });

  it("__TAURI__ 存在但没有 invoke 时为 false", () => {
    (globalThis as Record<string, unknown>).__TAURI__ = { core: {} };
    expect(isDesktop()).toBe(false);
  });
});

describe("浏览器模式下的行为", () => {
  beforeEach(clearTauri);

  it("调用命令抛出 BridgeUnavailableError", async () => {
    await expect(listModuleStates()).rejects.toBeInstanceOf(BridgeUnavailableError);
  });

  it("错误信息明确说明需要桌面端", async () => {
    try {
      await listModuleStates();
      expect.fail("应抛出错误");
    } catch (e) {
      expect((e as Error).message).toContain("桌面端");
    }
  });
});

describe("命令调用参数格式", () => {
  afterEach(clearTauri);

  it("list_module_states 不带参数", async () => {
    const calls: Call[] = [];
    installTauri("core", calls, []);
    await listModuleStates();
    expect(calls[0]?.cmd).toBe("list_module_states");
    expect(calls[0]?.args).toBeUndefined();
  });

  it("set_module_enabled 使用 request 包装（与 Rust 结构体匹配）", async () => {
    const calls: Call[] = [];
    installTauri("core", calls);
    await setModuleEnabled("mod-a", false, "optional");
    expect(calls[0]?.cmd).toBe("set_module_enabled");
    // Rust 侧签名是 request: SetEnabledRequest，字段是蛇形
    expect(calls[0]?.args).toEqual({
      request: { module_id: "mod-a", enabled: false, category: "optional" },
    });
  });

  it("uninstall_module 传 remove_files", async () => {
    const calls: Call[] = [];
    installTauri("core", calls);
    await uninstallModule("mod-b", "optional", true);
    expect(calls[0]?.args).toEqual({
      request: { module_id: "mod-b", category: "optional", remove_files: true },
    });
  });

  it("install_module 传 zip_base64、必填哈希与 manifest", async () => {
    const calls: Call[] = [];
    installTauri("core", calls, { module_id: "m", files: 1, bytes: 2, replaced: false, install_dir: "/x" });
    await installModule({
      moduleId: "m",
      category: "optional",
      version: "1.0.0",
      zipBase64: "AAA=",
      expectedSha256: "abc",
      manifest: { module_id: "m" },
    });
    const req = (calls[0]?.args as { request: Record<string, unknown> }).request;
    expect(req.module_id).toBe("m");
    expect(req.zip_base64).toBe("AAA=");
    expect(req.expected_sha256).toBe("abc");
    expect(req.manifest).toEqual({ module_id: "m" });
  });

  it("download_package 带默认重试次数", async () => {
    const calls: Call[] = [];
    installTauri("core", calls, {
      path: "/x.zip",
      bytes: 10,
      source: "https://dl.example.com/x.zip",
      resumed_from: 0,
      attempts: 1,
    });
    await downloadPackage({
      moduleId: "m",
      sources: ["https://dl.example.com/x.zip"],
      expectedSha256: "abc",
    });
    const req = (calls[0]?.args as { request: Record<string, unknown> }).request;
    expect(req.module_id).toBe("m");
    expect(req.sources).toEqual(["https://dl.example.com/x.zip"]);
    expect(req.expected_sha256).toBe("abc");
    expect(req.retries).toBe(2);
  });

  it("trust_info 不带参数", async () => {
    const calls: Call[] = [];
    installTauri("core", calls, { trusted_keys: 0, allow_unsigned: false, open: false });
    const info = await trustInfo();
    expect(calls[0]?.cmd).toBe("trust_info");
    expect(info.open).toBe(false);
  });

  it("purge_local_data 传三项开关", async () => {
    const calls: Call[] = [];
    installTauri("core", calls, {
      removed_caches: [],
      state_cleared: true,
      removed_module_dirs: [],
      errors: [],
    });
    await purgeLocalData({ caches: true, state: false, module_files: true });
    expect(calls[0]?.args).toEqual({
      request: { caches: true, state: false, module_files: true },
    });
  });

  it("refresh_catalog 传 catalogUrl", async () => {
    const calls: Call[] = [];
    installTauri("core", calls, {
      source: "cache",
      entries: [],
      allowlist: [],
      reason: null,
    });
    await refreshCatalog("https://dl.example.com/catalog.json");
    expect(calls[0]?.cmd).toBe("refresh_catalog");
    expect(calls[0]?.args).toEqual({ catalogUrl: "https://dl.example.com/catalog.json" });
  });

  it("probe_environment 不带参数", async () => {
    const calls: Call[] = [];
    installTauri("core", calls, {
      python: { available: true, version: "3.12.0", detail: "" },
      node: { available: true, version: "20.0.0", detail: "" },
      uv: { available: true, version: "0.10.0", detail: "" },
      gpus: [],
      disk: { path: "D:\\", free_gb: 10, ok: true },
      problems: [],
      healthy: true,
    });
    const r = await probeEnvironmentCmd();
    expect(calls[0]?.cmd).toBe("probe_environment");
    expect(r.healthy).toBe(true);
  });
});

describe("旧式 invoke 也能正常工作（兼容性）", () => {
  afterEach(clearTauri);

  it("legacy 形式下命令调用成功", async () => {
    const calls: Call[] = [];
    installTauri("legacy", calls, []);
    await listModuleStates();
    expect(calls).toHaveLength(1);
  });
});
