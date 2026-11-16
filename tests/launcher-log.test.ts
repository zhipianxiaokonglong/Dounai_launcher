/**
 * 启动器全局日志的界面入口。
 *
 * 「装依赖点了没反应」这类问题，用户先看的是操作层时间线，不是某个模块
 * 的 stdout。日志页必须有一个稳定的入口读启动器自己的日志，且这个入口
 * 不能和真实实例的 id 撞车——否则一次误选会把数据根当成实例目录读。
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const logs = readFileSync(resolve(process.cwd(), "src/ui/views/logs.ts"), "utf8");
const bridge = readFileSync(resolve(process.cwd(), "src/bridge.ts"), "utf8");
const stateRust = readFileSync(
  resolve(process.cwd(), "src-tauri/src/state_v2.rs"),
  "utf8",
);
const commands = readFileSync(
  resolve(process.cwd(), "src-tauri/src/commands_v2.rs"),
  "utf8",
);

describe("启动器日志入口", () => {
  it("前端与后端共用同一个伪实例 id", () => {
    expect(logs).toMatch(/export const LAUNCHER_LOG_ID = "__launcher__"/);
    expect(stateRust).toMatch(/pub const LAUNCHER_LOG_ID: &str = "__launcher__"/);
  });

  it("日志页把启动器放在实例下拉第一项", () => {
    expect(logs).toMatch(/<option value="\$\{LAUNCHER_LOG_ID\}"[^>]*>启动器（全局操作）<\/option>/);
    expect(logs.indexOf("LAUNCHER_LOG_ID") ).toBeLessThan(logs.indexOf("store.views"));
  });

  it("三个日志命令都按伪实例映射到数据根", () => {
    expect(stateRust).toMatch(/pub fn logs_dir_for/);
    expect(stateRust).toMatch(/if instance_id == LAUNCHER_LOG_ID/);
    expect(commands.match(/state\.logs_dir_for\(&instance_id\)/g)?.length).toBe(3);
  });

  it("装依赖同时写模块日志与启动器摘要", () => {
    expect(commands).toMatch(/开始装依赖 instance=\{instance_id\}/);
    expect(commands).toMatch(/装依赖完成 instance=\{instance_id\}/);
    expect(commands).toMatch(/装依赖失败 instance=\{instance_id\}/);
    // 阶段细节写进模块日志：定位 uv / 准备 Python / 解析 / 同步都要可读。
    expect(commands).toMatch(/crate::diag_log::write\(\s*log,\s*"env"/);
  });

  it("启动器日志没有子进程输出开关", () => {
    expect(logs).toMatch(/state\.instanceId !== LAUNCHER_LOG_ID/);
  });

  it("桥接层沿用现有日志命令，不新增调用面", () => {
    expect(bridge).toMatch(/read_instance_logs/);
    expect(bridge).toMatch(/list_instance_logs/);
  });
});
