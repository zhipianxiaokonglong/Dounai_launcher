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
    signature: { algo: asString(sigRaw.algo, "SHA-256 + Ed25519"), value: asString(sigRaw.value) },
  };
}
