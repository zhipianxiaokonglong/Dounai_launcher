/**
 * 管理页视图：模块自带的管理界面内嵌在启动器窗口里。
 *
 * 子 WebView 是原生视图，不会跟着 DOM 走：它固定在窗口客户区的某块坐标上。
 * 所以这个视图有三件事必须自己做——把容器位置算给后端、窗口缩放时重下发、
 * 切到别的视图时把原生视图藏起来（否则它会浮在别的页面上）。
 *
 * 页面内容完全由模块提供，启动器只负责开窗与生命周期，不读写任何配置。
 */

import {
  adminHostReload,
  adminHostResize,
  adminHostUnmount,
  adminHostVisible,
  isDesktop,
  openAdminEmbedded,
  openAdminWindow,
  type AdminBounds,
} from "../../bridge.js";
import { clampAdminBounds } from "../admin-bounds.js";
import { esc, toast } from "../dom.js";
import { describeError, store } from "../store.js";
import type { ViewSlots } from "./modules.js";

interface Mounted {
  label: string;
  moduleId: string;
  url: string;
  mode: "embedded" | "window";
}

let mounted: Mounted | null = null;
let slot: HTMLElement | null = null;
let bar: HTMLElement | null = null;
let observer: ResizeObserver | null = null;
let resizeTimer: number | null = null;
let currentModuleId: string | null = null;
let generation = 0;
/** 进程停了之后把原生视图藏起来，恢复时再显示并 reload。 */
let paused = false;
let wasHealthy = false;
/** 当前是否停在管理视图。离开后不能再 show，否则原生视图会浮在别的页上。 */
let active = false;

/* ------------------------------------------------------------------ */
/* 渲染                                                                */
/* ------------------------------------------------------------------ */

export function renderAdmin(slots: ViewSlots, arg?: string): void {
  const moduleId = arg || currentModuleId;
  currentModuleId = moduleId;
  active = true;
  slots.body.classList.add("pbody--admin");
  renderHead(slots.head, moduleId);
  slots.body.innerHTML = shellHtml(moduleId);
  slot = slots.body.querySelector<HTMLElement>("#admin-slot");
  bar = slots.body.querySelector<HTMLElement>("#admin-bar");
  bindToolbar();
  observe();

  if (!moduleId) {
    paintPlaceholder("还没有选模块", "在模块页点某个实例菜单里的「管理」，这一页就会装上它的界面。");
    return;
  }
  if (!isDesktop()) {
    paintPlaceholder("需要桌面端", "内嵌管理页由启动器创建原生 WebView，浏览器预览里看不到。");
    return;
  }
  const view = store.views.find((v) => v.instance.module_id === moduleId);
  if (!view) {
    paintPlaceholder("找不到这个模块", "它可能已经被卸载。回模块页看一下当前实例列表。");
    return;
  }
  if ((view.snapshot?.phase ?? "stopped") !== "running") {
    // 实例没起来时不显示连接错误：管理页本身没问题，是服务还没就绪。
    if (mounted?.mode === "embedded") void hideMounted();
    paused = true;
    paintPlaceholder("模块还没运行", `启动 ${esc(view.name)} 之后，这一页会自动恢复。`);
    return;
  }
  if (mounted && mounted.moduleId === moduleId && mounted.mode === "embedded") {
    void resumeMounted();
    return;
  }
  void mount(moduleId);
}

function renderHead(head: HTMLElement, moduleId: string | null): void {
  const name = moduleId
    ? (store.views.find((v) => v.instance.module_id === moduleId)?.name ?? moduleId)
    : "管理";
  head.innerHTML = `
    <div>
      <h1>管理</h1>
      <div class="sub">${esc(name)} · 模块自带界面</div>
    </div>
  `;
}

function shellHtml(moduleId: string | null): string {
  const usable = Boolean(moduleId) && isDesktop();
  return `
    <div class="admin">
      <div class="admin-bar" id="admin-bar">
        <span class="admin-bar__id" id="admin-url">${moduleId ? esc(moduleId) : "未选择模块"}</span>
        <span class="sp"></span>
        <button class="btn btn--sm" type="button" data-admin-act="reload" ${usable ? "" : "disabled"}>刷新</button>
        <button class="btn btn--sm" type="button" data-admin-act="popout" ${usable ? "" : "disabled"}>独立窗口打开</button>
        <button class="btn btn--sm" type="button" data-admin-act="close" ${usable ? "" : "disabled"}>关闭</button>
      </div>
      <p class="admin-notice">这一页由模块自己提供 · 启动器不存密钥，也不读写它的配置</p>
      <div class="admin-slot" id="admin-slot"></div>
    </div>
  `;
}

function paintPlaceholder(title: string, hint: string): void {
  if (!slot) return;
  slot.innerHTML = `
    <div class="admin-ph">
      <b>${title}</b>
      <p>${hint}</p>
    </div>
  `;
}

function bindToolbar(): void {
  bar?.addEventListener("click", (event) => {
    const target = (event.target as HTMLElement | null)?.closest<HTMLElement>("[data-admin-act]");
    const act = target?.dataset.adminAct;
    if (!act || target?.hasAttribute("disabled")) return;
    const moduleId = currentModuleId;
    if (act === "reload" && mounted?.mode === "embedded") {
      void adminHostReload(mounted.label).catch(() => toast("刷新失败", "error"));
    } else if (act === "popout" && moduleId) {
      void openAdminWindow(moduleId)
        .then(() => toast("已在独立窗口打开", "ok"))
        .catch((error) => toast(`打开失败：${describeError(error)}`, "error"));
    } else if (act === "close" && mounted) {
      const label = mounted.label;
      if (mounted.mode === "embedded") void adminHostUnmount(label).catch(() => undefined);
      mounted = null;
      paused = false;
      paintPlaceholder("已关闭", "重新点模块卡片里的「管理」可以再打开。");
    }
  });
}

/* ------------------------------------------------------------------ */
/* 坐标下发                                                            */
/* ------------------------------------------------------------------ */

function currentBounds(): AdminBounds | null {
  if (!slot) return null;
  const rect = slot.getBoundingClientRect();
  return clampAdminBounds(
    { x: rect.left, y: rect.top, width: rect.width, height: rect.height },
    { width: window.innerWidth, height: window.innerHeight },
  );
}

function pushBounds(): void {
  if (!mounted || mounted.mode !== "embedded" || paused) return;
  const bounds = currentBounds();
  if (!bounds) return;
  void adminHostResize(mounted.label, bounds).catch(() => undefined);
}

function scheduleBounds(): void {
  if (resizeTimer !== null) window.clearTimeout(resizeTimer);
  // 拖窗口时 resize 事件是连发的；120ms 一拍足够跟上，也不至于把命令面刷爆。
  resizeTimer = window.setTimeout(() => {
    resizeTimer = null;
    pushBounds();
  }, 120);
}

function observe(): void {
  observer?.disconnect();
  observer = null;
  window.removeEventListener("resize", scheduleBounds);
  window.addEventListener("resize", scheduleBounds);
  if (slot && typeof ResizeObserver !== "undefined") {
    observer = new ResizeObserver(scheduleBounds);
    observer.observe(slot);
  }
}

/* ------------------------------------------------------------------ */
/* 挂载与生命周期                                                      */
/* ------------------------------------------------------------------ */

async function mount(moduleId: string): Promise<void> {
  const gen = ++generation;
  const bounds = currentBounds();
  if (!bounds) {
    paintPlaceholder("窗口太小", "把启动器窗口拉大一点，再点一次「管理」。");
    return;
  }
  paintPlaceholder("正在打开…", "正在创建内嵌视图；失败时会自动改用独立窗口。");
  try {
    const result = await openAdminEmbedded(moduleId, bounds);
    if (gen !== generation) {
      // 期间又点了别的模块。后端已经建出这个原生视图了，必须销毁，
      // 否则它会一直浮在界面上，而前端再也不知道它的 label。
      if (result.mode === "embedded") {
        void adminHostUnmount(result.label).catch(() => undefined);
      }
      return;
    }
    mounted = {
      label: result.label,
      moduleId,
      url: result.url,
      mode: result.mode,
    };
    setUrl(result.url);
    if (result.mode === "window") {
      paintPlaceholder(
        "已在独立窗口打开",
        `${result.reason ?? "已改用独立窗口"}。页面没有丢，去新窗口继续操作。`,
      );
      return;
    }
    paused = false;
    wasHealthy = true;
    // 子 WebView 已经盖住这块区域，容器里不留下会露边的文字。
    if (slot) slot.innerHTML = "";
    // 挂载期间用户可能已经切走：视图是原生 HWND，不会跟着 DOM 消失，
    // 建好就得立刻藏起来，否则会浮在用户正在看的那一页上。
    if (!active) {
      void hideMounted();
      return;
    }
    pushBounds();
  } catch (error) {
    if (gen !== generation) return;
    paintPlaceholder("打不开管理界面", describeError(error));
  }
}

function setUrl(url: string): void {
  const node = bar?.querySelector<HTMLElement>("#admin-url");
  if (node) {
    node.textContent = url;
    node.title = url;
  }
}

async function resumeMounted(): Promise<void> {
  if (!mounted) return;
  paused = false;
  setUrl(mounted.url);
  if (slot) slot.innerHTML = "";
  try {
    await adminHostVisible(mounted.label, true);
    pushBounds();
  } catch {
    // 子 WebView 已被外部销毁（例如窗口重建）：重新挂一次，别卡在空白页。
    const moduleId = mounted.moduleId;
    mounted = null;
    void mount(moduleId);
  }
}

async function hideMounted(): Promise<void> {
  if (!mounted || mounted.mode !== "embedded") return;
  try {
    await adminHostVisible(mounted.label, false);
  } catch {
    // 已经不在了就当隐藏成功，下面照常显示占位。
  }
}

/** 切到别的视图：原生视图必须收起来，否则会浮在那一页上面。 */
export function disposeAdmin(): void {
  active = false;
  observer?.disconnect();
  observer = null;
  window.removeEventListener("resize", scheduleBounds);
  if (resizeTimer !== null) {
    window.clearTimeout(resizeTimer);
    resizeTimer = null;
  }
  slot = null;
  bar = null;
  if (mounted?.mode === "embedded") void hideMounted();
}

/** 实例卸载时销毁，而不是留一个还在连着已删服务的原生视图。 */
export function unmountAdmin(moduleId?: string): void {
  if (!mounted) return;
  if (moduleId && mounted.moduleId !== moduleId) return;
  const label = mounted.label;
  if (mounted.mode === "embedded") void adminHostUnmount(label).catch(() => undefined);
  mounted = null;
  currentModuleId = null;
  paused = false;
  paintPlaceholder("已关闭", "模块已卸载，管理页随之销毁。");
}

/**
 * 生命周期推送后调用（外壳刷新完 store 再调，保证读到的是新状态）。
 *
 * 进程停了显示占位，不显示连接错误；health 从非健康变健康时 reload 一次，
 * 让管理页重新拉自己的状态。运行期间不重复 reload——那会打断正在填的表单。
 */
export function adminLifecycleChanged(): void {
  if (!mounted || mounted.mode !== "embedded" || !currentModuleId) return;
  const view = store.views.find((v) => v.instance.module_id === currentModuleId);
  const running = (view?.snapshot?.phase ?? "stopped") === "running";
  const healthy = view?.snapshot?.health === "healthy";
  if (!running) {
    if (!paused) {
      paused = true;
      wasHealthy = false;
      if (active) {
        void hideMounted();
        paintPlaceholder("模块已停止", "启动它之后，这一页会自动恢复。");
      }
    }
    return;
  }
  if (paused) {
    paused = false;
    if (active) void resumeMounted();
    return;
  }
  // 离开管理页时仍跟踪健康状态，但不 reload、不 show，免得原生视图浮出来。
  if (healthy && !wasHealthy && active) {
    void adminHostReload(mounted.label).catch(() => undefined);
  }
  wasHealthy = healthy;
}

window.addEventListener("dounai:admin-unmount", (event) => {
  const detail = (event as CustomEvent<unknown>).detail;
  unmountAdmin(typeof detail === "string" ? detail : undefined);
});
