/**
 * 日志页：回答「刚才发生了什么」。
 *
 * 左侧选实例，右侧读该实例的日志尾部。日志由启动器写入
 * （拉起、探测结果、退出码、停止），子进程 stdout 需显式开关才落盘，
 * 所以这里不能放一个假的可点开关——开关是启动前的环境变量。
 */

import { isDesktop, readInstanceLogs, type InstanceLogs } from "../../bridge.js";
import { esc, toast } from "../dom.js";
import { icon } from "../icons.js";
import { describeError, isExternal, store } from "../store.js";
import type { ViewSlots } from "./modules.js";

interface LogState {
  selected: string | null;
  cache: Map<string, InstanceLogs>;
}

const state: LogState = {
  selected: null,
  cache: new Map(),
};

/** 从模块页跳进来时指定实例。 */
export function focusInstance(instanceId: string): void {
  state.selected = instanceId;
}

export async function renderLogs(slots: ViewSlots): Promise<void> {
  if (!state.selected) {
    const preferred =
      store.views.find((view) => view.snapshot?.phase === "running") ?? store.views[0];
    state.selected = preferred?.instance.instance_id ?? null;
  }
  renderHead(slots.head);
  renderBody(slots.body);
  await loadSelected();
}

function renderHead(head: HTMLElement): void {
  head.innerHTML = `
    <div>
      <h1>日志</h1>
      <div class="sub">${store.views.length} 个实例 · 2 MiB × 3 份自动轮转</div>
    </div>
    <div class="sp">
      <button class="btn btn--sm" type="button" data-act="reload">刷新</button>
    </div>
  `;
  head.querySelector<HTMLButtonElement>('[data-act="reload"]')?.addEventListener("click", () => {
    void loadSelected(true);
  });
}

function renderBody(body: HTMLElement): void {
  if (!isDesktop()) {
    body.innerHTML = `<div class="placeholder">
      <span class="ph-ico">${icon("logs", 18)}</span>
      <b>浏览器模式没有日志</b>
      <p>日志写在数据根的 <code>instances/&lt;key&gt;/logs/</code>，只有桌面端能读。</p>
    </div>`;
    return;
  }
  if (store.views.length === 0) {
    body.innerHTML = `<div class="placeholder">
      <span class="ph-ico">${icon("logs", 18)}</span>
      <b>还没有实例</b>
      <p>先到「导入本地」登记模块。启动器从第一次启动开始写日志。</p>
    </div>`;
    return;
  }

  body.classList.add("pbody--flush");
  body.innerHTML = `
    <div class="two" style="padding:16px 22px">
      <div class="envlist" style="align-self:start"></div>
      <div class="logbox"></div>
    </div>
  `;

  const list = body.querySelector(".envlist");
  if (list) {
    for (const view of store.views) {
      const id = view.instance.instance_id;
      const cached = state.cache.get(id);
      const row = document.createElement("button");
      row.type = "button";
      row.className = "envrow";
      row.style.width = "100%";
      row.style.border = "0";
      row.style.borderBottom = "1px solid var(--line)";
      row.style.background = id === state.selected ? "var(--accent-soft)" : "transparent";
      row.style.cursor = "pointer";
      row.style.textAlign = "left";
      row.innerHTML = `
        <span class="dot ${dotFor(id)}"></span>
        <span class="k" style="width:auto;flex:1;color:${id === state.selected ? "var(--accent-fg)" : "var(--fg-2)"};overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(id)}</span>
        <span class="sp inst-meta">${cached ? `${cached.files} 份` : isExternal(view) ? "外部" : ""}</span>
      `;
      row.addEventListener("click", () => {
        state.selected = id;
        const bodyEl = document.getElementById("pbody");
        if (bodyEl) renderBody(bodyEl);
        void loadSelected();
      });
      list.appendChild(row);
    }
  }

  renderLogBox(body);
}

function dotFor(instanceId: string): string {
  const view = store.views.find((v) => v.instance.instance_id === instanceId);
  const phase = view?.snapshot?.phase ?? "stopped";
  if (phase === "running") return "dot--run";
  if (phase === "starting" || phase === "stopping") return "dot--warn";
  if (phase === "failed") return "dot--fail";
  return "dot--idle";
}

function renderLogBox(body: HTMLElement): void {
  const box = body.querySelector(".logbox");
  if (!box) return;
  const id = state.selected;
  const logs = id ? state.cache.get(id) : undefined;

  if (!id) {
    box.innerHTML = `<div class="placeholder"><b>选一个实例</b></div>`;
    return;
  }

  if (!logs) {
    box.innerHTML = `<div class="loghead"><span style="font-size:11.5px;color:var(--fg-2)">${esc(id)}</span></div>
      <pre class="logview">正在读取…</pre>`;
    return;
  }

  box.innerHTML = `
    <div class="loghead">
      <span style="font-size:11.5px;color:var(--fg-2);overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(id)}</span>
      <span style="margin-left:auto;display:flex;gap:5px">
        <span class="logfile is-now">最新</span>
        <span class="logfile">共 ${logs.files} 份</span>
        ${logs.truncated ? '<span class="logfile">已截断</span>' : ""}
      </span>
    </div>
    <pre class="logview">${logs.lines.length === 0 ? esc(logs.note) : logs.lines.map(highlight).join("\n")}</pre>
    <div style="padding:8px 10px;border-top:1px solid var(--line);display:flex;gap:8px;align-items:center;font-size:11px;color:var(--fg-3)">
      <span>子进程输出</span>
      <span class="chip chip--stop" style="margin-left:auto">默认关</span>
      <span>需 <code>DOUNAI_LOG_CHILD_OUTPUT=1</code> 启动</span>
    </div>
  `;
}

/** 日志行着色：只按内容判定，不改写原文。 */
function highlight(line: string): string {
  const text = esc(line);
  if (/\b(error|failed|失败)\b/i.test(line)) return `<span class="er">${text}</span>`;
  if (/\b(stop|stopped|exit)\b/i.test(line) && !/\berror\b/i.test(line)) {
    return `<span class="wn">${text}</span>`;
  }
  if (/\b(health 200|phase=running|spawned|ok)\b/i.test(line)) {
    return `<span class="ok">${text}</span>`;
  }
  return text;
}

async function loadSelected(force = false): Promise<void> {
  const id = state.selected;
  if (!id || !isDesktop()) return;
  if (!force && state.cache.has(id) && store.views.some((v) => v.instance.instance_id === id)) {
    // 已有缓存也重新读一次：日志是追加写的，刷新才有意义。
  }
  try {
    const logs = await readInstanceLogs(id);
    state.cache.set(id, logs);
    const body = document.getElementById("pbody");
    if (body) renderLogBox(body);
    const list = body?.querySelector(".envlist");
    if (list) {
      const rows = list.querySelectorAll(".envrow");
      const index = store.views.findIndex((view) => view.instance.instance_id === id);
      const row = rows[index];
      if (row) {
        const meta = row.querySelector(".inst-meta");
        if (meta) meta.textContent = `${logs.files} 份`;
      }
    }
  } catch (error) {
    toast(`读取日志失败：${describeError(error)}`, "error");
  }
}
