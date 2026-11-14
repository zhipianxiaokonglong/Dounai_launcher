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

/**
 * 角色图形：只标识层级，不表达状态。
 *
 * 不用 `ROLE_SHORT` 的两个字母缩写，是因为模块页的卡片与概览的链路节点
 * 都要靠图形区分「这是哪一层」；缩写在缩到 24px 的方块里认不出来。
 */
export const ROLE_ICON: Record<Role, string> = {
  input: '<path d="M12 15a3.5 3.5 0 003.5-3.5V6a3.5 3.5 0 10-7 0v5.5A3.5 3.5 0 0012 15z"/><path d="M5.5 11.5a6.5 6.5 0 0013 0M12 18v2.5"/>',
  llm: '<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/><path d="M4 7.5l8 4.5 8-4.5M12 12v9"/>',
  output: '<path d="M4 10v4M8 7v10M12 4.5v15M16 7v10M20 10v4"/>',
  shell:
    '<rect x="3" y="4.5" width="18" height="15" rx="2"/><path d="M3 9h18M8 12.5h8M8 16h5"/>',
  auxiliary:
    '<circle cx="12" cy="12" r="3"/><path d="M12 3v3M12 18v3M3 12h3M18 12h3M6 6l2 2M16 16l2 2M18 6l-2 2M8 16l-2 2"/>',
};

/** 角色徽标上的英文名：数据面三层用同一套大写缩写，shell / auxiliary 用全称。 */
export const ROLE_TAG: Record<Role, string> = {
  input: "INPUT",
  llm: "LLM",
  output: "OUTPUT",
  shell: "SHELL",
  auxiliary: "AUX",
};

/** 分组标题下的那行说明：比 ROLE_DESC 短，只够一句。 */
export const ROLE_HINT: Record<Role, string> = {
  input: "麦克风、文本与图像送进模型层",
  llm: "推理、上下文与情绪标签",
  output: "把模型输出合成为语音",
  shell: "本地界面，不占端口",
  auxiliary: "旁路实例，端口由启动器分配",
};
