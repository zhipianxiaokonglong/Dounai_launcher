/**
 * manifest 与套装规划测试。
 *
 * 重点守护「最小安装原则」：
 * 模型权重默认一律不装，推荐项绝不能变成安装项。
 */

import { describe, expect, it } from "vitest";
import { ManifestError, parseManifest, type ModuleManifest } from "../src/manifest.js";
import { BundleError, parseBundle, planInstall } from "../src/bundle.js";

function rawManifest(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    module_id: "test-mod",
    name: "测试模块",
    version: "1.0.0",
    category: "core",
    entry: {
      type: "command",
      command: "echo hi",
      working_dir: ".",
      env: { PORT: "1234" },
      health: { url: "http://127.0.0.1:1234/healthz", timeout_ms: 3000 },
    },
    compat: { app_version: ">=0.1.0" },
    dependencies: [{ name: "Python", version: ">=3.11", type: "runtime" }],
    model_packs: [],
    permissions: { network: [], filesystem: [], devices: [] },
    signature: { algo: "SHA-256 + Ed25519", value: "" },
    ...overrides,
  };
}

describe("parseManifest", () => {
  it("解析最小合法 manifest", () => {
    const m = parseManifest(rawManifest());
    expect(m.module_id).toBe("test-mod");
    expect(m.category).toBe("core");
    expect(m.dependencies).toHaveLength(1);
    expect(m.model_packs).toHaveLength(0);
    expect(m.entry.env.PORT).toBe("1234");
  });

  it("容忍未知字段（前向兼容）", () => {
    const m = parseManifest(rawManifest({ future_field: { nested: 1 } }));
    expect(m.module_id).toBe("test-mod");
  });

  it("缺少 module_id 时报错", () => {
    expect(() => parseManifest(rawManifest({ module_id: undefined }))).toThrow(ManifestError);
  });

  it("未知 category 时报错", () => {
    expect(() => parseManifest(rawManifest({ category: "weird" }))).toThrow(ManifestError);
  });

  it("dependencies 与 model_packs 必须分开声明，缺失即报错", () => {
    expect(() => parseManifest(rawManifest({ dependencies: undefined }))).toThrow(ManifestError);
    expect(() => parseManifest(rawManifest({ model_packs: undefined }))).toThrow(ManifestError);
  });

  it("解析 model_packs 的硬件需求", () => {
    const m = parseManifest(
      rawManifest({
        model_packs: [
          {
            id: "pack-a",
            name: "A",
            size_mb: 100,
            hardware_req: { gpu: "required", ram_mb: 4096, disk_mb: 200 },
            privacy: "offline",
            recommended: true,
          },
        ],
      }),
    );
    expect(m.model_packs[0]?.hardware_req.gpu).toBe("required");
    expect(m.model_packs[0]?.size_mb).toBe(100);
  });

  describe("reserved 类模块", () => {
    const reservedBase = {
      category: "reserved",
      reserved: { default_enabled: false, removable: false, contract_version: "1.0", note: "" },
    };

    it("合法 reserved 模块可解析", () => {
      const m = parseManifest(rawManifest(reservedBase));
      expect(m.category).toBe("reserved");
      expect(m.reserved?.removable).toBe(false);
    });

    it("reserved 缺少 reserved 段时报错", () => {
      expect(() => parseManifest(rawManifest({ category: "reserved" }))).toThrow(ManifestError);
    });

    it("reserved 模块 removable=true 时报错（不可卸载）", () => {
      expect(() =>
        parseManifest(
          rawManifest({
            category: "reserved",
            reserved: { default_enabled: false, removable: true, contract_version: "1.0", note: "" },
          }),
        ),
      ).toThrow(ManifestError);
    });

    it("reserved 模块默认启用时报错（必须默认禁用）", () => {
      expect(() =>
        parseManifest(
          rawManifest({
            category: "reserved",
            reserved: { default_enabled: true, removable: false, contract_version: "1.0", note: "" },
          }),
        ),
      ).toThrow(ManifestError);
    });
  });
});

function rawBundle(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    bundle_id: "minimal",
    name: "最小可用",
    description: "",
    modules: ["test-mod"],
    model_packs: [],
    recommended_only: [],
    ...overrides,
  };
}

function registryOf(...manifests: ModuleManifest[]): Map<string, ModuleManifest> {
  return new Map(manifests.map((m) => [m.module_id, m]));
}

describe("parseBundle", () => {
  it("解析合法套装", () => {
    const b = parseBundle(rawBundle());
    expect(b.bundle_id).toBe("minimal");
    expect(b.model_packs).toEqual([]);
  });

  it("缺少 bundle_id 时报错", () => {
    expect(() => parseBundle(rawBundle({ bundle_id: undefined }))).toThrow(BundleError);
  });

  it("推荐项同时出现在安装项时报错（防变相预装）", () => {
    expect(() =>
      parseBundle(rawBundle({ model_packs: ["pack-a"], recommended_only: ["pack-a"] })),
    ).toThrow(BundleError);
  });
});

describe("planInstall：最小安装原则", () => {
  const core = parseManifest(
    rawManifest({
      module_id: "core-mod",
      name: "核心",
      category: "core",
      model_packs: [
        { id: "big-pack", name: "大模型", size_mb: 1000, privacy: "offline", recommended: true },
      ],
    }),
  );
  const optional = parseManifest(
    rawManifest({ module_id: "opt-mod", name: "可选", category: "optional" }),
  );
  const reserved = parseManifest(
    rawManifest({
      module_id: "res-mod",
      name: "预留",
      category: "reserved",
      reserved: { default_enabled: false, removable: false, contract_version: "1.0", note: "" },
    }),
  );

  it("最小套装不含任何模型权重", () => {
    const plan = planInstall(
      parseBundle(rawBundle({ modules: ["core-mod"], model_packs: [] })),
      registryOf(core),
    );
    expect(plan.steps.every((s) => s.kind === "module")).toBe(true);
    expect(plan.totalModelMb).toBe(0);
  });

  it("推荐项只出现在 recommendations，绝不进入安装步骤", () => {
    const plan = planInstall(
      parseBundle(rawBundle({ modules: ["core-mod"], recommended_only: ["big-pack"] })),
      registryOf(core),
    );
    expect(plan.recommendations.map((r) => r.id)).toContain("big-pack");
    expect(plan.steps.some((s) => s.id === "big-pack")).toBe(false);
  });

  it("显式列出的模型包才会安装，并计入体积", () => {
    const plan = planInstall(
      parseBundle(rawBundle({ modules: ["core-mod"], model_packs: ["big-pack"] })),
      registryOf(core),
    );
    const pack = plan.steps.find((s) => s.id === "big-pack");
    expect(pack?.kind).toBe("model_pack");
    expect(plan.totalModelMb).toBe(1000);
  });

  it("core 与 reserved 步骤被锁定（不可勾除）", () => {
    const plan = planInstall(
      parseBundle(rawBundle({ modules: ["core-mod", "res-mod"] })),
      registryOf(core, reserved),
    );
    expect(plan.steps.find((s) => s.id === "core-mod")?.locked).toBe(true);
    expect(plan.steps.find((s) => s.id === "res-mod")?.locked).toBe(true);
  });

  it("optional 模块默认不装，用户勾选后才装", () => {
    const bundle = parseBundle(rawBundle({ modules: ["core-mod"], optional_modules: ["opt-mod"] }));
    const planNo = planInstall(bundle, registryOf(core, optional));
    expect(planNo.steps.some((s) => s.id === "opt-mod")).toBe(false);

    const planYes = planInstall(bundle, registryOf(core, optional), ["opt-mod"]);
    expect(planYes.steps.some((s) => s.id === "opt-mod")).toBe(true);
  });

  it("引用未注册模块时给出警告而非崩溃", () => {
    const plan = planInstall(parseBundle(rawBundle({ modules: ["ghost"] })), registryOf(core));
    expect(plan.warnings.join()).toContain("ghost");
  });

  it("模型包属于未安装模块时跳过并警告", () => {
    const plan = planInstall(
      parseBundle(rawBundle({ modules: [], model_packs: ["big-pack"] })),
      registryOf(core),
    );
    expect(plan.steps).toHaveLength(0);
    expect(plan.warnings.join()).toContain("未安装");
  });

  it("离线套装要求显式同意", () => {
    const plan = planInstall(
      parseBundle(rawBundle({ bundle_id: "offline", requires_explicit_consent: true })),
      registryOf(core),
    );
    expect(plan.requiresConsent).toBe(true);
  });

  it("无模型权的套装给出符合最小安装原则的提示", () => {
    const plan = planInstall(parseBundle(rawBundle({ modules: ["core-mod"] })), registryOf(core));
    expect(plan.warnings.join()).toContain("最小安装原则");
  });
});
