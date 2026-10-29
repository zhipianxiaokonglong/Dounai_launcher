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
}

/** 启停命令返回的运行快照。 */
export interface RunView {
  module_id: string;
  run: ModuleRunState;
  pid: number | null;
  last_error: string | null;
  healthy: boolean;
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
  gpus: Array<{ name: string; vram_gb: number; usable: boolean }>;
  disk: { path: string; free_gb: number; ok: boolean };
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
/* 厂商密钥（三层统一入口）                                            */
/* ------------------------------------------------------------------ */

export interface VendorProvider {
  id: string;
  label: string;
  ready: boolean;
  /** 还需要哪些环境变量（只有名字，不含值） */
  missing: string[];
  /** 其中已由启动器保存的 */
  saved: string[];
  privacy: string;
}

export interface VendorLayer {
  module_id: string;
  label: string;
  installed: boolean;
  running: boolean;
  providers: VendorProvider[];
  error: string | null;
}

/** 读取三层厂商状态（不含任何密钥值）。 */
export async function vendorOverview(): Promise<VendorLayer[]> {
  return call<VendorLayer[]>("vendor_overview");
}

/** 保存一个厂商密钥（值不会被回读）。 */
export async function setVendorKey(
  moduleId: string,
  envName: string,
  value: string,
): Promise<VendorLayer[]> {
  return call<VendorLayer[]>("set_vendor_key", {
    request: { module_id: moduleId, env_name: envName, value },
  });
}

/** 清除一个厂商密钥。 */
export async function clearVendorKey(
  moduleId: string,
  envName: string,
): Promise<VendorLayer[]> {
  return call<VendorLayer[]>("clear_vendor_key", {
    request: { module_id: moduleId, env_name: envName },
  });
}

/* ------------------------------------------------------------------ */
/* 数据清除                                                            */
/* ------------------------------------------------------------------ */

export async function purgeLocalData(request: PurgeRequest): Promise<PurgeResponse> {
  return call<PurgeResponse>("purge_local_data", { request });
}
