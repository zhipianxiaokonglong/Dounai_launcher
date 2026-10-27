/**
 * 启停桥接测试。
 *
 * 重点验证命令名与参数形状：Rust 侧命令签名一旦改，
 * 这里会立刻失败，而不是等到运行时报"命令不存在"。
 */

import { describe, expect, it, afterEach } from "vitest";
import {
  startModule,
  stopModule,
  startAll,
  stopAll,
  BridgeUnavailableError,
} from "../src/bridge.js";

type Call = { cmd: string; args?: Record<string, unknown> };

function installTauri(result: unknown, calls: Call[]) {
  (globalThis as Record<string, unknown>).__TAURI__ = {
    core: {
      invoke: async (cmd: string, args?: Record<string, unknown>) => {
        calls.push({ cmd, args });
        return result;
      },
    },
  };
}

function clearTauri() {
  delete (globalThis as Record<string, unknown>).__TAURI__;
}

const runView = {
  module_id: "omninexus",
  run: "running",
  pid: 1234,
  last_error: null,
  healthy: true,
};

describe("启停桥接", () => {
  afterEach(clearTauri);

  it("start_module 传 moduleId", async () => {
    const calls: Call[] = [];
    installTauri(runView, calls);
    const v = await startModule("omninexus");
    expect(calls[0]?.cmd).toBe("start_module");
    expect(calls[0]?.args).toEqual({ moduleId: "omninexus" });
    expect(v.run).toBe("running");
    expect(v.pid).toBe(1234);
  });

  it("stop_module 传 moduleId", async () => {
    const calls: Call[] = [];
    installTauri({ ...runView, run: "stopped", pid: null }, calls);
    const v = await stopModule("moodvoice");
    expect(calls[0]?.cmd).toBe("stop_module");
    expect(calls[0]?.args).toEqual({ moduleId: "moodvoice" });
    expect(v.run).toBe("stopped");
  });

  it("start_all 不带参数并返回多个快照", async () => {
    const calls: Call[] = [];
    installTauri([runView, { ...runView, module_id: "moodvoice", run: "failed" }], calls);
    const list = await startAll();
    expect(calls[0]?.cmd).toBe("start_all");
    expect(calls[0]?.args).toBeUndefined();
    expect(list).toHaveLength(2);
    expect(list[1]?.run).toBe("failed");
  });

  it("stop_all 不带参数", async () => {
    const calls: Call[] = [];
    installTauri([], calls);
    await stopAll();
    expect(calls[0]?.cmd).toBe("stop_all");
    expect(calls[0]?.args).toBeUndefined();
  });

  it("浏览器模式抛 BridgeUnavailableError", async () => {
    await expect(startModule("x")).rejects.toBeInstanceOf(BridgeUnavailableError);
  });
});
