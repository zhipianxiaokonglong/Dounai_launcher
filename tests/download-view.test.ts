/**
 * 下载中心的筛选与分组。
 *
 * 这里守的是 M8 最核心的两条语义：
 *
 * 1. **跨源同名并存**。同一个 module_id 在两个源里都出现时，
 *    两条都要在列表里，不许界面替用户做「优选」——优选了就等于把
 *    「你信任哪个源」这件事偷偷替用户回答了。
 * 2. **同源多版本并列**。同一 module_id@variant 的多个版本要排在一起，
 *    按版本从新到旧排，用户能直接看出哪个是新的。
 *
 * 分组按模块聚合：同一模块的 stable 与 nightly 并排在同一张卡里比较，
 * 但变体内仍按来源与版本逐条列出，语义不变。
 */

import { describe, expect, it } from "vitest";
import { failedSources, filterEntries, groupModules } from "../src/ui/views/download.js";
import type { CatalogEntry, SourceStatusView } from "../src/bridge.js";

function entry(over: Partial<CatalogEntry> & Pick<CatalogEntry, "entry_key">): CatalogEntry {
  return {
    source_id: "official",
    source_name: "官方源",
    source_kind: "official",
    catalog_fingerprint: "sha256:official",
    module_id: "dounai/input",
    variant: "stable",
    name: "输入模块",
    version: "1.0.0",
    role: "input",
    sha256: "aa".repeat(32),
    size: 1024,
    download_url: "https://dl.example.com/a.zip",
    signature_required: true,
    publisher_fp: "sha256:publisher",
    min_launcher: null,
    min_version: null,
    expires: "2026-12-01T00:00:00Z",
    revoked: false,
    installed_state: "available",
    auto_update: true,
    source_revoked: false,
    note: null,
    ...over,
  };
}

const ENTRIES: CatalogEntry[] = [
  entry({ entry_key: "official:dounai/input:stable:1.0.0" }),
  entry({ entry_key: "official:dounai/input:stable:1.2.0", version: "1.2.0" }),
  entry({
    entry_key: "community:dounai/input:stable:0.9.0",
    source_id: "community",
    source_name: "社区源",
    source_kind: "community",
    version: "0.9.0",
  }),
  entry({
    entry_key: "nightly:dounai/input:nightly:2026.11.20",
    source_id: "nightly-gh",
    source_name: "官方 Nightly",
    variant: "nightly",
    version: "2026.11.20",
  }),
  entry({
    entry_key: "official:dounai/model:stable:2.0.0",
    module_id: "dounai/model",
    variant: "stable",
    name: "模型模块",
    version: "2.0.0",
    role: "llm",
  }),
];

describe("filterEntries", () => {
  const all = { query: "", sourceId: "all", role: "all", onlyAvailable: false };

  it("不过滤时原样返回", () => {
    expect(filterEntries(ENTRIES, all)).toHaveLength(5);
  });

  it("按来源过滤", () => {
    const picked = filterEntries(ENTRIES, { ...all, sourceId: "community" });
    expect(picked.map((e) => e.source_id)).toEqual(["community"]);
  });

  it("按角色过滤", () => {
    const picked = filterEntries(ENTRIES, { ...all, role: "llm" });
    expect(picked.map((e) => e.module_id)).toEqual(["dounai/model"]);
  });

  it("按关键词过滤名称、模块 ID、变体与来源名", () => {
    expect(filterEntries(ENTRIES, { ...all, query: "模型" })).toHaveLength(1);
    expect(filterEntries(ENTRIES, { ...all, query: "dounai/input" })).toHaveLength(4);
    expect(filterEntries(ENTRIES, { ...all, query: "社区" })).toHaveLength(1);
    expect(filterEntries(ENTRIES, { ...all, query: "不存在的词" })).toHaveLength(0);
  });

  it("只看可安装时排除已安装、升级与不兼容", () => {
    const mixed: CatalogEntry[] = [
      entry({ entry_key: "a", installed_state: "available" }),
      entry({ entry_key: "b", installed_state: "installed" }),
      entry({ entry_key: "c", installed_state: "upgrade" }),
      entry({ entry_key: "d", installed_state: "incompatible" }),
    ];
    const picked = filterEntries(mixed, { ...all, onlyAvailable: true });
    expect(picked.map((e) => e.entry_key)).toEqual(["a"]);
  });

  it("筛选条件叠加时取交集", () => {
    const picked = filterEntries(ENTRIES, {
      query: "stable",
      sourceId: "community",
      role: "input",
      onlyAvailable: false,
    });
    expect(picked).toHaveLength(1);
    expect(picked[0]?.source_id).toBe("community");
  });
});

describe("groupModules", () => {
  it("按模块聚合：同一模块的 stable 与 nightly 在同一张卡里", () => {
    const groups = groupModules(ENTRIES);
    const input = groups.find((g) => g.moduleId === "dounai/input");
    expect(input).toBeDefined();
    expect(input?.variants.map((v) => v.variant)).toEqual(["stable", "nightly"]);
    expect(groups).toHaveLength(2);
  });

  it("跨源同名并列在同一变体内，不做隐式优选", () => {
    const groups = groupModules(ENTRIES);
    const stable = groups
      .find((g) => g.moduleId === "dounai/input")
      ?.variants.find((v) => v.variant === "stable");
    // official 的两条版本与 community 的一条都在，三条并存。
    expect(stable?.entries.map((e) => e.source_id).sort()).toEqual([
      "community",
      "official",
      "official",
    ]);
  });

  it("同一变体的多个版本按新到旧排，不依赖传入顺序", () => {
    const shuffled = [ENTRIES[1]!, ENTRIES[0]!, ENTRIES[2]!, ENTRIES[3]!, ENTRIES[4]!];
    const stable = groupModules(shuffled)
      .find((g) => g.moduleId === "dounai/input")
      ?.variants.find((v) => v.variant === "stable");
    expect(stable?.entries.map((e) => e.version)).toEqual(["1.2.0", "1.0.0", "0.9.0"]);
  });

  it("模块之间按角色顺序排：输入模块在模型模块之前", () => {
    const groups = groupModules(ENTRIES);
    expect(groups.map((g) => g.moduleId)).toEqual(["dounai/input", "dounai/model"]);
  });

  it("空列表返回空分组", () => {
    expect(groupModules([])).toEqual([]);
  });
});

describe("failedSources", () => {
  const status = (
    id: string,
    state: SourceStatusView["state"],
    reason: string | null = null,
  ): SourceStatusView => ({ id, name: id, state, reason });

  it("只挑出网络不可达与验签失败，放过成功与主动停用的源", () => {
    const all = [
      status("ok-source", "ok"),
      status("off-source", "disabled"),
      status("net-source", "unavailable", "连接超时"),
      status("sig-source", "invalid", "签名不符"),
    ];
    expect(failedSources(all).map((s) => s.id)).toEqual(["net-source", "sig-source"]);
  });

  it("全部成功时没有失败项", () => {
    expect(failedSources([status("a", "ok"), status("b", "disabled")])).toEqual([]);
  });
});
