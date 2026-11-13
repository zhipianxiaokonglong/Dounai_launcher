/**
 * 模块页：按 role 分组的卡片，实例是卡片里的行。
 *
 * 为什么卡片承载模块、而不是「一行一个实例」：M2 之后同一模块可以有多个实例
 * （stable / local / 第三方变体），并排放在一起才看得出「它们抢的是同一个槽」。
 */

import {
  isDesktop,
  openAdminWindow,
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
  type InstanceRuntimeView,
  type Role,
} from "../../bridge.js";
import {
  bindActions,
  clear,
  confirmModal,
  esc,
  openModal,
  toast,
} from "../dom.js";
import { icon } from "../icons.js";
import {
  describeError,
  installedModuleIds,
  isExternal,
  isRunning,
  manifestOf,
  moduleLabel,
  refreshViews,
  runCounts,
  store,
} from "../store.js";

const ROLE_ORDER: Role[] = ["input", "llm", "output", "shell", "auxiliary"];

const ROLE_TITLE: Record<Role, string> = {
  input: "输入",
  llm: "模型",
  output: "输出",
  shell: "总装",
  auxiliary: "旁路",
};

const ROLE_SHORT: Record<Role, string> = {
  input: "in",
  llm: "llm",
  output: "out",
  shell: "sh",
  auxiliary: "aux",
};

const ROLE_DESC: Record<Role, string> = {
  input: "输入接入层：把麦克风、文本与图像送进模型层。",
  llm: "模型编排层：推理、上下文与情绪标签。",
  output: "输出表达层：把模型输出合成为语音。",
  shell: "总装与界面：本地 exe，不占用固定端口。",
  auxiliary: "旁路模块：不进组装层事件流，端口由启动器动态分配。",
};

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
      <button class="btn btn--sm" type="button" data-act="stop-all">停止全部</button>
      <button class="btn btn--primary btn--sm" type="button" data-act="start-all">一键启动</button>
    </div>
  `;

  bindActions(head, [
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
        <span class="ph-ico">${icon("bundles", 18)}</span>
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
            ? "到「导入本地」登记一个已下载的目录，或用离线包安装。登记不会复制也不会删除你的文件。"
            : "清空搜索词或取消「只看有问题的」再试。"
        }</p>
        ${store.views.length === 0 ? '<button class="btn btn--primary btn--sm" type="button" data-act="goto-import">登记本地目录</button>' : ""}
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

  body.appendChild(buildNotInstalled());
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
        `${view.instance.module_id} ${view.instance.instance_id} ${moduleLabel(view.instance.module_id)}`.toLowerCase();
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
  const manifest = manifestOf(moduleId);
  const counter = views.find((view) => view.active) ?? views[0];

  const statusChip = moduleStatusChip(views);
  const meta = [
    counter?.port ? `端口 <b>${counter.port}</b>` : "无规范端口",
    `v${esc(counter?.instance.version ?? "—")}`,
    esc(views[0]?.instance.source_kind ?? "zip"),
  ];
  if (hasDir) meta.push('<span class="warn">本地源码 · 未验证</span>');
  else if (unverified) meta.push('<span class="warn">未验证</span>');

  const desc =
    manifest !== null
      ? `授权 ${manifest.permissions.network.length} 个网络目标、${manifest.permissions.filesystem.length} 个文件路径，仅供参考，当前版本不强制。`
      : viewDescription(views[0], role);

  card.innerHTML = `
    <div class="card-top">
      <span class="role-ico" aria-hidden="true">${esc(ROLE_SHORT[role])}</span>
      <div style="min-width:0">
        <span class="card-name" title="${esc(moduleLabel(moduleId))}">${esc(moduleLabel(moduleId))}</span>
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
  `;
  return row;
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
  const running = isRunning(view);
  if (isExternal(view)) {
    return `<button class="btn btn--sm" type="button" data-act="detail" data-id="${id}">详情</button>`;
  }
  if (running) {
    return `<button class="btn btn--sm" type="button" data-act="stop" data-id="${id}">停止</button>`;
  }
  const disabledAttr = view.disabled ? " disabled title=\"模块已停用，先启用\"" : "";
  return `<button class="btn btn--sm btn--primary" type="button" data-act="start" data-id="${id}"${disabledAttr}>启动</button>`;
}

/** 卡片底栏：最常用的一个动作 + 更多菜单。 */
function cardFootHtml(moduleId: string, views: InstanceRuntimeView[]): string {
  const id = esc(moduleId);
  const primary = views.find((view) => !isExternal(view)) ?? views[0];
  const canManage = views.some((view) => isRunning(view));
  const manifest = manifestOf(moduleId);
  const webUi = manifest?.admin_ui?.web_ui;
  const dirInstance = views.find((view) => view.instance.source_kind === "dir");
  const needsConfirm = dirInstance && !dirInstance.instance.head_confirmed;
  const disabled = views[0]?.disabled ?? false;

  const parts: string[] = [];
  if (needsConfirm && dirInstance) {
    parts.push(
      `<button class="btn btn--sm btn--primary" type="button" data-act="confirm-source" data-id="${esc(dirInstance.instance.instance_id)}">确认变更并启动</button>`,
    );
  } else if (webUi && canManage) {
    parts.push(`<button class="btn btn--sm" type="button" data-act="admin" data-id="${id}">${esc(webUi.label)}</button>`);
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
        `<button type="button" data-act="prepare-env" data-id="${esc(primary.instance.instance_id)}">准备依赖（离线）</button>`,
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

/* ------------------------------------------------------------------ */
/* 未安装清单                                                          */
/* ------------------------------------------------------------------ */

function buildNotInstalled(): HTMLElement {
  const installed = new Set(installedModuleIds());
  const pending = [...store.manifests.values()].filter((m) => !installed.has(m.module_id));

  const box = document.createElement("section");
  const group = document.createElement("div");
  group.className = "grp";
  group.innerHTML = `
    <span class="grp-name">清单里的其他模块</span>
    <span class="grp-line"></span>
    <span class="grp-stat">${pending.length === 0 ? "全部已登记" : `${pending.length} 个未安装`}</span>
  `;
  box.appendChild(group);

  const cards = document.createElement("div");
  cards.className = "cards";
  for (const manifest of pending) {
    const card = document.createElement("article");
    card.className = "card";
    const healthUrl = manifest.entry.health.url;
    card.innerHTML = `
      <div class="card-top">
        <span class="role-ico" aria-hidden="true">${esc(manifest.category.slice(0, 3))}</span>
        <div style="min-width:0">
          <span class="card-name" title="${esc(manifest.name)}">${esc(manifest.name)}</span>
          <span class="card-id">${esc(manifest.module_id)}</span>
        </div>
        <span class="chip chip--stop" style="margin-left:auto">未安装</span>
      </div>
      <p class="card-desc">${esc(
        healthUrl
          ? `服务入口 ${healthUrl}`
          : `本地程序入口 ${manifest.entry.command || "（清单未声明）"}`,
      )}</p>
      <div class="meta"><span>${esc(manifest.category)}</span><span>v${esc(manifest.version)}</span><span>依赖 ${manifest.dependencies.length} 项</span></div>
      <div class="card-foot">
        <button class="btn btn--sm" type="button" data-act="goto-import">登记本地目录</button>
        <span class="sp"></span>
        <button class="btn btn--quiet btn--sm" type="button" data-act="manifest" data-id="${esc(manifest.module_id)}">清单</button>
      </div>
    `;
    cards.appendChild(card);
  }
  box.appendChild(cards);
  return box;
}

/* ------------------------------------------------------------------ */
/* 动作                                                                */
/* ------------------------------------------------------------------ */

function bindBodyActions(body: HTMLElement): void {
  bindActions(body, [
    { act: "start", run: (node) => startOne(node.dataset.id ?? "") },
    { act: "stop", run: (node) => stopOne(node.dataset.id ?? "") },
    { act: "restart", run: (node) => restartOne(node.dataset.id ?? "") },
    { act: "detail", run: (node) => showInstanceDetail(node.dataset.id ?? "") },
    { act: "logs", run: (node) => openLogsFor(node.dataset.id ?? "") },
    { act: "switch-active", run: (node) => makeActive(node.dataset.id ?? "") },
    { act: "confirm-source", run: (node) => confirmSource(node.dataset.id ?? "") },
    { act: "prepare-env", run: (node) => prepareEnv(node.dataset.id ?? "") },
    { act: "admin", run: (node) => openAdmin(node.dataset.id ?? "") },
    { act: "toggle", run: (node) => toggleModule(node.dataset.id ?? "") },
    { act: "uninstall", run: (node) => askUninstall(node.dataset.id ?? "") },
    { act: "unlink", run: (node) => askUnlink(node.dataset.id ?? "") },
    { act: "manifest", run: (node) => showManifest(node.dataset.id ?? "") },
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

async function startOne(instanceId: string): Promise<void> {
  if (!instanceId) return;
  try {
    toast(`正在启动 ${instanceId}…`);
    const view = await startModule(instanceId);
    if (view.run === "failed") {
      toast(`启动失败：${view.last_error ?? "未知原因"}`, "error");
    } else if (view.external) {
      toast(`${instanceId} 已在外部运行，本次没有重启`, "ok");
    } else {
      const pid = view.pid ? `（pid ${view.pid}）` : "";
      toast(`已启动 ${instanceId}${pid}`, "ok");
    }
  } catch (error) {
    toast(`启动失败：${describeError(error)}`, "error");
  }
  await refreshModules();
}

async function stopOne(instanceId: string): Promise<void> {
  if (!instanceId) return;
  try {
    await stopModule(instanceId);
    toast(`已停止 ${instanceId}`, "ok");
  } catch (error) {
    toast(`停止失败：${describeError(error)}`, "error");
  }
  await refreshModules();
}

async function restartOne(instanceId: string): Promise<void> {
  if (!instanceId) return;
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
  }
  await refreshModules();
}

async function startEverything(): Promise<void> {
  try {
    toast("正在按拓扑顺序启动…");
    const results = await startAll();
    const ok = results.filter((r) => r.run === "running").length;
    const failed = results.filter((r) => r.run === "failed");
    if (failed.length === 0) {
      toast(`已启动 ${ok} / ${results.length} 个实例`, "ok");
    } else {
      toast(
        `启动 ${ok} / ${results.length}，失败：${failed
          .map((r) => `${r.module_id}（${r.last_error ?? "未知原因"}）`)
          .join("；")}`,
        "error",
      );
    }
  } catch (error) {
    toast(`一键启动失败：${describeError(error)}`, "error");
  }
  await refreshModules();
}

async function stopEverything(): Promise<void> {
  try {
    await stopAll();
    toast("已停止全部托管实例", "ok");
  } catch (error) {
    toast(`停止失败：${describeError(error)}`, "error");
  }
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

async function prepareEnv(instanceId: string): Promise<void> {
  try {
    toast("正在准备依赖（离线优先）…");
    const python = await prepareInstanceEnv(instanceId);
    toast(`依赖已就绪：${python}`, "ok");
  } catch (error) {
    toast(`依赖准备失败：${describeError(error)}`, "error");
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
  const name = moduleLabel(view.instance.module_id);
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

function showManifest(moduleId: string): void {
  const manifest = manifestOf(moduleId);
  if (!manifest) return;
  const lines = [
    `入口命令：${manifest.entry.command || "—"}`,
    `工作目录：${manifest.entry.working_dir || "—"}`,
    `健康检查：${manifest.entry.health.url ?? "无"}`,
    `网络权限：${manifest.permissions.network.join("、") || "无"}`,
    `文件权限：${manifest.permissions.filesystem.join("、") || "无"}`,
    `设备权限：${manifest.permissions.devices.join("、") || "无"}`,
    `运行依赖：${manifest.dependencies.map((d) => `${d.name} ${d.version}`).join("、") || "无"}`,
    `模型包：${manifest.model_packs.map((p) => p.name).join("、") || "无"}`,
  ];
  openModal({
    title: manifest.name,
    subtitle: `${manifest.module_id} · ${manifest.category} · v${manifest.version}`,
    wide: true,
    bodyHtml: `<pre class="fp" style="white-space:pre-wrap">${esc(lines.join("\n"))}</pre>
      <p class="cap" style="margin:0">权限声明仅供参考，当前版本不强制。是否可信以包内的清单文件为准。</p>`,
    actions: [{ label: "关闭", tone: "quiet", autofocus: true }],
  });
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
