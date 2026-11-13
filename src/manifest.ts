/**
 * 模块 manifest 的类型与校验。
 *
 * 设计原则：
 *  - 契约字段冻结，但**容忍未知字段**（前向兼容），只在必填缺失或类型错误时报错；
 *  - `dependencies` 与 `model_packs` 必须分字段（最小安装原则的区分声明），
 *    合并或缺失都会被拒绝——这是"最小安装原则"的结构性保证；
 *  - `reserved` 类模块必须自带 reserved 声明，且 removable 必须为 false。
 */

export type ModuleCategory = "core" | "optional" | "reserved" | "experimental";

export class ManifestError extends Error {
  readonly moduleId: string;
  readonly field: string;
  constructor(moduleId: string, field: string, detail: string) {
    super(`manifest[${moduleId}].${field}: ${detail}`);
    this.name = "ManifestError";
    this.moduleId = moduleId;
    this.field = field;
  }
}

export interface ModuleDependency {
  name: string;
  version: string;
  type: "runtime" | "library";
}

export interface ModelPack {
  id: string;
  name: string;
  size_mb: number;
  hardware_req: { gpu: "none" | "optional" | "required"; ram_mb: number; disk_mb: number };
  privacy: "offline" | "online";
  recommended: boolean;
}

export interface ModulePermissions {
  network: string[];
  filesystem: string[];
  devices: string[];
}

export interface ModuleEntry {
  type: string;
  command: string;
  working_dir: string;
  env: Record<string, string>;
  health: { url: string | null; timeout_ms: number };
}

export interface ReservedSpec {
  default_enabled: boolean;
  removable: boolean;
  contract_version: string;
  note: string;
}

/**
 * 管理界面声明（`admin_ui`）。
 *
 * 为什么放进 manifest：这样"哪个模块对应哪个管理界面"是**模块自己声明的**，
 * 启动器不需要硬编码映射；插件作者也能照此约定接入自己的界面。
 *
 * 约定：
 * - **缺省** = 该模块没有管理界面，启动器不渲染；
 * - 只有**已安装**的模块才会渲染对应界面（未安装不显示）；
 * - `capabilities` 是能力白名单，启动器只暴露其中列出的操作；
 * - `config_source.kind`：
 *   - `env_file`   → 启动器直接读写模块根的配置文件；
 *   - `module_api` → 通过模块自己的 HTTP API 读写（密钥不由启动器持久化）。
 */
/**
 * 模块**自带的 Web 管理界面**声明。
 *
 * 这是本期的主路径：三个模块都已有完整管理页，启动器只负责打开它，
 * 不再自绘表单、不代理配置、不存密钥。
 */
export interface AdminWebUi {
  /** 相对模块服务根的路径，必须以 / 开头 */
  path: string;
  /** 按钮文案 */
  label: string;
  /** 是否要求模块先处于运行状态（通常为 true） */
  requires_running: boolean;
}

export interface AdminUiSpec {
  kind: "llm" | "tts" | "input" | "app";
  title: string;
  capabilities: string[];
  /** 模块自带的 Web 管理界面（有则优先使用） */
  web_ui?: AdminWebUi;
  config_source: {
    kind: "env_file" | "module_api";
    path?: string;
  };
  /** 各操作的相对路径（供 module_api 使用）。值必须是字符串。 */
  endpoints?: Record<string, string>;
  /** 给开发者看的说明，UI 不强制展示。 */
  note?: string;
}

export interface ModuleManifest {
  module_id: string;
  name: string;
  version: string;
  category: ModuleCategory;
  entry: ModuleEntry;
  compat: { app_version: string };
  dependencies: ModuleDependency[];
  model_packs: ModelPack[];
  permissions: ModulePermissions;
  reserved?: ReservedSpec;
  /** 管理界面声明；缺省表示没有管理界面 */
  admin_ui?: AdminUiSpec;
  signature: { algo: string; value: string };
}

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

const asString = (v: unknown, fallback = ""): string => (typeof v === "string" ? v : fallback);
const asNumber = (v: unknown, fallback = 0): number =>
  typeof v === "number" && Number.isFinite(v) ? v : fallback;
const asBool = (v: unknown, fallback = false): boolean =>
  typeof v === "boolean" ? v : fallback;

const CATEGORIES: ModuleCategory[] = ["core", "optional", "reserved", "experimental"];

const ADMIN_KINDS = ["llm", "tts", "input", "app"] as const;
const CONFIG_KINDS = ["env_file", "module_api"] as const;

/**
 * 解析 `admin_ui`。
 *
 * 结构不合法时**返回 undefined 而不是抛错**：一个坏的管理界面声明不该让
 * 整个模块无法显示。但会在 reason 里记录（由上层决定是否提示）。
 */
export function parseAdminUi(
  raw: unknown,
): { spec?: AdminUiSpec; reason?: string } {
  if (raw === undefined || raw === null) return {};
  if (!isRecord(raw)) return { reason: "admin_ui 不是对象" };

  const kind = raw.kind;
  if (typeof kind !== "string" || !ADMIN_KINDS.includes(kind as (typeof ADMIN_KINDS)[number])) {
    return { reason: `admin_ui.kind 取值非法：${String(kind)}` };
  }

  const title = asString(raw.title);
  if (!title) return { reason: "admin_ui.title 缺失" };

  const capabilities = Array.isArray(raw.capabilities)
    ? raw.capabilities.filter((x): x is string => typeof x === "string")
    : [];

  const cfgRaw = isRecord(raw.config_source) ? raw.config_source : {};
  const cfgKind = cfgRaw.kind;
  if (
    typeof cfgKind !== "string" ||
    !CONFIG_KINDS.includes(cfgKind as (typeof CONFIG_KINDS)[number])
  ) {
    return { reason: `admin_ui.config_source.kind 取值非法：${String(cfgKind)}` };
  }

  const endpoints: Record<string, string> = {};
  if (isRecord(raw.endpoints)) {
    for (const [k, v] of Object.entries(raw.endpoints)) {
      if (typeof v === "string" && v.startsWith("/")) endpoints[k] = v;
    }
  }

  // web_ui：模块自带管理页
  let webUi: AdminWebUi | undefined;
  const webRaw = raw.web_ui;
  if (isRecord(webRaw)) {
    const webPath = asString(webRaw.path);
    // 只接受以 / 开头的相对路径，拒绝绝对 URL（防外部注入）
    if (webPath.startsWith("/")) {
      webUi = {
        path: webPath,
        label: asString(webRaw.label) || "打开管理界面",
        requires_running:
          typeof webRaw.requires_running === "boolean" ? webRaw.requires_running : true,
      };
    }
  }

  return {
    spec: {
      kind: kind as AdminUiSpec["kind"],
      title,
      capabilities,
      ...(webUi ? { web_ui: webUi } : {}),
      config_source: {
        kind: cfgKind as "env_file" | "module_api",
        path: asString(cfgRaw.path) || undefined,
      },
      endpoints,
      note: asString(raw.note) || undefined,
    },
  };
}

export function parseManifest(raw: unknown): ModuleManifest {
  if (!isRecord(raw)) throw new ManifestError("?", "root", "不是对象");

  const moduleId = asString(raw.module_id);
  if (!moduleId) throw new ManifestError("?", "module_id", "缺失");

  const category = raw.category as ModuleCategory;
  if (!CATEGORIES.includes(category)) {
    throw new ManifestError(moduleId, "category", `未知取值 ${String(raw.category)}`);
  }

  // dependencies 与 model_packs 必须分开声明，缺一不可
  if (!Array.isArray(raw.dependencies)) {
    throw new ManifestError(moduleId, "dependencies", "必须是数组（运行依赖）");
  }
  if (!Array.isArray(raw.model_packs)) {
    throw new ManifestError(moduleId, "model_packs", "必须是数组（模型权重，可为空）");
  }

  const adminResult = parseAdminUi(raw.admin_ui);

  const entryRaw = isRecord(raw.entry) ? raw.entry : {};
  const healthRaw = isRecord(entryRaw.health) ? entryRaw.health : {};
  const envRaw = isRecord(entryRaw.env) ? entryRaw.env : {};
  const env: Record<string, string> = {};
  for (const [k, v] of Object.entries(envRaw)) {
    env[k] = asString(v);
  }

  const deps: ModuleDependency[] = raw.dependencies
    .filter(isRecord)
    .map((d) => ({
      name: asString(d.name),
      version: asString(d.version),
      type: (d.type === "runtime" ? "runtime" : "library") as "runtime" | "library",
    }))
    .filter((d) => d.name.length > 0);

  const packs: ModelPack[] = raw.model_packs
    .filter(isRecord)
    .map((p) => {
      const hw = isRecord(p.hardware_req) ? p.hardware_req : {};
      const gpu = hw.gpu === "required" || hw.gpu === "optional" ? hw.gpu : ("none" as const);
      return {
        id: asString(p.id),
        name: asString(p.name, asString(p.id)),
        size_mb: asNumber(p.size_mb),
        hardware_req: {
          gpu: gpu as "none" | "optional" | "required",
          ram_mb: asNumber(hw.ram_mb),
          disk_mb: asNumber(hw.disk_mb),
        },
        privacy: p.privacy === "online" ? ("online" as const) : ("offline" as const),
        recommended: asBool(p.recommended),
      };
    })
    .filter((p) => p.id.length > 0);

  const permRaw = isRecord(raw.permissions) ? raw.permissions : {};
  const strArray = (v: unknown): string[] =>
    Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];

  // reserved 类必须声明 reserved 段，且不可卸载
  let reserved: ReservedSpec | undefined;
  if (isRecord(raw.reserved)) {
    reserved = {
      default_enabled: asBool(raw.reserved.default_enabled),
      removable: asBool(raw.reserved.removable, true),
      contract_version: asString(raw.reserved.contract_version, "1.0"),
      note: asString(raw.reserved.note),
    };
  }
  if (category === "reserved") {
    if (!reserved) {
      throw new ManifestError(moduleId, "reserved", "reserved 类模块必须声明 reserved 段");
    }
    if (reserved.removable) {
      throw new ManifestError(moduleId, "reserved.removable", "reserved 模块不可卸载，removable 必须为 false");
    }
    if (reserved.default_enabled) {
      throw new ManifestError(moduleId, "reserved.default_enabled", "reserved 模块默认必须禁用");
    }
  }

  const sigRaw = isRecord(raw.signature) ? raw.signature : {};

  return {
    module_id: moduleId,
    name: asString(raw.name, moduleId),
    version: asString(raw.version, "0.0.0"),
    category,
    entry: {
      type: asString(entryRaw.type, "command"),
      command: asString(entryRaw.command),
      working_dir: asString(entryRaw.working_dir),
      env,
      health: {
        url: typeof healthRaw.url === "string" ? healthRaw.url : null,
        timeout_ms: asNumber(healthRaw.timeout_ms, 3000),
      },
    },
    compat: { app_version: asString((isRecord(raw.compat) ? raw.compat.app_version : ""), ">=0.0.0") },
    dependencies: deps,
    model_packs: packs,
    permissions: {
      network: strArray(permRaw.network),
      filesystem: strArray(permRaw.filesystem),
      devices: strArray(permRaw.devices),
    },
    reserved,
    ...(adminResult.spec ? { admin_ui: adminResult.spec } : {}),
    signature: { algo: asString(sigRaw.algo, "Ed25519"), value: asString(sigRaw.value) },
  };
}
