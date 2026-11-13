/**
 * 角色的展示文案与顺序。
 *
 * 单列一份而不是各页各写：模块页按 role 分组，目录弹窗也按同一顺序铺开，
 * 两处必须一致，否则同一个模块在两个地方会「换了个位置」。
 * role 本身是冻结契约（`contracts/roles-v1.json`），这里只有文案。
 */

import type { Role } from "../bridge.js";

/** 展示顺序：数据面三层在前，shell 与 auxiliary 在后。 */
export const ROLE_ORDER: Role[] = ["input", "llm", "output", "shell", "auxiliary"];

export const ROLE_TITLE: Record<Role, string> = {
  input: "输入",
  llm: "模型",
  output: "输出",
  shell: "总装",
  auxiliary: "旁路",
};

export const ROLE_SHORT: Record<Role, string> = {
  input: "in",
  llm: "llm",
  output: "out",
  shell: "sh",
  auxiliary: "aux",
};

export const ROLE_DESC: Record<Role, string> = {
  input: "输入接入层：把麦克风、文本与图像送进模型层。",
  llm: "模型编排层：推理、上下文与情绪标签。",
  output: "输出表达层：把模型输出合成为语音。",
  shell: "总装与界面：本地 exe，不占用固定端口。",
  auxiliary: "旁路模块：不进组装层事件流，端口由启动器动态分配。",
};

/** 数据面三层：一条能对话的最小链路就是这三个角色各一个实例。 */
export const DATA_PLANE: Role[] = ["input", "llm", "output"];
