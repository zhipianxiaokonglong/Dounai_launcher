/**
 * 日志行的解析与过滤。
 *
 * 这些断言守的是「用户点『错误』筛选时能不能看到出问题的原因」——
 * 判级顺序错了、跨天日期漏了、搜索只搜正文不搜来源，都不会让界面崩，
 * 只会让人在真机上找不到那条原因。
 */

import { describe, expect, it } from "vitest";
import { humanBytes, levelOf, matchesFilter, parseLine } from "../src/ui/loglines.js";

const NOW = new Date(2026, 10, 13, 21, 0, 0);

describe("parseLine", () => {
  it("把时间、来源与正文拆开", () => {
    const row = parseLine("2026-11-13T13:38:52Z [lifecycle] spawned pid=4132", 7, NOW);
    expect(row.index).toBe(7);
    expect(row.time).toBe("13:38:52");
    expect(row.scope).toBe("lifecycle");
    expect(row.message).toBe("spawned pid=4132");
    expect(row.day).toBe("");
    expect(row.level).toBe("ok");
  });

  it("跨天的行补上日期，今天的行不补", () => {
    expect(parseLine("2026-11-13T09:00:00Z [probe] ok", 1, NOW).day).toBe("");
    expect(parseLine("2026-11-12T23:59:59Z [probe] ok", 1, NOW).day).toBe("11-12");
  });

  it("格式不符的行原样保留正文，仍按内容判级", () => {
    const row = parseLine("Traceback (most recent call last):", 3, NOW);
    expect(row.time).toBe("");
    expect(row.scope).toBe("");
    expect(row.message).toBe("Traceback (most recent call last):");
    expect(row.level).toBe("error");
  });
});

describe("levelOf", () => {
  it("同时命中错误与警告时按错误算", () => {
    expect(levelOf("[health] probe failed, will retry in 2s")).toBe("error");
  });

  it("中文原因不被漏判", () => {
    expect(levelOf("[lifecycle] 启动失败：端口被占用")).toBe("error");
    expect(levelOf("[probe] 健康检查超时")).toBe("warn");
  });

  it("普通事件不算问题", () => {
    expect(levelOf("[lifecycle] start requested")).toBe("info");
    expect(levelOf("[probe] health=Healthy contract=Healthy")).toBe("info");
  });
});

describe("matchesFilter", () => {
  const rows = [
    parseLine("2026-11-13T10:00:00Z [lifecycle] spawned pid=1", 1, NOW),
    parseLine("2026-11-13T10:00:01Z [probe] health check timeout", 2, NOW),
    parseLine("2026-11-13T10:00:02Z [probe] failed: connection refused", 3, NOW),
  ];

  it("错误筛选只留错误", () => {
    expect(rows.filter((row) => matchesFilter(row, "error", "")).map((row) => row.index)).toEqual([3]);
  });

  it("警告筛选留警告与错误，不放行正常事件", () => {
    expect(rows.filter((row) => matchesFilter(row, "warn", "")).map((row) => row.index)).toEqual([2, 3]);
  });

  it("搜索覆盖来源而不是只看正文", () => {
    expect(rows.filter((row) => matchesFilter(row, "all", "lifecycle")).map((row) => row.index)).toEqual([1]);
  });

  it("空搜索串等于不过滤", () => {
    expect(rows.filter((row) => matchesFilter(row, "all", "   ")).length).toBe(3);
  });
});

describe("humanBytes", () => {
  it("按量级换单位", () => {
    expect(humanBytes(512)).toBe("512 B");
    expect(humanBytes(2048)).toBe("2.0 KiB");
    expect(humanBytes(2 * 1024 * 1024)).toBe("2.00 MiB");
  });
});
