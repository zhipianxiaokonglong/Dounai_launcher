/**
 * Tauri 命令桥接层。
 *
 * 为什么单独一层：UI 需要「在浏览器里也能跑」（便于开发与截图验证），
 * 但真实能力只在 Tauri 里可用。这里统一做能力探测，
 * 并把「不可用」表达为明确的返回值，而不是让调用方各自判断
 * `window.__TAURI__` 是否存在。
 *
 * 注意 Tauri 2 的 invoke 在 `__TAURI__.core.invoke`，不是 `__TAURI__.invoke`
 * ——Dounai 悬浮窗仓库曾因读错位置导致桌面端整体退回浏览器直连，
 * 这里沿用同一套兼容写法。
 */

export type ModuleInstallState = "not_installed" | "installed" | "installing" | "broken";
export type ModuleRunState = "stopped" | "starting" | "running" | "failed";

export interface ModuleView {
  module_id: string;
  install: ModuleInstallState;
  enabled: boolean;
  version: string;
  path: string;
  run: ModuleRunState;
  last_error: string | null;
  /** 运行中时的进程号 */
  pid: number | null;
  /** 最近一次健康检查是否通过 */
  healthy: boolean;
  /** 是否为外部链接模块（文件在用户自己的目录里） */
  linked: boolean;
  /**
   * 该进程不是本启动器拉起的（用户自行启动，或上一版启动器遗留）。
   *
   * 这类模块健康检查通过、状态显示为运行中，但**无法用「停止」按钮停掉**
   * ——它不在当前进程表里。UI 必须据此给出可读的下一步，
   * 而不是让用户点一个没反应的按钮。
   */
  external: boolean;
}

/** 启停命令返回的运行快照。 */
export interface RunView {
  module_id: string;
  run: ModuleRunState;
  pid: number | null;
  last_error: string | null;
  healthy: boolean;
  /** true 表示端口上已有外部启动的健康服务，本次没有拉起进程（D-4）。 */
  external: boolean;
}

export interface CatalogEntry {
  module_id: string;
  name: string;
  version: string;
  download_url: string;
  sha256: string;
}

export interface CatalogResponse {
  source: "remote" | "cache" | "unavailable";
  entries: CatalogEntry[];
  allowlist: string[];
  reason: string | null;
}

export interface InstallResponse {
  module_id: string;
  files: number;
  bytes: number;
  replaced: boolean;
  install_dir: string;
  /** 信任判定：signed（已验签）或 unsigned_allowed（调试放行） */
  trust: { kind: "signed"; key_index: number } | { kind: "unsigned_allowed" };
}

export interface DownloadResponse {
  path: string;
  bytes: number;
  source: string;
  resumed_from: number;
  attempts: number;
}

export interface TrustInfo {
  trusted_keys: number;
  allow_unsigned: boolean;
  /** 完全放行（无公钥且允许未签名）——仅开发期 */
  open: boolean;
}

export interface PurgeRequest {
  caches: boolean;
  state: boolean;
  module_files: boolean;
}

export interface PurgeResponse {
  removed_caches: string[];
  state_cleared: boolean;
  removed_module_dirs: string[];
  errors: string[];
}

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

interface TauriGlobal {
  core?: { invoke?: Invoke };
  invoke?: Invoke;
}

function getInvoke(): Invoke | null {
  const tauri = (globalThis as { __TAURI__?: TauriGlobal }).__TAURI__;
  return tauri?.core?.invoke ?? tauri?.invoke ?? null;
}

/** 是否运行在 Tauri 桌面端。 */
export function isDesktop(): boolean {
  return getInvoke() !== null;
}

/** 桥接不可用时的统一错误，便于 UI 显示一致的提示。 */
export class BridgeUnavailableError extends Error {
  constructor() {
    super("需要桌面端才能执行该操作（浏览器模式仅可预览界面）");
    this.name = "BridgeUnavailableError";
  }
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const invoke = getInvoke();
  if (!invoke) throw new BridgeUnavailableError();
  return (await invoke(cmd, args)) as T;
}

/* ------------------------------------------------------------------ */
/* 模块运行（启动 / 停止）                                             */
/* ------------------------------------------------------------------ */

export async function startModule(moduleId: string): Promise<RunView> {
  return call<RunView>("start_module", { moduleId });
}

export async function stopModule(moduleId: string): Promise<RunView> {
  return call<RunView>("stop_module", { moduleId });
}

/** 一键启动：按依赖顺序拉起所有已启用模块。 */
export async function startAll(): Promise<RunView[]> {
  return call<RunView[]>("start_all");
}

/** 全部停止。 */
export async function stopAll(): Promise<RunView[]> {
  return call<RunView[]>("stop_all");
}

/* ------------------------------------------------------------------ */
/* 模块状态                                                            */
/* ------------------------------------------------------------------ */

export async function listModuleStates(): Promise<ModuleView[]> {
  return call<ModuleView[]>("list_module_states");
}

export async function setModuleEnabled(
  moduleId: string,
  enabled: boolean,
  category: string,
): Promise<void> {
  await call("set_module_enabled", { request: { module_id: moduleId, enabled, category } });
}

export async function uninstallModule(
  moduleId: string,
  category: string,
  removeFiles: boolean,
): Promise<void> {
  await call("uninstall_module", {
    request: { module_id: moduleId, category, remove_files: removeFiles },
  });
}

/* ------------------------------------------------------------------ */
/* 本地模块（手动添加）                                                */
/* ------------------------------------------------------------------ */

/** 扫描到的候选模块。 */
export interface ScanCandidate {
  path: string;
  module_id: string;
  name: string;
  version: string;
  category: string;
  signed: boolean;
  registered: boolean;
}

/** 在给定目录下扫描含 manifest 的模块（自身 + 一层子目录）。 */
export async function scanLocalModules(root: string): Promise<ScanCandidate[]> {
  return call<ScanCandidate[]>("scan_local_modules", { root });
}

/** 把一个已存在的本地目录登记为模块（不复制文件）。 */
export async function addLocalModule(path: string): Promise<ModuleView> {
  return call<ModuleView>("add_local_module", { request: { path } });
}

/** 批量登记本地模块（一次登记多个，逐个汇报成败）。 */
export interface BulkAddResult {
  module_id: string;
  path: string;
  ok: boolean;
  error: string | null;
}

export async function addLocalModulesBulk(paths: string[]): Promise<BulkAddResult[]> {
  return call<BulkAddResult[]>("add_local_modules_bulk", { request: { paths } });
}

/** 解除本地模块登记（不删除文件）。 */
export async function removeLocalModule(moduleId: string): Promise<void> {
  await call("remove_local_module", { moduleId });
}

/* ------------------------------------------------------------------ */
/* 安装                                                                */
/* ------------------------------------------------------------------ */

/**
 * 安装一个模块。
 *
 * 调用方必须先完成下载并拿到**确定的 SHA-256**；
 * 桥接层不接受"没有哈希"的请求，Rust 侧也会拒绝空哈希。
 * `manifest` 用于签名校验——发布构建下未签名会被拒绝。
 */
export async function installModule(req: {
  moduleId: string;
  category: string;
  version: string;
  zipBase64: string;
  expectedSha256: string;
  manifest: Record<string, unknown>;
}): Promise<InstallResponse> {
  return call<InstallResponse>("install_module", {
    request: {
      module_id: req.moduleId,
      category: req.category,
      version: req.version,
      zip_base64: req.zipBase64,
      expected_sha256: req.expectedSha256,
      manifest: req.manifest,
    },
  });
}

/** 下载模块包（重试 / 断点续传 / 镜像加速 / 哈希校验）。 */
export async function downloadPackage(req: {
  moduleId: string;
  sources: string[];
  expectedSha256: string;
  retries?: number;
}): Promise<DownloadResponse> {
  return call<DownloadResponse>("download_package", {
    request: {
      module_id: req.moduleId,
      sources: req.sources,
      expected_sha256: req.expectedSha256,
      retries: req.retries ?? 2,
    },
  });
}

/** 查询当前信任策略（UI 用它提示"未签名模块能否安装"）。 */
export async function trustInfo(): Promise<TrustInfo> {
  return call<TrustInfo>("trust_info");
}

/* ------------------------------------------------------------------ */
/* 开发者模式（隐藏入口开启）                                          */
/* ------------------------------------------------------------------ */

export interface DevModeInfo {
  enabled: boolean;
}

/** 查询开发者模式是否已开启。 */
export async function devModeStatus(): Promise<DevModeInfo> {
  return call<DevModeInfo>("dev_mode_status");
}

/** 开启开发者模式（只进不退，本次会话内生效）。 */
export async function enableDevMode(): Promise<DevModeInfo> {
  return call<DevModeInfo>("enable_dev_mode");
}

/* ------------------------------------------------------------------ */
/* 清单与环境                                                          */
/* ------------------------------------------------------------------ */

/** Rust 侧 `probe_environment` 的原始返回。 */
export interface RustEnvReport {
  python: { available: boolean; version: string; detail: string };
  node: { available: boolean; version: string; detail: string };
  uv: { available: boolean; version: string; detail: string };
  /** truncated 只在回退到 uint32 的 AdapterRAM、疑似被截断时出现（D-2）。 */
  gpus: Array<{
    name: string;
    vram_gb: number;
    usable: boolean;
    truncated?: boolean;
  }>;
  /** is_data_root 恒为 true：检测的是数据根所在卷，不再是写死的 D 盘（D-3）。 */
  disk: { path: string; free_gb: number; ok: boolean; is_data_root?: boolean };
  problems: string[];
  healthy: boolean;
}

export async function probeEnvironmentCmd(): Promise<RustEnvReport> {
  return call<RustEnvReport>("probe_environment");
}

export async function refreshCatalog(catalogUrl: string): Promise<CatalogResponse> {
  return call<CatalogResponse>("refresh_catalog", { catalogUrl });
}

export async function hashFile(path: string): Promise<string> {
  return call<string>("hash_file", { path });
}


/* ------------------------------------------------------------------ */

export type ProviderAction =
  | "list"
  | "builtin"
  | "models"
  | "defaults"
  | "create"
  | "update"
  | "delete"
  | "set_enabled"
  | "test"
  | "test_saved"
  | "discover"
  | "discover_saved"
  | "set_defaults";


/* ------------------------------------------------------------------ */
/* 模块根 manifest（界面展示的权威来源）                                */
/* ------------------------------------------------------------------ */

/** 单个模块的模块根 manifest 读取结果。 */
export interface ModuleManifestView {
  module_id: string;
  /** manifest 原文；读不到时为 null */
  manifest: Record<string, unknown> | null;
  /** 读取失败原因 */
  error: string | null;
  /** 模块根绝对路径（排障用） */
  root: string;
}

/**
 * 读取所有**已登记**模块的模块根 manifest。
 *
 * 这是卡片展示的权威来源：与执行路径（entry_spec）读的是同一份文件，
 * 所以"卡片显示什么"与"实际跑什么"不会分裂。
 */
export async function readModuleManifests(): Promise<ModuleManifestView[]> {
  return call<ModuleManifestView[]>("read_module_manifests");
}

/* ------------------------------------------------------------------ */
/* 模块自带管理界面                                                    */
/* ------------------------------------------------------------------ */

/**
 * 打开模块自带的管理界面（独立顶层窗口）。
 *
 * 为什么不是 iframe：三个模块都设了 `X-Frame-Options: DENY` /
 * `frame-ancestors 'none'`（防点击劫持），iframe 会被浏览器拒绝。
 * 顶层窗口不受该限制，所以不需要上游做任何改动。
 *
 * URL 由 Rust 侧按模块 manifest 的 `admin_ui.web_ui.path` 拼装，
 * 前端不传任意地址。
 */
export async function openAdminWindow(moduleId: string): Promise<string> {
  return call<string>("open_admin_window", { request: { module_id: moduleId } });
}

/* ------------------------------------------------------------------ */
/* 数据清除                                                            */
/* ------------------------------------------------------------------ */

export async function purgeLocalData(request: PurgeRequest): Promise<PurgeResponse> {
  return call<PurgeResponse>("purge_local_data", { request });
}

/* ------------------------------------------------------------------ */
/* v2 实例面（M4 主界面按 role 分组、按实例渲染）                      */
/* ------------------------------------------------------------------ */

/** 与 `launcher_core::manifest::Role` 的 serde 形状一致。 */
export type Role = "input" | "llm" | "output" | "shell" | "auxiliary";
export type InstanceCategory = "core" | "optional" | "reserved";
/** 四级验签结果；D 类硬拒，永远不会出现在实例表里。 */
export type InstallVerdict = "A" | "B" | "C" | "D";
export type InstanceSourceKind = "zip" | "dir";
export type Phase = "stopped" | "starting" | "running" | "failed" | "stopping";
export type ProbeState = "not_checked" | "pending" | "healthy" | "unhealthy";

export interface GitObservation {
  present: boolean;
  origin?: string;
  branch?: string;
  head: string | null;
  dirty: boolean;
  observed_at: string;
}

/**
 * 一个已登记实例。
 *
 * 字段名与 `installed.json` 完全一致（Rust 侧没有 rename），
 * 所以这里不做 camelCase 转换——多一层映射只会多一处漂移点。
 */
export interface Instance {
  instance_id: string;
  module_id: string;
  variant: string;
  version: string;
  role: Role;
  category: InstanceCategory;
  publisher_fp: string | null;
  verdict: InstallVerdict;
  confirmed_at: string;
  installed_at: string;
  source_kind: InstanceSourceKind;
  source_path: string | null;
  git?: GitObservation | null;
  pinned: boolean;
  auto_update: boolean;
  zip_sha256?: string;
  expected_signer_fp?: string;
  signature_present?: boolean;
  allow_unsigned_once?: boolean;
  head_confirmed?: string;
  data_dir?: string;
  note?: string;
}

export interface InstanceSnapshot {
  instance_id: string;
  phase: Phase;
  ticket: number;
  pid: number | null;
  health: ProbeState;
  contract: ProbeState;
  exit_code: number | null;
  last_error: string | null;
}

/**
 * 卡片渲染所需的一次性合并视图。
 *
 * `port` 只从 Rust 的 `assign_ports` 来：前端自己推算规范端口/备用端口
 * 一定会与实际启动值分叉，那正是「显示什么 ≠ 跑什么」。
 */
export interface InstanceRuntimeView {
  instance: Instance;
  port: number | null;
  active: boolean;
  disabled: boolean;
  snapshot: InstanceSnapshot | null;
}

export async function listInstanceViews(): Promise<InstanceRuntimeView[]> {
  return call<InstanceRuntimeView[]>("list_instance_views");
}

export async function restartInstance(instanceId: string): Promise<RunView> {
  return call<RunView>("restart_instance", { instanceId });
}

/** 切换 role 的 active 槽：停旧、起新、过探测、提交，失败回滚。 */
export async function switchActive(instanceId: string): Promise<string[]> {
  return call<string[]>("switch_active", { instanceId });
}

/** 确认目录实例的源码现状（写入 head 锚点）。 */
export async function confirmLocalSource(instanceId: string): Promise<string> {
  return call<string>("confirm_local_source", { instanceId });
}

/** 准备实例依赖环境；`allowOnline` 缺省为 false，离线优先。 */
export async function prepareInstanceEnv(
  instanceId: string,
  allowOnline = false,
): Promise<string> {
  return call<string>("prepare_instance_env", { instanceId, allowOnline });
}

export interface InstanceLogs {
  instance_id: string;
  lines: string[];
  files: number;
  truncated: boolean;
  note: string;
}

export async function readInstanceLogs(instanceId: string): Promise<InstanceLogs> {
  return call<InstanceLogs>("read_instance_logs", { instanceId });
}

export async function getLifecycleSnapshot(): Promise<InstanceSnapshot[]> {
  return call<InstanceSnapshot[]>("get_lifecycle_snapshot");
}

/* ------------------------------------------------------------------ */
/* 设置面                                                              */
/* ------------------------------------------------------------------ */

export interface BuiltinKey {
  key_id: string;
  fingerprint: string;
  purpose: string;
  enabled: boolean;
  deletable: boolean;
}

export interface TrustedKey {
  fingerprint: string;
  label: string;
  confirmed_at: string;
  enabled: boolean;
  origin: string;
  revoked_at?: string;
  note?: string;
}

export interface TrustStore {
  schema_version: number;
  builtin: BuiltinKey[];
  trusted: TrustedKey[];
  last_updated?: string;
}

export interface RetentionPolicy {
  schema_version: number;
  keep_versions: number;
  pinned: string[];
  data_dir_policy: string;
  source_dir_policy: string;
  cleanup_trigger?: string;
  last_cleanup_at?: string;
}

export interface SettingsSnapshot {
  trust: TrustStore;
  retention: RetentionPolicy;
  k1_fingerprint: string;
  k5_fingerprint: string;
}

export async function settingsSnapshot(): Promise<SettingsSnapshot> {
  return call<SettingsSnapshot>("settings_snapshot");
}

export interface KeyInfo {
  fingerprint: string;
  accepted: boolean;
  note: string;
}

/** 会话内替换 K5 公钥；重启回落到内置值。 */
export async function setK5Pubkey(pubkeyB64: string): Promise<KeyInfo> {
  return call<KeyInfo>("set_k5_pubkey", { pubkeyB64 });
}

export async function confirmTrustedKey(
  fingerprint: string,
  label?: string,
): Promise<KeyInfo> {
  return call<KeyInfo>("confirm_trusted_key", { request: { fingerprint, label } });
}

export async function revokeTrustedKey(fingerprint: string): Promise<KeyInfo> {
  return call<KeyInfo>("revoke_trusted_key", { fingerprint });
}

export async function setKeepVersions(keepVersions: number): Promise<number> {
  return call<number>("set_keep_versions", { keepVersions });
}

/* ------------------------------------------------------------------ */
/* 生命周期事件                                                        */
/* ------------------------------------------------------------------ */

/**
 * 订阅生命周期快照推送。
 *
 * 只在 Tauri 里可用；浏览器模式返回一个不做事的退订函数，
 * 调用方不必分支。事件名与 Rust 侧 `lifecycle-changed` 一致。
 */
export function onLifecycleChanged(handler: () => void): () => void {
  const tauri = (globalThis as { __TAURI__?: { event?: { listen?: unknown } } }).__TAURI__;
  const listen = tauri?.event?.listen;
  if (typeof listen !== "function") return () => undefined;
  type UnlistenFn = () => void;
  const call = listen as (
    event: string,
    cb: () => void,
  ) => Promise<UnlistenFn>;
  let unlisten: UnlistenFn | null = null;
  let cancelled = false;
  void call("lifecycle-changed", handler)
    .then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    })
    .catch(() => undefined);
  return () => {
    cancelled = true;
    if (unlisten) {
      unlisten();
      unlisten = null;
    }
  };
}
