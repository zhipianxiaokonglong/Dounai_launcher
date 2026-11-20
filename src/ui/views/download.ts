/**
 * 下载中心：多源、多版本、多变体的独立视图。
 *
 * 目录不再是模块页的一个模态——装包时主界面仍可操作，切走再回来筛选条件
 * 不丢。每个条目都带来源与两枚指纹（catalog 签名者、包发布者），跨源同名
 * 不做隐式优选：用户点哪一行，Rust 就按那一行的 entry_key 装。
 */

import {
  addSource,
  catalogCache,
  inspectSource,
  inspectSourceToken,
  installFromCatalog,
  isDesktop,
  listSources,
  refreshCatalog,
  removeSource,
  revokeSource,
  setSourceEnabled,
  type CatalogEntry,
  type CatalogResponse,
  type Role,
  type SourcesView,
  type SourcePreview,
  type SourceStatusView,
  type SourceView,
} from "../../bridge.js";
import {
  bindActions,
  confirmModal,
  esc,
  openModal,
  toast,
} from "../dom.js";
import { describeError, refreshViews } from "../store.js";
import { ROLE_ORDER, ROLE_TITLE } from "../roles.js";
import type { ViewSlots } from "./modules.js";

/** 视图级状态：切页不卸载模块，筛选条件与上次结果都留着。 */
const view = {
  sources: null as SourcesView | null,
  catalog: null as CatalogResponse | null,
  query: "",
  sourceId: "all",
  role: "all",
  onlyAvailable: false,
  sourcesOpen: false,
  loaded: false,
  busy: false,
};

export interface DownloadFilter {
  query: string;
  sourceId: string;
  role: string;
  onlyAvailable: boolean;
}

export interface ModuleVariantGroup {
  variant: string;
  /** 同一变体下各来源、各版本；跨源同名并列，不做隐式优选。 */
  entries: CatalogEntry[];
}

export interface ModuleGroup {
  moduleId: string;
  name: string;
  role: Role;
  variants: ModuleVariantGroup[];
}

/** 纯函数：筛选条件只影响展示，不改变缓存里到底有哪些条目。 */
export function filterEntries(
  entries: CatalogEntry[],
  filter: DownloadFilter,
): CatalogEntry[] {
  const q = filter.query.trim().toLowerCase();
  return entries.filter((entry) => {
    if (filter.sourceId !== "all" && entry.source_id !== filter.sourceId) return false;
    if (filter.role !== "all" && entry.role !== filter.role) return false;
    if (filter.onlyAvailable && entry.installed_state !== "available") return false;
    if (!q) return true;
    return `${entry.name} ${entry.module_id} ${entry.variant} ${entry.source_name}`
      .toLowerCase()
      .includes(q);
  });
}

function versionParts(value: string): [number, number, number] {
  const parts = value.split(".").map((v) => Number(v) || 0);
  return [parts[0] ?? 0, parts[1] ?? 0, parts[2] ?? 0];
}

function compareVersion(a: string, b: string): number {
  const [x0, x1, x2] = versionParts(a);
  const [y0, y1, y2] = versionParts(b);
  if (x0 !== y0) return y0 - x0;
  if (x1 !== y1) return y1 - x1;
  if (x2 !== y2) return y2 - x2;
  return a.localeCompare(b);
}

/**
 * 纯函数：按模块聚合，模块内按变体分组，变体内各来源与版本并列。
 *
 * 与旧的「来源优先」分组不同：同一个 module_id 的 stable 与 nightly
 * 现在并排在同一张卡里，用户能直接比较；但变体内跨源同名仍各自保留，
 * 不替用户做优选——优选等于偷偷替用户回答了「你信任哪个源」。
 */
export function groupModules(entries: CatalogEntry[]): ModuleGroup[] {
  const modules = new Map<string, ModuleGroup>();
  for (const entry of entries) {
    let group = modules.get(entry.module_id);
    if (!group) {
      group = {
        moduleId: entry.module_id,
        name: entry.name || entry.module_id,
        role: entry.role,
        variants: [],
      };
      modules.set(entry.module_id, group);
    }
    let variant = group.variants.find((v) => v.variant === entry.variant);
    if (!variant) {
      variant = { variant: entry.variant, entries: [] };
      group.variants.push(variant);
    }
    variant.entries.push(entry);
  }
  const variantRank = (v: string) => (v === "stable" ? 0 : v === "nightly" ? 1 : 2);
  return [...modules.values()]
    .map((group) => ({
      ...group,
      variants: group.variants
        .map((v) => ({
          ...v,
          entries: [...v.entries].sort(
            (a, b) =>
              compareVersion(a.version, b.version) || a.source_id.localeCompare(b.source_id),
          ),
        }))
        .sort(
          (a, b) => variantRank(a.variant) - variantRank(b.variant) || a.variant.localeCompare(b.variant),
        ),
    }))
    .sort(
      (a, b) =>
        ROLE_ORDER.indexOf(a.role) - ROLE_ORDER.indexOf(b.role) ||
        a.moduleId.localeCompare(b.moduleId),
    );
}

export async function renderDownload(slots: ViewSlots): Promise<void> {
  renderHead(slots.head);
  renderBody(slots.body);
  if (!view.loaded && isDesktop()) {
    view.loaded = true;
    await loadSources();
    if (document.getElementById("pbody")?.dataset.view === "download") {
      renderHead(slots.head);
      renderBody(slots.body);
    }
  }
}

/* ------------------------------------------------------------------ */
/* 页头                                                                */
/* ------------------------------------------------------------------ */

function renderHead(head: HTMLElement): void {
  const count = visibleEntries().length;
  const sources = view.sources?.sources ?? [];
  const failed = failedSources(view.catalog?.sources ?? []);
  head.innerHTML = `
    <div>
      <h1>下载</h1>
      <div class="sub"><span id="dl-count">${sources.length} 个源 · ${count} 个已验签条目</span>${
        failed.length ? ` · <span class="dl-fail-count">${failed.length} 源拉取失败</span>` : ""
      }${view.busy ? " · 处理中…" : ""}</div>
    </div>
    <div class="sp">
      <button class="btn btn--sm" type="button" data-act="manage-sources">管理源${failed.length ? ` · ${failed.length} 失败` : ""}</button>
      <button class="btn btn--primary btn--sm" type="button" data-act="refresh-and-list">${view.catalog ? "重新拉取" : "拉取目录"}</button>
    </div>
    <span id="dl-live" class="sr-only" role="status" aria-live="polite"></span>
  `;
  bindActions(head, [
    { act: "manage-sources", run: () => toggleSources(bodyElement()) },
    { act: "refresh-and-list", run: () => void loadCatalog() },
  ]);
}

function bodyElement(): HTMLElement {
  return document.getElementById("pbody") as HTMLElement;
}

/** 当前筛选条件下的条目，页头计数与结果区共用，避免两处数字对不上。 */
function visibleEntries(): CatalogEntry[] {
  const res = view.catalog;
  if (!res || res.source === "unavailable") return [];
  return filterEntries(res.entries, {
    query: view.query,
    sourceId: view.sourceId,
    role: view.role,
    onlyAvailable: view.onlyAvailable,
  });
}

/* ------------------------------------------------------------------ */
/* 正文                                                                */
/* ------------------------------------------------------------------ */

/** 从逐源状态里筛出本次拉取失败的源。disabled 是用户主动停用，不算失败。 */
export function failedSources(sources: SourceStatusView[]): SourceStatusView[] {
  return sources.filter(
    (s) => s.state === "unavailable" || s.state === "invalid",
  );
}

/** 逐源失败汇总：role=alert 让读屏软件播报，重试触发整体重新拉取。 */
function sourceFailuresHtml(): string {
  const failed = failedSources(view.catalog?.sources ?? []);
  if (!failed.length) return "";
  const rows = failed
    .map(
      (s) =>
        `<li><b>${esc(s.name)}</b> · ${
          s.state === "invalid" ? "目录验签失败" : "无法连接"
        }${s.reason ? `：${esc(s.reason)}` : ""}</li>`,
    )
    .join("");
  return `<div class="alert dl-failures" role="alert" style="margin:12px 0">
    <b>${failed.length} 个源拉取失败</b>
    <p>这些源这次没能取到可用的已验签目录，下面只展示其余成功来源的条目。</p>
    <ul style="margin:6px 0 0;padding-left:20px">${rows}</ul>
    <button class="btn btn--sm" type="button" data-act="retry-sources" style="margin-top:8px">重新拉取全部源</button>
  </div>`;
}

function renderBody(body: HTMLElement): void {
  body.classList.remove("pbody--flush", "pbody--admin");
  if (!isDesktop()) {
    body.innerHTML = `<div class="placeholder">
      <span class="ph-ico">↓</span>
      <b>浏览器模式只能预览界面</b>
      <p>源注册表与目录都在启动器的数据根里，只有桌面端能读写。请用桌面端打开。</p>
    </div>`;
    return;
  }
  body.innerHTML = `
    ${sourcesHtml()}
    ${filtersHtml()}
    ${catalogHtml()}
  `;
  bindActions(body, bodyActions(body));
  wireFilters(body);
}

/** 每次重绘都装同一张动作表：bindActions 会替换旧表，不能只给半张。 */
function bodyActions(body: HTMLElement): Array<{ act: string; run: (el: HTMLElement) => void | Promise<void> }> {
  return [
    { act: "toggle-sources", run: () => toggleSources(body) },
    { act: "add-source", run: () => openAddSource() },
    { act: "toggle-source", run: (el) => void toggleSource(el) },
    { act: "revoke-source", run: (el) => void askRevokeSource(el) },
    { act: "remove-source", run: (el) => void askRemoveSource(el) },
    { act: "install", run: (el) => void runInstall(el, false) },
    { act: "install-confirm", run: (el) => void runInstall(el, true) },
    { act: "copy-fp", run: (el) => void copyFingerprint(el) },
    { act: "retry-sources", run: () => void loadCatalog() },
    { act: "clear-filter", run: () => {
      view.query = "";
      view.sourceId = "all";
      view.role = "all";
      view.onlyAvailable = false;
      const queryEl = body.querySelector<HTMLInputElement>("#dl-query");
      const sourceEl = body.querySelector<HTMLSelectElement>("#dl-source");
      const roleEl = body.querySelector<HTMLSelectElement>("#dl-role");
      const availableEl = body.querySelector<HTMLInputElement>("#dl-available");
      if (queryEl) queryEl.value = "";
      if (sourceEl) sourceEl.value = "all";
      if (roleEl) roleEl.value = "all";
      if (availableEl) availableEl.checked = false;
      repaintResults(body);
    } },
  ];
}

async function copyFingerprint(el: HTMLElement): Promise<void> {
  const value = el.dataset.value ?? "";
  try {
    await navigator.clipboard.writeText(value);
    toast("已复制指纹", "ok");
  } catch (error) {
    toast(`复制失败：${describeError(error)}`, "error");
  }
}

function toggleSources(body: HTMLElement): void {
  view.sourcesOpen = !view.sourcesOpen;
  renderBody(body);
  if (view.sourcesOpen) {
    body.querySelector<HTMLElement>(".dl-sources")?.scrollIntoView({ block: "nearest" });
  }
}

function sourcesHtml(): string {
  const sources = view.sources?.sources ?? [];
  if (sources.length === 0) {
    return `<p class="cap" style="margin:0 0 14px">正在读取源列表…</p>`;
  }
  const enabled = sources.filter((s) => s.enabled).length;
  const failed = failedSources(view.catalog?.sources ?? []).length;
  const summary = `${sources.length} 个源 · ${enabled} 启用${failed ? ` · ${failed} 失败` : ""}`;
  if (!view.sourcesOpen) {
    return `<section class="dl-sources">
      <button class="dl-sources-summary" type="button" data-act="toggle-sources" aria-expanded="false">
        <b>下载源</b><span class="cap">${esc(summary)}</span><span class="cap">管理 ▾</span>
      </button>
    </section>`;
  }
  return `<section class="dl-sources">
    <button class="dl-sources-summary" type="button" data-act="toggle-sources" aria-expanded="true">
      <b>下载源</b><span class="cap">${esc(summary)}</span><span class="cap">收起 ▴</span>
    </button>
    <p class="cap" style="margin:6px 0 8px">验 catalog 的公钥只在这里登记，与包信任表分开</p>
    <button class="btn btn--sm" type="button" data-act="add-source" style="margin-bottom:8px">添加源</button>
    ${sources.map(sourceRow).join("")}
    <p class="cap dl-allowlist">允许访问的域名：${esc((view.sources?.allowlist ?? []).join("、") || "（空集，不会发出远程请求）")}</p>
  </section>`;
}

function sourceRow(source: SourceView): string {
  const kindChip = source.kind === "official"
    ? `<span class="chip chip--run">官方</span>`
    : `<span class="chip">社区</span>`;
  const stateChip = source.revoked
    ? `<span class="chip chip--fail">公钥已撤销</span>`
    : source.enabled
      ? `<span class="chip chip--run">已启用</span>`
      : `<span class="chip chip--stop">已禁用</span>`;
  const controls = source.revoked
    ? (source.deletable ? `<button class="btn btn--sm" type="button" data-act="remove-source" data-id="${esc(source.id)}">删除</button>` : "")
    : `${source.enabled
        ? `<button class="btn btn--sm" type="button" data-act="toggle-source" data-id="${esc(source.id)}" data-enabled="false">禁用</button>`
        : `<button class="btn btn--sm btn--primary" type="button" data-act="toggle-source" data-id="${esc(source.id)}" data-enabled="true">启用</button>`}
      <button class="btn btn--sm btn--danger" type="button" data-act="revoke-source" data-id="${esc(source.id)}">撤销公钥</button>
      ${source.deletable ? `<button class="btn btn--sm" type="button" data-act="remove-source" data-id="${esc(source.id)}">删除</button>` : ""}`;
  return `<div class="dl-source${source.enabled ? "" : " is-off"}">
    <div class="dl-source-main">
      <div><b>${esc(source.name)}</b> ${kindChip} ${stateChip}</div>
      <span class="cap">${esc(source.base_url)}</span>
      <code class="fp">catalog 指纹 ${esc(source.catalog_fingerprint)}</code>
      <span class="cap">白名单：${esc(source.host_suffixes.join("、"))} · 自动更新：${source.auto_update ? "开" : "关"}${source.sequence ? ` · 水位 ${source.sequence}` : ""}</span>
      ${
        source.last_error
          ? `<span class="cap" style="color:var(--danger)">上次刷新失败（${esc(source.last_error_at ?? "")}）：${esc(source.last_error)}</span>`
          : ""
      }
    </div>
    <div class="dl-source-side">${controls}</div>
  </div>`;
}

function filtersHtml(): string {
  const sources = view.sources?.sources ?? [];
  const options = sources
    .map((s) => `<option value="${esc(s.id)}"${view.sourceId === s.id ? " selected" : ""}>${esc(s.name)}</option>`)
    .join("");
  const roles = ROLE_ORDER.map(
    (r) => `<option value="${r}"${view.role === r ? " selected" : ""}>${esc(ROLE_TITLE[r])}</option>`,
  ).join("");
  return `<div class="dl-filters">
    <input id="dl-query" class="input" placeholder="搜索名称或模块 ID" value="${esc(view.query)}" />
    <select id="dl-source" class="input"><option value="all">全部来源</option>${options}</select>
    <select id="dl-role" class="input"><option value="all">全部角色</option>${roles}</select>
    <label class="dl-check"><input id="dl-available" type="checkbox"${view.onlyAvailable ? " checked" : ""} />只看可安装</label>
    <button class="btn btn--sm" type="button" data-act="clear-filter">清空</button>
  </div>`;
}

/** 只重绘结果区，同时同步页头计数；不重建页头按钮，避免打断焦点。 */
function repaintResults(body: HTMLElement): void {
  const host = body.querySelector("#dl-results");
  if (host) {
    host.innerHTML = resultsHtml();
    bindActions(body, bodyActions(body));
  }
  const sourceCount = view.sources?.sources.length ?? 0;
  const countEl = document.getElementById("dl-count");
  if (countEl) {
    countEl.textContent = `${sourceCount} 个源 · ${visibleEntries().length} 个已验签条目`;
  }
}

function wireFilters(body: HTMLElement): void {
  body.querySelector<HTMLInputElement>("#dl-query")?.addEventListener("input", (event) => {
    view.query = (event.target as HTMLInputElement).value;
    // 只重绘列表区，避免输入框在每次按键后失焦。
    repaintResults(body);
  });
  body.querySelector<HTMLSelectElement>("#dl-source")?.addEventListener("change", (event) => {
    view.sourceId = (event.target as HTMLSelectElement).value;
    repaintResults(body);
  });
  body.querySelector<HTMLSelectElement>("#dl-role")?.addEventListener("change", (event) => {
    view.role = (event.target as HTMLSelectElement).value;
    repaintResults(body);
  });
  body.querySelector<HTMLInputElement>("#dl-available")?.addEventListener("change", (event) => {
    view.onlyAvailable = (event.target as HTMLInputElement).checked;
    repaintResults(body);
  });
}

function catalogHtml(): string {
  return `<div id="dl-results">${resultsHtml()}</div>`;
}

function resultsHtml(): string {
  const res = view.catalog;
  if (!res) return `<p class="cap" style="margin:10px 0 0">点右上「拉取目录」查看已验签条目。官方 catalog 尚未签发时，这里会如实显示不可用原因。</p>`;
  const failures = sourceFailuresHtml();
  if (res.source === "unavailable") {
    return `${failures}<div class="placeholder"><b>目录不可用</b><p>没有跳过验签或「仍然继续」的入口。允许访问的域名：${esc(res.allowlist.join("、") || "（空集）")}</p></div>`;
  }
  const entries = filterEntries(res.entries, {
    query: view.query,
    sourceId: view.sourceId,
    role: view.role,
    onlyAvailable: view.onlyAvailable,
  });
  const note = res.reason ? `<p class="cap" style="margin:10px 0">${esc(res.reason)}</p>` : "";
  if (entries.length === 0) return `${failures}${note}<p class="cap" style="margin:10px 0 0">没有符合条件的条目。</p>`;
  const groups = groupModules(entries);
  return `${failures}${note}<div class="dl-groups">${groups.map(groupHtml).join("")}</div>`;
}

/** 模块卡：卡头是模块名与角色，下面每个变体一段，stable 在前、nightly 在后。 */
function groupHtml(group: ModuleGroup): string {
  return `<section class="dl-group">
    <div class="dl-group-head">
      <b>${esc(group.name)}</b>
      <span class="chip">${esc(ROLE_TITLE[group.role] ?? group.role)}</span>
      <span class="cap">${esc(group.moduleId)}</span>
    </div>
    ${group.variants.map(variantHtml).join("")}
  </section>`;
}

function variantHtml(variant: ModuleVariantGroup): string {
  const tag = variant.variant === "nightly"
    ? `<span class="chip chip--warn">每日构建</span>`
    : `<span class="chip">${esc(variant.variant)}</span>`;
  return `<div class="dl-variant">
    <div class="dl-variant-head">${tag}<span class="cap">${variant.entries.length} 个版本</span></div>
    ${variant.entries.map(entryRow).join("")}
  </div>`;
}

function stateChip(entry: CatalogEntry): string {
  if (entry.source_revoked) return `<span class="chip chip--fail">发布者已不再受信任</span>`;
  if (entry.revoked || entry.installed_state === "downgrade") return `<span class="chip chip--fail">版本回退 / 已撤销</span>`;
  if (entry.installed_state === "incompatible") return `<span class="chip chip--fail">与当前启动器不兼容</span>`;
  if (entry.installed_state === "upgrade") return `<span class="chip chip--run">可升级</span>`;
  if (entry.installed_state === "installed") return `<span class="chip">已安装</span>`;
  return "";
}

/** 指纹里的类型前缀与值分开：值单独成段，复制只取值本身。 */
function fingerprintLine(label: string, value: string, short = false): string {
  const text = esc(value);
  const shown = short && value.length > 24 ? `${esc(value.slice(0, 12))}…${esc(value.slice(-8))}` : text;
  return `<div class="dl-fp-row">
    <span class="cap">${esc(label)}</span>
    <code class="fp" title="${text}">${shown}</code>
    <button class="btn btn--sm" type="button" data-act="copy-fp" data-value="${text}" aria-label="复制${esc(label)}">复制</button>
  </div>`;
}

/** 条目行：默认只显示决策信息；三枚指纹收进原生 details，键盘可达、无需额外状态。 */
function entryRow(entry: CatalogEntry): string {
  const blocked = entry.source_revoked || entry.revoked || entry.installed_state === "downgrade"
    || entry.installed_state === "incompatible";
  const label = blocked ? "无法安装"
    : entry.installed_state === "installed" ? "重装"
    : entry.installed_state === "upgrade" ? "升级" : "安装";
  return `<div class="cat-row" data-key="${esc(entry.entry_key)}">
    <div class="cat-main">
      <div><b>v${esc(entry.version)}</b> ${stateChip(entry)}</div>
      <span class="cap">${esc(entry.source_name)}（${entry.source_kind === "official" ? "官方" : "社区"}） · ${formatBytes(entry.size)}${entry.note ? ` · ${esc(entry.note)}` : ""}</span>
      <details class="dl-verify">
        <summary>验签详情</summary>
        <div class="dl-verify-body">
          ${fingerprintLine("catalog 指纹", entry.catalog_fingerprint)}
          ${fingerprintLine("发布者指纹", entry.publisher_fp)}
          ${fingerprintLine("包 sha256", entry.sha256)}
          <p class="cap">安装时按这三项逐条核验；信任下载位置不等于信任发布者签的任意包。</p>
        </div>
      </details>
    </div>
    <div class="cat-side">
      <button class="btn btn--sm${blocked ? "" : " btn--primary"}" type="button" data-act="install"${blocked ? " disabled" : ""}>${label}</button>
    </div>
  </div>`;
}

function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)} KiB`;
  return `${bytes} B`;
}

/* ------------------------------------------------------------------ */
/* 动作                                                                */
/* ------------------------------------------------------------------ */

async function loadSources(): Promise<void> {
  try {
    view.sources = await listSources();
  } catch (error) {
    toast(`读取源列表失败：${describeError(error)}`, "error");
    return;
  }
  refreshCurrent();
}

async function loadCatalog(): Promise<void> {
  view.busy = true;
  refreshCurrent();
  let announcement = "";
  try {
    view.catalog = await refreshCatalog();
    view.sources = await listSources();
    const failed = failedSources(view.catalog.sources).length;
    if (view.catalog.source === "unavailable") {
      announcement = `目录不可用：${view.catalog.reason ?? "未知原因"}`;
      toast(announcement, "error");
    } else {
      announcement = `已拉取 ${view.catalog.entries.length} 条，${failed ? `${failed} 个源失败，` : ""}来源 ${view.catalog.source}`;
      toast(`已拉取 ${view.catalog.entries.length} 条（${view.catalog.source}）`, "ok");
    }
  } catch (error) {
    announcement = `拉取目录失败：${describeError(error)}`;
    toast(announcement, "error");
  } finally {
    view.busy = false;
    refreshCurrent();
    announce(announcement);
  }
}

/** 拉取结果写进页头的 live region；节点可能刚被 refreshCurrent 重建，按 id 现查。 */
function announce(message: string): void {
  if (!message) return;
  const el = document.getElementById("dl-live");
  if (el) el.textContent = message;
}

function refreshCurrent(): void {
  const head = document.getElementById("phead");
  const body = document.getElementById("pbody");
  if (head && body && body.dataset.view === "download") {
    renderHead(head);
    renderBody(body);
  }
}

async function toggleSource(el: HTMLElement): Promise<void> {
  const id = el.dataset.id ?? "";
  const enabled = el.dataset.enabled === "true";
  try {
    view.sources = await setSourceEnabled(id, enabled);
    toast(`${id} 已${enabled ? "启用" : "禁用"}`, "ok");
    refreshCurrent();
  } catch (error) {
    toast(`切换失败：${describeError(error)}`, "error");
  }
}

function askRevokeSource(el: HTMLElement): void {
  const id = el.dataset.id ?? "";
  confirmModal({
    title: "撤销该源的 catalog 公钥",
    subtitle: id,
    bodyHtml: `<p style="margin:0 0 8px">撤销后该源立即停用，已安装的模块不会卸载，也不会自动更新。</p>
      <p class="cap" style="margin:0">这把钥匙只签目录，撤销不会写进包信任表。</p>`,
    confirmLabel: "撤销并停用",
    danger: true,
    onConfirm: async () => {
      try {
        const result = await revokeSource(id);
        view.sources = result.sources;
        if (view.catalog) view.catalog.entries = view.catalog.entries.filter((e) => e.source_id !== id);
        toast(result.affected.length
          ? `已停用 ${id}；${result.affected.length} 个实例标为「发布者已不再受信任」`
          : `已停用 ${id}`, "ok");
        refreshCurrent();
      } catch (error) {
        toast(`撤销失败：${describeError(error)}`, "error");
      }
    },
  });
}

function askRemoveSource(el: HTMLElement): void {
  const id = el.dataset.id ?? "";
  confirmModal({
    title: "删除下载源",
    subtitle: id,
    bodyHtml: `<p style="margin:0">只删除这条源配置，不动已安装的模块，也不写包信任表。内置官方源不能删除。</p>`,
    confirmLabel: "删除",
    danger: true,
    onConfirm: async () => {
      try {
        view.sources = await removeSource(id);
        if (view.catalog) view.catalog.entries = view.catalog.entries.filter((e) => e.source_id !== id);
        toast(`已删除源 ${id}`, "ok");
        refreshCurrent();
      } catch (error) {
        toast(`删除失败：${describeError(error)}`, "error");
      }
    },
  });
}

async function runInstall(button: HTMLElement, confirm: boolean): Promise<void> {
  const row = button.closest<HTMLElement>(".cat-row");
  const key = row?.dataset.key ?? "";
  if (!key) return;
  const entry = view.catalog?.entries.find((e) => e.entry_key === key);
  const idleLabel = button.textContent ?? "安装";
  button.setAttribute("disabled", "disabled");
  button.textContent = "下载中…";
  try {
    const res = await installFromCatalog(key, confirm);
    const slot = res.activated ? "，已设为该角色的主实例（占用规范端口）" : "";
    toast(`已安装 ${res.module_id}：${res.files} 个文件，${formatBytes(res.bytes)}${slot}`, "ok");
    // 只读会话缓存刷新状态，不重新拉网络：拉失败会把刚装好的条目一起清掉。
    view.catalog = await catalogCache();
    // 实例表也跟着变了一次。这里不重绘下载页（它在 finally 里自己刷），
    // 但不刷新 store 的话，「模块」页还停在启动时那份快照上——刚装好的
    // 模块（比如总装层）会看不见，直到下一次生命周期推送。
    await refreshViews().catch(() => undefined);
    window.dispatchEvent(new CustomEvent("dounai:views-updated"));
  } catch (error) {
    const message = describeError(error);
    button.removeAttribute("disabled");
    if (!confirm && message.includes("需要用户显式确认")) {
      // 两段式确认：判断由 Rust 做，界面只把来源与两枚指纹如实摆出来。
      confirmModal({
        title: "确认安装这个包",
        subtitle: `${entry?.source_name ?? "未知来源"} · ${entry?.module_id ?? key}`,
        bodyHtml: `<p style="margin:0 0 8px">${esc(message)}</p>
          <code class="fp">catalog 指纹 ${esc(entry?.catalog_fingerprint ?? "")}</code>
          <code class="fp">发布者指纹 ${esc(entry?.publisher_fp ?? "")}</code>
          <p class="cap" style="margin:8px 0 0">信任一个下载位置不等于信任这个发布者签的任意包；包的信任判定仍走四级。</p>`,
        confirmLabel: "确认并安装",
        danger: true,
        onConfirm: async () => {
          const fresh = document.querySelector<HTMLElement>(`.cat-row[data-key="${CSS.escape(key)}"] [data-act="install"]`);
          if (fresh) await runInstall(fresh, true);
        },
      });
      button.textContent = idleLabel;
      return;
    }
    button.textContent = idleLabel;
    toast(`安装失败：${message}`, "error");
  } finally {
    refreshCurrent();
  }
}

/* ------------------------------------------------------------------ */
/* 添加源向导：URL + 公钥 → 完整指纹 → 确认（默认焦点在取消）          */
/* ------------------------------------------------------------------ */

function openAddSource(): void {
  openModal({
    title: "添加下载源",
    subtitle: "推荐粘贴源发布的一行邀请码，也可手动填写",
    wide: true,
    bodyHtml: `
      <div class="frow"><div class="lab">源邀请码<small>源发布的一行字符串；粘贴后点「解析邀请码」。解析只算指纹，不写文件</small></div>
        <div class="ctl"><textarea id="src-token" class="input" rows="2" placeholder="dnsrc1:...（可选）"></textarea></div></div>
      <div class="frow"><div class="lab">或手动填写<small>下面三项与邀请码二选一</small></div><div class="ctl"></div></div>
      <div class="frow"><div class="lab">源地址<small>catalog.json 所在的目录，须用 https；本机回环测试源可用 http</small></div>
        <div class="ctl"><input id="src-url" class="input" placeholder="https://example.org/releases" /></div></div>
      <div class="frow"><div class="lab">catalog 公钥<small>Ed25519 公钥原始 32 字节的 base64</small></div>
        <div class="ctl"><textarea id="src-key" class="input" rows="2" placeholder="粘贴公钥，或公钥文件路径的内容"></textarea></div></div>
      <div class="frow"><div class="lab">域名白名单<small>留空自动从源地址派生；一行一个，不能填公共后缀</small></div>
        <div class="ctl"><textarea id="src-hosts" class="input" rows="2" placeholder="留空即自动派生"></textarea></div></div>
      <div class="frow"><div class="lab">名称与标识<small>标识只用于本机区分来源</small></div>
        <div class="ctl" style="display:flex;gap:8px">
          <input id="src-name" class="input" placeholder="名称（可留空）" />
          <input id="src-id" class="input" placeholder="id（可留空）" />
        </div></div>
      <div id="src-error" class="cap" style="margin:8px 0 0"></div>
    `,
    footNote: "下一步只计算指纹，不写任何文件",
    actions: [
      { label: "取消", tone: "quiet", autofocus: true },
      {
        label: "解析邀请码",
        tone: "primary",
        onClick: async (root) => {
          const token = root.querySelector<HTMLTextAreaElement>("#src-token")?.value.trim() ?? "";
          const errorHost = root.querySelector<HTMLElement>("#src-error");
          if (errorHost) errorHost.textContent = "";
          if (!token) {
            if (errorHost) errorHost.textContent = "请先粘贴一行 dnsrc1: 开头的邀请码";
            return false;
          }
          try {
            const preview = await inspectSourceToken(token);
            // 确认框会复用同一个 overlay 节点：openConfirmSource 内部先
            // closeModal 再重建。必须返回 false 拦住外层收尾，否则它会把
            // 刚打开的确认框一起关掉，表现就是弹窗消失、源没添加。
            openConfirmSource(preview);
            return false;
          } catch (error) {
            if (errorHost) errorHost.textContent = describeError(error);
            return false;
          }
        },
      },
      {
        label: "手动下一步",
        tone: "quiet",
        onClick: async (root) => {
          const url = root.querySelector<HTMLInputElement>("#src-url")?.value.trim() ?? "";
          const pubkey = root.querySelector<HTMLTextAreaElement>("#src-key")?.value.trim() ?? "";
          const hostsRaw = root.querySelector<HTMLTextAreaElement>("#src-hosts")?.value.trim() ?? "";
          const hosts = hostsRaw ? hostsRaw.split(/[\n,]/).map((s) => s.trim()).filter(Boolean) : null;
          const errorHost = root.querySelector<HTMLElement>("#src-error");
          if (errorHost) errorHost.textContent = "";
          try {
            const preview = await inspectSource({
              baseUrl: url,
              catalogPubkey: pubkey,
              hostSuffixes: hosts,
            });
            const name = root.querySelector<HTMLInputElement>("#src-name")?.value.trim() || preview.name;
            const id = root.querySelector<HTMLInputElement>("#src-id")?.value.trim() || preview.id;
            openConfirmSource({ ...preview, name, id });
            // 同上：确认框已接管 overlay，拦住外层关闭。
            return false;
          } catch (error) {
            if (errorHost) errorHost.textContent = describeError(error);
            return false;
          }
        },
      },
    ],
    onMount: (root) => root.querySelector<HTMLTextAreaElement>("#src-token")?.focus(),
  });
}

function openConfirmSource(preview: SourcePreview): void {
  confirmModal({
    title: "确认信任这个源的 catalog 公钥",
    subtitle: preview.base_url,
    bodyHtml: `
      <p style="margin:0 0 8px">你正在把<b>一个下载位置</b>交给启动器。此后该源签发的模块目录会被本地这把公钥验签；它不会被写进包信任表。</p>
      <code class="fp">完整指纹 ${esc(preview.catalog_fingerprint)}</code>
      <code class="fp">公钥 ${esc(preview.catalog_pubkey)}</code>
      <p class="cap" style="margin:8px 0 0">允许访问的域名：${esc(preview.host_suffixes.join("、"))}${preview.derived ? "（从源地址派生）" : "（你手工填写）"}</p>
      <p class="cap" style="margin:4px 0 0">社区源只提示有新版，不会自动安装。</p>
      <p class="cap" style="margin:4px 0 0">${esc(preview.note)}</p>
    `,
    confirmLabel: "确认并添加",
    danger: true,
    onConfirm: async () => {
      try {
        view.sources = await addSource({
          id: preview.id,
          name: preview.name,
          base_url: preview.base_url,
          catalog_pubkey: preview.catalog_pubkey,
          host_suffixes: preview.host_suffixes,
          confirm: true,
        });
        toast(`已添加源 ${preview.id}，请刷新目录`, "ok");
        refreshCurrent();
      } catch (error) {
        toast(`添加源失败：${describeError(error)}`, "error");
      }
    },
  });
}
