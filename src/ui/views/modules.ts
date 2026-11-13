/**
 * 模块页：按 role 分组的卡片，实例是卡片里的行。
 *
 * 为什么卡片承载模块、而不是「一行一个实例」：M2 之后同一模块可以有多个实例
 * （stable / local / 第三方变体），并排放在一起才看得出「它们抢的是同一个槽」。
 */

import {
  isDesktop,
  installFromCatalog,
  openAdminWindow,
  refreshCatalog,
  removeLocalModule,
  restartInstance,
  setModuleEnabled,
  startAll,
  startModule,
  stopAll,
  stopModule,
  switchActive,
  uninstallModule,
  confirmLocalSource,
  prepareInstanceEnv,
  type CatalogEntry,
  type CatalogResponse,
  type InstanceRuntimeView,
  type Role,
} from "../../bridge.js";
import {
  bindActions,
  clear,
  closeModal,
  confirmModal,
  esc,
  openModal,
  toast,
} from "../dom.js";
import { icon } from "../icons.js";
import {
  beginEnvJob,
  describeError,
  endEnvJob,
  envJobOf,
  hasLiveProcess,
  isExternal,
  isRunning,
  moduleName,
  refreshViews,
  runCounts,
  store,
  type EnvJob,
} from "../store.js";
import { ROLE_DESC, ROLE_ORDER, ROLE_SHORT, ROLE_TITLE } from "../roles.js";


/** 渲染上下文：页头与正文容器由外壳传入。 */
export interface ViewSlots {
  head: HTMLElement;
  body: HTMLElement;
}

/** 模块页的筛选条件（重绘时保留）。 */
const filter = {
  query: "",
  onlyProblems: false,
};

export function renderModules(slots: ViewSlots): void {
  renderHead(slots.head);
  renderBody(slots.body);
}

/* ------------------------------------------------------------------ */
/* 页头                                                                */
/* ------------------------------------------------------------------ */

function renderHead(head: HTMLElement): void {
  const counts = runCounts();
  const problems = counts.failed;
  const bits = [`${counts.running} / ${counts.total} 运行中`];
  if (counts.starting > 0) bits.push(`${counts.starting} 启动中`);
  if (problems > 0) bits.push(`${problems} 需处理`);
  if (counts.external > 0) bits.push(`${counts.external} 外部启动`);

  head.innerHTML = `
    <div>
      <h1>模块</h1>
      <div class="sub">${esc(bits.join(" · "))}</div>
    </div>
    <div class="sp">
      <button class="btn btn--sm" type="button" data-act="catalog">模块目录</button>
      <button class="btn btn--sm" type="button" data-act="stop-all">停止全部</button>
      <button class="btn btn--primary btn--sm" type="button" data-act="start-all">一键启动</button>
    </div>
  `;

  bindActions(head, [
    { act: "catalog", run: () => showCatalog() },
    { act: "start-all", run: () => startEverything() },
    { act: "stop-all", run: () => stopEverything() },
  ]);
}

/* ------------------------------------------------------------------ */
/* 正文                                                                */
/* ------------------------------------------------------------------ */

function renderBody(body: HTMLElement): void {
  const scrollTop = body.scrollTop;
  clear(body);
  body.classList.remove("pbody--flush");

  if (!isDesktop()) {
    body.innerHTML = `
      <div class="placeholder">
        <span class="ph-ico">${icon("modules", 18)}</span>
        <b>浏览器模式只能预览界面</b>
        <p>实例状态、启停与安装都需要桌面端。请用 <code>npm run tauri dev</code> 或安装包打开。</p>
      </div>
    `;
    return;
  }

  body.appendChild(buildToolbar());

  const visible = filteredViews();
  const grouped = new Map<Role, Map<string, InstanceRuntimeView[]>>();
  for (const view of visible) {
    const byModule = grouped.get(view.instance.role) ?? new Map<string, InstanceRuntimeView[]>();
    const list = byModule.get(view.instance.module_id) ?? [];
    list.push(view);
    byModule.set(view.instance.module_id, list);
    grouped.set(view.instance.role, byModule);
  }

  if (grouped.size === 0) {
    body.insertAdjacentHTML(
      "beforeend",
      `<div class="placeholder">
        <span class="ph-ico">${icon("modules", 18)}</span>
        <b>${store.views.length === 0 ? "还没有登记任何模块" : "没有匹配的实例"}</b>
        <p>${
          store.views.length === 0
            ? "从「模块目录」在线装一个签名包，或到「导入本地」装一个 .zip 离线包。登记源码目录不会复制也不会删除你的文件。"
            : "清空搜索词或取消「只看有问题的」再试。"
        }</p>
        ${
          store.views.length === 0
            ? `<div class="sp" style="gap:8px">
                 <button class="btn btn--primary btn--sm" type="button" data-act="catalog">打开模块目录</button>
                 <button class="btn btn--sm" type="button" data-act="goto-import">导入本地包</button>
               </div>`
            : ""
        }
      </div>`,
    );
  }

  for (const role of ROLE_ORDER) {
    const byModule = grouped.get(role);
    if (!byModule || byModule.size === 0) continue;

    const views = [...byModule.values()].flat();
    const running = views.filter((view) => isRunning(view)).length;
    const activeInstance = views.find((view) => view.active);
    const statParts = [`${running}/${views.length} 运行`];
    if (activeInstance) statParts.push(`active = ${activeInstance.instance.module_id}`);

    const group = document.createElement("section");
    group.innerHTML = `
      <div class="grp">
        <span class="grp-name">${esc(role)} · ${esc(ROLE_TITLE[role])}</span>
        <span class="grp-line"></span>
        <span class="grp-stat">${esc(statParts.join(" · "))}</span>
      </div>
      <div class="cards"></div>
    `;
    const cards = group.querySelector(".cards");
    if (cards) {
      for (const [moduleId, list] of byModule) {
        cards.appendChild(buildCard(moduleId, list));
      }
    }
    body.appendChild(group);
  }

  bindBodyActions(body);
  body.scrollTop = scrollTop;
}

function buildToolbar(): HTMLElement {
  const bar = document.createElement("div");
  bar.className = "toolbar";
  bar.innerHTML = `
    <input id="module-search" class="input grow" type="search" placeholder="搜索模块或实例…" value="${esc(filter.query)}" />
    <label class="check">
      <input id="module-problems" type="checkbox" ${filter.onlyProblems ? "checked" : ""} />
      只看有问题的
    </label>
  `;
  bar.querySelector<HTMLInputElement>("#module-search")?.addEventListener("input", (event) => {
    filter.query = (event.target as HTMLInputElement).value.trim().toLowerCase();
    const body = document.getElementById("pbody");
    if (body) renderBody(body);
  });
  bar
    .querySelector<HTMLInputElement>("#module-problems")
    ?.addEventListener("change", (event) => {
      filter.onlyProblems = (event.target as HTMLInputElement).checked;
      const body = document.getElementById("pbody");
      if (body) renderBody(body);
    });
  return bar;
}

function filteredViews(): InstanceRuntimeView[] {
  const q = filter.query;
  return store.views.filter((view) => {
    if (q) {
      const haystack =
        `${view.instance.module_id} ${view.instance.instance_id} ${moduleName(view.instance.module_id)}`.toLowerCase();
      if (!haystack.includes(q)) return false;
    }
    if (filter.onlyProblems) {
      const phase = view.snapshot?.phase ?? "stopped";
      const unverified = view.instance.source_kind === "dir" || view.instance.verdict !== "A";
      if (phase !== "failed" && !unverified) return false;
    }
    return true;
  });
}

/* ------------------------------------------------------------------ */
/* 卡片                                                                */
/* ------------------------------------------------------------------ */

function buildCard(moduleId: string, views: InstanceRuntimeView[]): HTMLElement {
  const card = document.createElement("article");
  card.className = "card";

  const hasDir = views.some((view) => view.instance.source_kind === "dir");
  const unverified = hasDir || views.some((view) => view.instance.verdict !== "A");
  if (unverified) card.classList.add("card--warn");

  const role = views[0]?.instance.role ?? "auxiliary";
  const counter = views.find((view) => view.active) ?? views[0];

  const statusChip = moduleStatusChip(views);
  const meta = [
    counter?.port ? `端口 <b>${counter.port}</b>` : "无规范端口",
    `v${esc(counter?.instance.version ?? "—")}`,
    esc(views[0]?.instance.source_kind ?? "zip"),
  ];
  if (hasDir) meta.push('<span class="warn">本地源码 · 未验证</span>');
  else if (unverified) meta.push('<span class="warn">未验证</span>');

  const desc = viewDescription(views[0], role);

  card.innerHTML = `
    <div class="card-top">
      <span class="role-ico" aria-hidden="true">${esc(ROLE_SHORT[role])}</span>
      <div style="min-width:0">
        <span class="card-name" title="${esc(moduleName(moduleId))}">${esc(moduleName(moduleId))}</span>
        <span class="card-id" title="${esc(moduleId)}">${esc(moduleId)}</span>
      </div>
      <span style="margin-left:auto">${statusChip}</span>
    </div>
    <p class="card-desc">${esc(desc)}</p>
    <div class="meta">${meta.map((m) => `<span>${m}</span>`).join("")}</div>
    <div class="inst-list"></div>
    <div class="card-foot"></div>
  `;

  const list = card.querySelector(".inst-list");
  if (list) {
    for (const view of views) list.appendChild(buildInstanceRow(view));
  }

  const foot = card.querySelector(".card-foot");
  if (foot) foot.innerHTML = cardFootHtml(moduleId, views);

  return card;
}

/** 卡片级状态：优先级 running > starting > failed > stopped。 */
function moduleStatusChip(views: InstanceRuntimeView[]): string {
  const phases = views.map((view) => view.snapshot?.phase ?? "stopped");
  const external = views.some((view) => isExternal(view));
  const running = phases.filter((p) => p === "running").length;
  if (running > 0) {
    const extra = external ? " · 含外部" : "";
    return `<span class="chip chip--run"><i class="dot dot--run"></i>运行中${extra}</span>`;
  }
  if (phases.includes("starting")) {
    return '<span class="chip chip--warn"><i class="dot dot--warn"></i>启动中</span>';
  }
  const failedView = views.find((view) => view.snapshot?.phase === "failed");
  if (failedView) {
    // 进程还在，只是没在启动预算内就绪：不是崩溃，别写成「启动失败」。
    if (hasLiveProcess(failedView)) {
      return '<span class="chip chip--warn"><i class="dot dot--warn"></i>启动超时</span>';
    }
    const code = failedView.snapshot?.exit_code;
    const label = code === null || code === undefined ? "启动失败" : `已退出 ${code}`;
    return `<span class="chip chip--fail"><i class="dot dot--fail"></i>${esc(label)}</span>`;
  }
  return '<span class="chip chip--stop">○ 已停止</span>';
}

function viewDescription(view: InstanceRuntimeView | undefined, role: Role): string {
  if (!view) return ROLE_DESC[role];
  if (view.instance.note) return view.instance.note;
  return ROLE_DESC[role];
}

function buildInstanceRow(view: InstanceRuntimeView): HTMLElement {
  const row = document.createElement("div");
  const external = isExternal(view);
  row.className = `inst${external ? " inst--warn" : ""}`;

  const dotClass = phaseDot(view);
  const chips: string[] = [];
  if (view.active) chips.push('<span class="chip chip--active">active</span>');
  if (external) chips.push('<span class="chip chip--warn">外部启动</span>');
  else if (view.instance.source_kind === "dir") chips.push('<span class="chip chip--warn">未验证</span>');
  if (view.disabled) chips.push('<span class="chip chip--stop">已停用</span>');
  if (!view.prepared && !isRunning(view)) chips.push('<span class="chip chip--warn">依赖未装</span>');
  // 包内没声明 snapshot_sha256 时，这份快照是运行时算出来的，不是发布者的承诺。
  // 不是错误，但要让人知道「换成重装能恢复锚定」这条线索存在。
  if (view.prepared && view.env_pinned === false && view.instance.source_kind === "zip") {
    chips.push(
      '<span class="chip chip--warn" title="这个包的 manifest 没有声明依赖快照哈希，本次哈希由本地解析得出">快照未锚定</span>',
    );
  }

  const health = healthChip(view);
  const detail = instanceDetailText(view);

  row.innerHTML = `
    <div class="inst-row">
      <span class="dot ${dotClass}"></span>
      <span class="inst-name" title="${esc(view.instance.instance_id)}">${esc(view.instance.instance_id)}</span>
      ${chips.join("")}
      <span class="inst-meta">${esc(detail)}</span>
    </div>
    <div class="inst-row">
      ${health}
      <span class="sp" style="margin-left:auto"></span>
      ${instanceButtons(view)}
    </div>
    <div class="env-slot" data-env-job="${esc(view.instance.instance_id)}"></div>
  `;
  paintEnvJob(row, view.instance.instance_id);
  return row;
}

/* ------------------------------------------------------------------ */
/* 依赖安装进度                                                        */
/* ------------------------------------------------------------------ */

/** 引擎阶段的序号与总数，只用于把「同步依赖」这类文案变成 4/6。 */
const PHASE_STEP: Record<string, number> = {
  uv: 1,
  python: 2,
  resolve: 3,
  sync: 4,
  verify: 5,
  done: 6,
};
const PHASE_TOTAL = 6;

/**
 * 一行进度。失败后保留原因，成功后整行消失。
 *
 * 为什么不做百分比进度条：uv 不提供「已装 N 个」的稳定接口，按行猜出来的数字
 * 会是假的。这里只按阶段推进，细节交给后端报上来的包名。
 */
function envJobHtml(job: EnvJob): string {
  if (job.error) {
    return `
      <div class="env-job env-job--fail" title="${esc(job.error)}">
        <span class="env-job-title">安装依赖失败</span>
        <span class="env-job-detail">${esc(job.error)}</span>
      </div>`;
  }
  const step = job.phase === "starting" ? 0 : PHASE_STEP[job.phase] ?? 0;
  const elapsed = Math.max(0, Math.round((Date.now() - job.startedAt) / 1000));
  const counter = step > 0 ? `${step}/${PHASE_TOTAL} ` : "";
  const detail = job.detail
    ? `<span class="env-job-detail" title="${esc(job.detail)}">${esc(job.detail)}</span>`
    : "";
  const width = Math.round((step / PHASE_TOTAL) * 100);
  return `
    <div class="env-job">
      <span class="env-job-spin" aria-hidden="true"></span>
      <span class="env-job-title">安装依赖 · ${counter}${esc(job.title)}</span>
      <span class="env-job-time">${elapsed}s</span>
      ${detail}
      <span class="env-job-bar" aria-hidden="true"><i style="width:${width}%"></i></span>
    </div>`;
}

function paintEnvJob(root: ParentNode, instanceId: string): void {
  const slot = Array.from(root.querySelectorAll<HTMLElement>("[data-env-job]")).find(
    (node) => node.dataset.envJob === instanceId,
  );
  if (!slot) return;
  const job = envJobOf(instanceId);
  slot.innerHTML = job ? envJobHtml(job) : "";
}

/** 后端推来一条进度：只改这一行，不重绘整页。 */
export function patchEnvJob(instanceId: string): void {
  const slots = Array.from(document.querySelectorAll<HTMLElement>("[data-env-job]")).filter(
    (node) => node.dataset.envJob === instanceId,
  );
  if (slots.length === 0) return;
  const job = envJobOf(instanceId);
  const html = job ? envJobHtml(job) : "";
  for (const node of slots) node.innerHTML = html;
}

function phaseDot(view: InstanceRuntimeView): string {
  const phase = view.snapshot?.phase ?? "stopped";
  if (phase === "running") return "dot--run";
  if (phase === "starting" || phase === "stopping") return "dot--warn";
  if (phase === "failed") return "dot--fail";
  return "dot--idle";
}

function healthChip(view: InstanceRuntimeView): string {
  const health = view.snapshot?.health ?? "not_checked";
  if (health === "healthy") return '<span class="chip chip--run">health ok</span>';
  if (health === "unhealthy") return '<span class="chip chip--fail">health 失败</span>';
  if (health === "pending") return '<span class="chip chip--warn">health 待定</span>';
  return '<span class="chip chip--stop">未探测</span>';
}

function instanceDetailText(view: InstanceRuntimeView): string {
  const parts: string[] = [];
  if (view.port !== null) parts.push(`:${view.port}`);
  if (view.snapshot?.pid) parts.push(`pid ${view.snapshot.pid}`);
  if (view.instance.verdict !== "A") parts.push(`判级 ${view.instance.verdict}`);
  return parts.join(" · ");
}

function instanceButtons(view: InstanceRuntimeView): string {
  const id = esc(view.instance.instance_id);
  const running = isRunning(view) || hasLiveProcess(view);
  if (isExternal(view)) {
    return `<button class="btn btn--sm" type="button" data-act="detail" data-id="${id}">详情</button>`;
  }
  if (running) {
    return `<button class="btn btn--sm" type="button" data-act="stop" data-id="${id}">停止</button>`;
  }
  const disabledAttr = view.disabled ? " disabled title=\"模块已停用，先启用\"" : "";
  // 依赖没装就启动只会失败，还容易让人以为模块坏了。先给装依赖这一步。
  if (!view.prepared) {
    return `<button class="btn btn--sm btn--primary" type="button" data-act="prepare" data-id="${id}"${disabledAttr} title="这个模块要先装依赖才能启动">安装依赖</button>`;
  }
  return `<button class="btn btn--sm btn--primary" type="button" data-act="start" data-id="${id}"${disabledAttr}>启动</button>`;
}

/** 卡片底栏：最常用的一个动作 + 更多菜单。 */
function cardFootHtml(moduleId: string, views: InstanceRuntimeView[]): string {
  const id = esc(moduleId);
  const primary = views.find((view) => !isExternal(view)) ?? views[0];
  const canManage = views.some((view) => isRunning(view));
  const adminUi = primary?.admin_ui ?? null;
  // 声明了 requires_running 的界面，实例没跑起来时不给按钮——
  // 点了只会打开一个连不上的页面。
  const adminReady = adminUi !== null && (!adminUi.requires_running || canManage);
  const dirInstance = views.find((view) => view.instance.source_kind === "dir");
  const needsConfirm = dirInstance && !dirInstance.instance.head_confirmed;
  const disabled = views[0]?.disabled ?? false;

  const parts: string[] = [];
  if (needsConfirm && dirInstance) {
    parts.push(
      `<button class="btn btn--sm btn--primary" type="button" data-act="confirm-source" data-id="${esc(dirInstance.instance.instance_id)}">确认变更并启动</button>`,
    );
  } else if (adminReady && adminUi) {
    parts.push(`<button class="btn btn--sm" type="button" data-act="admin" data-id="${id}">${esc(adminUi.label)}</button>`);
  } else if (primary) {
    parts.push(
      `<button class="btn btn--sm" type="button" data-act="detail" data-id="${esc(primary.instance.instance_id)}">详情</button>`,
    );
  }

  const menuItems: string[] = [];
  if (primary) {
    menuItems.push(`<button type="button" data-act="detail" data-id="${esc(primary.instance.instance_id)}">实例详情</button>`);
    menuItems.push(`<button type="button" data-act="logs" data-id="${esc(primary.instance.instance_id)}">查看日志</button>`);
    menuItems.push(`<button type="button" data-act="restart" data-id="${esc(primary.instance.instance_id)}">重启</button>`);
    if (primary.instance.role !== "auxiliary" && !primary.active) {
      menuItems.push(`<button type="button" data-act="switch-active" data-id="${esc(primary.instance.instance_id)}">设为 active</button>`);
    }
    if (primary.instance.role !== "shell") {
      menuItems.push(
        `<button type="button" data-act="prepare" data-id="${esc(primary.instance.instance_id)}">安装依赖</button>`,
      );
      menuItems.push(
        `<button type="button" data-act="prepare-offline" data-id="${esc(primary.instance.instance_id)}">安装依赖（离线）</button>`,
      );
    }
  }
  menuItems.push(
    `<button type="button" data-act="toggle" data-id="${id}">${disabled ? "启用模块" : "停用模块"}</button>`,
  );
  if (primary?.instance.source_kind === "dir") {
    menuItems.push(`<button type="button" data-act="unlink" data-id="${id}">解除登记</button>`);
  } else if (primary) {
    menuItems.push(
      `<button class="btn danger" type="button" data-act="uninstall" data-id="${esc(primary.instance.instance_id)}" style="border:0;background:transparent;text-align:left;padding:6px 8px;border-radius:4px;font:400 11.5px var(--sans)">卸载…</button>`,
    );
  }

  parts.push(`
    <details class="menu">
      <summary class="btn btn--quiet btn--sm" aria-label="更多动作">⋯</summary>
      <div class="menu-body">${menuItems.join("")}</div>
    </details>
  `);

  return parts.join("");
}

function bindBodyActions(body: HTMLElement): void {
  bindActions(body, [
    { act: "start", run: (node) => startOne(node.dataset.id ?? "") },
    { act: "stop", run: (node) => stopOne(node.dataset.id ?? "") },
    { act: "restart", run: (node) => restartOne(node.dataset.id ?? "") },
    { act: "detail", run: (node) => showInstanceDetail(node.dataset.id ?? "") },
    { act: "logs", run: (node) => openLogsFor(node.dataset.id ?? "") },
    { act: "switch-active", run: (node) => makeActive(node.dataset.id ?? "") },
    { act: "confirm-source", run: (node) => confirmSource(node.dataset.id ?? "") },
    { act: "prepare", run: (node) => prepareEnv(node.dataset.id ?? "", true) },
    { act: "prepare-offline", run: (node) => prepareEnv(node.dataset.id ?? "", false) },
    { act: "admin", run: (node) => openAdmin(node.dataset.id ?? "") },
    { act: "toggle", run: (node) => toggleModule(node.dataset.id ?? "") },
    { act: "uninstall", run: (node) => askUninstall(node.dataset.id ?? "") },
    { act: "unlink", run: (node) => askUnlink(node.dataset.id ?? "") },
    { act: "catalog", run: () => showCatalog() },
    {
      act: "goto-import",
      run: () => {
        window.dispatchEvent(new CustomEvent("dounai:navigate", { detail: "import" }));
      },
    },
  ]);

  // 点开一个菜单时收起其它菜单：避免多个浮层叠在一起
  body.addEventListener("click", (event) => {
    const target = event.target as HTMLElement | null;
    if (target?.closest(".menu")) return;
    body.querySelectorAll<HTMLDetailsElement>(".menu[open]").forEach((menu) => {
      menu.open = false;
    });
  });
}

/**
 * 同一实例的操作正在进行时，忽略重复点击。
 *
 * 委托监听已经幂等，但按钮在异步返回前仍然可点：双击、或用户看到没反应
 * 再点一次，都会发出第二个请求，后端只会回「实例已在启动中」。
 */
const inFlight = new Set<string>();
/** 一键启停共用一个闸门：这类操作按拓扑串行，重复触发没有意义。 */
let bulkBusy = false;

async function runBulk(label: string, run: () => Promise<void>): Promise<void> {
  if (bulkBusy) {
    toast(`${label} 正在执行，请稍候…`);
    return;
  }
  bulkBusy = true;
  try {
    await run();
  } finally {
    bulkBusy = false;
  }
}

async function startOne(instanceId: string): Promise<void> {
  if (!instanceId) return;
  if (inFlight.has(`start:${instanceId}`)) {
    toast(`${instanceId} 正在启动中，请稍候…`);
    return;
  }
  inFlight.add(`start:${instanceId}`);
  try {
    const view = store.views.find((item) => item.instance.instance_id === instanceId);
    // 依赖没装就点启动：先把这一步补上，而不是让后端报「环境未就绪」。
    if (view && !view.prepared) {
      toast(`${instanceId} 还没装依赖，先装依赖…`);
      const ready = await prepareEnv(instanceId, true);
      if (!ready) return;
    }
    toast(`正在启动 ${instanceId}…`);
    const result = await startModule(instanceId);
    if (result.run === "failed") {
      toast(`启动失败：${result.last_error ?? "未知原因"}`, "error");
    } else if (result.external) {
      toast(`${instanceId} 已在外部运行，本次没有重启`, "ok");
    } else {
      const pid = result.pid ? `（pid ${result.pid}）` : "";
      toast(`已启动 ${instanceId}${pid}`, "ok");
    }
  } catch (error) {
    toast(`启动失败：${describeError(error)}`, "error");
  } finally {
    inFlight.delete(`start:${instanceId}`);
  }
  await refreshModules();
}

async function stopOne(instanceId: string): Promise<void> {
  if (!instanceId) return;
  if (inFlight.has(`stop:${instanceId}`)) return;
  inFlight.add(`stop:${instanceId}`);
  try {
    await stopModule(instanceId);
    toast(`已停止 ${instanceId}`, "ok");
  } catch (error) {
    toast(`停止失败：${describeError(error)}`, "error");
  } finally {
    inFlight.delete(`stop:${instanceId}`);
  }
  await refreshModules();
}

async function restartOne(instanceId: string): Promise<void> {
  if (!instanceId) return;
  if (inFlight.has(`restart:${instanceId}`)) return;
  inFlight.add(`restart:${instanceId}`);
  try {
    toast(`正在重启 ${instanceId}…`);
    const view = await restartInstance(instanceId);
    if (view.run === "failed") {
      toast(`重启失败：${view.last_error ?? "未知原因"}`, "error");
    } else {
      toast(`已重启 ${instanceId}`, "ok");
    }
  } catch (error) {
    toast(`重启失败：${describeError(error)}`, "error");
  } finally {
    inFlight.delete(`restart:${instanceId}`);
  }
  await refreshModules();
}

async function startEverything(): Promise<void> {
  await runBulk("一键启动", async () => {
    try {
      toast("正在按拓扑顺序启动…");
      const results = await startAll();
      // 启动返回时实例通常还在 Starting，等健康检查才转 Running。
      // 只数 running 会报「已启动 0 个」，把正在起的也算上，另说清有几个在等。
      const started = results.filter((r) => r.run === "running" || r.run === "starting").length;
      const waiting = results.filter((r) => r.run === "starting").length;
      const failed = results.filter((r) => r.run === "failed");
      if (failed.length === 0) {
        const tail = waiting > 0 ? `，其中 ${waiting} 个正在等健康检查` : "";
        toast(`已启动 ${started} / ${results.length} 个实例${tail}`, "ok");
      } else {
        toast(
          `启动 ${started} / ${results.length}，失败：${failed
            .map((r) => `${r.module_id}（${r.last_error ?? "未知原因"}）`)
            .join("；")}`,
          "error",
        );
      }
    } catch (error) {
      toast(`一键启动失败：${describeError(error)}`, "error");
    }
  });
  await refreshModules();
}

async function stopEverything(): Promise<void> {
  await runBulk("一键停止", async () => {
    try {
      await stopAll();
      toast("已停止全部托管实例", "ok");
    } catch (error) {
      toast(`停止失败：${describeError(error)}`, "error");
    }
  });
  await refreshModules();
}

async function makeActive(instanceId: string): Promise<void> {
  try {
    toast(`正在切换 active：${instanceId}（停旧 → 起新 → 探测）…`);
    await switchActive(instanceId);
    toast(`active 已切到 ${instanceId}`, "ok");
  } catch (error) {
    toast(`切换失败：${describeError(error)}`, "error");
  }
  await refreshModules();
}

async function confirmSource(instanceId: string): Promise<void> {
  const view = store.views.find((v) => v.instance.instance_id === instanceId);
  const git = view?.instance.git;
  const body = `
    <div class="alert">
      <b>这是本地源码目录，启动器没有验签</b>
      启动器只能确认目录当前状态，无法验证代码内容。确认后会记住这次的 HEAD，
      源码再变会重新要求确认。
    </div>
    <div class="fp">路径　${esc(view?.instance.source_path ?? "（未记录）")}
HEAD　${esc(git?.head ?? "（不是 git 仓库或读取失败）")}
分支　${esc(git?.branch ?? "—")}${git?.dirty ? "\n状态　工作区有未提交改动" : ""}</div>
  `;
  confirmModal({
    title: "确认源码现状并启动",
    subtitle: instanceId,
    bodyHtml: body,
    confirmLabel: "确认并启动",
    onConfirm: async () => {
      try {
        await confirmLocalSource(instanceId);
        toast("已记录当前源码状态", "ok");
        await startOne(instanceId);
        await refreshModules();
      } catch (error) {
        toast(`确认失败：${describeError(error)}`, "error");
      }
    },
  });
}

/**
 * 装依赖。返回是否成功，供「启动前顺手补依赖」决定要不要继续。
 *
 * 进度由后端按阶段推事件，这里只管开始与结束两个端点：点下去先落一行
 * 「准备开始」，避免前几秒看起来像没反应。
 */
async function prepareEnv(instanceId: string, allowOnline = true): Promise<boolean> {
  beginEnvJob(instanceId);
  patchEnvJob(instanceId);
  try {
    toast(
      allowOnline
        ? `正在安装依赖（允许联网）：${instanceId}…这一步可能要几分钟`
        : `正在安装依赖（离线，只用本机缓存）：${instanceId}…`,
    );
    const python = await prepareInstanceEnv(instanceId, allowOnline);
    endEnvJob(instanceId);
    toast(`依赖装好了：${python}`, "ok");
    return true;
  } catch (error) {
    const message = describeError(error);
    // 失败原因留在卡片上：toast 会自己消失，装依赖失败通常要看第二遍。
    endEnvJob(instanceId, message);
    toast(`安装依赖失败：${message}`, "error");
    return false;
  } finally {
    patchEnvJob(instanceId);
    await refreshModules();
  }
}

async function openAdmin(moduleId: string): Promise<void> {
  try {
    const url = await openAdminWindow(moduleId);
    toast(`已打开管理界面：${url}`, "ok");
  } catch (error) {
    toast(`打开管理界面失败：${describeError(error)}`, "error");
  }
}

function openLogsFor(instanceId: string): void {
  window.dispatchEvent(
    new CustomEvent("dounai:navigate", { detail: `logs:${instanceId}` }),
  );
}

async function toggleModule(moduleId: string): Promise<void> {
  const current = store.views.find((view) => view.instance.module_id === moduleId);
  const next = !(current?.disabled ?? false);
  const category = current?.instance.category ?? "optional";
  try {
    await setModuleEnabled(moduleId, next, category);
    toast(next ? `已启用 ${moduleId}` : `已停用 ${moduleId}（进程不会被自动停止）`, "ok");
  } catch (error) {
    toast(`操作失败：${describeError(error)}`, "error");
  }
  await refreshModules();
}

function askUninstall(instanceId: string): void {
  const view = store.views.find((v) => v.instance.instance_id === instanceId);
  if (!view) return;
  const name = moduleName(view.instance.module_id);
  confirmModal({
    title: `卸载 ${name}？`,
    subtitle: instanceId,
    danger: true,
    confirmLabel: "卸载并删除文件",
    bodyHtml: `
      <div class="alert">
        <b>会删除这个实例的包、venv 与日志</b>
        实例的数据目录（<code>data/</code>）按契约保留，不在这里删除。
      </div>
      <p class="cap" style="margin:0">上游模块自己的对话记录与记忆库不在启动器范围内，需要到模块里清除。</p>
    `,
    onConfirm: async () => {
      try {
        await uninstallModule(view.instance.module_id, view.instance.category, true);
        toast(`已卸载 ${name}`, "ok");
      } catch (error) {
        toast(`卸载失败：${describeError(error)}`, "error");
      }
      await refreshModules();
    },
  });
}

function askUnlink(moduleId: string): void {
  confirmModal({
    title: "解除登记？",
    subtitle: moduleId,
    confirmLabel: "解除登记",
    bodyHtml: `<p class="cap" style="margin:0">只是让启动器不再管理它。目录里的文件不会被删除，也不会被移动。</p>`,
    onConfirm: async () => {
      try {
        await removeLocalModule(moduleId);
        toast(`已解除登记 ${moduleId}`, "ok");
      } catch (error) {
        toast(`解除失败：${describeError(error)}`, "error");
      }
      await refreshModules();
    },
  });
}

function showInstanceDetail(instanceId: string): void {
  const view = store.views.find((v) => v.instance.instance_id === instanceId);
  if (!view) return;
  const i = view.instance;
  const rows: Array<[string, string]> = [
    ["instance_id", i.instance_id],
    ["module_id", i.module_id],
    ["role", i.role],
    ["变体 / 版本", `${i.variant} / v${i.version}`],
    ["来源", i.source_kind === "dir" ? `本地源码目录 ${i.source_path ?? ""}` : `zip 包`],
    ["验签判级", `${i.verdict}${i.verdict === "A" ? "（已验签）" : "（需确认）"}`],
    ["端口", view.port === null ? "无（shell/旁路）" : String(view.port)],
    ["active", view.active ? "是" : "否"],
    ["安装时间", i.installed_at],
    ["确认时间", i.confirmed_at],
    ["发布者指纹", i.publisher_fp ?? "—"],
    [
      "依赖快照",
      view.env_pinned === null || view.env_pinned === undefined
        ? "未准备"
        : view.env_pinned
          ? "已由签名包的 pinned 哈希锚定"
          : "未锚定（包内没声明 snapshot_sha256，本次哈希由本地解析得出）",
    ],
    ["zip sha256", i.zip_sha256 ?? "—"],
    ["数据目录", i.data_dir ?? "—"],
  ];
  if (i.git) {
    rows.push(["git HEAD", i.git.head ?? "—"]);
    rows.push(["git 工作区", i.git.dirty ? "有未提交改动" : "干净"]);
  }
  if (view.snapshot) {
    rows.push(["phase", view.snapshot.phase]);
    rows.push(["health / contract", `${view.snapshot.health} / ${view.snapshot.contract}`]);
    rows.push(["退出码", view.snapshot.exit_code === null ? "—" : String(view.snapshot.exit_code)]);
    if (view.snapshot.last_error) rows.push(["最近错误", view.snapshot.last_error]);
  }

  openModal({
    title: "实例详情",
    subtitle: i.instance_id,
    wide: true,
    bodyHtml: rows
      .map(
        ([key, value]) =>
          `<div class="envrow"><span class="k">${esc(key)}</span><span class="v" style="white-space:normal;word-break:break-all">${esc(value)}</span></div>`,
      )
      .join(""),
    actions: [{ label: "关闭", tone: "quiet", autofocus: true }],
  });
}

/* ------------------------------------------------------------------ */
/* 模块目录（在线安装）                                                */
/* ------------------------------------------------------------------ */

function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)} KiB`;
  return `${bytes} B`;
}

/**
 * 目录条目 → 一行。
 *
 * 已安装的也给按钮：重装（同版本覆盖）是排障最常用的一步，
 * 但按钮文案要写清楚是「重装」而不是「安装」。
 */
function catalogRow(entry: CatalogEntry): string {
  const installed = store.views.some((v) => v.instance.module_id === entry.module_id);
  const label = installed ? "重装" : "安装";
  return `
    <div class="cat-row" data-module="${esc(entry.module_id)}" data-variant="${esc(entry.variant)}">
      <div class="cat-main">
        <b>${esc(entry.name || entry.module_id)}</b>
        <span class="cap">${esc(entry.module_id)}@${esc(entry.variant)} · v${esc(entry.version)} · ${esc(ROLE_TITLE[entry.role] ?? entry.role)} · ${esc(formatBytes(entry.size))}</span>
        <code class="fp">${esc(entry.sha256)}</code>
      </div>
      <div class="cat-side">
        ${installed ? '<span class="chip chip--run">已安装</span>' : ""}
        <button class="btn btn--sm${installed ? "" : " btn--primary"}" type="button" data-act="cat-install">${label}</button>
      </div>
    </div>
  `;
}

/** 拉目录并摊开给用户看。目录取不到时明确说原因，不给「空列表」。 */
async function showCatalog(): Promise<void> {
  let res: CatalogResponse;
  try {
    res = await refreshCatalog("");
  } catch (error) {
    toast(`读取模块目录失败：${describeError(error)}`, "error");
    return;
  }

  if (res.source === "unavailable") {
    openModal({
      title: "模块目录",
      subtitle: "取不到目录",
      bodyHtml: `
        <p style="margin:0 0 8px">${esc(res.reason ?? "未知原因")}</p>
        <p class="cap" style="margin:0">允许访问的域名：${esc(res.allowlist.join("、"))}</p>
        <p class="cap" style="margin:8px 0 0">网络不通时也可以到「导入本地」装一个 .zip 离线包。</p>
      `,
      actions: [{ label: "关闭", tone: "quiet", autofocus: true }],
    });
    return;
  }

  const body = res.entries.length
    ? res.entries.map(catalogRow).join("")
    : `<p class="cap" style="margin:0">目录是空的：还没有模块上架。</p>`;
  const note =
    res.source === "cache"
      ? `<p class="cap" style="margin:0 0 8px">在线目录不可用，以下是本次会话内已验签的目录。原因：${esc(res.reason ?? "")}</p>`
      : "";

  openModal({
    title: "模块目录",
    subtitle: `${res.entries.length} 个可安装模块`,
    wide: true,
    bodyHtml: `${note}<div class="cat-list">${body}</div>`,
    footNote: "包按目录声明的 sha256 校验后再安装",
    actions: [{ label: "关闭", tone: "quiet", autofocus: true }],
    onMount: (root) => {
      bindActions(root, [
        { act: "cat-install", run: (el) => runCatalogInstall(el, false) },
        { act: "cat-force", run: (el) => runCatalogInstall(el, true) },
      ]);
    },
  });
}

/**
 * 安装一行。
 *
 * `confirm=false` 先试一次：B/C 类包 Rust 会拒绝并说明原因，
 * 这里把同一个按钮改成「确认并安装」，用户看到原因再决定。
 * 确认这个判断不放进前端——前端只负责把用户的决定回传。
 */
async function runCatalogInstall(button: HTMLElement, confirm: boolean): Promise<void> {
  const row = button.closest<HTMLElement>(".cat-row");
  if (!row) return;
  const moduleId = row.dataset.module ?? "";
  const variant = row.dataset.variant ?? "";
  if (!moduleId) return;

  const idleLabel = button.textContent ?? "安装";
  button.setAttribute("disabled", "disabled");
  button.textContent = confirm ? "安装中…" : "下载中…";
  try {
    const res = await installFromCatalog(moduleId, variant || null, confirm);
    closeModal();
    const slot = res.activated ? "，已设为该角色的主实例（占用规范端口）" : "";
    toast(`已安装 ${res.module_id}：${res.files} 个文件，${formatBytes(res.bytes)}${slot}`, "ok");
    await refreshModules();
  } catch (error) {
    const message = describeError(error);
    button.removeAttribute("disabled");
    if (!confirm && message.includes("需要用户显式确认")) {
      button.textContent = "确认并安装";
      button.dataset.act = "cat-force";
      row.querySelector(".cat-note")?.remove();
      row.insertAdjacentHTML("beforeend", `<p class="cat-note">${esc(message)}</p>`);
      return;
    }
    button.textContent = idleLabel;
    toast(`安装 ${moduleId} 失败：${message}`, "error");
  }
}

/* ------------------------------------------------------------------ */

/** 重新拉状态并重绘当前页（生命周期事件与动作完成后都走这里）。 */
export async function refreshModules(): Promise<void> {
  try {
    await refreshViews();
  } catch (error) {
    toast(`读取实例状态失败：${describeError(error)}。稍后重试，或重启启动器。`, "error");
    return;
  }
  const body = document.getElementById("pbody");
  const head = document.getElementById("phead");
  if (body && head && body.dataset.view === "modules") {
    renderHead(head);
    renderBody(body);
  }
  window.dispatchEvent(new CustomEvent("dounai:views-updated"));
}
