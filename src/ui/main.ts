/**
 * 启动器 UI 主入口。
 *
 * 负责三块界面：模块卡片、套装规划、环境体检。
 *
 * 所有会改变本机状态的操作（安装、启停、卸载、数据清除）
 * 都通过 `../bridge.js` 调用 Rust 命令；
 * 浏览器模式下桥接不可用，界面会明确提示「需桌面端」而非静默失败。
 */

import type { ModuleManifest } from "../manifest.js";
import { parseManifest } from "../manifest.js";
import { buildLocalInstallRequest } from "../localinstall.js";
import { type BundleDefinition, parseBundle, planInstall, type InstallPlan } from "../bundle.js";
import {
  isDesktop,
  probeEnvironmentCmd,
  listModuleStates,
  setModuleEnabled,
  uninstallModule,
  installModule,
  refreshCatalog,
  purgeLocalData,
  scanLocalModules,
  addLocalModule,
  removeLocalModule,
  startModule,
  stopModule,
  startAll,
  stopAll,
  trustInfo,
  BridgeUnavailableError,
  type ModuleView,
  type RustEnvReport,
  type TrustInfo,
} from "../bridge.js";

interface El {
  envPill: HTMLElement;
  moduleGrid: HTMLElement;
  modulesEmpty: HTMLElement;
  search: HTMLInputElement;
  filterCategory: HTMLSelectElement;
  onlyMissing: HTMLInputElement;
  bundleList: HTMLElement;
  plan: HTMLElement;
  planBody: HTMLElement;
  planRecos: HTMLElement;
  planLog: HTMLElement;
  btnExecute: HTMLButtonElement;
  btnCancel: HTMLButtonElement;
  envBody: HTMLElement;
  btnRefresh: HTMLButtonElement;
}

function must<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`缺少元素 #${id}`);
  return node as T;
}

const el: El = {
  envPill: must("env-pill"),
  moduleGrid: must("module-grid"),
  modulesEmpty: must("modules-empty"),
  search: must("search"),
  filterCategory: must("filter-category"),
  onlyMissing: must("only-missing"),
  bundleList: must("bundle-list"),
  plan: must("plan"),
  planBody: must("plan-body"),
  planRecos: must("plan-recos"),
  planLog: must("plan-log"),
  btnExecute: must("btn-execute"),
  btnCancel: must("btn-cancel"),
  envBody: must("env-body"),
  btnRefresh: must("btn-refresh"),
};

const state = {
  modules: new Map<string, ModuleManifest>(),
  bundles: [] as BundleDefinition[],
  selectedBundle: null as BundleDefinition | null,
  includeOptional: [] as string[],
  plan: null as InstallPlan | null,
  /** 各模块的持久化状态（来自 Rust，浏览器模式下为空） */
  moduleStates: new Map<string, ModuleView>(),
  envReport: null as RustEnvReport | null,
  /** 信任策略（发布版 fail-closed 时用于提示） */
  trust: null as TrustInfo | null,
};

/** HTML 转义：manifest 与套装文件属于外部输入，一律按纯文本渲染，防注入。 */
function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/**
 * 环境探测走 Rust 侧的 `probe_environment` 命令。
 *
 * 浏览器模式下没有该命令，此时明确显示「需桌面端」，
 * 而不是伪造一份"检测通过"——环境体检的价值完全建立在数据真实之上。
 */
async function probeEnvironment(): Promise<void> {
  if (!isDesktop()) {
    el.envPill.textContent = "需桌面端";
    el.envPill.dataset.tone = "warn";
    state.envReport = null;
    renderEnv();
    renderModules();
    return;
  }

  try {
    const report = await probeEnvironmentCmd();
    state.envReport = report;
    el.envPill.textContent = report.healthy ? "环境正常" : `${report.problems.length} 项待处理`;
    el.envPill.dataset.tone = report.healthy ? "ok" : "error";
    renderEnv();
    renderModules();
  } catch (error) {
    el.envPill.textContent = "检测失败";
    el.envPill.dataset.tone = "error";
    console.warn("环境探测失败", error);
  }
}

/** 拉取模块持久化状态（安装/启用/运行）。浏览器模式下保持为空。 */
async function loadModuleStates(): Promise<void> {
  if (!isDesktop()) return;
  try {
    const views = await listModuleStates();
    state.moduleStates.clear();
    for (const v of views) state.moduleStates.set(v.module_id, v);
  } catch (error) {
    console.warn("读取模块状态失败", error);
  }
}

/** 拉取信任策略。发布版未配置公钥时是 fail-closed，用户需要知道。 */
async function loadTrustInfo(): Promise<void> {
  if (!isDesktop()) return;
  try {
    state.trust = await trustInfo();
  } catch (error) {
    console.warn("读取信任策略失败", error);
  }
}

/** 用一句话说明当前安装为何不可用（或可用）。 */
function installBlockerText(): string | null {
  const t = state.trust;
  if (!t) return null;
  if (t.trusted_keys === 0 && !t.allow_unsigned) {
    return "当前为安全模式：未配置签名公钥，所有未签名模块将被拒绝安装。";
  }
  if (t.allow_unsigned) {
    return "当前为开发模式：允许安装未签名模块（发布版会拒绝）。";
  }
  return `已配置 ${t.trusted_keys} 个签名公钥，仅接受验签通过的模块。`;
}

/** 统一处理桥接错误的提示。 */
function describeError(error: unknown): string {
  if (error instanceof BridgeUnavailableError) return error.message;
  if (error instanceof Error) return error.message;
  return String(error);
}

/* ------------------------------------------------------------------ */
/* 数据加载（一期用打包进 dist 的静态 manifest；后续替换为启动器后端）   */
/* ------------------------------------------------------------------ */

async function loadModules(): Promise<void> {
  const mods = await loadManifestFiles();
  for (const raw of mods) {
    try {
      const m = parseManifest(raw);
      state.modules.set(m.module_id, m);
    } catch (error) {
      console.warn("跳过非法 manifest", error);
    }
  }
}

async function loadManifestFiles(): Promise<unknown[]> {
  // 由 vite 在构建时把 manifests/*.json 内联为静态导入
  const files = import.meta.glob("/manifests/*.json", { eager: true, import: "default" });
  return Object.values(files) as unknown[];
}

async function loadBundles(): Promise<void> {
  const files = import.meta.glob("/bundles/*.json", { eager: true, import: "default" }) as Record<
    string,
    unknown
  >;
  state.bundles = [];
  for (const raw of Object.values(files)) {
    try {
      state.bundles.push(parseBundle(raw));
    } catch (error) {
      console.warn("跳过非法套装", error);
    }
  }
  // 最小可用排最前
  state.bundles.sort((a, b) => {
    const order = ["minimal", "lite", "full", "offline"];
    return order.indexOf(a.bundle_id) - order.indexOf(b.bundle_id);
  });
}

/* ------------------------------------------------------------------ */
/* 模块卡片渲染                                                        */
/* ------------------------------------------------------------------ */

function categoryBadge(m: ModuleManifest): string {
  const map: Record<string, string> = {
    core: "badge--core",
    optional: "badge--optional",
    reserved: "badge--reserved",
    experimental: "badge--optional",
  };
  const label: Record<string, string> = {
    core: "core · 不可卸载",
    optional: "optional",
    reserved: "reserved · 默认禁用",
    experimental: "experimental",
  };
  return `<span class="badge ${map[m.category] ?? ""}">${label[m.category] ?? m.category}</span>`;
}

function renderModules(): void {
  const q = el.search.value.trim().toLowerCase();
  const cat = el.filterCategory.value;
  const onlyMissing = el.onlyMissing.checked;

  const list = [...state.modules.values()].filter((m) => {
    if (cat && m.category !== cat) return false;
    if (q && !(`${m.name} ${m.module_id}`.toLowerCase().includes(q))) return false;
    if (onlyMissing) {
      const missing = m.dependencies.filter((d) => !isDependencyMet(d.name));
      if (missing.length === 0) return false;
    }
    return true;
  });

  el.moduleGrid.innerHTML = "";
  el.modulesEmpty.hidden = list.length > 0;

  for (const m of list) {
    const missingDeps = m.dependencies.filter((d) => !isDependencyMet(d.name));
    const st = moduleStateOf(m.module_id);
    const installed = st?.install === "installed";
    const card = document.createElement("div");
    card.className = "card";
    card.innerHTML = `
      <div class="card__head">
        <div>
          <div class="card__name">${escapeHtml(m.name)}</div>
          <div class="card__id">${escapeHtml(m.module_id)} v${escapeHtml(m.version)}</div>
        </div>
        <div style="display:flex;flex-direction:column;gap:4px;align-items:flex-end">
          ${categoryBadge(m)}
          ${stateBadge(m)}
          ${runBadge(m)}
          ${linkBadge(m)}
        </div>
      </div>
      <div class="card__desc">${escapeHtml(describeEntry(m))}</div>
      <div class="card__meta">
        <div>运行依赖：<strong>${m.dependencies.length}</strong> 项${
          missingDeps.length > 0
            ? ` · <span class="badge badge--missing">缺 ${missingDeps.length}</span>`
            : ` · <span class="badge badge--ok">齐全</span>`
        }</div>
        <div>模型包：<strong>${m.model_packs.length}</strong> 个${
          m.model_packs.length > 0 ? `（推荐 ${m.model_packs.filter((p) => p.recommended).length}）` : ""
        }</div>
        <div>网络权限：${m.permissions.network.length === 0 ? "无（纯本地）" : m.permissions.network.join(", ")}</div>
        ${st?.last_error ? `<div style="color:var(--danger)">${escapeHtml(st.last_error)}</div>` : ""}
      </div>
      <div class="card__actions">
        <button class="btn" data-act="detail" data-id="${m.module_id}">详情</button>
        ${renderActionButtons(m, installed)}
      </div>
    `;
    el.moduleGrid.appendChild(card);
  }

  el.moduleGrid.querySelectorAll("button[data-act]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const id = (btn as HTMLElement).dataset.id ?? "";
      const act = (btn as HTMLElement).dataset.act;
      if (act === "detail") showModuleDetail(id);
      if (act === "toggle") void handleToggle(id);
      if (act === "uninstall") void handleUninstall(id);
      if (act === "start") void handleStartStop(id, "start");
      if (act === "stop") void handleStartStop(id, "stop");
      if (act === "restart") void handleStartStop(id, "restart");
      if (act === "unlink") void handleUnlink(id);
    });
  });
}

/**
 * 按分类与状态决定可用操作。
 *
 * core 与 reserved 的「不可卸载」由 Rust 侧强制（state.rs），
 * 这里只是前置显示，避免用户点了才被拒——两处规则一致，
 * 但**以后端为准**（前端禁用只是体验优化）。
 */
function renderActionButtons(m: ModuleManifest, installed: boolean): string {
  // 启动/停止与「能否卸载」是两件不同的事，不能混在一个按钮里：
  // v0.1.0 对 core/reserved 直接返回「不可卸载」，
  // 于是核心模块连启动入口都没有——这正是"启动器不能启动"的根因。
  if (!installed) {
    return '<button class="btn" disabled>未安装</button>';
  }

  const st = moduleStateOf(m.module_id);
  const running = st?.run === "running" || st?.run === "starting";
  const failed = st?.run === "failed";
  const enabled = st?.enabled ?? true;

  const parts: string[] = [];

  if (running) {
    parts.push(`<button class="btn" data-act="stop" data-id="${m.module_id}">停止</button>`);
  } else if (failed) {
    parts.push(`<button class="btn" data-act="restart" data-id="${m.module_id}">重试</button>`);
  } else if (!enabled) {
    parts.push('<button class="btn" disabled title="已停用，先启用">启动</button>');
  } else {
    parts.push(
      `<button class="btn btn--primary" data-act="start" data-id="${m.module_id}">启动</button>`,
    );
  }

  // 卸载权限来自分类，与运行状态无关
  // 链接模块的文件属于用户：只能「解除登记」，不能删文件
  if (st?.linked) {
    parts.push(
      `<button class="btn" data-act="unlink" data-id="${m.module_id}">解除登记</button>`,
    );
    return parts.join("\n");
  }

  const locked = m.category === "core" || m.category === "reserved";
  if (locked) {
    const label = m.category === "reserved" ? "不可卸载（Reserved）" : "不可卸载（核心）";
    parts.push(`<button class="btn" disabled>${label}</button>`);
  } else {
    parts.push(
      `<button class="btn" data-act="toggle" data-id="${m.module_id}">${enabled ? "停用" : "启用"}</button>`,
      `<button class="btn" data-act="uninstall" data-id="${m.module_id}">卸载</button>`,
    );
  }

  return parts.join("\n");
}

/** 运行状态徽标（与「已安装」徽标分开：能不能装 ≠ 在不在跑）。 */
/** 外部链接标记：文件在用户自己目录里，启动器不拥有。 */
function linkBadge(m: ModuleManifest): string {
  const st = moduleStateOf(m.module_id);
  if (!st || !st.linked) return "";
  return `<span class="badge badge--warn" title="${escapeHtml(st.path)}">本地目录</span>`;
}

function runBadge(m: ModuleManifest): string {
  const st = moduleStateOf(m.module_id);
  if (!st || st.install !== "installed") return "";
  switch (st.run) {
    case "running":
      return '<span class="badge badge--ok" title="进程运行中">● 运行中</span>';
    case "starting":
      return '<span class="badge badge--warn">◐ 启动中</span>';
    case "failed":
      return `<span class="badge badge--missing" title="${escapeHtml(st.last_error ?? "启动失败")}">● 启动失败</span>`;
    default:
      return '<span class="badge">○ 已停止</span>';
  }
}

/* ------------------------------------------------------------------ */
/* 本地模块：扫描 / 添加 / 解除登记                                    */
/* ------------------------------------------------------------------ */

function localMsg(text: string): void {
  const node = document.getElementById("local-msg");
  if (!node) return;
  node.hidden = false;
  node.textContent = text;
}

function localPathValue(): string {
  const input = document.getElementById("local-path") as HTMLInputElement | null;
  return input?.value.trim() ?? "";
}

/** 扫描候选模块并列出，点击某项填入路径。 */
async function handleScan(): Promise<void> {
  const box = document.getElementById("scan-results");
  if (!box) return;
  const root = localPathValue();
  if (!root) {
    localMsg("请先填写要扫描的目录（可以是包含多个模块的上级目录）。");
    return;
  }
  if (!isDesktop()) {
    localMsg("需要桌面端才能扫描本地目录。");
    return;
  }

  localMsg("扫描中…");
  box.innerHTML = "";
  try {
    const list = await scanLocalModules(root);
    if (list.length === 0) {
      localMsg(
        "没有找到含 manifest.json 的模块目录。\n" +
          "如果你拉取的是模型权重（.gguf / .safetensors 等），它不是可添加的模块。",
      );
      return;
    }

    localMsg(`找到 ${list.length} 个候选，点击即可填入路径。`);
    for (const item of list) {
      const row = document.createElement("div");
      row.className = "step";
      row.style.cursor = "pointer";
      row.innerHTML = `
        <span>${item.registered ? "✅" : "📦"}</span>
        <span>${escapeHtml(item.name)}</span>
        <span class="badge badge--optional">${escapeHtml(item.category)}</span>
        ${item.signed ? '<span class="badge badge--ok">已签名</span>' : '<span class="badge badge--warn">未签名</span>'}
        ${item.registered ? '<span class="badge badge--ok">已登记</span>' : ""}
        <span class="step__reason">${escapeHtml(item.path)}</span>
      `;
      row.addEventListener("click", () => {
        const input = document.getElementById("local-path") as HTMLInputElement | null;
        if (input) input.value = item.path;
        localMsg(`已填入：${item.path}，点「添加」登记。`);
      });
      box.appendChild(row);
    }
  } catch (error) {
    localMsg(`扫描失败：${describeError(error)}`);
  }
}

/** 添加本地模块。 */
async function handleAddLocal(): Promise<void> {
  const path = localPathValue();
  if (!path) {
    localMsg("请填写模块目录路径。");
    return;
  }
  if (!isDesktop()) {
    localMsg("需要桌面端才能添加本地模块。");
    return;
  }

  localMsg("正在校验并登记…");
  try {
    const view = await addLocalModule(path);
    localMsg(
      [
        `已登记：${view.module_id}`,
        `  版本：${view.version}`,
        `  路径：${view.path}`,
        "",
        "可以在卡片上点「启动」了。",
        "注意：卸载/解除登记都不会删除该目录里的文件。",
      ].join("\n"),
    );
    await loadModuleStates();
    renderModules();
  } catch (error) {
    localMsg(`添加失败：${describeError(error)}`);
  }
}

/** 解除本地模块登记（不删文件）。 */
async function handleUnlink(moduleId: string): Promise<void> {
  const m = state.modules.get(moduleId);
  const name = m?.name ?? moduleId;
  if (
    !confirm(
      `解除「${name}」的登记？\n\n` +
        "只是让启动器不再管理它，目录里的文件不会被删除。",
    )
  ) {
    return;
  }
  try {
    await removeLocalModule(moduleId);
    await loadModuleStates();
    renderModules();
    localMsg(`已解除登记：${name}`);
  } catch (error) {
    localMsg(`解除登记失败：${describeError(error)}`);
  }
}

/* ------------------------------------------------------------------ */
/* 启停操作                                                            */
/* ------------------------------------------------------------------ */

/** 启动 / 停止 / 重试，失败原因直接显示在日志区。 */
async function handleStartStop(
  moduleId: string,
  action: "start" | "stop" | "restart",
): Promise<void> {
  const m = state.modules.get(moduleId);
  if (!m) return;

  try {
    if (action === "stop") {
      await stopModule(moduleId);
    } else {
      if (action === "restart") {
        await stopModule(moduleId).catch(() => undefined);
      }
      await startModule(moduleId);
    }
  } catch (error) {
    // 启动失败必须说清原因：未安装 / 已停用 / 健康检查失败 / 工作目录越界
    el.planLog.hidden = false;
    el.planLog.textContent = `启动「${m.name}」失败：${describeError(error)}`;
  }

  await loadModuleStates();
  renderModules();
}

/** 一键启动整套：逐模块报告，失败不阻塞其余。 */
async function handleStartAll(): Promise<void> {
  if (!isDesktop()) {
    maintLog("需要桌面端才能启动模块。");
    return;
  }
  maintLog("正在按顺序启动…");
  try {
    const results = await startAll();
    const lines = ["启动结果："];
    for (const r of results) {
      const mark = r.run === "running" ? "✓" : "✗";
      const extra = r.last_error ? ` — ${r.last_error}` : "";
      lines.push(`  ${mark} ${r.module_id}: ${r.run}${extra}`);
    }
    const ok = results.filter((r) => r.run === "running").length;
    lines.push("", `成功 ${ok} / 共 ${results.length}`);
    maintLog(lines.join("\n"));
    await loadModuleStates();
    renderModules();
  } catch (error) {
    maintLog(`一键启动失败：${describeError(error)}`);
  }
}

/** 全部停止。 */
async function handleStopAll(): Promise<void> {
  if (!isDesktop()) {
    maintLog("需要桌面端才能停止模块。");
    return;
  }
  try {
    const results = await stopAll();
    const lines = [`已停止 ${results.length} 个模块：`];
    for (const r of results) lines.push(`  · ${r.module_id} → ${r.run}`);
    maintLog(lines.join("\n"));
    await loadModuleStates();
    renderModules();
  } catch (error) {
    maintLog(`全部停止失败：${describeError(error)}`);
  }
}

/** 启用/停用模块（调用真实命令）。 */
async function handleToggle(moduleId: string): Promise<void> {
  const m = state.modules.get(moduleId);
  if (!m) return;
  const st = moduleStateOf(moduleId);
  const next = !(st?.enabled ?? true);

  try {
    await setModuleEnabled(moduleId, next, m.category);
    await loadModuleStates();
    renderModules();
  } catch (error) {
    el.planLog.hidden = false;
    el.planLog.textContent = `操作失败：${describeError(error)}`;
  }
}

/** 卸载模块（含删除文件）。 */
async function handleUninstall(moduleId: string): Promise<void> {
  const m = state.modules.get(moduleId);
  if (!m) return;
  if (!confirm(`确定卸载「${m.name}」并删除其文件？此操作不可撤销。`)) return;

  try {
    await uninstallModule(moduleId, m.category, true);
    await loadModuleStates();
    renderModules();
  } catch (error) {
    el.planLog.hidden = false;
    el.planLog.textContent = `卸载失败：${describeError(error)}`;
  }
}

function describeEntry(m: ModuleManifest): string {
  if (m.entry.health.url) return `服务 · ${m.entry.health.url}`;
  return `入口 · ${m.entry.command || "—"}`;
}

function showModuleDetail(id: string): void {
  const m = state.modules.get(id);
  if (!m) return;
  alert(
    [
      `${m.name} (${m.module_id})`,
      `版本：${m.version}   分类：${m.category}`,
      "",
      `入口：${m.entry.command}`,
      m.entry.health.url ? `健康检查：${m.entry.health.url}` : "",
      "",
      "运行依赖：",
      ...m.dependencies.map((d) => `  · ${d.name} ${d.version} (${d.type})`),
      "",
      m.model_packs.length > 0 ? "模型包（默认不装）：" : "模型包：无",
      ...m.model_packs.map(
        (p) => `  · ${p.name} — ${p.size_mb} MB, 隐私=${p.privacy}, 推荐=${p.recommended}`,
      ),
      "",
      m.reserved ? `Reserved：默认禁用=${!m.reserved.default_enabled ? "是" : "否"}, 可卸载=${m.reserved.removable}` : "",
    ]
      .filter(Boolean)
      .join("\n"),
  );
}

/**
 * 依赖是否已满足。
 *
 * 用环境体检里对应运行时的可用性判断；未体检时视为"未知"，
 * 返回 true 以免在体检前就把所有模块标成缺依赖（那会误导用户）。
 */
function isDependencyMet(name: string): boolean {
  const env = state.envReport;
  if (!env) return true;
  const n = name.toLowerCase();
  if (n.includes("python")) return env.python.available;
  if (n.includes("node")) return env.node.available;
  if (n === "uv") return env.uv.available;
  // WebView2 等非运行时依赖不在此判断
  return true;
}

/** 取模块的持久化状态（浏览器模式下没有）。 */
function moduleStateOf(id: string): ModuleView | undefined {
  return state.moduleStates.get(id);
}

/** 状态徽标文案。 */
function stateBadge(m: ModuleManifest): string {
  const st = moduleStateOf(m.module_id);
  if (!st) return "";
  switch (st.install) {
    case "installed":
      return `<span class="badge badge--ok">已安装${st.enabled ? "" : " · 已停用"}</span>`;
    case "installing":
      return '<span class="badge badge--warn">安装中</span>';
    case "broken":
      return '<span class="badge badge--missing">已损坏</span>';
    default:
      return "";
  }
}

/* ------------------------------------------------------------------ */
/* 套装与安装计划                                                      */
/* ------------------------------------------------------------------ */

function renderBundles(): void {
  el.bundleList.innerHTML = "";
  for (const b of state.bundles) {
    const div = document.createElement("div");
    div.className = "bundle" + (state.selectedBundle?.bundle_id === b.bundle_id ? " is-selected" : "");
    const packCount = b.model_packs.length;
    div.innerHTML = `
      <div class="bundle__name">${escapeHtml(b.name)}</div>
      <div class="bundle__desc">${escapeHtml(b.description)}</div>
      ${b.requires_explicit_consent ? '<span class="bundle__tag">需要显式确认</span>' : ""}
      ${
        packCount === 0
          ? '<span class="bundle__tag" style="background:var(--accent-soft);color:var(--accent)">不含模型权重</span>'
          : `<span class="bundle__tag">含 ${packCount} 个模型包</span>`
      }
    `;
    div.addEventListener("click", () => selectBundle(b));
    el.bundleList.appendChild(div);
  }
}

function selectBundle(b: BundleDefinition): void {
  state.selectedBundle = b;
  state.includeOptional = [];
  renderBundles();
  recomputePlan();
}

function recomputePlan(): void {
  const b = state.selectedBundle;
  if (!b) {
    el.plan.hidden = true;
    return;
  }
  const plan = planInstall(b, state.modules, state.includeOptional);
  state.plan = plan;
  el.plan.hidden = false;

  el.planBody.innerHTML = "";
  for (const step of plan.steps) {
    const row = document.createElement("div");
    row.className = "step" + (step.kind === "model_pack" ? " step--pack" : "");
    row.innerHTML = `
      <span>${step.kind === "model_pack" ? "📦" : "🧩"}</span>
      <span>${escapeHtml(step.name)}</span>
      ${step.locked ? '<span class="step__lock">🔒 不可勾除</span>' : ""}
      <span class="step__reason">${escapeHtml(step.reason)}</span>
    `;
    el.planBody.appendChild(row);
  }

  el.planRecos.innerHTML = "";
  if (plan.recommendations.length > 0) {
    const title = document.createElement("div");
    title.className = "card__desc";
    title.textContent = "推荐但默认不安装（最小安装原则）：";
    el.planRecos.appendChild(title);
    for (const r of plan.recommendations) {
      const row = document.createElement("div");
      row.className = "reco";
      row.innerHTML = `<span>💡</span><span>${escapeHtml(r.name)}</span>
        <span class="reco__size">${r.size_mb} MB</span>`;
      el.planRecos.appendChild(row);
    }
  }

  const warn = plan.warnings.length
    ? plan.warnings.map((w) => `· ${w}`).join("\n")
    : "";
  if (warn) {
    const note = document.createElement("div");
    note.className = "card__desc";
    note.style.marginTop = "10px";
    note.textContent = warn;
    el.planRecos.appendChild(note);
  }

  el.btnExecute.disabled = plan.requiresConsent;
  el.btnExecute.textContent = plan.requiresConsent ? "需显式确认后安装" : "开始安装";
}

/* ------------------------------------------------------------------ */
/* 环境面板                                                            */
/* ------------------------------------------------------------------ */

function renderEnv(): void {
  const env = state.envReport;
  if (!env) {
    el.envBody.innerHTML = isDesktop()
      ? '<p class="empty">点击右上角「检测环境」运行体检。</p>'
      : '<p class="empty">浏览器模式无法检测本机环境，请在桌面端打开启动器。</p>';
    return;
  }
  el.envBody.innerHTML = "";

  const group = (title: string, rows: string) => {
    const g = document.createElement("div");
    g.className = "env__group";
    g.innerHTML = `<h3>${title}</h3>${rows}`;
    el.envBody.appendChild(g);
  };

  const runtimes: Array<[string, RustEnvReport["python"]]> = [
    ["Python", env.python],
    ["Node.js", env.node],
    ["uv", env.uv],
  ];

  group(
    "运行时",
    runtimes
      .map(
        ([label, r]) => `
      <div class="env__row">
        <span class="env__name">${escapeHtml(label)}</span>
        <span class="env__detail">${escapeHtml(r.detail || "—")}</span>
        <span class="env__state badge ${r.available ? "badge--ok" : "badge--missing"}">
          ${r.available ? "可用" : "缺失"}
        </span>
      </div>`,
      )
      .join(""),
  );

  if (env.gpus.length > 0) {
    group(
      "显卡",
      env.gpus
        .map(
          (g) => `
        <div class="env__row">
          <span class="env__name">${escapeHtml(g.name)}</span>
          <span class="env__detail">显存 ${g.vram_gb} GB</span>
          <span class="env__state badge ${g.usable ? "badge--ok" : ""}">
            ${g.usable ? "可用于本地模型" : "算力不足"}
          </span>
        </div>`,
        )
        .join(""),
    );
  }

  group(
    "磁盘",
    `
      <div class="env__row">
        <span class="env__name">${escapeHtml(env.disk.path)}</span>
        <span class="env__detail">剩余 ${env.disk.free_gb} GB</span>
        <span class="env__state badge ${env.disk.ok ? "badge--ok" : "badge--missing"}">
          ${env.disk.ok ? "空间充足" : "空间不足"}
        </span>
      </div>`,
  );

  // 安全策略：让用户知道"未签名模块能不能装"，而不是安装时才发现被拒
  const t = state.trust;
  if (t) {
    const tone = t.trusted_keys > 0 ? "badge--ok" : "badge--warn";
    const label =
      t.trusted_keys > 0
        ? `已配置 ${t.trusted_keys} 个公钥`
        : t.allow_unsigned
          ? "开发模式（允许未签名）"
          : "安全模式（拒绝未签名）";
    group(
      "模块签名策略",
      `
      <div class="env__row">
        <span class="env__name">manifest 验签</span>
        <span class="env__detail">${escapeHtml(installBlockerText() ?? "")}</span>
        <span class="env__state badge ${tone}">${escapeHtml(label)}</span>
      </div>`,
    );
  }

  if (env.problems.length > 0) {
    group(
      "需要处理",
      env.problems.map((p) => `<div class="env__row"><span>⚠️ ${escapeHtml(p)}</span></div>`).join(""),
    );
  }

  // 维护操作（一键清除、刷新清单）
  // 放在环境页而不是单独菜单：这两件事都是"维护本机状态"，
  // 和体检结果放在一起，用户在发现问题时能立刻处理。
  group(
    "维护",
    `
      <div class="env__row">
        <span class="env__name">清除本地数据</span>
        <span class="env__detail">清缓存、状态记录与模块安装目录（不影响上游对话/记忆）</span>
        <span class="env__state">
          <button class="btn" id="btn-purge">清除…</button>
        </span>
      </div>
      <div class="env__row">
        <span class="env__name">一键启动 Dounai</span>
        <span class="env__detail">按顺序拉起输入层 → 模型层 → 语音层 → 悬浮窗；单个失败不影响其余</span>
        <span class="env__state">
          <button class="btn btn--primary" id="btn-start-all">启动</button>
          <button class="btn" id="btn-stop-all">全部停止</button>
        </span>
      </div>
      <div class="env__row">
        <span class="env__name">刷新模块清单</span>
        <span class="env__detail">从白名单内的源拉取最新模块列表</span>
        <span class="env__state">
          <button class="btn" id="btn-catalog">刷新</button>
        </span>
      </div>
      <pre id="maint-log" class="log" hidden></pre>
    `,
  );

  document.getElementById("btn-start-all")?.addEventListener("click", () => void handleStartAll());
  document.getElementById("btn-stop-all")?.addEventListener("click", () => void handleStopAll());
  document.getElementById("btn-purge")?.addEventListener("click", () => void handlePurge());
  document.getElementById("btn-catalog")?.addEventListener("click", () => void handleCatalog());
}

/** 维护操作的结果输出区。 */
function maintLog(text: string): void {  const node = document.getElementById("maint-log");
  if (!node) return;
  node.hidden = false;
  node.textContent = text;
}

/** 一键清除本地数据。 */
async function handlePurge(): Promise<void> {
  if (!isDesktop()) {
    maintLog("需要桌面端才能清除数据。");
    return;
  }
  const ok = confirm(
    [
      "将清除启动器的本地数据：",
      "  · 下载与清单缓存",
      "  · 模块状态记录",
      "  · 已安装的模块文件",
      "",
      "不会影响上游模块的对话记录与记忆库——那些需要到对应模块里清除。",
      "",
      "确定继续？",
    ].join("\n"),
  );
  if (!ok) return;

  try {
    const r = await purgeLocalData({ caches: true, state: true, module_files: true });
    const lines = [
      "清除完成：",
      `  缓存项：${r.removed_caches.length} 个${r.removed_caches.length ? `（${r.removed_caches.join("、")}）` : ""}`,
      `  状态文件：${r.state_cleared ? "已清除" : "未清除"}`,
      `  模块目录：${r.removed_module_dirs.length} 个`,
    ];
    if (r.errors.length > 0) lines.push("", "错误：", ...r.errors.map((e) => `  · ${e}`));
    lines.push("", "注意：上游模块的对话/记忆数据不在此范围内，需到对应模块清除。");
    maintLog(lines.join("\n"));
    await loadModuleStates();
    renderModules();
  } catch (error) {
    maintLog(`清除失败：${describeError(error)}`);
  }
}

/** 刷新模块清单。 */
async function handleCatalog(): Promise<void> {
  if (!isDesktop()) {
    maintLog("需要桌面端才能联网拉取清单。");
    return;
  }
  maintLog("正在刷新…");
  try {
    const r = await refreshCatalog("https://dl.example.com/catalog.json");
    const lines: string[] = [];
    if (r.source === "unavailable") {
      lines.push("清单不可用。");
      if (r.reason) lines.push(`原因：${r.reason}`);
      lines.push("");
      lines.push("这是预期行为：当前版本的出网白名单为空集，");
      lines.push("任何外部请求都会被拒绝（见 docs/ALLOWLIST.md）。");
      lines.push("已安装模块的查看、启停、卸载不受影响。");
    } else {
      lines.push(`来源：${r.source}（${r.source === "cache" ? "本地缓存" : "远端"}）`);
      lines.push(`模块条目：${r.entries.length} 个`);
    }
    if (r.allowlist.length === 0) {
      lines.push("", "白名单：空集（不允许任何出网）");
    }
    maintLog(lines.join("\n"));
  } catch (error) {
    maintLog(`刷新失败：${describeError(error)}`);
  }
}

/* ------------------------------------------------------------------ */
/* 本地安装（离线可用）                                                */
/* ------------------------------------------------------------------ */

function localLog(text: string): void {
  const node = document.getElementById("local-log");
  if (!node) return;
  node.hidden = false;
  node.textContent = text;
}

/**
 * 从本地 zip 安装模块。
 *
 * 哈希在前端算完后传给后端，后端会**再算一次并比对**——
 * 前端算的哈希只用于把"期望值"传下去，真正的校验在后端做，
 * 避免前端被绕过时失去防线。
 */
async function handleLocalInstall(): Promise<void> {
  const zipInput = document.getElementById("local-zip") as HTMLInputElement | null;
  const idInput = document.getElementById("local-id") as HTMLInputElement | null;
  const catInput = document.getElementById("local-category") as HTMLSelectElement | null;

  if (!zipInput || !idInput || !catInput) return;

  const file = zipInput.files?.[0];
  if (!file) {
    localLog("请先选择一个 zip 文件。");
    return;
  }
  if (!isDesktop()) {
    localLog("需要桌面端才能安装模块。");
    return;
  }

  try {
    localLog(`正在读取 ${file.name}（${(file.size / 1024).toFixed(1)} KB）…`);
    const buffer = await file.arrayBuffer();
    const req = await buildLocalInstallRequest(buffer, idInput.value, catInput.value);
    localLog(`SHA-256: ${req.expectedSha256}\n正在安装…`);

    const result = await installModule(req);

    const trustLabel =
      result.trust.kind === "signed"
        ? `已验签（公钥 #${result.trust.key_index}）`
        : "未验签（开发模式放行）";

    localLog(
      [
        `安装成功：${result.module_id}`,
        `  文件数：${result.files}`,
        `  解压大小：${(result.bytes / 1024).toFixed(1)} KB`,
        `  ${result.replaced ? "已覆盖旧版本" : "全新安装"}`,
        `  安装位置：${result.install_dir}`,
        `  信用状态：${trustLabel}`,
      ].join("\n"),
    );

    await loadModuleStates();
    renderModules();
  } catch (error) {
    localLog(`安装失败：${describeError(error)}`);
  }
}

/* ------------------------------------------------------------------ */
/* 交互                                                                */
/* ------------------------------------------------------------------ */

el.search.addEventListener("input", renderModules);
el.filterCategory.addEventListener("change", renderModules);
el.onlyMissing.addEventListener("change", renderModules);

document.getElementById("btn-local-install")?.addEventListener("click", () => void handleLocalInstall());
document.getElementById("btn-scan")?.addEventListener("click", () => void handleScan());
document.getElementById("btn-add-local")?.addEventListener("click", () => void handleAddLocal());

el.btnCancel.addEventListener("click", () => {
  el.plan.hidden = true;
  state.selectedBundle = null;
  renderBundles();
});

el.btnExecute.addEventListener("click", () => {
  if (!state.plan) return;
  el.planLog.hidden = false;

  // 安装需要三样东西，缺一不可。这里逐条列出真实状态，
  // 而不是笼统说"尚未实现"——用户需要知道具体卡在哪。
  const t = state.trust;
  const blockers: string[] = [];

  if (t && t.trusted_keys === 0 && !t.allow_unsigned) {
    blockers.push("未配置签名公钥：未签名模块会被拒绝安装");
  }
  blockers.push("未配置模块下载源：无法取到模块包");

  const lines = [
    `安装计划「${state.plan.bundleName}」`,
    `共 ${state.plan.steps.length} 项，模型权重合计 ${state.plan.totalModelMb} MB`,
    "",
    "暂时无法开始安装，待解决：",
    ...blockers.map((b) => `  · ${b}`),
    "",
    "安装链路本身已就绪：",
    "  下载(重试/续传/镜像) → 哈希校验(必填) → 签名校验 → 防路径穿越解压 → 原子替换 → 状态记录",
    "",
    "配置方式：",
    "  · 下载源见 docs/USER_GUIDE.md「出网白名单」",
    "  · 签名密钥见 docs/HANDOVER.md 的「发布签名密钥」一节",
  ];
  el.planLog.textContent = lines.join("\n");
});

el.btnRefresh.addEventListener("click", async () => {
  el.envPill.textContent = "检测中…";
  el.envPill.dataset.tone = "warn";
  await probeEnvironment();
});

document.querySelectorAll(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    const target = (tab as HTMLElement).dataset.tab ?? "modules";
    document.querySelectorAll(".tab").forEach((t) => t.classList.remove("is-active"));
    tab.classList.add("is-active");
    for (const name of ["modules", "bundles", "env"]) {
      const panel = document.getElementById(`panel-${name}`);
      if (panel) panel.hidden = name !== target;
    }
  });
});

/* ------------------------------------------------------------------ */
/* 启动                                                                */
/* ------------------------------------------------------------------ */

async function boot(): Promise<void> {
  await loadModules();
  await loadBundles();
  await loadModuleStates();
  await loadTrustInfo();
  renderModules();
  renderBundles();
  renderEnv();
  await probeEnvironment();
}

void boot();
