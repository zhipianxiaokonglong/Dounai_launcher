/**
 * 界面入口：外壳、路由与全局刷新。
 *
 * 各页只负责自己那块 DOM，外壳负责三件事——左侧导航、页头/正文容器、
 * 状态栏。路由是内存里的一个字符串，不做 history：这是桌面启动器，
 * 没有「后退到上一页」的语义，浏览器历史只会让 Backspace 误触。
 */

import {
  completeOnboarding,
  isDesktop,
  onCloseRequested,
  onEnvProgress,
  onLifecycleChanged,
  onUsageUpdated,
  resolveClose,
} from "../bridge.js";
import { esc, must, openModal, toast } from "./dom.js";
import { initTheme } from "./theme.js";
import {
  applyEnvProgress,
  applyUsage,
  describeError,
  formatBytes,
  formatPercent,
  refreshDevMode,
  refreshEnv,
  refreshSettings,
  refreshUsage,
  refreshViews,
  runCounts,
  store,
  usageTotals,
} from "./store.js";
import { adminLifecycleChanged, disposeAdmin, renderAdmin } from "./views/admin.js";
import { renderChains } from "./views/chains.js";
import { renderEnv } from "./views/env.js";
import { renderDownload } from "./views/download.js";
import { patchHomeUsage, refreshHome, renderHome } from "./views/home.js";
import { renderImport } from "./views/import.js";
import { disposeLogs, focusInstance, renderLogs } from "./views/logs.js";
import {
  patchEnvJob,
  patchModuleUsage,
  refreshModules,
  renderModules,
} from "./views/modules.js";
import { renderSettings } from "./views/settings.js";
import { startTour } from "./tour.js";

type ViewId =
  | "home"
  | "modules"
  | "download"
  | "chains"
  | "env"
  | "logs"
  | "settings"
  | "import"
  | "admin";

const VIEWS: ViewId[] = [
  "home",
  "modules",
  "download",
  "chains",
  "env",
  "logs",
  "settings",
  "import",
  "admin",
];

let current: ViewId = "home";

const slots = {
  head: must<HTMLElement>("phead"),
  body: must<HTMLElement>("pbody"),
};

/* ------------------------------------------------------------------ */
/* 渲染                                                                */
/* ------------------------------------------------------------------ */

async function renderView(view: ViewId, arg?: string): Promise<void> {
  // 内嵌管理页是原生视图，切走时必须收起来，否则会浮在下一页上面。
  if (current === "admin" && view !== "admin") disposeAdmin();
  current = view;
  // 日志页的自动刷新要随切页停掉，否则后台一直在读磁盘。
  if (view !== "logs") disposeLogs();
  slots.body.dataset.view = view;
  // 设置页会把容器切成无内边距的两栏，切页时必须还原。
  slots.body.className = "pbody";
  markNav(view);

  try {
    switch (view) {
      case "home":
        // 概览与模块页读同一份实例表。进页先拉一次的理由和模块页一样：
        // 安装 / 卸载不保证触发生命周期推送。
        try {
          await refreshViews();
        } catch (error) {
          toast(`读取实例状态失败：${describeError(error)}`, "error");
        }
        // 占用只是附加信息，拉不到就沿用上一份快照（首屏时是空，写「—」）。
        await refreshUsage().catch(() => undefined);
        renderHome(slots);
        break;
      case "modules":
        // 实例表会被「安装 / 卸载 / 启用停用」改写，而这些动作不保证触发
        // 生命周期推送。进页先拉一次，否则看到的是启动时那份快照——
        // 表现为「刚装好的模块不在列表里」。读失败就沿用上一份，不挡渲染。
        try {
          await refreshViews();
        } catch (error) {
          toast(`读取实例状态失败：${describeError(error)}`, "error");
        }
        await refreshUsage().catch(() => undefined);
        renderModules(slots);
        break;
      case "download":
        await renderDownload(slots);
        break;
      case "chains":
        await renderChains(slots);
        break;
      case "env":
        await renderEnv(slots);
        break;
      case "logs":
        if (arg) focusInstance(arg);
        await renderLogs(slots);
        break;
      case "settings":
        await renderSettings(slots);
        break;
      case "import":
        renderImport(slots);
        break;
      case "admin":
        renderAdmin(slots, arg);
        break;
    }
  } catch (error) {
    slots.body.innerHTML = `
      <div class="placeholder">
        <span class="ph-ico">!</span>
        <b>这一页没有渲染出来</b>
        <p>${esc(describeError(error))}。切到别的页再回来可以重试；如果一直失败，重启启动器。</p>
      </div>
    `;
  }
  updateChrome();
}

function markNav(view: ViewId): void {
  document.querySelectorAll<HTMLButtonElement>("#nav [data-view]").forEach((button) => {
    const on = button.dataset.view === view;
    button.classList.toggle("is-active", on);
    button.setAttribute("aria-current", on ? "page" : "false");
  });
}

/**
 * 状态栏的占用合计。
 *
 * 只在有读数时出现：一个实例都没跑的时候写「内存 0 KB」只是噪音。
 */
function usageBits(): string {
  const totals = usageTotals();
  if (!totals) return "";
  const parts = [`内存 ${formatBytes(totals.working_set_bytes)}`];
  if (totals.cpu_percent !== null) parts.push(`cpu ${formatPercent(totals.cpu_percent)}`);
  return parts.join(" · ");
}

/** 采样推送：只改当前页的占用数字与状态栏，不重绘整页。 */
function applyUsageTick(): void {
  updateChrome();
  if (current === "home") patchHomeUsage();
  else if (current === "modules") patchModuleUsage();
}

/** 导航计数与状态栏。任何会改变事实的动作之后都要调一次。 */
function updateChrome(): void {
  const counts = runCounts();
  setCount("nav-count-modules", String(counts.total), counts.failed > 0);
  const problems = store.env ? store.env.problems.length : 0;
  setCount("nav-count-env", problems > 0 ? String(problems) : "", problems > 0);
  setCount("nav-count-logs", counts.running > 0 ? String(counts.running) : "", false);

  const bits: string[] = [
    `<span class="keyline">运行中 ${counts.running}/${counts.total}</span>`,
  ];
  if (counts.starting > 0) bits.push(`${counts.starting} 启动中`);
  if (counts.failed > 0) bits.push(`<span class="warn-text">${counts.failed} 需处理</span>`);
  if (counts.external > 0) bits.push(`${counts.external} 外部启动`);
  const usage = usageBits();
  if (usage) bits.push(usage);

  const env = store.env;
  if (env) {
    const disk = env.disk.ok
      ? `数据盘剩余 ${env.disk.free_gb.toFixed(1)} GB`
      : "数据盘读取失败";
    bits.push(disk);
  }

  const sbar = document.getElementById("sbar");
  if (sbar) {
    sbar.innerHTML = `
      ${bits.join('<span class="sep">·</span>')}
      <span class="sp"></span>
      <span>launcher ${esc(__APP_VERSION__)}${isDesktop() ? "" : " · 浏览器模式"}</span>
    `;
  }

  const rail = document.getElementById("rail-foot");
  if (rail) {
    const now = new Date();
    rail.innerHTML = `
      <div class="row"><span>状态</span><b>${counts.failed > 0 ? "需处理" : counts.running > 0 ? "运行中" : "空闲"}</b></div>
      <div class="row"><span>实例</span><b>${counts.total}</b></div>
      <div class="row"><span>更新于</span><b>${now.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}</b></div>
    `;
  }
}

function setCount(id: string, text: string, alert: boolean): void {
  const node = document.getElementById(id);
  if (!node) return;
  node.textContent = text;
  node.closest(".nav-item")?.classList.toggle("is-alert", alert);
}

/* ------------------------------------------------------------------ */
/* 导航                                                                */
/* ------------------------------------------------------------------ */

function wireNav(): void {
  const nav = must<HTMLElement>("nav");
  nav.addEventListener("click", (event) => {
    const target = (event.target as HTMLElement | null)?.closest<HTMLElement>("[data-view]");
    const view = target?.dataset.view as ViewId | undefined;
    if (!view || !VIEWS.includes(view)) return;
    void renderView(view);
  });
}

/* ------------------------------------------------------------------ */
/* 启动                                                                */
/* ------------------------------------------------------------------ */

async function boot(): Promise<void> {
  initTheme();

  const version = document.getElementById("brand-version");
  if (version) version.textContent = `launcher ${__APP_VERSION__}`;

  updateChrome();
  // 先把外壳接好再拉数据：环境检测要跑子进程，几百毫秒到几秒不等，
  // 期间界面必须已经能点、能切页，否则等数据回来之前就是一块死界面。
  wireNav();
  wireEvents();
  // 首屏这一下已经把实例表拉回来了（概览页进页自读）。
  await renderView("home");

  // 其余三组事实并行拉取：任何一组失败都不该挡住界面，只在状态栏与页面上体现。
  // 实例表不在这批里——环境探测要 spawn 子进程（PowerShell / WMI / nvidia-smi），
  // 几百毫秒到几秒不等；把数据渲染挂在它后面，刷新后就会先看到「0 个模块」，
  // 而且用户在这期间切走的话，那个页面再也不会被补渲染。
  const results = await Promise.allSettled([refreshSettings(), refreshDevMode(), refreshEnv()]);
  const failure = results.find((item): item is PromiseRejectedResult => item.status === "rejected");
  if (failure) {
    toast(`部分状态没有读到：${describeError(failure.reason)}`, "error");
  }
  // 概览页要拿环境事实（数据盘、Python / uv、待处理项）才算完整，所以环境
  // 回来之后重绘一次；模块页不依赖环境，只更新外壳计数。
  if (current === "home") {
    await renderView("home");
  } else if (current === "modules") {
    updateChrome();
  } else {
    await renderView(current);
  }

  // 引导放在最后：等实例表回来才知道该不该弹（装过模块的人不用看）。
  void maybeStartTour();
}

/**
 * 首次是否自动弹引导。
 *
 * 三个条件必须同时成立：桌面端、没看过、一个实例都没装。第三条是给老用户的
 * ——升级后已经装过模块的人不需要再被教一遍「模块是什么」，想看就去设置页。
 */
async function maybeStartTour(): Promise<void> {
  if (!isDesktop()) return;
  if (store.onboarding?.completed) return;
  if (store.views.length > 0) return;
  startTour();
}

/** 生命周期推送：模块页整页刷新，其它页只更新计数与状态栏。 */
let pendingRefresh: number | null = null;
function scheduleRefresh(): void {
  if (pendingRefresh !== null) return;
  pendingRefresh = window.setTimeout(() => {
    pendingRefresh = null;
    void (async () => {
      if (current === "modules") {
        await refreshModules();
      } else if (current === "home") {
        await refreshHome();
      } else {
        try {
          await refreshViews();
        } catch {
          // 推送只是提示，读失败时保持上一次状态，不打断当前操作。
        }
      }
      updateChrome();
      // 刷新完 store 再通知管理页，保证它读到的是新快照而不是上一帧。
      adminLifecycleChanged();
    })();
  }, 250);
}

function wireEvents(): void {
  window.addEventListener("dounai:navigate", (event) => {
    const detail = String((event as CustomEvent<unknown>).detail ?? "");
    if (detail.startsWith("logs:")) {
      void renderView("logs", detail.slice("logs:".length));
      return;
    }
    if (detail.startsWith("admin:")) {
      void renderView("admin", detail.slice("admin:".length));
      return;
    }
    if (VIEWS.includes(detail as ViewId)) void renderView(detail as ViewId);
  });

  window.addEventListener("dounai:views-updated", () => updateChrome());

  // 引导结束时落盘：走完与跳过的区别只体现在 skipped 上，两者都不再自动弹。
  window.addEventListener("dounai:tour-done", (event) => {
    const skipped = Boolean((event as CustomEvent<unknown>).detail);
    void completeOnboarding(skipped)
      .then((next) => {
        store.onboarding = next;
        window.dispatchEvent(new CustomEvent("dounai:settings-updated"));
      })
      .catch(() => undefined);
  });

  window.addEventListener("dounai:settings-updated", () => {
    void refreshSettings()
      .then(() => updateChrome())
      .catch(() => undefined);
  });

  onLifecycleChanged(() => scheduleRefresh());

  // 资源占用：后端每 2 秒推一次全量快照。只刷数字，不动结构——
  // 这个频率下任何整页重绘都会让滚动位置和 hover 状态一直被打断。
  onUsageUpdated((snapshot) => {
    applyUsage(snapshot);
    applyUsageTick();
  });

  // 依赖安装进度：只改对应卡片上的那一行。整页重绘会把滚动位置、展开的菜单
  // 都打回原样，而这个事件在装包时每 150ms 就可能来一次。
  onEnvProgress((progress) => {
    applyEnvProgress(progress);
    patchEnvJob(progress.instanceId);
  });

  onCloseRequested(askClose);
}

/**
 * 关窗提示：后端拦下关闭事件之后问一次。
 *
 * 用户按 Esc 或点遮罩关掉弹框不算回答，窗口本来就没关；再点一次 × 会重新
 * 触发，所以这里不需要向后端「撤销」什么。`promptOpen` 只防重复点击时叠出
 * 两个弹框，`#overlay` 的可见性用来避开别的弹框正在打开的情况。
 */
let promptOpen = false;

function askClose(): void {
  if (promptOpen) return;
  const overlay = document.getElementById("overlay");
  if (overlay && !overlay.hidden) return;
  promptOpen = true;

  const running = store.views.filter((view) => view.snapshot?.phase === "running").length;
  const remembered = (root: HTMLElement) =>
    root.querySelector<HTMLInputElement>("#close-remember")?.checked ?? false;

  openModal({
    title: "关闭启动器窗口？",
    subtitle: running > 0 ? `${running} 个实例正在运行` : "当前没有运行中的实例",
    bodyHtml: `
      <p class="cap" style="margin:0 0 10px">实例的进程由启动器持有：窗口关掉、进程还在，实例就能继续跑；启动器进程一退，Windows 会把整棵进程树一并收掉。</p>
      <label class="check"><input type="checkbox" id="close-remember" /> 记住我的选择，下次不再问</label>
    `,
    actions: [
      {
        label: "取消",
        tone: "quiet",
        autofocus: true,
        onClick: () => {
          promptOpen = false;
          void resolveClose("cancel", false);
        },
      },
      {
        label: "收进托盘",
        tone: "primary",
        onClick: async (root) => {
          promptOpen = false;
          await resolveClose("tray", remembered(root));
        },
      },
      {
        label: "停止并退出",
        tone: "danger",
        onClick: async (root) => {
          promptOpen = false;
          await resolveClose("quit", remembered(root));
        },
      },
    ],
  });
}

void boot();
