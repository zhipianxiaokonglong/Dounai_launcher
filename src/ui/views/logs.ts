/**
 * 日志页：回答「刚才发生了什么」。
 *
 * 一个实例的日志会轮转成多份，默认看跨文件的最新尾部；下拉可以切到某一份
 * 历史文件——排查「昨天的崩溃」和「刚才的启动」用的是两种读法，界面必须两者都给。
 *
 * 布局上是「工具条 + 全宽日志」而不是左右两栏：日志行本身很长（`[lifecycle]
 * health=Unhealthy contract=Unhealthy`），左右分栏会把等宽内容挤到半屏，
 * 一屏看不到 20 行。实例与文件的选择都收进下拉，换来全宽的行区域。
 *
 * 过滤与搜索都在前端做：后端一次给 300 行尾部，改条件不该再走一次 IO。
 */

import {
  isDesktop,
  listInstanceLogs,
  openInstanceLogsDir,
  readInstanceLogs,
  setChildOutput,
  type InstanceLogs,
  type LogFileInfo,
} from "../../bridge.js";
import { bindActions, esc, toast } from "../dom.js";
import { icon } from "../icons.js";
import {
  humanBytes,
  matchesFilter,
  parseLine,
  type LevelFilter,
  type ParsedLine,
} from "../loglines.js";
import { describeError, isExternal, refreshSettings, store } from "../store.js";
import type { ViewSlots } from "./modules.js";

interface LogState {
  instanceId: string | null;
  /** null 表示跨文件的最新尾部。 */
  file: string | null;
  level: LevelFilter;
  query: string;
  wrap: boolean;
  follow: boolean;
  files: LogFileInfo[];
  data: InstanceLogs | null;
  lines: ParsedLine[];
  error: string | null;
  loading: boolean;
  timer: number | null;
  /** 下次刷新时是否贴底；用户往上翻过就不再抢滚动位置。 */
  stickToBottom: boolean;
}

const state: LogState = {
  instanceId: null,
  file: null,
  level: "all",
  query: "",
  wrap: false,
  follow: true,
  files: [],
  data: null,
  lines: [],
  error: null,
  loading: false,
  timer: null,
  stickToBottom: true,
};

const REFRESH_MS = 2000;

/** 从模块页跳进来时指定实例。 */
export function focusInstance(instanceId: string): void {
  if (state.instanceId !== instanceId) state.file = null;
  state.instanceId = instanceId;
}

/** 离开日志页时收掉定时器，否则后台会一直读磁盘。 */
export function disposeLogs(): void {
  stopTimer();
}

/* ------------------------------------------------------------------ */
/* 渲染                                                                */
/* ------------------------------------------------------------------ */

export async function renderLogs(slots: ViewSlots): Promise<void> {
  stopTimer();
  renderHead(slots.head);

  if (!isDesktop()) {
    slots.body.innerHTML = `<div class="placeholder">
      <span class="ph-ico">${icon("logs", 18)}</span>
      <b>浏览器模式没有日志</b>
      <p>日志写在数据根的 <code>instances/&lt;key&gt;/logs/</code>，只有桌面端能读。</p>
    </div>`;
    return;
  }
  if (store.views.length === 0) {
    slots.body.innerHTML = `<div class="placeholder">
      <span class="ph-ico">${icon("logs", 18)}</span>
      <b>还没有实例</b>
      <p>先到「导入本地」登记模块。启动器从第一次启动开始写日志。</p>
      <button class="btn btn--sm" type="button" data-act="go-import">去导入本地</button>
    </div>`;
    bindActions(slots.body, [
      {
        act: "go-import",
        run: () => {
          window.dispatchEvent(new CustomEvent("dounai:navigate", { detail: "import" }));
        },
      },
    ]);
    return;
  }

  if (!state.instanceId || !store.views.some((v) => v.instance.instance_id === state.instanceId)) {
    state.instanceId = defaultInstance();
    state.file = null;
  }

  slots.body.classList.add("pbody--flush");
  slots.body.innerHTML = `
    <div class="logpage">
      <div class="logbar">
        <label class="logfield">
          <span>实例</span>
          <select id="log-instance" class="input">${instanceOptions()}</select>
        </label>
        <label class="logfield">
          <span>文件</span>
          <select id="log-file" class="input">${fileOptions()}</select>
        </label>
        <div class="seg" role="group" aria-label="级别过滤">
          <button type="button" data-act="level" data-level="all" class="${state.level === "all" ? "is-on" : ""}">全部</button>
          <button type="button" data-act="level" data-level="warn" class="${state.level === "warn" ? "is-on" : ""}">警告+</button>
          <button type="button" data-act="level" data-level="error" class="${state.level === "error" ? "is-on" : ""}">错误</button>
        </div>
        <input id="log-search" class="input logsearch" type="search" placeholder="搜索内容（不区分大小写）" value="${esc(state.query)}" />
        <span class="sp"></span>
        <label class="check"><input type="checkbox" id="log-wrap" ${state.wrap ? "checked" : ""} /> 换行</label>
        <label class="check"><input type="checkbox" id="log-follow" ${state.follow ? "checked" : ""} /> 自动刷新</label>
        <button class="btn btn--sm" type="button" data-act="copy">复制</button>
        <button class="btn btn--sm" type="button" data-act="open-dir">打开目录</button>
      </div>
      <div class="logmeta" id="log-meta"></div>
      <div class="logpane" id="log-pane"><div class="loglines" id="log-lines"></div></div>
    </div>
  `;

  wire(slots.body);
  await reloadFiles();
  await refresh({ silent: false });
  if (state.follow) startTimer();
  paintMeta();
  paintLines();
}

function defaultInstance(): string | null {
  // 优先落在正在跑的实例上：用户点「日志」几乎都是为了看刚启动的那个。
  const running = store.views.find((view) => view.snapshot?.phase === "running");
  const chosen = running ?? store.views[0];
  return chosen ? chosen.instance.instance_id : null;
}

function renderHead(head: HTMLElement): void {
  const running = store.views.filter((v) => v.snapshot?.phase === "running").length;
  head.innerHTML = `
    <div>
      <h1>日志</h1>
      <div class="sub">${store.views.length} 个实例 · ${running} 个运行中 · 单文件 2 MiB × 3 份自动轮转</div>
    </div>
    <div class="sp">
      <button class="btn btn--sm" type="button" data-act="reload">重新读取</button>
    </div>
  `;
  bindActions(head, [{ act: "reload", run: () => refresh({ silent: false }) }]);
}

function instanceOptions(): string {
  return store.views
    .map((view) => {
      const id = view.instance.instance_id;
      const name = view.name || view.instance.module_id;
      const phase = phaseText(view.snapshot?.phase);
      return `<option value="${esc(id)}" ${id === state.instanceId ? "selected" : ""}>${esc(name)} · ${esc(id)} · ${esc(phase)}</option>`;
    })
    .join("");
}

function fileOptions(): string {
  const head = `<option value="" ${state.file === null ? "selected" : ""}>最新（跨文件尾部）</option>`;
  if (state.files.length <= 1) return head;
  return (
    head +
    state.files
      .map((file) => {
        const label = `${file.current ? "最新 · " : ""}${file.name.replace(/\.log$/, "")} · ${humanBytes(file.size)}`;
        return `<option value="${esc(file.name)}" ${file.name === state.file ? "selected" : ""}>${esc(label)}</option>`;
      })
      .join("")
  );
}

/* ------------------------------------------------------------------ */
/* 交互                                                                */
/* ------------------------------------------------------------------ */

function wire(body: HTMLElement): void {
  body.querySelector<HTMLSelectElement>("#log-instance")?.addEventListener("change", (event) => {
    const value = (event.target as HTMLSelectElement).value;
    if (value === state.instanceId) return;
    state.instanceId = value;
    state.file = null;
    state.data = null;
    state.lines = [];
    state.stickToBottom = true;
    void (async () => {
      await reloadFiles();
      await refresh({ silent: false });
      const select = body.querySelector<HTMLSelectElement>("#log-file");
      if (select) select.innerHTML = fileOptions();
      paintMeta();
      paintLines();
    })();
  });

  body.querySelector<HTMLSelectElement>("#log-file")?.addEventListener("change", (event) => {
    const value = (event.target as HTMLSelectElement).value;
    state.file = value === "" ? null : value;
    state.stickToBottom = true;
    void refresh({ silent: false }).then(() => {
      paintMeta();
      paintLines();
    });
  });

  const search = body.querySelector<HTMLInputElement>("#log-search");
  let searchTimer: number | null = null;
  search?.addEventListener("input", () => {
    // 输入法连打时不必每键重排，150ms 合并一次。
    if (searchTimer !== null) window.clearTimeout(searchTimer);
    searchTimer = window.setTimeout(() => {
      state.query = search.value.trim();
      paintMeta();
      paintLines();
    }, 150);
  });

  body.querySelector<HTMLInputElement>("#log-wrap")?.addEventListener("change", (event) => {
    state.wrap = (event.target as HTMLInputElement).checked;
    paintLines();
  });

  body.querySelector<HTMLInputElement>("#log-follow")?.addEventListener("change", (event) => {
    state.follow = (event.target as HTMLInputElement).checked;
    if (state.follow) {
      startTimer();
      void refresh({ silent: true }).then(() => paintLines());
    } else {
      stopTimer();
    }
    paintMeta();
  });

  const pane = body.querySelector<HTMLElement>("#log-pane");
  pane?.addEventListener("scroll", () => {
    const distance = pane.scrollHeight - pane.scrollTop - pane.clientHeight;
    state.stickToBottom = distance < 32;
  });

  bindActions(body, [
    {
      act: "level",
      run: (node) => {
        const level = node.dataset.level as LevelFilter | undefined;
        if (!level || level === state.level) return;
        state.level = level;
        body
          .querySelectorAll<HTMLElement>('[data-act="level"]')
          .forEach((button) => button.classList.toggle("is-on", button.dataset.level === level));
        paintMeta();
        paintLines();
      },
    },
    { act: "copy", run: () => copyVisible() },
    { act: "open-dir", run: () => void openDir() },
    { act: "toggle-child", run: () => void toggleChildOutput() },
  ]);
}

/* ------------------------------------------------------------------ */
/* 数据                                                                */
/* ------------------------------------------------------------------ */

async function reloadFiles(): Promise<void> {
  if (!state.instanceId) return;
  try {
    state.files = await listInstanceLogs(state.instanceId);
  } catch {
    // 文件列表只是选项，读不到不影响正文；正文会给出原因。
    state.files = [];
  }
  if (state.file && !state.files.some((file) => file.name === state.file)) {
    state.file = null;
  }
}

async function refresh(options: { silent: boolean }): Promise<void> {
  const id = state.instanceId;
  if (!id) return;
  if (!options.silent) state.loading = true;
  try {
    const data = await readInstanceLogs(id, state.file);
    state.data = data;
    state.error = null;
    state.lines = data.lines.map((line, index) => parseLine(line, index + 1));
    // 读到跨文件尾部时顺带刷新文件列表，轮转后下拉要跟上。
    if (state.file === null) {
      const files = await listInstanceLogs(id).catch(() => null);
      if (files) {
        state.files = files;
        const select = document.getElementById("log-file") as HTMLSelectElement | null;
        if (select && document.activeElement !== select) select.innerHTML = fileOptions();
      }
    }
  } catch (error) {
    state.error = describeError(error);
    if (!options.silent) toast(`读取日志失败：${state.error}`, "error");
  } finally {
    state.loading = false;
  }
}

function startTimer(): void {
  stopTimer();
  if (!state.follow) return;
  state.timer = window.setInterval(() => {
    if (document.getElementById("pbody")?.dataset.view !== "logs") {
      stopTimer();
      return;
    }
    void refresh({ silent: true }).then(() => {
      paintMeta();
      paintLines();
    });
  }, REFRESH_MS);
}

function stopTimer(): void {
  if (state.timer !== null) {
    window.clearInterval(state.timer);
    state.timer = null;
  }
}

async function copyVisible(): Promise<void> {
  const rows = visible();
  if (rows.length === 0) {
    toast("没有可复制的行", "info");
    return;
  }
  const text = rows.map((row) => row.raw).join("\n");
  try {
    await navigator.clipboard.writeText(text);
    toast(`已复制 ${rows.length} 行`, "ok");
  } catch (error) {
    toast(`复制失败：${describeError(error)}`, "error");
  }
}

/**
 * 打开子进程输出捕获。
 *
 * 只对之后拉起的实例生效——改管道要重启进程，而这会打断用户手里的模块，
 * 所以不自动重启，只把「下次启动生效」写清楚。
 */
async function toggleChildOutput(): Promise<void> {
  const next = !(state.data?.child_output ?? false);
  try {
    const enabled = await setChildOutput(next);
    if (state.data) state.data.child_output = enabled;
    await refreshSettings().catch(() => undefined);
    paintMeta();
    paintLines();
    toast(
      enabled ? "已开启子进程输出捕获，重启该实例后开始记录" : "已关闭子进程输出捕获",
      "ok",
    );
  } catch (error) {
    toast(`切换失败：${describeError(error)}`, "error");
  }
}

async function openDir(): Promise<void> {
  if (!state.instanceId) return;
  try {
    const dir = await openInstanceLogsDir(state.instanceId);
    toast(`已在文件管理器中打开：${dir}`, "ok");
  } catch (error) {
    toast(`打开目录失败：${describeError(error)}`, "error");
  }
}

/* ------------------------------------------------------------------ */
/* 上色                                                                */
/* ------------------------------------------------------------------ */

function paintMeta(): void {
  const node = document.getElementById("log-meta");
  if (!node) return;
  const view = store.views.find((v) => v.instance.instance_id === state.instanceId);
  const data = state.data;
  const shown = visible().length;
  const total = state.lines.length;

  const bits: string[] = [];
  if (view) {
    bits.push(`<span class="dot ${dotFor(view.snapshot?.phase)}"></span>${esc(phaseText(view.snapshot?.phase))}`);
    if (isExternal(view)) bits.push("外部启动");
    if (typeof view.port === "number") bits.push(`端口 ${view.port}`);
  }
  if (data) {
    bits.push(`${data.files} 份文件`);
    bits.push(state.file === null ? "跨文件尾部" : esc(state.file));
    bits.push(`显示 ${shown}/${total} 行`);
    if (data.truncated) bits.push('<span class="warn-text">已截断</span>');
  }
  if (state.query) bits.push(`匹配「${esc(state.query)}」`);
  if (state.loading) bits.push("读取中…");

  const child = data
    ? `<button class="chip chip--act ${data.child_output ? "chip--run" : "chip--stop"}" type="button" data-act="toggle-child" title="模块 stdout/stderr 里可能有你的对话内容，默认丢弃。开启后只影响之后拉起的实例。">子进程输出 ${data.child_output ? "已开" : "关 · 开启"}</button>`
    : "";

  node.innerHTML = `<span class="logmeta-line">${bits.join('<span class="sep">·</span>')}</span>
    <span class="sp"></span>${child}`;
}

function paintLines(): void {
  const box = document.getElementById("log-lines");
  const pane = document.getElementById("log-pane");
  if (!box) return;

  box.classList.toggle("is-wrap", state.wrap);
  const rows = visible();

  if (state.error) {
    box.innerHTML = `<div class="logempty">
      <b>读不到日志</b>
      <p>${esc(state.error)}</p>
      <p class="cap">目录：<code>${esc(state.data?.dir ?? "")}</code></p>
    </div>`;
    return;
  }
  if (rows.length === 0) {
    const hint =
      state.lines.length > 0
        ? `当前 ${state.lines.length} 行都不满足过滤条件。`
        : (state.data?.note ?? "该实例还没有日志：启动过一次才会写入。");
    const openChild = state.data && !state.data.child_output && state.lines.length === 0;
    box.innerHTML = `<div class="logempty">
      <b>没有可显示的行</b>
      <p>${esc(hint)}</p>
      <p class="cap">日志目录：<code>${esc(state.data?.dir ?? "")}</code></p>
      <div class="logempty-act">
        ${openChild ? '<button class="btn btn--sm" type="button" data-act="toggle-child">开启子进程输出</button>' : ""}
        <button class="btn btn--sm" type="button" data-act="open-dir">打开日志目录</button>
      </div>
    </div>`;
    return;
  }

  const html: string[] = [];
  for (const row of rows) {
    const hit = state.query ? mark(row.message, state.query) : esc(row.message);
    html.push(
      `<div class="logline logline--${row.level}">` +
        `<span class="ln">${row.index}</span>` +
        `<span class="lt">${esc(row.day ? `${row.day} ${row.time}` : row.time)}</span>` +
        `<span class="ls">${esc(row.scope)}</span>` +
        `<span class="lm" title="${esc(row.raw)}">${hit}</span>` +
        `</div>`,
    );
  }
  box.innerHTML = html.join("");
  if (pane && state.stickToBottom) pane.scrollTop = pane.scrollHeight;
}

function visible(): ParsedLine[] {
  return state.lines.filter((row) => matchesFilter(row, state.level, state.query));
}

/** 在已转义的文本上做高亮，避免把用户输入当 HTML。 */
function mark(text: string, query: string): string {
  const escaped = esc(text);
  const needle = esc(query);
  if (!needle) return escaped;
  const lower = escaped.toLowerCase();
  const target = needle.toLowerCase();
  let out = "";
  let from = 0;
  for (;;) {
    const at = lower.indexOf(target, from);
    if (at < 0) break;
    out += escaped.slice(from, at) + `<mark>${escaped.slice(at, at + needle.length)}</mark>`;
    from = at + needle.length;
  }
  return out + escaped.slice(from);
}

/* ------------------------------------------------------------------ */
/* 小工具                                                              */
/* ------------------------------------------------------------------ */

function dotFor(phase: string | undefined): string {
  if (phase === "running") return "dot--run";
  if (phase === "starting" || phase === "stopping") return "dot--warn";
  if (phase === "failed") return "dot--fail";
  return "dot--idle";
}

function phaseText(phase: string | undefined): string {
  switch (phase) {
    case "running":
      return "运行中";
    case "starting":
      return "启动中";
    case "stopping":
      return "停止中";
    case "failed":
      return "需处理";
    default:
      return "未运行";
  }
}
