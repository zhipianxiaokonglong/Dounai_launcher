/**
 * 链路页：M6 的第 8 视图。
 *
 * 一键启动就是启动隐式默认链路，和自定义链路共用 Rust 的 run_chain：
 * 一轮批量提交，步骤之间不等待健康结论。所以状态点不能在命令返回时
 * 定格——之后的 Failed 要靠 lifecycle-changed 回填到对应那一步。
 */

import {
  DEFAULT_CHAIN_ID,
  deleteChain,
  isDesktop,
  listChains,
  onChainProgress,
  onLifecycleChanged,
  prepareInstanceEnv,
  saveChain,
  startChain,
  stopChain,
  type ChainProgressEvent,
  type ChainRunResult,
  type ChainStepResult,
  type ChainStepState,
  type ChainsView,
  type ChainView,
  type Phase,
} from "../../bridge.js";
import { bindActions, confirmModal, esc, openModal, toast } from "../dom.js";
import { describeError, store } from "../store.js";
import type { ViewSlots } from "./modules.js";

export const STATE_TITLE: Record<ChainStepState, string> = {
  started: "已提交启动",
  external: "外部服务",
  failed: "失败",
  blocked: "依赖未装",
  dangling: "实例缺失",
};

export interface ChainSummary {
  total: number;
  started: number;
  external: number;
  failed: number;
  blocked: number;
  dangling: number;
  /** 逐步文案，第 N 步 · 实例 · 结论 · 原因。 */
  lines: string[];
}

/** 纯函数：模块页的一键启动与本页共用同一套结果解读。 */
export function formatChainResult(result: ChainRunResult): ChainSummary {
  const count = (state: ChainStepState): number =>
    result.steps.filter((step) => step.state === state).length;
  return {
    total: result.steps.length,
    started: count("started"),
    external: count("external"),
    failed: count("failed"),
    blocked: count("blocked"),
    dangling: count("dangling"),
    lines: result.steps.map((step) => stepLine(step)),
  };
}

export function stepLine(step: ChainStepResult): string {
  const reason = step.reason ? ` · ${step.reason}` : "";
  return `第 ${step.index} 步 · ${step.instance_id} · ${STATE_TITLE[step.state]}${reason}`;
}

/** 上下移只改顺序，不改集合；越界时原样返回。 */
export function moveStep(steps: string[], from: number, to: number): string[] {
  if (from < 0 || to < 0 || from >= steps.length || to >= steps.length) {
    return [...steps];
  }
  const next = [...steps];
  const item = next[from]!;
  next.splice(from, 1);
  next.splice(to, 0, item);
  return next;
}

export function toggleStep(steps: string[], id: string): string[] {
  return steps.includes(id) ? steps.filter((item) => item !== id) : [...steps, id];
}

/**
 * 命令返回后，把后续转 Failed 的实例回填到它那一步。
 * 只覆盖「已提交启动」的状态：external / blocked / dangling 已有明确结论，
 * 不该被后来的进程状态改写。
 */
export function reconcileFailedSteps(
  result: ChainRunResult,
  failed: Map<string, string>,
): ChainRunResult {
  return {
    ...result,
    steps: result.steps.map((step) => {
      const reason = failed.get(step.instance_id);
      if (step.state !== "started" || !reason) return step;
      return { ...step, state: "failed" as const, reason };
    }),
  };
}

/** 视图级状态：切页不卸载，最近一次结果与筛选都留着。 */
const view = {
  data: null as ChainsView | null,
  run: null as ChainRunResult | null,
  progress: null as ChainProgressEvent | null,
  loaded: false,
  busy: false,
};

let slots: ViewSlots | null = null;

interface EditorState {
  id: string | null;
  name: string;
  steps: string[];
}

let editor: EditorState | null = null;

/** 模块页的一键启动把结果交给本页，步骤清单与状态点用同一份数据渲染。 */
export function setLastChainRun(result: ChainRunResult): void {
  view.run = result;
  if (isCurrentView()) renderCurrent();
}

function isCurrentView(): boolean {
  return document.getElementById("pbody")?.dataset.view === "chains";
}

export async function renderChains(next: ViewSlots): Promise<void> {
  slots = next;
  renderHead(next.head);
  renderBody(next.body);
  if (!view.loaded && isDesktop()) {
    view.loaded = true;
    try {
      view.data = await listChains();
    } catch (error) {
      toast(`链路列表没读到：${describeError(error)}`, "error");
    }
    if (isCurrentView()) renderCurrent();
  }
}

function renderCurrent(): void {
  if (!slots) return;
  renderHead(slots.head);
  renderBody(slots.body);
}

/* ------------------------------------------------------------------ */
/* 页头与正文                                                          */
/* ------------------------------------------------------------------ */

function renderHead(head: HTMLElement): void {
  const chains = view.data?.chains.length ?? 0;
  const defaults = view.data?.default_steps.length ?? 0;
  head.innerHTML = `
    <div>
      <h1>链路</h1>
      <div class="sub">${chains} 条自定义链路 · 默认链路 ${defaults} 步${view.busy ? " · 处理中…" : ""}</div>
    </div>
    <div class="sp">
      <button class="btn btn--sm" type="button" data-act="new-chain">新建链路</button>
      <button class="btn btn--sm" type="button" data-act="refresh-chains">刷新</button>
      <button class="btn btn--sm" type="button" data-act="stop-default">停止默认链路</button>
      <button class="btn btn--primary btn--sm" type="button" data-act="start-default">启动默认链路</button>
    </div>
  `;
  bindActions(head, [
    { act: "new-chain", run: () => openEditor(null) },
    { act: "refresh-chains", run: () => void loadChains() },
    { act: "start-default", run: () => void runStart(DEFAULT_CHAIN_ID) },
    { act: "stop-default", run: () => void runStop(DEFAULT_CHAIN_ID, false) },
  ]);
}

function renderBody(body: HTMLElement): void {
  body.classList.remove("pbody--flush", "pbody--admin");
  if (!isDesktop()) {
    body.innerHTML = `<div class="placeholder">
      <span class="ph-ico">⇢</span>
      <b>浏览器模式只能预览界面</b>
      <p>链路存在启动器的数据根里，启停需要桌面端进程。请用桌面端打开。</p>
    </div>`;
    return;
  }

  const data = view.data;
  const parts: string[] = [];
  parts.push(defaultCard(data?.default_steps ?? []));
  parts.push(resultCard());
  if (!data || data.chains.length === 0) {
    parts.push(`<div class="card card--dashed"><div class="card-top"><b>还没有自定义链路</b></div>
      <p class="cap" style="margin:4px 0 0">点右上「新建链路」勾选实例并排序。默认链路不需要创建，它会跟着已启用的实例自动变化。</p></div>`);
  } else {
    parts.push(...data.chains.map(chainCard));
  }
  body.innerHTML = parts.join("");
  bindActions(body, [
    { act: "chain-start", run: (node) => void runStart(node.dataset.id ?? "") },
    { act: "chain-stop", run: (node) => void runStop(node.dataset.id ?? "", false) },
    { act: "chain-rollback", run: (node) => void runStop(node.dataset.id ?? "", true) },
    { act: "chain-edit", run: (node) => openEditor(findChain(node.dataset.id ?? "")) },
    { act: "chain-delete", run: (node) => confirmDelete(node.dataset.id ?? "") },
    { act: "chain-install-deps", run: () => void installMissingDeps() },
  ]);
}

function defaultCard(steps: string[]): string {
  const list = steps.length
    ? steps
        .map((id, i) => `<li><span class="chain-idx">${i + 1}</span><code class="fp">${esc(id)}</code></li>`)
        .join("")
    : `<li class="cap">没有已启用的实例，默认链路为空。</li>`;
  return `<div class="card chain-card">
    <div class="card-top">
      <div><b>默认链路</b><span class="chip chip--active">隐式</span></div>
      <span class="cap">按 role 拓扑生成，不落盘</span>
    </div>
    <p class="cap" style="margin:6px 0">模块页的「一键启动」走的就是这条。增删或停用实例后，步骤自动跟着变。</p>
    <ol class="chain-steps">${list}</ol>
  </div>`;
}

/**
 * 实例相位到步骤状态点文案的映射。
 *
 * 外部启动的实例也是 running，但 pid 为 null（Supervisor 合成快照），
 * 界面上要能区分，否则用户会以为是自己没点启动。
 */
export function stepPhaseLabel(
  phase: Phase | undefined,
  pid: number | null,
): { dot: string; text: string } {
  switch (phase) {
    case "running":
      return pid === null
        ? { dot: "ok", text: "运行中 · 外部启动" }
        : { dot: "ok", text: "运行中" };
    case "starting":
      return { dot: "warn", text: "启动中" };
    case "stopping":
      return { dot: "warn", text: "停止中" };
    case "failed":
      return { dot: "fail", text: "异常退出" };
    default:
      return { dot: "idle", text: "未运行" };
  }
}

/**
 * 没有本次链路运行结果时的兜底：读实例当前的相位。
 *
 * 卡片列的是「这条链路现在是什么状况」，不是「上一次点启动时发生了什么」。
 * 用户从模块页一键启动（默认链路）后，自定义链路里这些实例已经在跑，若还写
 * 「待启动」，界面上就出现两份互相矛盾的事实。
 */
function liveStep(instanceId: string): { dot: string; text: string; reason: string | null } | null {
  const found = store.views.find((item) => item.instance.instance_id === instanceId);
  if (!found) return null;
  const snapshot = found.snapshot;
  const label = stepPhaseLabel(snapshot?.phase, snapshot?.pid ?? null);
  return {
    dot: label.dot,
    text: label.text,
    reason: snapshot?.phase === "failed" ? snapshot.last_error : null,
  };
}

function chainCard(chain: ChainView): string {
  const missing = chain.missing_count > 0
    ? `<span class="chip chip--fail">缺 ${chain.missing_count} 项</span>`
    : "";
  const steps = chain.steps.map((step) => {
    const result = stepResult(chain.id, step.index, step.instance_id);
    const live = result || !step.exists ? null : liveStep(step.instance_id);
    const dot = result ? dotClass(result.state) : live?.dot ?? "idle";
    const text = result
      ? STATE_TITLE[result.state]
      : step.exists
        ? live?.text ?? "未运行"
        : "实例缺失";
    const reason = result?.reason ?? live?.reason ?? null;
    return `<li class="chain-step">
      <span class="chain-idx">${step.index}</span>
      <span class="chain-dot chain-dot--${dot}" aria-hidden="true"></span>
      <span class="chain-step__name">${esc(step.name)}${step.exists ? "" : ' <b class="warn-text">（已卸载）</b>'}</span>
      <code class="fp">${esc(step.instance_id)}</code>
      <span class="sp"></span>
      <span class="cap">${esc(text)}${reason ? ` · ${esc(reason)}` : ""}</span>
    </li>`;
  }).join("");
  const failed = view.run?.chain_id === chain.id
    && view.run.steps.some((s) => s.state === "failed");
  return `<div class="card chain-card">
    <div class="card-top">
      <div><b>${esc(chain.name)}</b>${missing}<span class="cap">${chain.steps.length} 步</span></div>
      <div class="ctl--row">
        <button class="btn btn--sm" type="button" data-act="chain-edit" data-id="${esc(chain.id)}">编辑</button>
        <button class="btn btn--sm" type="button" data-act="chain-delete" data-id="${esc(chain.id)}">删除</button>
        <button class="btn btn--sm" type="button" data-act="chain-stop" data-id="${esc(chain.id)}">停止</button>
        ${failed
          ? `<button class="btn btn--sm" type="button" data-act="chain-rollback" data-id="${esc(chain.id)}">回滚停止</button>`
          : ""}
        <button class="btn btn--primary btn--sm" type="button" data-act="chain-start" data-id="${esc(chain.id)}">启动</button>
      </div>
    </div>
    <p class="cap" style="margin:4px 0">${esc(chain.id)} · 更新于 ${esc(chain.updated_at)}</p>
    <ol class="chain-steps">${steps}</ol>
  </div>`;
}

function resultCard(): string {
  const run = view.run;
  if (!run) return "";
  const summary = formatChainResult(run);
  const progress = view.progress
    ? `<p class="cap">正在提交第 ${view.progress.index} / ${view.progress.total} 步 · ${esc(view.progress.instance_id)}</p>`
    : "";
  const deps = run.missing_deps.length
    ? `<div class="chain-deps">
        <span>有 ${run.missing_deps.length} 个实例依赖未装：${esc(run.missing_deps.join("、"))}</span>
        <button class="btn btn--sm" type="button" data-act="chain-install-deps">一键补装</button>
      </div>`
    : "";
  const tone = summary.failed > 0 ? "chain-result--bad" : "chain-result--ok";
  return `<div class="card chain-result ${tone}">
    <div class="card-top">
      <div><b>最近一次启动</b><span class="cap">${esc(run.chain_id)}${run.is_default ? " · 默认链路" : ""}</span></div>
      <span class="cap">${summary.started} 提交 · ${summary.external} 外部 · ${summary.failed} 失败 · ${summary.blocked} 依赖未装 · ${summary.dangling} 实例缺失</span>
    </div>
    ${progress}${deps}
    <ul class="chain-steps">${summary.lines.map((line) => `<li class="cap">${esc(line)}</li>`).join("")}</ul>
    <p class="cap" style="margin:6px 0 0">状态点会跟着生命周期事件继续更新；进程启动几秒后失败，会回填到对应那一步。</p>
  </div>`;
}

function stepResult(
  chainId: string,
  index: number,
  instanceId: string,
): ChainStepResult | undefined {
  if (view.run?.chain_id !== chainId) return undefined;
  return view.run.steps.find(
    (step) => step.index === index && step.instance_id === instanceId,
  );
}

function dotClass(state: ChainStepState): string {
  switch (state) {
    case "started":
    case "external":
      return "ok";
    case "failed":
      return "fail";
    case "blocked":
    case "dangling":
      return "warn";
    default:
      return "idle";
  }
}

function findChain(id: string): ChainView | null {
  return view.data?.chains.find((chain) => chain.id === id) ?? null;
}

/* ------------------------------------------------------------------ */
/* 动作                                                                */
/* ------------------------------------------------------------------ */

async function loadChains(): Promise<void> {
  try {
    view.data = await listChains();
  } catch (error) {
    toast(`链路列表没读到：${describeError(error)}`, "error");
  }
  renderCurrent();
}

async function runStart(chainId: string): Promise<void> {
  if (!chainId || view.busy) return;
  view.busy = true;
  view.progress = null;
  renderCurrent();
  try {
    const result = await startChain(chainId);
    view.run = result;
    const summary = formatChainResult(result);
    const problems = summary.failed + summary.blocked + summary.dangling;
    const head = `链路启动 ${summary.started + summary.external} / ${summary.total} 步`;
    if (problems > 0) {
      const detail = result.steps
        .filter((s) => s.state === "failed" || s.state === "blocked" || s.state === "dangling")
        .map((s) => `第 ${s.index} 步 ${s.instance_id}（${s.reason ?? STATE_TITLE[s.state]}）`)
        .join("；");
      toast(`${head}，需注意：${detail}`, "error");
    } else {
      toast(`${head}${summary.external ? `，其中 ${summary.external} 个是外部服务` : ""}`, "ok");
    }
  } catch (error) {
    toast(`链路启动失败：${describeError(error)}`, "error");
  } finally {
    view.busy = false;
    view.progress = null;
    renderCurrent();
  }
}

async function runStop(chainId: string, onlyStarted: boolean): Promise<void> {
  if (!chainId) return;
  try {
    const result = await stopChain(chainId, onlyStarted);
    const tail = result.failed.length
      ? `，${result.failed.length} 个失败：${result.failed.map((f) => `第 ${f.index} 步 ${f.instance_id}`).join("；")}`
      : "";
    toast(
      `${onlyStarted ? "回滚" : ""}停止了 ${result.stopped.length} 个实例${tail}`,
      result.failed.length ? "error" : "ok",
    );
  } catch (error) {
    toast(`链路停止失败：${describeError(error)}`, "error");
  }
}

async function installMissingDeps(): Promise<void> {
  const ids = view.run?.missing_deps ?? [];
  if (ids.length === 0) return;
  view.busy = true;
  renderCurrent();
  try {
    for (const id of ids) {
      toast(`正在补装 ${id} 的依赖…`);
      await prepareInstanceEnv(id);
    }
    toast(`已补装 ${ids.length} 个实例的依赖，可以重新启动链路`, "ok");
  } catch (error) {
    toast(`补装依赖失败：${describeError(error)}`, "error");
  } finally {
    view.busy = false;
    renderCurrent();
  }
}

function confirmDelete(id: string): void {
  const chain = findChain(id);
  if (!chain) return;
  confirmModal({
    title: `删除链路 ${chain.name}`,
    subtitle: chain.id,
    bodyHtml: `<p style="margin:0">只删除这条编排，<b>不会卸载任何实例</b>。chains.json 里对应的条目会被移除。</p>`,
    confirmLabel: "删除链路",
    danger: true,
    onConfirm: async () => {
      try {
        view.data = await deleteChain(id);
        toast(`已删除链路 ${chain.name}`, "ok");
        renderCurrent();
      } catch (error) {
        toast(`删除失败：${describeError(error)}`, "error");
      }
    },
  });
}

/* ------------------------------------------------------------------ */
/* 编辑模态                                                            */
/* ------------------------------------------------------------------ */

function openEditor(chain: ChainView | null): void {
  editor = {
    id: chain?.id ?? null,
    name: chain?.name ?? "",
    steps: chain ? chain.steps.map((step) => step.instance_id) : [],
  };
  openModal({
    title: chain ? `编辑链路 ${chain.name}` : "新建链路",
    subtitle: "步骤按这里的顺序提交；启动不会等待上一步的健康检查。",
    bodyHtml: `<div id="chain-editor"></div>`,
    footNote: "缺失实例要显式移除，启动器不会替你改写已保存的步骤。",
    wide: true,
    actions: [
      { label: "取消", tone: "quiet", autofocus: true },
      { label: "保存", tone: "primary", onClick: (root) => saveEditor(root) },
    ],
    onMount: (root) => {
      bindActions(root, [
        {
          act: "chain-toggle",
          run: (node) => {
            syncName(root);
            const id = node.dataset.id ?? "";
            if (editor && id) editor.steps = toggleStep(editor.steps, id);
            renderEditor(root);
          },
        },
        {
          act: "chain-up",
          run: (node) => moveEditorStep(root, Number(node.dataset.index), -1),
        },
        {
          act: "chain-down",
          run: (node) => moveEditorStep(root, Number(node.dataset.index), 1),
        },
        {
          act: "chain-remove",
          run: (node) => {
            syncName(root);
            const index = Number(node.dataset.index);
            if (editor && Number.isInteger(index) && index >= 0) {
              editor.steps.splice(index, 1);
            }
            renderEditor(root);
          },
        },
      ]);
      renderEditor(root);
      window.setTimeout(() => root.querySelector<HTMLInputElement>("#chain-name")?.focus(), 0);
    },
  });
}

function moveEditorStep(root: HTMLElement, index: number, delta: number): void {
  syncName(root);
  if (editor && Number.isInteger(index)) {
    editor.steps = moveStep(editor.steps, index, index + delta);
  }
  renderEditor(root);
}

/** 列表重绘会清掉输入框内容，先把名称收回内存态。 */
function syncName(root: HTMLElement): void {
  const input = root.querySelector<HTMLInputElement>("#chain-name");
  if (input && editor) editor.name = input.value;
}

function renderEditor(root: HTMLElement): void {
  const host = root.querySelector<HTMLElement>("#chain-editor");
  if (!host || !editor) return;
  const names = new Map(store.views.map((v) => [v.instance.instance_id, v.name]));
  const chosen = editor.steps;
  const rows = chosen.length
    ? chosen
        .map((id, i) => {
          const missing = !names.has(id);
          return `<div class="chain-editor__row">
            <span class="chain-idx">${i + 1}</span>
            <span class="chain-editor__name">${esc(names.get(id) ?? id)}${missing ? ' <b class="warn-text">缺失</b>' : ""}</span>
            <code class="fp">${esc(id)}</code>
            <span class="sp"></span>
            <button class="btn btn--sm" type="button" data-act="chain-up" data-index="${i}"${i === 0 ? " disabled" : ""}>↑</button>
            <button class="btn btn--sm" type="button" data-act="chain-down" data-index="${i}"${i === chosen.length - 1 ? " disabled" : ""}>↓</button>
            <button class="btn btn--sm" type="button" data-act="chain-remove" data-index="${i}">移除</button>
          </div>`;
        })
        .join("")
    : `<p class="cap">还没有选实例。在下方勾选，或先去下载页安装模块。</p>`;
  const candidates = store.views.length
    ? store.views
        .map((v) => {
          const id = v.instance.instance_id;
          return `<label class="chain-editor__cand">
            <input type="checkbox" data-act="chain-toggle" data-id="${esc(id)}"${chosen.includes(id) ? " checked" : ""} />
            <span>${esc(v.name)}</span><code class="fp">${esc(id)}</code>
          </label>`;
        })
        .join("")
    : `<p class="cap">没有已安装实例。</p>`;
  host.innerHTML = `
    <label class="frow"><span>名称</span><input id="chain-name" class="input" value="${esc(editor.name)}" placeholder="例如：日常陪伴" /></label>
    <p class="cap">已选 ${chosen.length} / 32 步。顺序就是提交顺序，不按 role 重排。</p>
    <div class="chain-editor__list">${rows}</div>
    <p class="cap" style="margin-top:10px">可添加的实例</p>
    <div class="chain-editor__candidates">${candidates}</div>
  `;
}

async function saveEditor(root: HTMLElement): Promise<boolean | void> {
  syncName(root);
  if (!editor) return false;
  const name = editor.name.trim();
  const steps = [...editor.steps];
  if (!name) {
    toast("链路名称不能为空", "error");
    return false;
  }
  if (steps.length === 0) {
    toast("至少选一个实例", "error");
    return false;
  }
  try {
    view.data = await saveChain({ id: editor.id, name, steps });
    toast("链路已保存", "ok");
    renderCurrent();
    editor = null;
  } catch (error) {
    toast(`保存链路失败：${describeError(error)}`, "error");
    return false;
  }
}

/* ------------------------------------------------------------------ */
/* 事件订阅：注册一次，视图重绘不重复挂监听器                          */
/* ------------------------------------------------------------------ */

onChainProgress((progress) => {
  if (!isCurrentView()) return;
  view.progress = progress.phase === "submitting" ? progress : null;
  renderCurrent();
});

onLifecycleChanged(() => {
  const run = view.run;
  if (!run || !isCurrentView()) return;
  const failed = new Map<string, string>();
  for (const item of store.views) {
    if (item.snapshot?.phase === "failed") {
      failed.set(item.instance.instance_id, item.snapshot.last_error ?? "进程启动后退出");
    }
  }
  if (failed.size === 0) return;
  view.run = reconcileFailedSteps(run, failed);
  renderCurrent();
});
