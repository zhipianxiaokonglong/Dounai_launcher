/**
 * 界面共享状态与数据加载。
 *
 * 一个原则：**每个事实只有一个来源**。
 * 实例与端口来自 `list_instance_views`，环境来自 `probe_environment`，
 * 信任与保留数来自 `settings_snapshot`；界面层不自己推算端口、
 * 不自己拼指纹、不缓存会与后端分叉的派生值。
 */

import {
  isDesktop,
  listInstanceViews,
  probeEnvironmentCmd,
  settingsSnapshot,
  trustInfo,
  devModeStatus,
  BridgeUnavailableError,
  type CatalogResponse,
  type InstanceRuntimeView,
  type RustEnvReport,
  type SettingsSnapshot,
  type TrustInfo,
} from "../bridge.js";
import { type BundleDefinition, parseBundle } from "../bundle.js";
import { type ModuleManifest, parseManifest } from "../manifest.js";

export interface Store {
  /** 已登记实例的合并视图（含端口、active、快照）。 */
  views: InstanceRuntimeView[];
  /** 打包进 dist 的内置清单：未安装模块的展示名与描述来自这里。 */
  manifests: Map<string, ModuleManifest>;
  bundles: BundleDefinition[];
  env: RustEnvReport | null;
  /** 上次环境检测时间（本地时钟，只用于「上次检测 …」这一行文案）。 */
  envCheckedAt: Date | null;
  settings: SettingsSnapshot | null;
  trust: TrustInfo | null;
  catalog: CatalogResponse | null;
  devMode: boolean;
}

export const store: Store = {
  views: [],
  manifests: new Map(),
  bundles: [],
  env: null,
  envCheckedAt: null,
  settings: null,
  trust: null,
  catalog: null,
  devMode: false,
};

/** 桥接错误的可读文案。 */
export function describeError(error: unknown): string {
  if (error instanceof BridgeUnavailableError) return error.message;
  if (error instanceof Error) return error.message;
  return String(error);
}

/* ------------------------------------------------------------------ */
/* 内置元数据（构建期内联，永远可读）                                  */
/* ------------------------------------------------------------------ */

export async function loadMeta(): Promise<void> {
  const manifestFiles = import.meta.glob("/manifests/*.json", {
    eager: true,
    import: "default",
  });
  for (const raw of Object.values(manifestFiles)) {
    try {
      const manifest = parseManifest(raw);
      store.manifests.set(manifest.module_id, manifest);
    } catch (error) {
      console.warn("跳过非法内置清单", error);
    }
  }

  const bundleFiles = import.meta.glob("/bundles/*.json", { eager: true, import: "default" });
  const order = ["minimal", "lite", "full", "offline"];
  store.bundles = Object.values(bundleFiles)
    .map((raw) => {
      try {
        return parseBundle(raw);
      } catch (error) {
        console.warn("跳过非法套装", error);
        return null;
      }
    })
    .filter((bundle): bundle is BundleDefinition => bundle !== null)
    .sort((a, b) => order.indexOf(a.bundle_id) - order.indexOf(b.bundle_id));
}

/* ------------------------------------------------------------------ */
/* 后端事实                                                            */
/* ------------------------------------------------------------------ */

/** 拉取实例视图。浏览器模式下清空——界面会显示「需桌面端」而不是假数据。 */
export async function refreshViews(): Promise<void> {
  if (!isDesktop()) {
    store.views = [];
    return;
  }
  store.views = await listInstanceViews();
}

export async function refreshEnv(): Promise<void> {
  if (!isDesktop()) {
    store.env = null;
    return;
  }
  store.env = await probeEnvironmentCmd();
  store.envCheckedAt = new Date();
}

export async function refreshSettings(): Promise<void> {
  if (!isDesktop()) {
    store.settings = null;
    store.trust = null;
    return;
  }
  const [snapshot, trust] = await Promise.all([settingsSnapshot(), trustInfo()]);
  store.settings = snapshot;
  store.trust = trust;
}

export async function refreshDevMode(): Promise<void> {
  try {
    store.devMode = (await devModeStatus()).enabled;
  } catch {
    store.devMode = false;
  }
}

/* ------------------------------------------------------------------ */
/* 派生查询（只读，不缓存）                                            */
/* ------------------------------------------------------------------ */

/** 模块展示名：优先内置清单，其次模块根 manifest 读回的名字，最后退回 id。 */
export function moduleLabel(moduleId: string): string {
  const manifest = store.manifests.get(moduleId);
  if (manifest) return manifest.name;
  const first = store.views.find((view) => view.instance.module_id === moduleId);
  return first?.instance.module_id ?? moduleId;
}

export function manifestOf(moduleId: string): ModuleManifest | null {
  return store.manifests.get(moduleId) ?? null;
}

/** 已登记的模块 id（去重、稳定顺序）。 */
export function installedModuleIds(): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const view of store.views) {
    if (seen.has(view.instance.module_id)) continue;
    seen.add(view.instance.module_id);
    out.push(view.instance.module_id);
  }
  return out;
}

/** 有实例在跑（含外部启动）就算运行中。 */
export function isRunning(view: InstanceRuntimeView): boolean {
  const phase = view.snapshot?.phase;
  return phase === "running" || phase === "starting";
}

export function isExternal(view: InstanceRuntimeView): boolean {
  // 外部实例由 Supervisor 合成快照：Running 但没有 pid。
  return view.snapshot?.phase === "running" && view.snapshot.pid === null;
}

/** 页头与状态栏用的计数。 */
export interface RunCounts {
  running: number;
  starting: number;
  failed: number;
  external: number;
  total: number;
}

export function runCounts(): RunCounts {
  let running = 0;
  let starting = 0;
  let failed = 0;
  let external = 0;
  for (const view of store.views) {
    const phase = view.snapshot?.phase;
    if (phase === "running") {
      running += 1;
      if (isExternal(view)) external += 1;
    } else if (phase === "starting") {
      starting += 1;
    } else if (phase === "failed") {
      failed += 1;
    }
  }
  return { running, starting, failed, external, total: store.views.length };
}
