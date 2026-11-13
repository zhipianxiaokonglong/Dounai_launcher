/**
 * 依赖安装进度的界面状态机。
 *
 * 只测状态落位与清理：进度事件是后端的事实，前端不能自己编造阶段，
 * 也不该在成功后留下一条「安装中」。
 */

import { afterEach, describe, expect, it } from "vitest";
import { applyEnvProgress, beginEnvJob, endEnvJob, envJobOf, store } from "../src/ui/store.js";

const ID = "dounai/dounai-input@stable";

afterEach(() => {
  delete store.envJobs[ID];
});

describe("env job", () => {
  it("begin 后立刻有一条可显示的「准备开始」", () => {
    beginEnvJob(ID);
    const job = envJobOf(ID);
    expect(job).not.toBeNull();
    expect(job?.phase).toBe("starting");
    expect(job?.error).toBeNull();
  });

  it("进度事件保留开始时间，并清掉上一次的错误", () => {
    beginEnvJob(ID);
    endEnvJob(ID, "上次失败");
    expect(envJobOf(ID)?.error).toBe("上次失败");

    const started = envJobOf(ID)?.startedAt ?? 0;
    applyEnvProgress({
      instanceId: ID,
      phase: "sync",
      title: "同步依赖到 venv",
      detail: "安装 uvicorn 0.34.0",
      current: null,
      total: 42,
    });
    const job = envJobOf(ID);
    expect(job?.phase).toBe("sync");
    expect(job?.detail).toBe("安装 uvicorn 0.34.0");
    expect(job?.total).toBe(42);
    expect(job?.error).toBeNull();
    // 开始时间不因新事件重置，卡片的「已用 Ns」才不会跳回 0。
    expect(job?.startedAt).toBe(started);
  });

  it("成功后整条抹掉，失败则留下原因", () => {
    beginEnvJob(ID);
    endEnvJob(ID);
    expect(envJobOf(ID)).toBeNull();

    beginEnvJob(ID);
    applyEnvProgress({
      instanceId: ID,
      phase: "resolve",
      title: "解析依赖",
      detail: null,
      current: null,
      total: null,
    });
    endEnvJob(ID, "依赖准备失败：SnapshotMismatch");
    expect(envJobOf(ID)?.error).toContain("SnapshotMismatch");
    expect(envJobOf(ID)?.phase).toBe("resolve");
  });
});
