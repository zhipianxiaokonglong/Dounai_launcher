/**
 * 入门引导：聚光灯式，指着真实界面讲。
 *
 * 与常见的 product tour 有两处不同，都是为了不让引导变成路障：
 *
 * 1. **不挡点击**。遮罩层与挖洞都是 `pointer-events: none`，只有气泡可点。
 *    用户随时能自己去点界面、去装模块，回来接着看。改成挡住的话，
 *    它就从「边配边学」退化成「先看完再动手」——那正是被换掉的四屏向导。
 * 2. **只指向稳定锚点**。步骤的目标一律是侧栏项、页头按钮、状态栏这类
 *    不会随数据重绘的节点。模块卡片装依赖时会整块重画，聚光灯套上去
 *    洞就没了。需要用户动手的地方，气泡里给一个按钮代跳，不代为执行。
 *
 * 目标因为用户自己切页而消失时，气泡降级到居中并说明，不静默卡住。
 */

export type TourView = "modules" | "env" | "logs" | "settings" | "import";

export interface TourStep {
  /** 目标选择器；不给则气泡居中，用于开场的说明页。 */
  target?: string;
  /** 进入这一步前先切到这个视图。 */
  view?: TourView;
  /** 目标满足后才进入这一步；用稳定的选择器，别用会在重绘中消失的。 */
  title: string;
  body: string;
  /** 气泡优先贴哪一侧；默认按空间自动选。 */
  prefer?: Side;
}

export type Side = "top" | "bottom" | "left" | "right";

export interface Rect {
  left: number;
  top: number;
  width: number;
  height: number;
}

export interface Size {
  width: number;
  height: number;
}

/**
 * 引导的七步。
 *
 * 顺序跟着新用户的真实路径走：先知道这是干什么的，再知道模块是什么、
 * 从哪装、依赖装在哪，然后是启动与密钥边界，最后一脚踹去日志页。
 * 每一步都对应界面上一个真实存在的地方——没有一处是纯文案。
 */
export const STEPS: TourStep[] = [
  {
    title: "这是 Dounai 的启动器",
    body: "它只做三件事：装模块、拉起进程、把日志摊开。密钥和对话内容都不经过它。七步看完，你就知道每一块在哪。",
  },
  {
    view: "modules",
    target: '#nav [data-view="modules"]',
    title: "模块页：实例都在这里",
    body: "一个模块可以装多个实例，各有自己的端口和数据目录。想对比两个模型层，就装两份，互不干扰。",
  },
  {
    target: '#phead [data-act="catalog"]',
    title: "从模块目录开始装",
    body: "目录是一份签过名的清单，装包前先验签名再解压。列表里写着每个包的体积，点之前就知道要下多少。",
  },
  {
    view: "env",
    target: '#nav [data-view="env"]',
    title: "依赖是每个实例自己的",
    body: "每个实例有独立的 Python 环境，放在 instances/<实例>/ 下。装完模块要点「准备依赖」，第一次会下解释器和轮子。",
  },
  {
    view: "modules",
    target: '#phead [data-act="start-all"]',
    title: "一键启动按依赖顺序来",
    body: "输入 → 模型 → 输出依次拉起，每个都过了健康检查才算启动成功。主实例占 8101-8103，其余自动往后排。",
  },
  {
    target: "#sbar",
    prefer: "top",
    title: "这里不会出现密钥",
    body: "模型 key 配在模块自己的设置里，启动器不存也不转。状态栏这句话是它对自己的约定，不是装饰。",
  },
  {
    title: "起不来先看日志",
    body: "日志按实例分开，能按级别筛、按关键词搜，装依赖的每一行也实时写进去。子进程输出默认丢弃，需要时再打开。",
  },
];

/* ------------------------------------------------------------------ */
/* 布局（纯函数，可单测）                                              */
/* ------------------------------------------------------------------ */

const GAP = 12;
const EDGE = 12;

function clamp(value: number, low: number, high: number): number {
  if (high < low) return low;
  return Math.min(Math.max(value, low), high);
}

/**
 * 气泡位置。
 *
 * 先试首选侧，再按「下 → 上 → 右 → 左」找第一个装得下的；
 * 都装不下就退回下方并夹进视口——宁可压住目标一点，也不能把气泡挤到屏幕外。
 * 侧栏那种又高又窄的目标会自然落到右侧，底部状态栏会落到上方。
 */
export function placePop(
  target: Rect,
  pop: Size,
  view: Size,
  preferred?: Side,
): { top: number; left: number; side: Side } {
  const fits = {
    bottom: view.height - (target.top + target.height) - GAP - EDGE >= pop.height,
    top: target.top - GAP - EDGE >= pop.height,
    right: view.width - (target.left + target.width) - GAP - EDGE >= pop.width,
    left: target.left - GAP - EDGE >= pop.width,
  };
  const order: Side[] = preferred
    ? [preferred, "bottom", "top", "right", "left"]
    : ["bottom", "top", "right", "left"];
  const side = order.find((candidate) => fits[candidate]) ?? "bottom";

  let top: number;
  let left: number;
  switch (side) {
    case "top":
      top = target.top - GAP - pop.height;
      left = target.left + target.width / 2 - pop.width / 2;
      break;
    case "right":
      left = target.left + target.width + GAP;
      top = target.top + target.height / 2 - pop.height / 2;
      break;
    case "left":
      left = target.left - GAP - pop.width;
      top = target.top + target.height / 2 - pop.height / 2;
      break;
    default:
      top = target.top + target.height + GAP;
      left = target.left + target.width / 2 - pop.width / 2;
  }

  return {
    side,
    top: clamp(top, EDGE, view.height - pop.height - EDGE),
    left: clamp(left, EDGE, view.width - pop.width - EDGE),
  };
}

/**
 * 引导要用「屏幕上真正看得见的那一块」当视口，而不是 layout viewport。
 *
 * WebView 冷启动时窗口尺寸还没同步进渲染进程，innerHeight 可能是一个远大于
 * 实际窗口的值（实测见过上万像素）。此时按它定位，气泡会落在窗口下方，
 * 直到用户拖一下窗口、触发一次 resize 才出现。visualViewport 记的是物理
 * 可见区域，优先用；最后再用屏幕可用尺寸钳一道上界，兜住没有这个 API、
 * 或它同样没同步的环境。视口不可能比屏幕还高，钳错了也只会把气泡往上挪。
 */
export interface ViewportInputs {
  /** innerWidth / innerHeight：冷启动时可能远大于真实窗口。 */
  layout: Size;
  /** visualViewport：物理可见区域；老环境没有时为 null。 */
  visual: Size | null;
  /** outerWidth / outerHeight：含窗口边框，通常比视口大几十像素。 */
  outer: Size;
  /** screen.availWidth / availHeight：物理上界。 */
  screen: Size;
}

export function viewport(input: ViewportInputs): Size {
  const widths = [input.layout.width, input.visual?.width, input.outer.width, input.screen.width];
  const heights = [
    input.layout.height,
    input.visual?.height,
    input.outer.height,
    input.screen.height,
  ];
  // 取所有候选里最小的那个：真实可见区域不会比其中任何一个更大，而冷启动
  // 时只有部分是错的。只信一个来源（哪怕是 visualViewport）都可能再次踩空。
  const smallest = (values: Array<number | undefined>): number => {
    const usable = values.filter((v): v is number => typeof v === "number" && v > 0);
    return Math.max(1, usable.length > 0 ? Math.min(...usable) : 1);
  };
  return { width: smallest(widths), height: smallest(heights) };
}

function readViewport(): Size {
  const vv = window.visualViewport;
  return viewport({
    layout: { width: window.innerWidth, height: window.innerHeight },
    visual: vv ? { width: vv.width, height: vv.height } : null,
    outer: { width: window.outerWidth, height: window.outerHeight },
    screen: { width: window.screen.availWidth, height: window.screen.availHeight },
  });
}

/** 无目标步骤的居中位置。返回左上角坐标，不用 CSS 百分比。 */
export function centerPop(pop: Size, view: Size): { top: number; left: number } {
  return {
    top: clamp((view.height - pop.height) / 2, EDGE, view.height - pop.height - EDGE),
    left: clamp((view.width - pop.width) / 2, EDGE, view.width - pop.width - EDGE),
  };
}

/** 挖洞：目标四周留出呼吸位，并夹进视口，避免负值把遮罩撕开。 */
export function holeOf(target: Rect, view: Size, pad = 6): Rect {
  const left = Math.max(0, target.left - pad);
  const top = Math.max(0, target.top - pad);
  return {
    left,
    top,
    width: Math.min(view.width - left, target.width + pad * 2),
    height: Math.min(view.height - top, target.height + pad * 2),
  };
}

/* ------------------------------------------------------------------ */
/* 引擎                                                               */
/* ------------------------------------------------------------------ */

let index = 0;
let tracking: number | null = null;
let layer: HTMLElement | null = null;
let keyHandler: ((event: KeyboardEvent) => void) | null = null;
/** 目标连续多少次探测不到就降级；120ms 一次，约 0.5 秒。 */
const MISS_LIMIT = 4;
let misses = 0;

function el(id: string): HTMLElement | null {
  return document.getElementById(id);
}

/**
 * 把气泡摆到可见视口正中。
 *
 * 坐标由 JS 算成像素再写进 style，不用 `top: 50%`：百分比相对 layout
 * viewport，冷启动那个错误尺寸会把它放到窗口外。每 120ms 重算一次，
 * 窗口尺寸一旦同步过来，下一拍就跟上，不需要用户手动拖窗口。
 */
function center(pop: HTMLElement): void {
  pop.classList.add("is-center");
  const at = centerPop(
    { width: pop.offsetWidth, height: pop.offsetHeight },
    readViewport(),
  );
  pop.style.top = `${at.top}px`;
  pop.style.left = `${at.left}px`;
}

function navigate(view: TourView): void {
  window.dispatchEvent(new CustomEvent("dounai:navigate", { detail: view }));
}

/**
 * 等目标出现。
 *
 * 切页是异步的（视图要拉数据），所以不能切完立刻定位。
 * 超时不抛错——降级到居中气泡，比让引导停在一个不存在的洞上强。
 */
async function waitFor(selector: string, timeout = 1200): Promise<HTMLElement | null> {
  const deadline = performance.now() + timeout;
  for (;;) {
    const node = document.querySelector<HTMLElement>(selector);
    if (node && node.getBoundingClientRect().width > 0) return node;
    if (performance.now() > deadline) return null;
    await new Promise((resolve) => window.setTimeout(resolve, 60));
  }
}

export function isTourOpen(): boolean {
  return !!layer && !layer.hidden;
}

export function startTour(): void {
  const host = el("tour");
  if (!host) return;
  if (isTourOpen()) return;
  layer = host;
  layer.hidden = false;
  layer.innerHTML = `
    <div class="tour-hole" hidden></div>
    <div class="tour-pop" role="dialog" aria-modal="false" aria-live="polite">
      <div class="tour-head">
        <span class="tour-count"></span>
        <span class="tour-dots" aria-hidden="true"></span>
        <button class="tour-x" type="button" data-tour="skip" aria-label="跳过引导">×</button>
      </div>
      <b class="tour-title"></b>
      <p class="tour-body"></p>
      <div class="tour-foot">
        <button class="btn btn--sm btn--quiet" type="button" data-tour="skip">跳过</button>
        <span class="sp"></span>
        <button class="btn btn--sm" type="button" data-tour="prev">上一步</button>
        <button class="btn btn--sm btn--primary" type="button" data-tour="next">下一步</button>
      </div>
    </div>
  `;
  layer.addEventListener("click", onClick);
  keyHandler = (event: KeyboardEvent) => {
    if (!isTourOpen()) return;
    if (event.key === "Escape") {
      event.preventDefault();
      stopTour(true);
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      go(index + 1);
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      go(index - 1);
    }
  };
  document.addEventListener("keydown", keyHandler);
  void go(0);
}

function onClick(event: MouseEvent): void {
  const target = (event.target as HTMLElement | null)?.closest<HTMLElement>("[data-tour]");
  const act = target?.dataset.tour;
  if (!act) return;
  if (act === "skip") stopTour(true);
  else if (act === "prev") go(index - 1);
  else if (act === "next") go(index + 1);
  else if (act === "back") void enter(index);
  else if (act === "jump") {
    // 从气泡里跳走属于「用完了」，不是跳过。
    const to = target.dataset.to as TourView | undefined;
    if (to) navigate(to);
    stopTour(false);
  }
}

/**
 * 结束引导。`skipped` 只影响落盘的字段——两种都算「看过了」，不再自动弹。
 * 用户主动关掉和一步步走完的区别要留给设置页显示，所以不能合并。
 */
export function stopTour(skipped: boolean): void {
  if (tracking !== null) {
    window.clearInterval(tracking);
    tracking = null;
  }
  if (keyHandler) {
    document.removeEventListener("keydown", keyHandler);
    keyHandler = null;
  }
  if (layer) {
    layer.removeEventListener("click", onClick);
    layer.hidden = true;
    layer.innerHTML = "";
    layer = null;
  }
  misses = 0;
  window.dispatchEvent(new CustomEvent("dounai:tour-done", { detail: skipped }));
}

async function go(next: number): Promise<void> {
  if (next < 0 || next >= STEPS.length) {
    // 走完最后一步算「看完」，不是跳过。
    stopTour(false);
    return;
  }
  index = next;
  await enter(index);
}

async function enter(at: number): Promise<void> {
  const step = STEPS[at];
  const host = layer;
  if (!host || !step) return;

  paintText(at);

  if (step.view) navigate(step.view);
  const node = step.target ? await waitFor(step.target) : null;
  if (!layer) return;
  if (step.target && !node) {
    paintOrphan(step);
    if (tracking !== null) window.clearInterval(tracking);
    tracking = null;
    track(null, true);
    return;
  }
  if (tracking !== null) {
    window.clearInterval(tracking);
    tracking = null;
  }
  misses = 0;
  track(node, !step.target);
}

function paintText(at: number): void {
  const host = layer;
  const step = STEPS[at];
  if (!host || !step) return;
  const count = host.querySelector(".tour-count");
  if (count) count.textContent = `${at + 1} / ${STEPS.length}`;
  const dots = host.querySelector(".tour-dots");
  if (dots) {
    dots.innerHTML = STEPS.map(
      (_, i) => `<i class="${i === at ? "is-on" : i < at ? "is-past" : ""}"></i>`,
    ).join("");
  }
  const title = host.querySelector(".tour-title");
  if (title) title.textContent = step.title;
  const body = host.querySelector(".tour-body");
  if (body) body.textContent = step.body;

  const prev = host.querySelector<HTMLButtonElement>('[data-tour="prev"]');
  if (prev) prev.disabled = at === 0;
  const next = host.querySelector<HTMLButtonElement>('[data-tour="next"]');
  if (next) next.textContent = at === STEPS.length - 1 ? "开始使用" : "下一步";
}

/** 目标不在当前页（用户自己切走了，或切页失败）：居中说明，不假装还指着。 */
function paintOrphan(step: TourStep): void {
  const host = layer;
  if (!host) return;
  const hole = host.querySelector<HTMLElement>(".tour-hole");
  if (hole) hole.hidden = true;
  const pop = host.querySelector<HTMLElement>(".tour-pop");
  if (!pop) return;
  center(pop);
  const body = host.querySelector(".tour-body");
  if (body) {
    body.textContent = `${step.body}（这一步要指的位置现在不在页面上，切回去就能看到。）`;
  }
  const foot = host.querySelector(".tour-foot");
  if (foot && !foot.querySelector('[data-tour="back"]')) {
    const back = document.createElement("button");
    back.className = "btn btn--sm btn--quiet";
    back.type = "button";
    back.dataset.tour = "back";
    back.textContent = "切回去";
    foot.insertBefore(back, foot.querySelector(".sp"));
  }
}

/** 每 120ms 校一次位置：目标可能因数据刷新移动，也可能整块被重绘掉。 */
function track(node: HTMLElement | null, startCentered: boolean): void {
  let centered = startCentered;
  const tick = (): void => {
    const host = layer;
    if (!host) return;
    const hole = host.querySelector<HTMLElement>(".tour-hole");
    const pop = host.querySelector<HTMLElement>(".tour-pop");
    if (!hole || !pop) return;

    // 开场与收尾本来就没有目标：直接居中。这跟「目标丢了」是两件事，
    // 不能走同一个判定，否则第一步的气泡会落在 static position 上（左上角，
    // 正好压在侧栏底下），要等半秒才被发现并挪走。
    if (centered || !STEPS[index]?.target) {
      hole.hidden = true;
      center(pop);
      return;
    }

    if (!node || !node.isConnected) {
      misses += 1;
      if (misses >= MISS_LIMIT && !centered) {
        // 目标没了（用户切页或重绘）。退到居中，避免洞停在一个空位置上；
        // 之后仍跟着窗口变化重算，不把气泡钉在旧尺寸上。
        const current = STEPS[index];
        if (current) paintOrphan(current);
        centered = true;
      }
      return;
    }
    misses = 0;

    const rect = node.getBoundingClientRect();
    const view = readViewport();
    const spot = holeOf(rect, view);
    hole.hidden = false;
    hole.style.top = `${spot.top}px`;
    hole.style.left = `${spot.left}px`;
    hole.style.width = `${spot.width}px`;
    hole.style.height = `${spot.height}px`;

    pop.classList.remove("is-center");
    const placed = placePop(
      { left: rect.left, top: rect.top, width: rect.width, height: rect.height },
      { width: pop.offsetWidth, height: pop.offsetHeight },
      view,
      STEPS[index]?.prefer,
    );
    pop.style.top = `${placed.top}px`;
    pop.style.left = `${placed.left}px`;
    pop.dataset.side = placed.side;
  };
  tick();
  tracking = window.setInterval(tick, 120);
}
