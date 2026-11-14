/**
 * 概览页：一屏之内回答四个问题——现在什么状态、链路缺哪一层、
 * 各实例的进程与资源、环境与最近改了什么。
 *
 * 这里不做任何推算：实例与端口来自 `listInstanceViews`，环境来自
 * `probe_environment`，「最近活动」只按 `installed_at` 与本次环境自检时间
 * 排序。没有采样就没有数字——CPU 与内存两列留空并写明未接入，
 * 比填一个假装是实时的 0% 要好。
 */

import { isDesktop, type InstanceRuntimeView, type Role } from "../../bridge.js";
import { bindActions, clear, esc, toast } from "../dom.js";
import { icon } from "../icons.js";
import { DATA_PLANE, ROLE_ORDER, ROLE_TAG, ROLE_TITLE } from "../roles.js";
import {
  describeError,
  hasLiveProcess,
  installedModuleIds,
  isExternal,
  isRunning,
  moduleName,
  refreshEnv,
  refreshViews,
  runCounts,
  store,
} from "../store.js";
import { startEverything, stopEverything } from "../bulk.js";

export interface HomeSlots {
  head: HTMLElement;
  body: HTMLElement;
}

export function renderHome(slots: HomeSlots): void {
  renderHead(slots.head);
  renderBody(slots.body);
}

/** CSS 里的角色令牌后缀：auxiliary 的变量名短一截。 */
function roleKey(role: Role): string {
  return role === "auxiliary" ? "aux" : role;
}

/* ------------------------------------------------------------------ */
/* 页头                                                                */
/* ------------------------------------------------------------------ */

function renderHead(head: HTMLElement): void {
  const counts = runCounts();
  const modules = installedModuleIds().length;
  const bits = [`${modules} 个模块`, `${counts.total} 个实例`, `${counts.running} 运行中`];
  if (counts.failed > 0) bits.push(`${counts.failed} 需处理`);

  head.innerHTML = `
    <div>
      <h1>概览</h1>
      <div class="sub">${esc(bits.join(" · "))}</div>
    </div>
    <div class="sp">
      <button class="btn btn--sm" type="button" data-act="stop-all" ${counts.running === 0 ? "disabled" : ""}>停止全部</button>
      <button class="btn btn--primary btn--sm" type="button" data-act="start-all" ${
        counts.total === 0 ? "disabled" : ""
      } title="启动隐式默认链路，步骤结果见「链路」页">${icon("play", 11)}一键启动</button>
    </div>
  `;

  bindActions(head, [
    { act: "start-all", run: () => runStartAll() },
    { act: "stop-all", run: () => runStopAll() },
  ]);
}

/* ------------------------------------------------------------------ */
/* 正文                                                                */
/* ------------------------------------------------------------------ */

function renderBody(body: HTMLElement): void {
  const scrollTop = body.scrollTop;
  clear(body);

  if (!isDesktop()) {
    body.innerHTML = `
      <div class="placeholder">
        <span class="ph-ico">${icon("home", 18)}</span>
        <b>浏览器模式只能预览界面</b>
        <p>实例状态、启停与安装都需要桌面端。请用 <code>npm run tauri dev</code> 或安装包打开。</p>
      </div>
    `;
    return;
  }

  body.innerHTML = `${heroHtml()}${pipelineHtml()}${lowerHtml()}`;
  bindBodyActions(body);
  body.scrollTop = scrollTop;
}

/* ---------------------- hero ---------------------- */

function heroHtml(): string {
  const counts = runCounts();
  const facts = heroFacts();

  return `
    <section class="hero">
      <div>
        <div class="hero-kicker">系统状态</div>
        <h2>${esc(heroTitle(counts.total, counts.running, counts.failed))}</h2>
        <p>${esc(heroLine(counts.total))}</p>
        <div class="hero-actions">${heroActions(counts.total)}</div>
      </div>
      <div class="hero-facts">${facts}</div>
    </section>
  `;
}

function heroTitle(total: number, running: number, failed: number): string {
  if (total === 0) return "还没有装任何模块";
  if (failed > 0) return `${failed} 个实例需要处理`;
  if (running === total) return `全部 ${total} 个实例在运行`;
  if (running > 0) return `${running} 个在运行，${total - running} 个停着`;
  return `${total} 个实例都停着`;
}

function heroLine(total: number): string {
  if (total === 0) {
    return "到「下载」从已信任的源装签名包，或用「导入本地」装离线 .zip。装好后这里会画出整条链路。";
  }
  return "一键启动按默认链路把输入、模型、输出与总装一次提交，不等待前一步就绪；能否起来看各节点的状态点。";
}

function heroActions(total: number): string {
  if (total === 0) {
    return `
      <button class="btn btn--primary" type="button" data-act="goto-download">打开下载中心</button>
      <button class="btn" type="button" data-act="goto-import">导入本地包</button>
    `;
  }
  const counts = runCounts();
  return `
    <button class="btn btn--primary" type="button" data-act="start-all">一键启动全部 ${counts.total} 个</button>
    <button class="btn" type="button" data-act="goto-chains">查看链路</button>
  `;
}

function heroFacts(): string {
  const counts = runCounts();
  const env = store.env;
  const modules = installedModuleIds().length;

  const problems = env ? env.problems.length : null;
  const disk = env ? (env.disk.ok ? `${env.disk.free_gb.toFixed(1)} GB` : "读取失败") : "—";

  return `
    <div class="fact"><b>${counts.running}/${counts.total}</b><span>实例运行中</span></div>
    <div class="fact"><b class="${problems && problems > 0 ? "warn" : ""}">${
      problems === null ? "—" : String(problems)
    }</b><span>环境待处理</span></div>
    <div class="fact"><b>${esc(disk)}</b><span>数据盘剩余</span></div>
    <div class="fact"><b>${modules}</b><span>个模块</span></div>
  `;
}

/* ---------------------- 链路 ---------------------- */

function pipelineHtml(): string {
  const nodes: string[] = [];
  for (const role of ROLE_ORDER) {
    const views = store.views.filter((view) => view.instance.role === role);
    if (views.length === 0 && !DATA_PLANE.includes(role)) continue;
    if (nodes.length > 0) nodes.push(`<div class="link" aria-hidden="true">${icon("chevron", 14)}</div>`);
    nodes.push(views.length === 0 ? missNodeHtml(role) : nodeHtml(role, views));
  }

  return `
    <section class="sec">
      <div class="sec-head">
        <h3>链路</h3>
        <span class="hint">按数据流排列 · 点节点进模块页</span>
        <span class="line"></span>
      </div>
      <div class="pipe">${nodes.join("")}</div>
    </section>
  `;
}

function nodeHtml(role: Role, views: InstanceRuntimeView[]): string {
  const first = views[0];
  if (!first) return missNodeHtml(role);
  const counter = views.find((view) => isRunning(view)) ?? views.find((view) => view.active) ?? first;
  const running = views.some((view) => isRunning(view));
  const failed = !running && views.some((view) => view.snapshot?.phase === "failed");
  const phase = running ? "node--run" : failed ? "node--fail" : "node--off";

  const chip = running
    ? '<span class="chip chip--run"><i class="dot dot--run"></i>运行中</span>'
    : failed
      ? '<span class="chip chip--fail">已退出</span>'
      : '<span class="chip chip--stop">已停止</span>';

  const port = counter.port === null ? "无端口" : `:${counter.port}`;
  const versions = new Set(views.map((view) => view.instance.version));
  const version = versions.size === 1 ? `v${[...versions][0]}` : `${views.length} 个版本`;
  const more = views.length > 1 ? ` · ${views.length} 个实例` : "";

  return `
    <button class="node ${phase}" type="button" data-act="goto-modules" title="到模块页查看 ${esc(
      moduleName(counter.instance.module_id),
    )}">
      <span class="node-top">
        <span class="role" data-role="${esc(role)}"><i></i>${esc(ROLE_TAG[role])}</span>
        ${chip}
      </span>
      <span class="node-name">${esc(moduleName(counter.instance.module_id))}</span>
      <span class="node-meta">${esc(port)} · ${esc(version)}${esc(more)}</span>
    </button>
  `;
}

function missNodeHtml(role: Role): string {
  return `
    <button class="node node--miss" type="button" data-act="goto-download" title="到下载中心找 ${esc(
      ROLE_TITLE[role],
    )}层的模块">
      <span class="node-top">
        <span class="role" data-role="${esc(role)}"><i></i>${esc(ROLE_TAG[role])}</span>
        <span class="chip chip--warn">未安装</span>
      </span>
      <span class="node-name">缺${esc(ROLE_TITLE[role])}层</span>
      <span class="node-meta">这一层没有可启动的实例</span>
    </button>
  `;
}

/* ---------------------- 资源 + 环境 / 活动 ---------------------- */

function lowerHtml(): string {
  return `
    <div class="lower">
      <section class="sec sec--res">
        <div class="sec-head">
          <h3>资源占用</h3>
          <span class="hint">进程采样 · 按实例 PID</span>
          <span class="line"></span>
        </div>
        ${resTableHtml()}
      </section>
      <div class="side">
        ${envTileHtml()}
        ${feedTileHtml()}
      </div>
    </div>
  `;
}

function resTableHtml(): string {
  const rows = orderedViews();
  if (rows.length === 0) {
    return `
      <div class="res">
        <div class="res-head">
          <span>实例</span><span>状态</span><span>CPU</span><span title="私有工作集">内存</span><span class="th-r">进程</span>
        </div>
        <div class="res-empty">还没有实例。装好模块后这里按实例列出进程与占用。</div>
        <div class="res-foot"><span>采样未接入，CPU 与内存列已预留</span></div>
      </div>
    `;
  }

  const body = rows
    .map((view) => {
      const live = hasLiveProcess(view);
      const pid = view.snapshot?.pid;
      return `
        <div class="res-row ${isRunning(view) ? "" : "is-idle"}" style="--role: var(--role-${roleKey(
          view.instance.role,
        )})">
          <span class="res-name">
            <span class="dot"></span>
            <b>${esc(moduleName(view.instance.module_id))}</b>
            ${resStateChip(view)}
          </span>
          <span class="res-na" title="CPU 采样尚未接入">—</span>
          <span class="res-na" title="内存采样尚未接入">—</span>
          <span class="res-pid">${live && typeof pid === "number" ? `pid ${pid}` : "—"}</span>
        </div>
      `;
    })
    .join("");

  const withPid = rows.filter((view) => typeof view.snapshot?.pid === "number").length;

  return `
    <div class="res">
      <div class="res-head">
        <span>实例</span><span>状态</span><span>CPU</span><span title="私有工作集">内存</span><span class="th-r">进程</span>
      </div>
      ${body}
      <div class="res-foot">
        <span>采样未接入，CPU 与内存列已预留</span>
        <span class="sp"></span>
        <span>${rows.length} 实例 · ${withPid} 有进程</span>
      </div>
    </div>
  `;
}

function resStateChip(view: InstanceRuntimeView): string {
  const phase = view.snapshot?.phase ?? "stopped";
  if (phase === "running") {
    return isExternal(view)
      ? '<span class="chip chip--warn">外部</span>'
      : '<span class="chip chip--run">运行中</span>';
  }
  if (phase === "starting") return '<span class="chip chip--warn">启动中</span>';
  if (phase === "failed") return '<span class="chip chip--fail">已退出</span>';
  return '<span class="chip chip--stop">待机</span>';
}

function envTileHtml(): string {
  const env = store.env;
  const rows: string[] = [];

  if (env) {
    const pyOk = env.python.available && env.uv.available;
    const missing = [!env.python.available ? "Python" : "", !env.uv.available ? "uv" : ""].filter(Boolean);
    rows.push(
      `<div class="kv"><span>Python / uv</span><b class="${pyOk ? "ok" : "fail"}">${
        pyOk ? "已就绪" : `缺 ${esc(missing.join(" / "))}`
      }</b></div>`,
    );
    rows.push(
      `<div class="kv"><span>Node</span><b class="${env.node.available ? "ok" : ""}">${
        env.node.available ? esc(env.node.version || "已就绪") : "未装（预览界面才需要）"
      }</b></div>`,
    );
    rows.push(
      `<div class="kv"><span>数据盘</span><b class="${env.disk.ok ? "" : "fail"}">${
        env.disk.ok ? `剩余 ${env.disk.free_gb.toFixed(1)} GB` : "读取失败"
      }</b></div>`,
    );
    if (env.memory) {
      rows.push(
        `<div class="kv"><span>内存</span><b>可用 ${env.memory.free_gb.toFixed(1)} / ${env.memory.total_gb.toFixed(
          1,
        )} GB</b></div>`,
      );
    }
    const gpu = env.gpus[0];
    if (gpu) {
      rows.push(`<div class="kv"><span>GPU</span><b>${esc(gpu.name)} · ${gpu.vram_gb.toFixed(1)} GB</b></div>`);
    }
    if (env.problems.length > 0) {
      rows.push(
        `<div class="kv"><span>待处理</span><b class="warn" title="${esc(env.problems.join("；"))}">${
          env.problems.length
        } 项</b></div>`,
      );
    }
  } else {
    rows.push('<div class="kv"><span>环境</span><b>尚未检测</b></div>');
  }

  return `
    <div class="tile tile--env">
      <div class="tile-head">
        <h4>环境与目录</h4>
        <span class="sp"></span>
        <button class="btn btn--quiet btn--sm" type="button" data-act="recheck-env">重新检测</button>
      </div>
      ${rows.join("")}
    </div>
  `;
}

/* ---------------------- 最近活动 ---------------------- */

interface FeedItem {
  at: number;
  html: string;
}

/**
 * 最近活动只有两个真实来源：实例的安装时间与本次环境自检时间。
 * 启动器没有持久的事件流，这里不编造「几点启动了谁」。
 */
function feedItems(): FeedItem[] {
  const out: FeedItem[] = [];
  for (const view of store.views) {
    const at = Date.parse(view.instance.installed_at);
    if (Number.isNaN(at)) continue;
    out.push({
      at,
      html: `安装 <b>${esc(moduleName(view.instance.module_id))} ${esc(view.instance.version)}</b>`,
    });
  }
  if (store.env && store.envCheckedAt) {
    const env = store.env;
    const tail = env.problems.length > 0 ? `，${env.problems.length} 项待处理` : "，Python / uv 就绪";
    out.push({ at: store.envCheckedAt.getTime(), html: `环境自检${esc(tail)}` });
  }
  return out.sort((a, b) => b.at - a.at).slice(0, 6);
}

function feedTileHtml(): string {
  const items = feedItems();
  return `
    <div class="tile tile--feed">
      <div class="tile-head">
        <h4>最近活动</h4>
        <span class="sp"></span>
        <button class="btn btn--quiet btn--sm" type="button" data-act="goto-logs">全部日志</button>
      </div>
      ${
        items.length === 0
          ? '<div class="empty" style="padding:14px 0">还没有安装记录</div>'
          : `<div class="feed">${items
              .map(
                (item) =>
                  `<div class="feed-item"><time>${esc(clock(item.at))}</time><span class="txt">${item.html}</span></div>`,
              )
              .join("")}</div>`
      }
    </div>
  `;
}

/** 今天的只显示时分，更早的带一个月日。 */
function clock(at: number): string {
  const date = new Date(at);
  const now = new Date();
  const sameDay =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();
  if (sameDay) {
    return date.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  }
  return `${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}

/* ------------------------------------------------------------------ */
/* 排序与事件                                                          */
/* ------------------------------------------------------------------ */

/** 资源表按角色分层，同层按 module_id：与模块页的阅读顺序一致。 */
function orderedViews(): InstanceRuntimeView[] {
  const rank = new Map(ROLE_ORDER.map((role, index) => [role, index]));
  return [...store.views].sort((a, b) => {
    const ra = rank.get(a.instance.role) ?? ROLE_ORDER.length;
    const rb = rank.get(b.instance.role) ?? ROLE_ORDER.length;
    if (ra !== rb) return ra - rb;
    return a.instance.module_id.localeCompare(b.instance.module_id);
  });
}

function bindBodyActions(body: HTMLElement): void {
  bindActions(body, [
    { act: "start-all", run: () => runStartAll() },
    { act: "stop-all", run: () => runStopAll() },
    { act: "recheck-env", run: () => recheckEnv() },
    { act: "goto-modules", run: () => navigate("modules") },
    { act: "goto-download", run: () => navigate("download") },
    { act: "goto-import", run: () => navigate("import") },
    { act: "goto-chains", run: () => navigate("chains") },
    { act: "goto-logs", run: () => navigate("logs") },
  ]);
}

function navigate(view: string): void {
  window.dispatchEvent(new CustomEvent("dounai:navigate", { detail: view }));
}

async function runStartAll(): Promise<void> {
  await startEverything();
  await refreshHome();
}

async function runStopAll(): Promise<void> {
  await stopEverything();
  await refreshHome();
}

async function recheckEnv(): Promise<void> {
  toast("正在重新检测环境…");
  try {
    await refreshEnv();
    toast("环境检测完成", "ok");
  } catch (error) {
    toast(`环境检测失败：${describeError(error)}`, "error");
  }
  await refreshHome();
}

/** 重新拉实例表并重绘（生命周期事件、启停完成后都走这里）。 */
export async function refreshHome(): Promise<void> {
  try {
    await refreshViews();
  } catch (error) {
    toast(`读取实例状态失败：${describeError(error)}。稍后重试，或重启启动器。`, "error");
    return;
  }
  const body = document.getElementById("pbody");
  const head = document.getElementById("phead");
  if (body && head && body.dataset.view === "home") {
    renderHead(head);
    renderBody(body);
  }
  window.dispatchEvent(new CustomEvent("dounai:views-updated"));
}
