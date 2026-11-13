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
  onEnvProgress,
  onLifecycleChanged,
} from "../bridge.js";
import { esc, must, toast } from "./dom.js";
import { initTheme } from "./theme.js";
import {
  applyEnvProgress,
  describeError,
  refreshDevMode,
  refreshEnv,
  refreshSettings,
  refreshViews,
  runCounts,
  store,
} from "./store.js";
import { renderEnv } from "./views/env.js";
import { renderImport } from "./views/import.js";
import { disposeLogs, focusInstance, renderLogs } from "./views/logs.js";
import { patchEnvJob, refreshModules, renderModules } from "./views/modules.js";
import { renderSettings } from "./views/settings.js";
import { startTour } from "./tour.js";

type ViewId = "modules" | "env" | "logs" | "settings" | "import";

const VIEWS: ViewId[] = ["modules", "env", "logs", "settings", "import"];

let current: ViewId = "modules";

const slots = {
  head: must<HTMLElement>("phead"),
  body: must<HTMLElement>("pbody"),
};

/* ------------------------------------------------------------------ */
/* 渲染                                                                */
/* ------------------------------------------------------------------ */

async function renderView(view: ViewId, arg?: string): Promise<void> {
  current = view;
  // 日志页的自动刷新要随切页停掉，否则后台一直在读磁盘。
  if (view !== "logs") disposeLogs();
  slots.body.dataset.view = view;
  // 设置页会把容器切成无内边距的两栏，切页时必须还原。
  slots.body.className = "pbody";
  markNav(view);

  try {
    switch (view) {
      case "modules":
        renderModules(slots);
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
      <span class="keyline">启动器不存密钥</span>
      <span class="sep">|</span>
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
  await renderView("modules");

  // 四组事实并行拉取：任何一组失败都不该挡住界面，只在状态栏与页面上体现。
  const results = await Promise.allSettled([
    refreshViews(),
    refreshSettings(),
    refreshDevMode(),
    refreshEnv(),
  ]);
  const failure = results.find((item): item is PromiseRejectedResult => item.status === "rejected");
  if (failure) {
    toast(`部分状态没有读到：${describeError(failure.reason)}`, "error");
  }
  await renderView(current);

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
      } else {
        try {
          await refreshViews();
        } catch {
          // 推送只是提示，读失败时保持上一次状态，不打断当前操作。
        }
      }
      updateChrome();
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

  // 依赖安装进度：只改对应卡片上的那一行。整页重绘会把滚动位置、展开的菜单
  // 都打回原样，而这个事件在装包时每 150ms 就可能来一次。
  onEnvProgress((progress) => {
    applyEnvProgress(progress);
    patchEnvJob(progress.instanceId);
  });
}

void boot();
