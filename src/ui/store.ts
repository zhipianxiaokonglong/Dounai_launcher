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
  type EnvPhaseName,
  type EnvProgressEvent,
  type InstanceRuntimeView,
  type RustEnvReport,
  type SettingsSnapshot,
  type TrustInfo,
} from "../bridge.js";

/**
 * 一次依赖安装在界面上的状态。
 *
 * 这是纯界面状态，不进后端：Rust 只报「现在在哪一步」，开始与结束由发起方
 * 决定——invoke 返回或抛错就是结束，不需要一条额外的结束事件。
 */
export interface EnvJob {
  phase: EnvPhaseName | "starting";
  title: string;
  detail: string | null;
  current: number | null;
  total: number | null;
  /** 失败原因；成功后为 null。有值时卡片保留这段文字，直到下次重试。 */
  error: string | null;
  startedAt: number;
}

export interface Store {
  /** 已登记实例的合并视图（含端口、active、快照）。 */
  views: InstanceRuntimeView[];
  env: RustEnvReport | null;
  /** 上次环境检测时间（本地时钟，只用于「上次检测 …」这一行文案）。 */
  envCheckedAt: Date | null;
  settings: SettingsSnapshot | null;
  trust: TrustInfo | null;
  devMode: boolean;
  /** 依赖安装的进行中/失败状态，按 instance_id 存。 */
  envJobs: Record<string, EnvJob>;
}

export const store: Store = {
  views: [],
  env: null,
  envCheckedAt: null,
  settings: null,
  trust: null,
  devMode: false,
  envJobs: {},
};

/** 桥接错误的可读文案。 */
export function describeError(error: unknown): string {
  if (error instanceof BridgeUnavailableError) return error.message;
  if (error instanceof Error) return error.message;
  return String(error);
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

/**
 * 模块展示名。
 *
 * 名字只有两个来源：实例视图里 Rust 读回的清单名，或 module_id 本身。
 * 以前这里还查一份打包进 dist 的内置清单，那份清单是 v1 的 module_id
 * （`dounai-input` vs `dounai/dounai-input`），查表恒落空，等于白写。
 */
export function moduleName(moduleId: string): string {
  const first = store.views.find((view) => view.instance.module_id === moduleId);
  if (first && first.name) return first.name;
  return moduleId;
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

/**
 * 进程还在（哪怕状态机已经判失败）。
 *
 * 启动探测超时会保留 pid：进程活着但没在预算内就绪。此时不能再给「启动」，
 * 先给「停止」，否则点下去只会收到「进程还在运行，先停止再启动」。
 */
export function hasLiveProcess(view: InstanceRuntimeView): boolean {
  return typeof view.snapshot?.pid === "number";
}

/** 页头与状态栏用的计数。 */export interface RunCounts {
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

/* ------------------------------------------------------------------ */
/* 依赖安装进度（纯界面状态）                                          */
/* ------------------------------------------------------------------ */

/** 开始一次安装：先把卡片切到「准备中」，避免点下去几秒内没有任何反馈。 */
export function beginEnvJob(instanceId: string): void {
  store.envJobs[instanceId] = {
    phase: "starting",
    title: "准备开始",
    detail: null,
    current: null,
    total: null,
    error: null,
    startedAt: Date.now(),
  };
}

/** 收到后端的一次阶段上报。 */
export function applyEnvProgress(progress: EnvProgressEvent): void {
  const current = store.envJobs[progress.instanceId];
  store.envJobs[progress.instanceId] = {
    phase: progress.phase,
    title: progress.title,
    detail: progress.detail,
    current: progress.current,
    total: progress.total,
    // 新阶段意味着重试过一次，旧错误就不再成立。
    error: null,
    startedAt: current?.startedAt ?? Date.now(),
  };
}

/**
 * 结束一次安装。
 *
 * 成功就整条抹掉——卡片回到「依赖已装」的正常样子；失败留下 error，
 * 卡片继续显示原因，用户重试时才覆盖。
 */
export function endEnvJob(instanceId: string, error?: string): void {
  const current = store.envJobs[instanceId];
  if (!error) {
    delete store.envJobs[instanceId];
    return;
  }
  store.envJobs[instanceId] = {
    phase: current?.phase ?? "starting",
    title: current?.title ?? "安装中断",
    detail: null,
    current: null,
    total: null,
    error,
    startedAt: current?.startedAt ?? Date.now(),
  };
}

export function envJobOf(instanceId: string): EnvJob | null {
  return store.envJobs[instanceId] ?? null;
}
