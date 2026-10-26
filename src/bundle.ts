/**
 * 套装（bundle）解析与安装计划生成（套装定义与最小安装原则）。
 *
 * 这一层是「最小安装原则」的执行者，因此规则都写成硬性逻辑而非提示：
 *  - 模型权重**默认一律不装**，只有 `model_packs` 显式列出才装；
 *  - `recommended_only` 只用于生成"推荐角标"，**不产生任何安装项**；
 *  - `core` 与 `reserved` 不可勾除；
 *  - 需要显式同意的套装（离线版）必须带 `requires_explicit_consent`。
 */

import type { ModuleManifest } from "./manifest.js";

export class BundleError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "BundleError";
  }
}

export interface BundleDefinition {
  bundle_id: string;
  name: string;
  description: string;
  modules: string[];
  /** 显式要安装的模型权重（可为空） */
  model_packs: string[];
  /** 仅推荐、不安装 */
  recommended_only: string[];
  optional_modules?: string[];
  requires_explicit_consent?: boolean;
}

export interface InstallStep {
  kind: "module" | "model_pack";
  id: string;
  name: string;
  /** 该步骤被引入的原因，用于向用户解释 */
  reason: string;
  locked: boolean;
}

export interface InstallPlan {
  bundleId: string;
  bundleName: string;
  requiresConsent: boolean;
  steps: InstallStep[];
  /** 被推荐但默认不安装的模型（UI 显示角标用） */
  recommendations: Array<{ id: string; name: string; size_mb: number; reason: string }>;
  /** 计算得到的总体积与硬件需求（仅模型包计入体积） */
  totalModelMb: number;
  warnings: string[];
}

export function parseBundle(raw: unknown): BundleDefinition {
  if (typeof raw !== "object" || raw === null) throw new BundleError("套装定义不是对象");
  const r = raw as Record<string, unknown>;
  const strArr = (v: unknown): string[] =>
    Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];

  const bundleId = typeof r.bundle_id === "string" ? r.bundle_id : "";
  if (!bundleId) throw new BundleError("套装缺少 bundle_id");

  const modules = strArr(r.modules);
  const modelPacks = strArr(r.model_packs);
  const recommended = strArr(r.recommended_only);

  // 硬约束：推荐项绝不能同时出现在安装项里，否则等于变相预装
  const conflict = recommended.filter((id) => modelPacks.includes(id));
  if (conflict.length > 0) {
    throw new BundleError(
      `套装 ${bundleId} 把推荐模型 ${conflict.join(", ")} 同时列入了安装项——` +
        `推荐项必须保持"默认不装"，否则违反最小安装原则`,
    );
  }

  return {
    bundle_id: bundleId,
    name: typeof r.name === "string" ? r.name : bundleId,
    description: typeof r.description === "string" ? r.description : "",
    modules,
    model_packs: modelPacks,
    recommended_only: recommended,
    optional_modules: strArr(r.optional_modules),
    requires_explicit_consent: r.requires_explicit_consent === true,
  };
}

/**
 * 生成安装计划。
 *
 * @param bundle 套装定义
 * @param registry 已加载的 manifest 索引（module_id → manifest）
 * @param includeOptional 用户是否勾选了可选模块
 */
export function planInstall(
  bundle: BundleDefinition,
  registry: Map<string, ModuleManifest>,
  includeOptional: string[] = [],
): InstallPlan {
  const warnings: string[] = [];
  const steps: InstallStep[] = [];
  const seenModules = new Set<string>();

  const pushModule = (id: string, reason: string, fromOptional = false) => {
    if (seenModules.has(id)) return;
    const manifest = registry.get(id);
    if (!manifest) {
      warnings.push(`套装引用了未注册的模块 ${id}，已跳过`);
      return;
    }
    // optional 模块需要用户勾选才装
    if (manifest.category === "optional" && fromOptional && !includeOptional.includes(id)) {
      return;
    }
    seenModules.add(id);
    steps.push({
      kind: "module",
      id: manifest.module_id,
      name: manifest.name,
      reason,
      // core 与 reserved 不可勾除
      locked: manifest.category === "core" || manifest.category === "reserved",
    });
  };

  // 必需模块（bundle.modules 里的 core/reserved 一定装；optional 由 includeOptional 决定）
  for (const id of bundle.modules) {
    const manifest = registry.get(id);
    const isOptional = manifest?.category === "optional";
    if (isOptional && !includeOptional.includes(id)) continue;
    pushModule(id, `套装「${bundle.name}」必需`, false);
  }

  // 用户额外勾选的可选模块
  for (const id of includeOptional) {
    if (bundle.modules.includes(id)) continue;
    pushModule(id, "用户手动勾选", true);
  }

  // 显式指定的模型权重
  let totalModelMb = 0;
  const packIndex = new Map<string, { pack: NonNullable<ModuleManifest["model_packs"]>[number]; owner: string }>();
  for (const manifest of registry.values()) {
    for (const pack of manifest.model_packs) {
      packIndex.set(pack.id, { pack, owner: manifest.module_id });
    }
  }

  for (const packId of bundle.model_packs) {
    const found = packIndex.get(packId);
    if (!found) {
      warnings.push(`套装引用的模型包 ${packId} 不存在，已跳过`);
      continue;
    }
    if (!seenModules.has(found.owner)) {
      warnings.push(`模型包 ${packId} 所属模块 ${found.owner} 未安装，已跳过`);
      continue;
    }
    totalModelMb += found.pack.size_mb;
    steps.push({
      kind: "model_pack",
      id: found.pack.id,
      name: found.pack.name,
      reason: `套装「${bundle.name}」显式选择`,
      locked: false,
    });
  }

  // 推荐项：只收集元信息，绝不进入 steps
  const recommendations = bundle.recommended_only
    .map((id) => packIndex.get(id))
    .filter((x): x is { pack: NonNullable<ModuleManifest["model_packs"]>[number]; owner: string } => Boolean(x))
    .map((x) => ({
      id: x.pack.id,
      name: x.pack.name,
      size_mb: x.pack.size_mb,
      reason: `推荐（约 ${x.pack.size_mb} MB），默认不安装`,
    }));

  if (bundle.model_packs.length === 0 && recommendations.length === 0) {
    warnings.push("本套装不含任何模型权重（符合最小安装原则）");
  }

  return {
    bundleId: bundle.bundle_id,
    bundleName: bundle.name,
    requiresConsent: bundle.requires_explicit_consent === true,
    steps,
    recommendations,
    totalModelMb,
    warnings,
  };
}
