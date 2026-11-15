/**
 * M6 链路视图的纯函数与桥接形状。
 *
 * 守两条语义：一是结果按「第 N 步」定位，失败不抹掉其余步骤；
 * 二是命令参数形状与 Rust 侧签名对齐，改了签名这里先失败。
 */

import { describe, expect, it, afterEach } from "vitest";
import {
  DEFAULT_CHAIN_ID,
  deleteChain,
  listChains,
  saveChain,
  startChain,
  stopChain,
  type ChainRunResult,
  type ChainStepResult,
  type ChainStepState,
} from "../src/bridge.js";
import {
  formatChainResult,
  moveStep,
  reconcileFailedSteps,
  stepLine,
  stepPhaseLabel,
  toggleStep,
} from "../src/ui/views/chains.js";

type Call = { cmd: string; args?: Record<string, unknown> };

function installTauri(result: unknown, calls: Call[]): void {
  (globalThis as Record<string, unknown>).__TAURI__ = {
    core: {
      invoke: async (cmd: string, args?: Record<string, unknown>) => {
        calls.push({ cmd, args });
        return result;
      },
    },
  };
}

afterEach(() => {
  delete (globalThis as Record<string, unknown>).__TAURI__;
});

function step(
  index: number,
  id: string,
  state: ChainStepState,
  reason: string | null = null,
): ChainStepResult {
  return { index, instance_id: id, state, reason };
}

function result(steps: ChainStepResult[]): ChainRunResult {
  return {
    chain_id: "chain-1",
    is_default: false,
    steps,
    missing_deps: [],
  };
}

describe("链路结果解读", () => {
  it("逐状态汇总，失败步骤带着第几步与原因", () => {
    const run = result([
      step(1, "a@stable", "started"),
      step(2, "b@stable", "external", "端口上已有外部健康服务"),
      step(3, "c@stable", "failed", "端口被占"),
      step(4, "d@stable", "blocked", "依赖未准备"),
      step(5, "e@stable", "dangling", "实例已卸载"),
    ]);
    const summary = formatChainResult(run);
    expect(summary.total).toBe(5);
    expect(summary.started).toBe(1);
    expect(summary.external).toBe(1);
    expect(summary.failed).toBe(1);
    expect(summary.blocked).toBe(1);
    expect(summary.dangling).toBe(1);
    expect(summary.lines).toHaveLength(5);
    expect(summary.lines[2]).toContain("第 3 步");
    expect(summary.lines[2]).toContain("c@stable");
    expect(summary.lines[2]).toContain("端口被占");
  });

  it("stepLine 不把 reason 为 null 的步骤写成 null", () => {
    expect(stepLine(step(1, "a@stable", "started"))).toBe("第 1 步 · a@stable · 已提交启动");
  });

  it("生命周期失败只回填已提交启动的那一步", () => {
    const run = result([
      step(1, "a@stable", "started"),
      step(2, "b@stable", "external", "外部服务"),
      step(3, "c@stable", "blocked", "依赖未准备"),
      step(4, "d@stable", "dangling", "实例缺失"),
    ]);
    const next = reconcileFailedSteps(run, new Map([["a@stable", "进程启动后退出"]]));
    expect(next.steps[0]?.state).toBe("failed");
    expect(next.steps[0]?.reason).toBe("进程启动后退出");
    expect(next.steps[1]?.state).toBe("external");
    expect(next.steps[2]?.state).toBe("blocked");
    expect(next.steps[3]?.state).toBe("dangling");
    // 原结果不被就地改写。
    expect(run.steps[0]?.state).toBe("started");
  });

  it("没有对应失败实例时保持原结果", () => {
    const run = result([step(1, "a@stable", "started")]);
    expect(reconcileFailedSteps(run, new Map()).steps[0]?.state).toBe("started");
  });
});

describe("编辑顺序", () => {
  it("上下移只换顺序，不改集合", () => {
    const steps = ["a", "b", "c"];
    expect(moveStep(steps, 0, 2)).toEqual(["b", "c", "a"]);
    expect(moveStep(steps, 2, 0)).toEqual(["c", "a", "b"]);
    expect(moveStep(steps, 1, 1)).toEqual(steps);
    expect(steps).toEqual(["a", "b", "c"]);
  });

  it("越界移动原样返回，不丢步骤", () => {
    const steps = ["a", "b"];
    expect(moveStep(steps, -1, 0)).toEqual(steps);
    expect(moveStep(steps, 0, 5)).toEqual(steps);
  });

  it("勾选与取消都不改原数组", () => {
    const steps = ["a", "b"];
    expect(toggleStep(steps, "c")).toEqual(["a", "b", "c"]);
    expect(toggleStep(steps, "a")).toEqual(["b"]);
    expect(steps).toEqual(["a", "b"]);
  });
});

describe("链路命令形状", () => {
  it("list_chains 不带参数", async () => {
    const calls: Call[] = [];
    installTauri({ chains: [], default_steps: [] }, calls);
    await listChains();
    expect(calls[0]?.cmd).toBe("list_chains");
    expect(calls[0]?.args).toBeUndefined();
  });

  it("save_chain 把 request 整体传过去", async () => {
    const calls: Call[] = [];
    installTauri({ chains: [], default_steps: [] }, calls);
    await saveChain({ name: "日常陪伴", steps: ["a@stable", "b@stable"] });
    expect(calls[0]?.cmd).toBe("save_chain");
    expect(calls[0]?.args).toEqual({
      request: { name: "日常陪伴", steps: ["a@stable", "b@stable"] },
    });
  });

  it("delete_chain 传 id", async () => {
    const calls: Call[] = [];
    installTauri({ chains: [], default_steps: [] }, calls);
    await deleteChain("chain-1");
    expect(calls[0]?.args).toEqual({ id: "chain-1" });
  });

  it("start_chain 传 chainId，默认链路用保留 id", async () => {
    const calls: Call[] = [];
    installTauri({}, calls);
    await startChain(DEFAULT_CHAIN_ID);
    expect(calls[0]?.cmd).toBe("start_chain");
    expect(calls[0]?.args).toEqual({ chainId: "default" });
  });

  it("stop_chain 的 onlyStarted 显式带上，回滚停止才生效", async () => {
    const calls: Call[] = [];
    installTauri({}, calls);
    await stopChain("chain-1", true);
    expect(calls[0]?.cmd).toBe("stop_chain");
    expect(calls[0]?.args).toEqual({ chainId: "chain-1", onlyStarted: true });
    await stopChain("chain-1");
    expect(calls[1]?.args).toEqual({ chainId: "chain-1", onlyStarted: false });
  });
});

/**
 * 没有本次运行结果时，步骤状态点必须反映实例当前相位。
 *
 * 之前的实现只按「实例在不在」给文案：实例通过模块页一键启动、已经在跑，
 * 自定义链路卡片仍写「待启动」，同一块界面上出现两份矛盾的事实。
 */
describe("stepPhaseLabel", () => {
  it("运行中给绿点", () => {
    expect(stepPhaseLabel("running", 14484)).toEqual({ dot: "ok", text: "运行中" });
  });

  it("外部启动的实例也是绿点，但文案要区分", () => {
    expect(stepPhaseLabel("running", null).text).toContain("外部启动");
    expect(stepPhaseLabel("running", null).dot).toBe("ok");
  });

  it("启动中与停止中是黄点，不是未运行", () => {
    expect(stepPhaseLabel("starting", null).dot).toBe("warn");
    expect(stepPhaseLabel("stopping", 0).dot).toBe("warn");
  });

  it("异常退出是红点", () => {
    expect(stepPhaseLabel("failed", null)).toEqual({ dot: "fail", text: "异常退出" });
  });

  it("真正没跑才是未运行的灰点", () => {
    expect(stepPhaseLabel("stopped", null)).toEqual({ dot: "idle", text: "未运行" });
    expect(stepPhaseLabel(undefined, null).text).toBe("未运行");
  });
});
