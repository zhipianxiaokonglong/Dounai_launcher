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
  type SourceView,
} from "../../bridge.js";
import {
  bindActions,
  confirmModal,
  esc,
  openModal,
  toast,
} from "../dom.js";
import { describeError } from "../store.js";
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
  loaded: false,
  busy: false,
};

export interface DownloadFilter {
  query: string;
  sourceId: string;
  role: string;
  onlyAvailable: boolean;
}

export interface DownloadGroup {
  sourceId: string;
  sourceName: string;
  role: Role;
  moduleId: string;
  variant: string;
  /** 同一 module_id@variant 的多个版本并列，按版本降序。 */
  entries: CatalogEntry[];
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

/** 纯函数：来源 → role → module_id → variant 分组，组内多版本并列。 */
export function groupEntries(entries: CatalogEntry[]): DownloadGroup[] {
  const groups = new Map<string, DownloadGroup>();
  for (const entry of entries) {
    const key = [entry.source_id, entry.role, entry.module_id, entry.variant].join("|");
    const group = groups.get(key);
    if (group) group.entries.push(entry);
    else
      groups.set(key, {
        sourceId: entry.source_id,
        sourceName: entry.source_name,
        role: entry.role,
        moduleId: entry.module_id,
        variant: entry.variant,
        entries: [entry],
      });
  }
  return [...groups.values()]
    .map((group) => ({
      ...group,
      entries: [...group.entries].sort((a, b) => compareVersion(a.version, b.version)),
    }))
    .sort((a, b) =>
      a.sourceId.localeCompare(b.sourceId) ||
      a.role.localeCompare(b.role) ||
      a.moduleId.localeCompare(b.moduleId) ||
      a.variant.localeCompare(b.variant),
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
  const count = view.catalog?.entries.length ?? 0;
  const enabled = view.sources?.sources.filter((s) => s.enabled).length ?? 0;
  head.innerHTML = `
    <div>
      <h1>下载</h1>
      <div class="sub">${enabled} 个启用源 · ${count} 个已验签条目${view.busy ? " · 处理中…" : ""}</div>
    </div>
    <div class="sp">
      <button class="btn btn--sm" type="button" data-act="add-source">添加源</button>
      <button class="btn btn--sm" type="button" data-act="refresh">刷新目录</button>
      <button class="btn btn--primary btn--sm" type="button" data-act="refresh-and-list">${view.catalog ? "重新拉取" : "拉取目录"}</button>
    </div>
  `;
  bindActions(head, [
    { act: "add-source", run: () => openAddSource() },
    { act: "refresh", run: () => void loadSources() },
    { act: "refresh-and-list", run: () => void loadCatalog() },
  ]);
}

/* ------------------------------------------------------------------ */
/* 正文                                                                */
/* ------------------------------------------------------------------ */

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
    { act: "toggle-source", run: (el) => void toggleSource(el) },
    { act: "revoke-source", run: (el) => void askRevokeSource(el) },
    { act: "remove-source", run: (el) => void askRemoveSource(el) },
    { act: "install", run: (el) => void runInstall(el, false) },
    { act: "install-confirm", run: (el) => void runInstall(el, true) },
    { act: "clear-filter", run: () => {
      view.query = "";
      view.sourceId = "all";
      view.role = "all";
      view.onlyAvailable = false;
      renderBody(body);
    } },
  ];
}

function sourcesHtml(): string {
  const sources = view.sources?.sources ?? [];
  if (sources.length === 0) {
    return `<p class="cap" style="margin:0 0 14px">正在读取源列表…</p>`;
  }
  return `<section class="dl-sources">
    <div class="dl-section-head"><b>下载源</b><span class="cap">验 catalog 的公钥只在这里登记，与包信任表分开</span></div>
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
      <button class="btn btn--sm" type="button" data-act="revoke-source" data-id="${esc(source.id)}">撤销公钥</button>
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

function wireFilters(body: HTMLElement): void {
  body.querySelector<HTMLInputElement>("#dl-query")?.addEventListener("input", (event) => {
    view.query = (event.target as HTMLInputElement).value;
    // 只重绘列表区，避免输入框在每次按键后失焦。
    const host = body.querySelector("#dl-results");
    if (host) {
      host.innerHTML = resultsHtml();
      bindActions(body, bodyActions(body));
    }
  });
  body.querySelector<HTMLSelectElement>("#dl-source")?.addEventListener("change", (event) => {
    view.sourceId = (event.target as HTMLSelectElement).value;
    const host = body.querySelector("#dl-results");
    if (host) {
      host.innerHTML = resultsHtml();
      bindActions(body, bodyActions(body));
    }
  });
  body.querySelector<HTMLSelectElement>("#dl-role")?.addEventListener("change", (event) => {
    view.role = (event.target as HTMLSelectElement).value;
    const host = body.querySelector("#dl-results");
    if (host) {
      host.innerHTML = resultsHtml();
      bindActions(body, bodyActions(body));
    }
  });
  body.querySelector<HTMLInputElement>("#dl-available")?.addEventListener("change", (event) => {
    view.onlyAvailable = (event.target as HTMLInputElement).checked;
    const host = body.querySelector("#dl-results");
    if (host) {
      host.innerHTML = resultsHtml();
      bindActions(body, bodyActions(body));
    }
  });
}

function catalogHtml(): string {
  return `<div id="dl-results">${resultsHtml()}</div>`;
}

function resultsHtml(): string {
  const res = view.catalog;
  if (!res) return `<p class="cap" style="margin:10px 0 0">点右上「拉取目录」查看已验签条目。官方 catalog 尚未签发时，这里会如实显示不可用原因。</p>`;
  const note = res.reason ? `<p class="cap" style="margin:10px 0">${esc(res.reason)}</p>` : "";
  if (res.source === "unavailable") {
    return `${note}<div class="placeholder"><b>目录不可用</b><p>没有跳过验签或「仍然继续」的入口。允许访问的域名：${esc(res.allowlist.join("、") || "（空集）")}</p></div>`;
  }
  const entries = filterEntries(res.entries, {
    query: view.query,
    sourceId: view.sourceId,
    role: view.role,
    onlyAvailable: view.onlyAvailable,
  });
  if (entries.length === 0) return `${note}<p class="cap" style="margin:10px 0 0">没有符合条件的条目。</p>`;
  const groups = groupEntries(entries);
  return `${note}<div class="dl-groups">${groups.map(groupHtml).join("")}</div>`;
}

function groupHtml(group: DownloadGroup): string {
  return `<section class="dl-group">
    <div class="dl-group-head">
      <b>${esc(group.sourceName)}</b>
      <span class="cap">${esc(ROLE_TITLE[group.role] ?? group.role)} · ${esc(group.moduleId)}@${esc(group.variant)} · ${group.entries.length} 个版本</span>
    </div>
    ${group.entries.map(entryRow).join("")}
  </section>`;
}

function stateChip(entry: CatalogEntry): string {
  if (entry.source_revoked) return `<span class="chip chip--fail">发布者已不再受信任</span>`;
  if (entry.revoked || entry.installed_state === "downgrade") return `<span class="chip chip--fail">版本回退 / 已撤销</span>`;
  if (entry.installed_state === "incompatible") return `<span class="chip chip--fail">与当前启动器不兼容</span>`;
  if (entry.installed_state === "upgrade") return `<span class="chip chip--run">可升级</span>`;
  if (entry.installed_state === "installed") return `<span class="chip">已安装</span>`;
  return "";
}

function entryRow(entry: CatalogEntry): string {
  const blocked = entry.source_revoked || entry.revoked || entry.installed_state === "downgrade"
    || entry.installed_state === "incompatible";
  const label = blocked ? "无法安装"
    : entry.installed_state === "installed" ? "重装"
    : entry.installed_state === "upgrade" ? "升级" : "安装";
  return `<div class="cat-row" data-key="${esc(entry.entry_key)}">
    <div class="cat-main">
      <div><b>${esc(entry.name || entry.module_id)}</b> ${stateChip(entry)}</div>
      <span class="cap">v${esc(entry.version)} · ${esc(entry.source_name)}（${entry.source_kind === "official" ? "官方" : "社区"}） · ${formatBytes(entry.size)}</span>
      <code class="fp">catalog ${esc(entry.catalog_fingerprint)}</code>
      <code class="fp">publisher ${esc(entry.publisher_fp)}</code>
      <code class="fp">${esc(entry.sha256)}</code>
      ${entry.note ? `<span class="cap">${esc(entry.note)}</span>` : ""}
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
  try {
    view.catalog = await refreshCatalog();
    view.sources = await listSources();
    if (view.catalog.source === "unavailable") {
      toast(`目录不可用：${view.catalog.reason ?? "未知原因"}`, "error");
    } else {
      toast(`已拉取 ${view.catalog.entries.length} 条（${view.catalog.source}）`, "ok");
    }
  } catch (error) {
    toast(`拉取目录失败：${describeError(error)}`, "error");
  } finally {
    view.busy = false;
    refreshCurrent();
  }
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
    subtitle: "带外取得的 catalog 签名公钥",
    wide: true,
    bodyHtml: `
      <div class="frow"><div class="lab">源地址<small>catalog.json 所在的目录，必须 https</small></div>
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
        label: "下一步",
        tone: "primary",
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
            return true;
          } catch (error) {
            if (errorHost) errorHost.textContent = describeError(error);
            return false;
          }
        },
      },
    ],
    onMount: (root) => root.querySelector<HTMLInputElement>("#src-url")?.focus(),
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
