/**
 * 导入本地：两条互不相同的路。
 *
 * 1. 本地包（.zip）：按离线包安装，验签、判级、解压到包目录，可卸载。
 * 2. 本地目录：只登记，不复制也不删除文件——这句话必须出现在页面上，
 *    因为「导入」在别的软件里通常意味着搬运。
 */

import {
  addLocalModule,
  addLocalModulesBulk,
  inspectLocalPackage,
  installLocalPackage,
  isDesktop,
  pickPackageFile,
  removeLocalModule,
  scanLocalModules,
  type LocalPackagePreview,
  type ScanCandidate,
} from "../../bridge.js";
import { bindActions, confirmModal, esc, toast } from "../dom.js";
import { icon } from "../icons.js";
import { describeError, refreshViews, store } from "../store.js";
import type { ViewSlots } from "./modules.js";

type Role = "input" | "llm" | "output" | "shell" | "auxiliary";

const ROLE_TITLE: Record<Role, string> = {
  input: "输入",
  llm: "模型",
  output: "输出",
  shell: "总装",
  auxiliary: "旁路",
};

const dirState: {
  root: string;
  candidates: ScanCandidate[];
  /** 扫描或登记的结果说明；空串表示还没做过动作。 */
  note: string;
} = {
  root: "",
  candidates: [],
  note: "",
};

const pkgState: {
  path: string;
  preview: LocalPackagePreview | null;
  note: string;
  busy: boolean;
} = {
  path: "",
  preview: null,
  note: "",
  busy: false,
};

export function renderImport(slots: ViewSlots): void {
  renderHead(slots.head);
  renderBody(slots.body);
}

function renderHead(head: HTMLElement): void {
  head.innerHTML = `
    <div>
      <h1>导入本地</h1>
      <div class="sub">装一个离线包，或登记你已有的源码目录</div>
    </div>
    <div class="sp">
      <button class="btn btn--sm" type="button" data-act="goto-modules">返回模块</button>
    </div>
  `;
  bindActions(head, [{ act: "goto-modules", run: () => navigate("modules") }]);
}

function renderBody(body: HTMLElement): void {
  body.innerHTML = `
    <div class="field">
      <label for="pkg-path">模块包（.zip）</label>
      <div class="toolbar" style="margin-bottom:0">
        <input id="pkg-path" class="input grow" type="text" spellcheck="false"
               placeholder="选择或粘贴一个 .zip 的路径" value="${esc(pkgState.path)}" />
        <button class="btn" type="button" data-act="pick">选择文件…</button>
        <button class="btn btn--primary" type="button" data-act="inspect">检查</button>
      </div>
      <div class="cap">选好包后先点「检查」：会读包内清单，告诉你装的是什么、谁签的名、要不要你确认。检查不会写入任何文件。</div>
    </div>

    <div id="pkg-result" style="margin-top:14px"></div>

    <div class="field" style="margin-top:26px">
      <label for="import-root">模块目录（源码，含 manifest.json）</label>
      <div class="toolbar" style="margin-bottom:0">
        <input id="import-root" class="input grow" type="text" spellcheck="false"
               placeholder="例如 E:\\Dounai-ai\\OmniNexus" value="${esc(dirState.root)}" />
        <button class="btn" type="button" data-act="scan">扫描目录</button>
        <button class="btn btn--primary" type="button" data-act="add">登记目录</button>
      </div>
      <div class="cap">扫描会读取该目录和它下面一层子目录，找出含 <code>manifest.json</code> 的模块。模型权重（.gguf、.safetensors）不是模块，扫描不会列出。</div>
    </div>

    <div class="alert" style="margin:14px 0">
      <b>第三方与本地源码模块不在沙箱内</b>
      <div>这些模块以你的权限运行，启动器只能约束进程与端口，不能限制模块读写本机文件。只装或只登记你信任来源的东西。</div>
    </div>

    <div id="import-result"></div>
  `;

  const pkgInput = body.querySelector<HTMLInputElement>("#pkg-path");
  pkgInput?.addEventListener("input", () => {
    pkgState.path = pkgInput.value;
  });

  const input = body.querySelector<HTMLInputElement>("#import-root");
  input?.addEventListener("input", () => {
    dirState.root = input.value;
  });

  bindActions(body, [
    { act: "pick", run: () => pick() },
    { act: "inspect", run: () => inspect() },
    { act: "install", run: () => install() },
    { act: "drop-preview", run: () => dropPreview() },
    { act: "goto-modules", run: () => navigate("modules") },
    { act: "scan", run: () => scan() },
    { act: "add", run: () => addOne(dirState.root) },
    { act: "add-path", run: (node) => addOne(node.dataset.path ?? "") },
    { act: "bulk-add", run: () => addAllPending() },
    { act: "unlink", run: (node) => unlink(node.dataset.id ?? "") },
  ]);

  renderPkgResult();
  renderResult();
}

/* ------------------------------------------------------------------ */
/* 本地包                                                              */
/* ------------------------------------------------------------------ */

function renderPkgResult(): void {
  const host = document.getElementById("pkg-result");
  if (!host) return;

  if (!isDesktop()) {
    host.innerHTML = `
      <div class="placeholder">
        <span class="ph-ico">${icon("import", 18)}</span>
        <b>浏览器模式不能安装模块包</b>
        <p>装包要写程序目录和 <code>installed.json</code>，只有桌面端能做。用启动器打开这个页面再试。</p>
      </div>
    `;
    return;
  }

  if (pkgState.busy) {
    host.innerHTML = `
      <div class="placeholder">
        <span class="ph-ico">${icon("import", 18)}</span>
        <b>正在读包…</b>
        <p>正在校验整包哈希并解出清单，几秒内完成。</p>
      </div>
    `;
    return;
  }

  const preview = pkgState.preview;
  if (!preview) {
    host.innerHTML = `
      <div class="placeholder">
        <span class="ph-ico">${icon("import", 18)}</span>
        <b>还没有检查过包</b>
        <p>点「选择文件…」挑一个 .zip，再点「检查」。看清内容与签名后再决定装不装。</p>
      </div>
      ${pkgState.note ? `<div class="cap" style="margin-top:10px">${esc(pkgState.note)}</div>` : ""}
    `;
    return;
  }

  const blocked = preview.verdict === "D";
  const rows = [
    [
      "模块",
      `${esc(preview.name)} <span class="cap" style="margin:0">${esc(preview.module_id)} · ${esc(preview.variant)}</span>`,
    ],
    ["版本", esc(preview.version)],
    ["角色", esc(ROLE_TITLE[preview.role as Role] ?? preview.role)],
    ["内容", `${preview.files} 个文件 · ${fmtBytes(preview.bytes)}`],
    ["签名", trustStatus(preview)],
    ["整包哈希", `<code style="font-size:11px">${esc(preview.sha256)}</code>`],
  ]
    .map(
      ([k, v]) => `
      <div class="envrow">
        <span class="k">${k}</span>
        <span class="v">${v}</span>
      </div>
    `,
    )
    .join("");

  const warnings = [
    preview.needs_confirmation && preview.reason ? `为什么需要确认：${esc(preview.reason)}` : "",
    preview.replace_existing
      ? "这台机器上已经装过这个模块的同一个版本，继续会覆盖原有包目录；模块的数据目录不受影响。"
      : "",
  ]
    .filter(Boolean)
    .map((text) => `<div class="cap" style="margin:6px 0 0">${text}</div>`)
    .join("");

  host.innerHTML = `
    <div class="envlist">${rows}</div>
    ${warnings}
    <div class="toolbar" style="margin-top:12px">
      <span class="cap" style="margin:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(preview.path)}</span>
      <span class="sp"></span>
      <button class="btn btn--sm btn--quiet" type="button" data-act="drop-preview">换一个包</button>
      ${
        blocked
          ? ""
          : `<button class="btn btn--primary btn--sm" type="button" data-act="install">${
              preview.needs_confirmation ? "确认并安装" : "安装"
            }</button>`
      }
    </div>
    ${blocked ? `<div class="alert" style="margin-top:10px"><b>这个包不能安装</b><div>${esc(preview.reason)}</div></div>` : ""}
    ${pkgState.note ? `<div class="cap" style="margin-top:10px">${esc(pkgState.note)}</div>` : ""}
  `;
}

/** 把判级翻译成人话；B 类必须展示完整指纹，这条在弹窗里做。 */
function trustStatus(preview: LocalPackagePreview): string {
  if (preview.verdict === "A") {
    return `<span class="chip chip--run">签名有效</span> <span class="cap" style="margin:0">发布者公钥已登记</span>`;
  }
  if (preview.verdict === "B") {
    return `<span class="chip chip--warn">未登记的发布者</span> <span class="cap" style="margin:0">签名有效，但这台机器第一次见到它</span>`;
  }
  if (preview.verdict === "C") {
    return `<span class="chip chip--warn">未签名</span> <span class="cap" style="margin:0">来源无法验证</span>`;
  }
  return `<span class="chip chip--fail">已拒绝</span> <span class="cap" style="margin:0">发布者公钥已撤销或包格式不可信</span>`;
}

async function pick(): Promise<void> {
  try {
    const path = await pickPackageFile();
    if (!path) return;
    pkgState.path = path;
    const input = document.getElementById("pkg-path") as HTMLInputElement | null;
    if (input) input.value = path;
    await inspect();
  } catch (error) {
    pkgState.note = `选择文件失败：${describeError(error)}`;
    renderPkgResult();
    toast("选择文件失败，原因见下方说明", "error");
  }
}

async function inspect(): Promise<void> {
  const path = pkgState.path.trim();
  if (!path) {
    toast("先选择一个 .zip 再检查", "error");
    return;
  }
  pkgState.busy = true;
  pkgState.preview = null;
  pkgState.note = "";
  renderPkgResult();
  try {
    pkgState.preview = await inspectLocalPackage(path);
  } catch (error) {
    pkgState.note = `读包失败：${describeError(error)}。确认这个文件是完整的 .zip 模块包。`;
    toast("读包失败，原因见下方说明", "error");
  } finally {
    pkgState.busy = false;
    renderPkgResult();
  }
}

function install(): void {
  const preview = pkgState.preview;
  if (!preview) return;

  if (!preview.needs_confirmation) {
    void doInstall(preview, false);
    return;
  }

  confirmModal({
    title: "确认安装未验证的包",
    subtitle: `${preview.module_id} · ${preview.version}`,
    bodyHtml: `
      <p>${
        preview.verdict === "C"
          ? "这个包没有签名，来源无法验证。"
          : "签名有效，但这台机器第一次见到这个发布者。"
      }</p>
      ${
        preview.publisher_fp
          ? `<p class="cap" style="margin:8px 0 0">发布者公钥指纹</p>
             <p style="font-family:var(--mono);font-size:11px;word-break:break-all;margin:4px 0 0">${esc(preview.publisher_fp)}</p>`
          : ""
      }
      <p class="cap" style="margin:10px 0 0">装进来的模块以你的权限运行，不受沙箱限制。确认来源可信再继续。</p>
    `,
    confirmLabel: "确认并安装",
    danger: true,
    onConfirm: () => doInstall(preview, true),
  });
}

async function doInstall(preview: LocalPackagePreview, confirm: boolean): Promise<void> {
  pkgState.busy = true;
  pkgState.note = "";
  renderPkgResult();
  try {
    const result = await installLocalPackage(preview.path, preview.sha256, confirm);
    pkgState.preview = null;
    pkgState.note = `已安装 ${result.module_id}（${result.files} 个文件，${fmtBytes(result.bytes)}）${
      result.replaced ? "，覆盖了原有包目录" : ""
    }。到模块页可以启动它。`;
    toast(`已安装 ${result.module_id}`, "ok");
    await refreshViews();
    window.dispatchEvent(new CustomEvent("dounai:views-updated"));
  } catch (error) {
    pkgState.note = `安装失败：${describeError(error)}`;
    toast("安装失败，原因见下方说明", "error");
  } finally {
    pkgState.busy = false;
    renderPkgResult();
  }
}

function dropPreview(): void {
  pkgState.preview = null;
  pkgState.note = "";
  renderPkgResult();
}

/* ------------------------------------------------------------------ */
/* 本地目录                                                            */
/* ------------------------------------------------------------------ */

function renderResult(): void {
  const host = document.getElementById("import-result");
  if (!host) return;

  if (!isDesktop()) {
    host.innerHTML = `
      <div class="placeholder">
        <span class="ph-ico">${icon("import", 18)}</span>
        <b>浏览器模式不能登记目录</b>
        <p>登记要读写 <code>installed.json</code>，只有桌面端能做。用启动器打开这个页面再试。</p>
      </div>
    `;
    return;
  }

  if (dirState.candidates.length === 0) {
    host.innerHTML = `
      <div class="placeholder">
        <span class="ph-ico">${icon("import", 18)}</span>
        <b>还没有扫描结果</b>
        <p>填一个目录后点「扫描目录」，找到的模块会列在这里，可以逐个登记，也可以一次全部登记。</p>
      </div>
      ${dirState.note ? `<div class="cap" style="margin-top:10px">${esc(dirState.note)}</div>` : ""}
    `;
    return;
  }

  const pending = dirState.candidates.filter((item) => !item.registered);
  const rows = dirState.candidates
    .map(
      (item) => `
      <div class="envrow">
        <span class="k">${item.registered ? "已登记" : "待登记"}</span>
        <span class="v">${esc(item.name)}</span>
        <span class="cap">${esc(item.module_id)} · ${esc(item.version)}</span>
        <span class="sp"></span>
        <span class="chip ${item.signed ? "chip--run" : "chip--warn"}">${item.signed ? "已签名" : "未签名"}</span>
        <span class="cap" style="max-width:320px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(item.path)}</span>
        ${
          item.registered
            ? `<button class="btn btn--sm btn--quiet" type="button" data-act="unlink" data-id="${esc(item.module_id)}">解除登记</button>`
            : `<button class="btn btn--sm" type="button" data-act="add-path" data-path="${esc(item.path)}">登记</button>`
        }
      </div>
    `,
    )
    .join("");

  host.innerHTML = `
    <div class="toolbar">
      <span class="cap" style="margin:0">找到 ${dirState.candidates.length} 个候选，${pending.length} 个未登记。</span>
      <span class="sp"></span>
      ${
        pending.length > 0
          ? `<button class="btn btn--primary btn--sm" type="button" data-act="bulk-add">全部登记（${pending.length}）</button>`
          : ""
      }
    </div>
    <div class="envlist">${rows}</div>
    ${dirState.note ? `<div class="cap">${esc(dirState.note)}</div>` : ""}
  `;
}

async function scan(): Promise<void> {
  if (!dirState.root.trim()) {
    toast("先填一个目录路径再扫描", "error");
    return;
  }
  dirState.note = "正在扫描…";
  renderResult();
  try {
    dirState.candidates = await scanLocalModules(dirState.root.trim());
    dirState.note =
      dirState.candidates.length === 0
        ? "这个目录里没有含 manifest.json 的模块。确认路径指向模块目录或它的上级目录。"
        : "";
  } catch (error) {
    dirState.candidates = [];
    dirState.note = `扫描失败：${describeError(error)}。确认路径存在并且可以读取。`;
    toast("扫描失败，原因见列表上方说明", "error");
  } finally {
    renderResult();
  }
}

async function addOne(path: string): Promise<void> {
  if (!path.trim()) {
    toast("先填一个目录路径再登记", "error");
    return;
  }
  try {
    const view = await addLocalModule(path.trim());
    toast(`已登记 ${view.module_id}`, "ok");
    await afterChange(`已登记 ${view.module_id}。模块页里可以启动它。`);
  } catch (error) {
    dirState.note = `登记失败：${describeError(error)}。确认这个目录里有 manifest.json，并且当前账号可以读取。`;
    renderResult();
    toast("登记失败，原因见列表上方说明", "error");
  }
}

async function addAllPending(): Promise<void> {
  const paths = dirState.candidates.filter((item) => !item.registered).map((item) => item.path);
  if (paths.length === 0) return;
  try {
    const results = await addLocalModulesBulk(paths);
    const failed = results.filter((item) => !item.ok);
    dirState.note =
      failed.length === 0
        ? `已登记 ${results.length} 个模块。`
        : `成功 ${results.length - failed.length} 个，失败 ${failed.length} 个：${failed
            .map((item) => `${item.path}（${item.error ?? "未知原因"}）`)
            .join("；")}`;
    await afterChange(dirState.note);
  } catch (error) {
    dirState.note = `批量登记失败：${describeError(error)}`;
    renderResult();
    toast("批量登记失败，原因见列表上方说明", "error");
  }
}

function unlink(moduleId: string): void {
  const name = store.views.find((view) => view.instance.module_id === moduleId)?.instance.module_id;
  confirmModal({
    title: "解除登记",
    subtitle: moduleId,
    bodyHtml: `<p>解除后启动器不再管理这个模块。目录里的文件不会被删除，之后可以重新登记。</p>
      <p class="cap" style="margin:8px 0 0">目标：${esc(name ?? moduleId)}</p>`,
    confirmLabel: "解除登记",
    danger: true,
    onConfirm: async () => {
      try {
        await removeLocalModule(moduleId);
        await afterChange(`已解除登记 ${moduleId}。目录里的文件仍然保留。`);
      } catch (error) {
        toast(`解除登记失败：${describeError(error)}`, "error");
      }
    },
  });
}

/** 动作完成后重新扫描一遍并刷新全局视图，让行状态与实际一致。 */
async function afterChange(note: string): Promise<void> {
  dirState.note = note;
  await refreshViews();
  try {
    dirState.candidates = await scanLocalModules(dirState.root.trim());
  } catch {
    // 扫描失败不影响登记结果，保留上一次的候选列表。
  }
  renderResult();
  window.dispatchEvent(new CustomEvent("dounai:views-updated"));
}

function fmtBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function navigate(view: string): void {
  window.dispatchEvent(new CustomEvent("dounai:navigate", { detail: view }));
}
