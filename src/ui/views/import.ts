/**
 * 导入本地：把已下载的目录登记成模块。
 *
 * 登记只写 `installed.json`，不复制也不删除目录里的文件——
 * 这句话必须出现在页面上，因为「导入」在别的软件里通常意味着搬运。
 */

import {
  addLocalModule,
  addLocalModulesBulk,
  isDesktop,
  removeLocalModule,
  scanLocalModules,
  type ScanCandidate,
} from "../../bridge.js";
import { bindActions, confirmModal, esc, toast } from "../dom.js";
import { icon } from "../icons.js";
import { describeError, refreshViews, store } from "../store.js";
import type { ViewSlots } from "./modules.js";

const state: {
  root: string;
  candidates: ScanCandidate[];
  /** 扫描或登记的结果说明；空串表示还没做过动作。 */
  note: string;
} = {
  root: "",
  candidates: [],
  note: "",
};

export function renderImport(slots: ViewSlots): void {
  renderHead(slots.head);
  renderBody(slots.body);
}

function renderHead(head: HTMLElement): void {
  head.innerHTML = `
    <div>
      <h1>导入本地</h1>
      <div class="sub">登记已有目录 · 不复制也不删除文件</div>
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
      <label for="import-root">模块目录或它的上级目录</label>
      <div class="toolbar" style="margin-bottom:0">
        <input id="import-root" class="input grow" type="text" spellcheck="false"
               placeholder="例如 E:\\Dounai-ai\\OmniNexus" value="${esc(state.root)}" />
        <button class="btn" type="button" data-act="scan">扫描目录</button>
        <button class="btn btn--primary" type="button" data-act="add">登记目录</button>
      </div>
      <div class="cap">扫描会读取该目录和它下面一层子目录，找出含 <code>manifest.json</code> 的模块。模型权重（.gguf、.safetensors）不是模块，扫描不会列出。</div>
    </div>

    <div class="alert" style="margin:14px 0">
      <b>第三方与本地源码模块不在沙箱内</b>
      <div>这些模块以你的权限运行，启动器只能约束进程与端口，不能限制模块读写本机文件。只登记你信任来源的目录。</div>
    </div>

    <div id="import-result"></div>
  `;

  const input = body.querySelector<HTMLInputElement>("#import-root");
  input?.addEventListener("input", () => {
    state.root = input.value;
  });

  bindActions(body, [
    { act: "scan", run: () => scan() },
    { act: "add", run: () => addOne(state.root) },
    { act: "add-path", run: (node) => addOne(node.dataset.path ?? "") },
    { act: "bulk-add", run: () => addAllPending() },
    { act: "unlink", run: (node) => unlink(node.dataset.id ?? "") },
  ]);

  renderResult();
}

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

  if (state.candidates.length === 0) {
    host.innerHTML = `
      <div class="placeholder">
        <span class="ph-ico">${icon("import", 18)}</span>
        <b>还没有扫描结果</b>
        <p>填一个目录后点「扫描目录」，找到的模块会列在这里，可以逐个登记，也可以一次全部登记。</p>
      </div>
      ${state.note ? `<div class="cap" style="margin-top:10px">${esc(state.note)}</div>` : ""}
    `;
    return;
  }

  const pending = state.candidates.filter((item) => !item.registered);
  const rows = state.candidates
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
      <span class="cap" style="margin:0">找到 ${state.candidates.length} 个候选，${pending.length} 个未登记。</span>
      <span class="sp"></span>
      ${
        pending.length > 0
          ? `<button class="btn btn--primary btn--sm" type="button" data-act="bulk-add">全部登记（${pending.length}）</button>`
          : ""
      }
    </div>
    <div class="envlist">${rows}</div>
    ${state.note ? `<div class="cap">${esc(state.note)}</div>` : ""}
  `;
}

async function scan(): Promise<void> {
  if (!state.root.trim()) {
    toast("先填一个目录路径再扫描", "error");
    return;
  }
  state.note = "正在扫描…";
  renderResult();
  try {
    state.candidates = await scanLocalModules(state.root.trim());
    state.note =
      state.candidates.length === 0
        ? "这个目录里没有含 manifest.json 的模块。确认路径指向模块目录或它的上级目录。"
        : "";
  } catch (error) {
    state.candidates = [];
    state.note = `扫描失败：${describeError(error)}。确认路径存在并且可以读取。`;
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
    state.note = `登记失败：${describeError(error)}。确认这个目录里有 manifest.json，并且当前账号可以读取。`;
    renderResult();
    toast("登记失败，原因见列表上方说明", "error");
  }
}

async function addAllPending(): Promise<void> {
  const paths = state.candidates.filter((item) => !item.registered).map((item) => item.path);
  if (paths.length === 0) return;
  try {
    const results = await addLocalModulesBulk(paths);
    const failed = results.filter((item) => !item.ok);
    state.note =
      failed.length === 0
        ? `已登记 ${results.length} 个模块。`
        : `成功 ${results.length - failed.length} 个，失败 ${failed.length} 个：${failed
            .map((item) => `${item.path}（${item.error ?? "未知原因"}）`)
            .join("；")}`;
    await afterChange(state.note);
  } catch (error) {
    state.note = `批量登记失败：${describeError(error)}`;
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
  state.note = note;
  await refreshViews();
  try {
    state.candidates = await scanLocalModules(state.root.trim());
  } catch {
    // 扫描失败不影响登记结果，保留上一次的候选列表。
  }
  renderResult();
  window.dispatchEvent(new CustomEvent("dounai:views-updated"));
}

function navigate(view: string): void {
  window.dispatchEvent(new CustomEvent("dounai:navigate", { detail: view }));
}
