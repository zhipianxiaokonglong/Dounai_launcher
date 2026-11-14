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

export type SourceClass = "official" | "community";

/** sources.json 里一条源的只读快照。 */
export interface SourceView {
  id: string;
  name: string;
  kind: SourceClass;
  base_url: string;
  catalog_fingerprint: string;
  host_suffixes: string[];
  enabled: boolean;
  deletable: boolean;
  auto_update: boolean;
  revoked: boolean;
  sequence: number | null;
  expires: string | null;
  /** 最近一次刷新失败的摘要与时间；成功刷新后清空。 */
  last_error: string | null;
  last_error_at: string | null;
}

export interface SourcesView {
  sources: SourceView[];
  /** 只由已启用源派生；空集即不发起任何远程请求。 */
  allowlist: string[];
}

export interface SourceStatusView {
  id: string;
  name: string;
  state: "ok" | "unavailable" | "invalid" | "disabled";
  reason: string | null;
}

/** 添加源第一步的结果：指纹与派生白名单，尚未落盘。 */
export interface SourcePreview {
  id: string;
  name: string;
  base_url: string;
  catalog_pubkey: string;
  catalog_fingerprint: string;
  host_suffixes: string[];
  derived: boolean;
  note: string;
}

export interface RevokeSourceResult {
  sources: SourcesView;
  /** 从该源安装的实例 ID；不卸载、不自动更新。 */
  affected: string[];
}

export interface CatalogEntry {
  /** 会话缓存主键，安装时原样回传，界面不做隐式优选。 */
  entry_key: string;
  source_id: string;
  source_name: string;
  source_kind: SourceClass;
  catalog_fingerprint: string;
  module_id: string;
  variant: string;
  name: string;
  version: string;
  role: Role;
  /** 目录声明的整包哈希，安装时 Rust 用它校验下载结果。 */
  sha256: string;
  size: number;
  download_url: string;
  signature_required: boolean;
  publisher_fp: string;
  min_launcher: string | null;
  min_version: string | null;
  expires: string;
  revoked: boolean;
  installed_state: "available" | "installed" | "upgrade" | "downgrade" | "incompatible";
  auto_update: boolean;
  source_revoked: boolean;
  note: string | null;
}

export interface CatalogResponse {
  source: "remote" | "partial" | "cache" | "unavailable";
  entries: CatalogEntry[];
  sources: SourceStatusView[];
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
  /**
   * 装完顺便占下该 role 的 active 槽时的实例 ID，否则 null。
   *
   * 只有槽原本为空才会发生：非 active 实例跑 18101+ 备用端口，
   * 组装层只认 8101-8103，所以每个角色的第一件模块必须上位。
   */
  activated: string | null;
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
type Unlisten = () => void;

interface TauriGlobal {
  core?: { invoke?: Invoke };
  invoke?: Invoke;
  event?: {
    listen?: (event: string, handler: (event: { payload: unknown }) => void) => Promise<Unlisten>;
  };
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
/* 本地包（.zip）                                                      */
/* ------------------------------------------------------------------ */

/** 打开系统文件选择器，返回选中的 .zip 绝对路径；取消返回 null。 */
export async function pickPackageFile(): Promise<string | null> {
  const invoke = getInvoke();
  if (!invoke) throw new BridgeUnavailableError();
  const picked = await invoke("plugin:dialog|open", {
    options: {
      title: "选择模块包",
      multiple: false,
      directory: false,
      filters: [{ name: "模块包", extensions: ["zip"] }],
    },
  });
  return typeof picked === "string" && picked.length > 0 ? picked : null;
}

/**
 * 监听窗口的拖拽投放。
 *
 * Tauri 在操作系统层拦截拖拽，浏览器的 `dragover` / `drop` 不会触发，
 * 只能听这四个事件。它们由 Tauri 发给当前 webview，`listen` 不带 target
 * 就能收到；`paths` 是绝对路径，正是命令层要的东西。
 *
 * 浏览器模式下返回空操作，调用方不必分支。
 */
export async function onPackageDrop(handlers: {
  onEnter: () => void;
  onLeave: () => void;
  onDrop: (paths: string[]) => void;
}): Promise<Unlisten> {
  const event = (globalThis as { __TAURI__?: TauriGlobal }).__TAURI__?.event;
  if (!event?.listen) return () => {};

  const off = await Promise.all([
    event.listen("tauri://drag-enter", () => handlers.onEnter()),
    event.listen("tauri://drag-over", () => handlers.onEnter()),
    event.listen("tauri://drag-leave", () => handlers.onLeave()),
    event.listen("tauri://drag-drop", (payload) => {
      handlers.onLeave();
      const typed = payload.payload as { paths?: string[] } | null;
      handlers.onDrop(typed?.paths ?? []);
    }),
  ]);
  return () => off.forEach((unlisten) => unlisten());
}

/** 装之前先把包内清单摊开：装的是什么、谁签的、要不要你确认。 */
export interface LocalPackagePreview {
  path: string;
  module_id: string;
  name: string;
  version: string;
  variant: string;
  role: string;
  category: string;
  files: number;
  bytes: number;
  sha256: string;
  publisher_fp: string | null;
  /** A/B/C/D。D 不给确认入口，直接拒绝。 */
  verdict: string;
  reason: string;
  signed: boolean;
  needs_confirmation: boolean;
  replace_existing: boolean;
}

export async function inspectLocalPackage(path: string): Promise<LocalPackagePreview> {
  return call<LocalPackagePreview>("inspect_local_package", { path });
}

/**
 * 安装本地包。
 *
 * `expectedSha256` 是检查时看到的哈希：安装会再算一次并比对，
 * 文件在中途被换掉会直接失败，而不是装进去一个没看过的包。
 */
export async function installLocalPackage(
  path: string,
  expectedSha256: string,
  confirm: boolean,
): Promise<InstallResponse> {
  return call<InstallResponse>("install_local_package", {
    path,
    expectedSha256,
    confirm,
  });
}

/* ------------------------------------------------------------------ */
/* 在线安装：catalog 选条目 → 下载并校验 → 同一条安装链路                */
/* ------------------------------------------------------------------ */

/**
 * 从已验签的模块目录安装。
 *
 * 目录条目里的 sha256 与 size 一份三用：下载校验、安装校验、界面展示。
 * `entryKey` 来自用户点选的那一行，跨源同名与多版本不做隐式优选。
 * `confirm` 用于 B/C 类包：为 false 时 Rust 直接拒绝并说明原因，
 * 界面据此弹确认框后带 true 重试——确认这一步不交给界面自己判断。
 */
export async function installFromCatalog(
  entryKey: string,
  confirm: boolean,
): Promise<InstallResponse> {
  return call<InstallResponse>("install_from_catalog", {
    entryKey,
    confirm,
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
    /** 远控、投屏与串流软件创建的虚拟显示器，不计入机器能力。 */
    virtual_adapter?: boolean;
  }>;
  /** 读不到时不出现，界面显示「读不到」而不是 0 GB。 */
  memory?: { total_gb: number; free_gb: number };
  /** is_data_root 恒为 true：检测的是数据根所在卷，不再是写死的 D 盘（D-3）。 */
  disk: { path: string; free_gb: number; ok: boolean; is_data_root?: boolean };
  problems: string[];
  healthy: boolean;
}

export async function probeEnvironmentCmd(): Promise<RustEnvReport> {
  return call<RustEnvReport>("probe_environment");
}

/** 刷新所有已启用源。没有会话级 URL 参数，源只能在 sources.json 里配置。 */
export async function refreshCatalog(): Promise<CatalogResponse> {
  return call<CatalogResponse>("refresh_catalog");
}

/**
 * 只读本次会话内已验签过的目录，不发网络请求。
 *
 * 安装之后刷新「已安装 / 可升级」状态用它：重新拉网络一旦失败，
 * 界面会把刚装好的条目连同整张列表一起清掉。
 */
export async function catalogCache(): Promise<CatalogResponse> {
  return call<CatalogResponse>("catalog_cache");
}

/* ------------------------------------------------------------------ */
/* 下载中心：可配置源                                                  */
/* ------------------------------------------------------------------ */

export async function listSources(): Promise<SourcesView> {
  return call<SourcesView>("list_sources");
}

/** 只算指纹与派生白名单，不写任何文件。 */
export async function inspectSource(input: {
  baseUrl: string;
  catalogPubkey: string;
  hostSuffixes?: string[] | null;
}): Promise<SourcePreview> {
  return call<SourcePreview>("inspect_source", {
    request: {
      base_url: input.baseUrl,
      catalog_pubkey: input.catalogPubkey,
      host_suffixes: input.hostSuffixes ?? null,
    },
  });
}

/** `confirm` 为 false 时后端一律拒绝写入，界面必须先展示完整指纹。 */
export async function addSource(
  input: Omit<SourcePreview, "catalog_fingerprint" | "derived" | "note"> & {
    confirm: boolean;
  },
): Promise<SourcesView> {
  return call<SourcesView>("add_source", {
    request: {
      id: input.id,
      name: input.name,
      base_url: input.base_url,
      catalog_pubkey: input.catalog_pubkey,
      host_suffixes: input.host_suffixes,
      confirm: input.confirm,
    },
  });
}

export async function setSourceEnabled(id: string, enabled: boolean): Promise<SourcesView> {
  return call<SourcesView>("set_source_enabled", { request: { id, enabled } });
}

export async function revokeSource(id: string): Promise<RevokeSourceResult> {
  return call<RevokeSourceResult>("revoke_source", { request: { id } });
}

export async function removeSource(id: string): Promise<SourcesView> {
  return call<SourcesView>("remove_source", { request: { id } });
}

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
/* 同窗口内嵌管理页                                                    */
/* ------------------------------------------------------------------ */

/** 相对主窗口客户区的逻辑像素；由管理页容器一处算好再下发。 */
export interface AdminBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface AdminMountResult {
  /** embedded = 内嵌成功；window = 内嵌失败但已退化为独立窗口。 */
  mode: "embedded" | "window";
  label: string;
  url: string;
  reason: string | null;
}

export async function openAdminEmbedded(
  moduleId: string,
  bounds: AdminBounds,
): Promise<AdminMountResult> {
  return call<AdminMountResult>("open_admin_embedded", {
    request: { module_id: moduleId },
    bounds,
  });
}

export async function adminHostResize(label: string, bounds: AdminBounds): Promise<void> {
  await call<void>("admin_host_resize", { request: { label }, bounds });
}

export async function adminHostReload(label: string): Promise<void> {
  await call<void>("admin_host_reload", { request: { label } });
}

export async function adminHostUnmount(label: string): Promise<void> {
  await call<void>("admin_host_unmount", { request: { label } });
}

export async function adminHostVisible(label: string, visible: boolean): Promise<void> {
  await call<void>("admin_host_visible", { request: { label }, visible });
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
/** 模块自带管理界面的入口声明（读自模块根 manifest）。 */
export interface AdminUiView {
  label: string;
  /** 以 `/` 开头；完整地址由 Rust 按端口拼，前端不拼。 */
  path: string;
  requires_running: boolean;
}

export interface InstanceRuntimeView {
  instance: Instance;
  /** 清单里的展示名；读不到即 module_id。 */
  name: string;
  /** 未声明管理界面时为 null，界面据此不渲染按钮。 */
  admin_ui: AdminUiView | null;
  port: number | null;
  active: boolean;
  disabled: boolean;
  /** 依赖环境是否已就绪；未就绪时界面先引导装依赖，不给「启动」。 */
  prepared: boolean;
  /** 快照是否由签名包锚定；null 表示无记录（未准备或 shell）。 */
  env_pinned: boolean | null;
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

/** 准备实例依赖环境；`allowOnline` 缺省为 true，首次装依赖必须能拉包。 */
export async function prepareInstanceEnv(
  instanceId: string,
  allowOnline = true,
): Promise<string> {
  return call<string>("prepare_instance_env", { instanceId, allowOnline });
}

export interface InstanceLogs {
  instance_id: string;
  lines: string[];
  files: number;
  truncated: boolean;
  note: string;
  /** 本次读的是哪一份；null 表示跨文件的最新尾部。 */
  file: string | null;
  /** 子进程输出是否落盘。 */
  child_output: boolean;
  /** 日志目录绝对路径。 */
  dir: string;
}

export interface LogFileInfo {
  name: string;
  size: number;
  modified_at: string;
  current: boolean;
}

export async function readInstanceLogs(
  instanceId: string,
  file: string | null = null,
): Promise<InstanceLogs> {
  return call<InstanceLogs>("read_instance_logs", { instanceId, file });
}

export async function listInstanceLogs(instanceId: string): Promise<LogFileInfo[]> {
  return call<LogFileInfo[]>("list_instance_logs", { instanceId });
}

/** 在文件管理器里打开日志目录；返回打开的路径。 */
export async function openInstanceLogsDir(instanceId: string): Promise<string> {
  return call<string>("open_instance_logs_dir", { instanceId });
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
  /** 启动时补写/校正 install.json 的说明；null 表示本来就一致。 */
  install_note: string | null;
  /** 新拉起的实例是否捕获子进程 stdout/stderr。 */
  child_output: boolean;
}

export async function settingsSnapshot(): Promise<SettingsSnapshot> {
  return call<SettingsSnapshot>("settings_snapshot");
}

export interface KeyInfo {
  fingerprint: string;
  accepted: boolean;
  note: string;
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

/** 开关子进程输出捕获；只影响之后拉起的实例。 */
export async function setChildOutput(enabled: boolean): Promise<boolean> {
  return call<boolean>("set_child_output", { enabled });
}

/* ------------------------------------------------------------------ */
/* 存储与引导状态                                                      */
/* ------------------------------------------------------------------ */

/**
 * 入门引导是否走过。字段与 `state/onboarding.json` 一致。
 *
 * 名字沿用 onboarding：语义就是「新人第一次那件事做完了没有」，
 * 实现从四屏向导换成了聚光灯引导，状态文件不必跟着改名。
 */
export interface OnboardingState {
  schema_version: number;
  completed: boolean;
  /** true 表示上次是中途跳过，不是一步步走完。 */
  skipped: boolean;
  completed_at: string | null;
}

/** 存储事实；字节数都是解压后的大小。 */
export interface StorageFacts {
  data_root: string;
  /** 已登记实例数；0 表示这台机器还没装过模块。 */
  instances: number;
  python_bytes: number;
  cache_bytes: number;
  /** 数据根整体占用（含实例目录、日志、venv）。 */
  data_bytes: number;
}

export async function onboardingState(): Promise<OnboardingState> {
  return call<OnboardingState>("onboarding_state");
}

export async function storageFacts(): Promise<StorageFacts> {
  return call<StorageFacts>("storage_facts");
}

export async function completeOnboarding(skipped: boolean): Promise<OnboardingState> {
  return call<OnboardingState>("complete_onboarding", { skipped });
}

/** 让引导下次启动再走一遍。 */
export async function resetOnboarding(): Promise<OnboardingState> {
  return call<OnboardingState>("reset_onboarding");
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
  return listenEvent("lifecycle-changed", () => handler());
}

/* ------------------------------------------------------------------ */
/* 依赖准备进度                                                        */
/* ------------------------------------------------------------------ */

export type EnvPhaseName = "uv" | "python" | "resolve" | "sync" | "verify" | "done";

/** 引擎每一步上报的内容。字段与 Rust 侧 `EnvProgressEvent` 一一对应。 */
export interface EnvProgressEvent {
  instanceId: string;
  phase: EnvPhaseName;
  title: string;
  /** 更细的一行：包名、哈希、路径。没有时为 null。 */
  detail: string | null;
  current: number | null;
  total: number | null;
}

/** 订阅依赖准备进度。浏览器模式同样返回不做事的退订函数。 */
export function onEnvProgress(handler: (event: EnvProgressEvent) => void): () => void {
  return listenEvent<EnvProgressEvent>("instance-env-progress", (payload) => {
    if (payload && typeof payload.instanceId === "string") handler(payload);
  });
}

/**
 * 事件订阅的唯一出口。
 *
 * Tauri 2 的 `listen` 是异步注册，取消可能早于注册完成，这里用一个标记挡住
 * 迟到的注册，避免卸载后仍然收到回调。
 */
function listenEvent<T>(
  name: string,
  handler: (payload: T) => void,
): () => void {
  const tauri = (globalThis as { __TAURI__?: { event?: { listen?: unknown } } }).__TAURI__;
  const listen = tauri?.event?.listen;
  if (typeof listen !== "function") return () => undefined;
  type UnlistenFn = () => void;
  const call = listen as (
    event: string,
    cb: (event: { payload: unknown }) => void,
  ) => Promise<UnlistenFn>;
  let unlisten: UnlistenFn | null = null;
  let cancelled = false;
  void call(name, (event) => handler(event.payload as T))
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
