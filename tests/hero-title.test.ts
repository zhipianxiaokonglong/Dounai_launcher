/**
 * 概览hero的状态文案口径。
 *
 * 默认链路每个 role 只启动 active 那一个实例。同 role 的非 active 实例是
 * 备用，未运行属于预期，不该和「该起却没起」一起算成「停着」。此前把
 * 4 跑 1 备用写成「4 个在运行，1 个停着」，看着像有实例出错。这里钉住
 * 备用单独措辞、停着只数真正的空闲，以及空表/故障/全跑等边界。
 */

import { describe, expect, it } from "vitest";
import { heroTitle } from "../src/ui/views/home.js";

describe("heroTitle 状态口径", () => {
  it("备用未运行不算停着，单独标备用待命", () => {
    // 5 个实例：4 个在跑，1 个是同 role 备用。
    expect(heroTitle(5, 4, 0, 1)).toBe("4 个在运行，1 个备用待命");
  });

  it("备用与真停着同时存在时分开报", () => {
    // 6 个：3 跑、1 备用、2 个该起却没起。idle = 6 - 3 - 1。
    expect(heroTitle(6, 3, 0, 1)).toBe("3 个在运行，1 个备用待命，2 个停着");
  });

  it("没有备用时停着照旧按差值算", () => {
    expect(heroTitle(5, 3, 0, 0)).toBe("3 个在运行，2 个停着");
  });

  it("故障优先于备用与停着", () => {
    expect(heroTitle(5, 3, 1, 1)).toBe("1 个实例需要处理");
  });

  it("空表、全跑、全停三种边界不走备用措辞", () => {
    expect(heroTitle(0, 0, 0, 0)).toBe("还没有装任何模块");
    expect(heroTitle(5, 5, 0, 0)).toBe("全部 5 个实例在运行");
    expect(heroTitle(3, 0, 0, 0)).toBe("3 个实例都停着");
  });
});
