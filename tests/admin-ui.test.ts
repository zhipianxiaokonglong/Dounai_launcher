/** admin_ui 解析测试：坏结构不应让整个模块无法显示。 */

import { describe, expect, it } from "vitest";

import { parseAdminUi, parseManifest } from "../src/manifest.js";

const baseManifest = {
  module_id: "test-mod",
  name: "测试模块",
  version: "0.1.0",
  category: "core",
  entry: { type: "command", command: "echo hi", working_dir: "", env: {} },
  dependencies: [],
  model_packs: [],
  permissions: { network: [], filesystem: [], devices: [] },
  signature: { algo: "Ed25519", value: "" },
};

describe("admin_ui 解析", () => {
  it("缺省时返回空（表示没有管理界面）", () => {
    expect(parseAdminUi(undefined)).toEqual({});
    expect(parseAdminUi(null)).toEqual({});
  });

  it("合法声明被正确解析", () => {
    const r = parseAdminUi({
      kind: "llm",
      title: "模型厂商",
      capabilities: ["list", "create", "test"],
      config_source: { kind: "module_api" },
      endpoints: { custom: "/v1/custom-providers" },
    });
    expect(r.spec?.kind).toBe("llm");
    expect(r.spec?.title).toBe("模型厂商");
    expect(r.spec?.capabilities).toEqual(["list", "create", "test"]);
    expect(r.spec?.config_source.kind).toBe("module_api");
    expect(r.spec?.endpoints?.custom).toBe("/v1/custom-providers");
  });

  it("kind 非法时给出原因，不抛错", () => {
    const r = parseAdminUi({ kind: "unknown", title: "x", config_source: { kind: "env_file" } });
    expect(r.spec).toBeUndefined();
    expect(r.reason).toContain("kind");
  });

  it("config_source.kind 非法时给出原因", () => {
    const r = parseAdminUi({ kind: "llm", title: "x", config_source: { kind: "bad" } });
    expect(r.spec).toBeUndefined();
    expect(r.reason).toContain("config_source");
  });

  it("title 缺失时拒绝", () => {
    const r = parseAdminUi({ kind: "llm", config_source: { kind: "env_file" } });
    expect(r.spec).toBeUndefined();
    expect(r.reason).toContain("title");
  });

  it("endpoints 只接受以 / 开头的字符串（防外部 URL 注入）", () => {
    const r = parseAdminUi({
      kind: "llm",
      title: "x",
      config_source: { kind: "module_api" },
      endpoints: { ok: "/v1/x", bad: "https://evil.example/x", num: 1 },
    });
    expect(r.spec?.endpoints).toEqual({ ok: "/v1/x" });
  });

  it("坏 admin_ui 不影响 manifest 其余字段解析", () => {
    const m = parseManifest({ ...baseManifest, admin_ui: { kind: "bad" } });
    expect(m.module_id).toBe("test-mod");
    expect(m.admin_ui).toBeUndefined();
  });

  it("合法 admin_ui 会挂到 manifest 上", () => {
    const m = parseManifest({
      ...baseManifest,
      admin_ui: {
        kind: "tts",
        title: "语音厂商",
        capabilities: ["list"],
        config_source: { kind: "env_file", path: ".env" },
      },
    });
    expect(m.admin_ui?.kind).toBe("tts");
    expect(m.admin_ui?.config_source.path).toBe(".env");
  });
});
