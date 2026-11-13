/**
 * 界面基础件：转义、取元素、toast、模态与遮罩。
 *
 * 所有插入模板的字符串都必须过 `esc()`；模块 manifest 与路径属于外部输入，
 * 一律按纯文本渲染（沿用 M3 的防注入口径）。
 */

/** HTML 转义：模板字符串里插外部值时必须调用。 */
export function esc(value: string): string {
  return value.replace(/[&<>"']/g, (ch) => {
    const map: Record<string, string> = {
      "&": "&amp;",
      "<": "&lt;",
      ">": "&gt;",
      '"': "&quot;",
      "'": "&#39;",
    };
    return map[ch] ?? ch;
  });
}

/** 取元素；缺失即抛错——骨架与视图不一致时立刻暴露，而不是静默半渲染。 */
export function must<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`缺少元素 #${id}`);
  return node as T;
}

export function clear(node: Element): void {
  while (node.firstChild) node.removeChild(node.firstChild);
}

/* ------------------------------------------------------------------ */
/* toast                                                               */
/* ------------------------------------------------------------------ */

let toastTimer: number | null = null;

/**
 * 轻量反馈条。
 *
 * 用它而不是 alert：启停与安装是高频动作，模态会打断连续操作。
 * 需要用户做决定的地方仍走模态（危险动作不能只靠一条会消失的提示）。
 */
export function toast(message: string, tone: "info" | "ok" | "error" = "info"): void {
  const node = document.getElementById("toast");
  if (!node) return;
  node.textContent = message;
  node.hidden = false;
  node.classList.toggle("is-error", tone === "error");
  node.classList.toggle("is-ok", tone === "ok");
  if (toastTimer !== null) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(
    () => {
      node.hidden = true;
      toastTimer = null;
    },
    tone === "error" ? 9000 : 4200,
  );
}

export function hideToast(): void {
  const node = document.getElementById("toast");
  if (node) node.hidden = true;
  if (toastTimer !== null) {
    window.clearTimeout(toastTimer);
    toastTimer = null;
  }
}

/* ------------------------------------------------------------------ */
/* 模态                                                                */
/* ------------------------------------------------------------------ */

export interface ModalOptions {
  title: string;
  /** 标题下的一行等宽说明，通常放实例 id 或路径。 */
  subtitle?: string;
  bodyHtml: string;
  /** 底栏左侧说明。 */
  footNote?: string;
  /** 底栏按钮；`primary` 会拿到主色。 */
  actions: Array<{
    label: string;
    tone?: "primary" | "danger" | "quiet";
    /** 默认点击后关闭；返回 false 可阻止关闭。 */
    onClick?: (root: HTMLElement) => boolean | void | Promise<boolean | void>;
    autofocus?: boolean;
  }>;
  wide?: boolean;
  /** 挂载后回调，用于绑定表单与焦点。 */
  onMount?: (root: HTMLElement) => void;
}

let overlayKeyHandler: ((event: KeyboardEvent) => void) | null = null;

export function closeModal(): void {
  const overlay = document.getElementById("overlay");
  if (overlay) {
    overlay.hidden = true;
    clear(overlay);
  }
  if (overlayKeyHandler) {
    document.removeEventListener("keydown", overlayKeyHandler);
    overlayKeyHandler = null;
  }
}

/**
 * 打开模态。
 *
 * 默认焦点放在第一个 `autofocus` 按钮，没有则放第一个按钮——
 * 危险确认框会把「取消」放在第一位并 autofocus（M4 的 UX 规则）。
 */
export function openModal(options: ModalOptions): void {
  const overlay = document.getElementById("overlay");
  if (!overlay) return;
  closeModal();

  const actionsHtml = options.actions
    .map((action, index) => {
      const tone = action.tone ? ` btn--${action.tone}` : "";
      return `<button class="btn${tone}" type="button" data-modal-act="${index}">${esc(action.label)}</button>`;
    })
    .join("");

  overlay.innerHTML = `
    <div class="modal${options.wide ? " modal--wide" : ""}" role="dialog" aria-modal="true" aria-label="${esc(options.title)}">
      <div class="modal-head">
        <h2>${esc(options.title)}</h2>
        ${options.subtitle ? `<p>${esc(options.subtitle)}</p>` : ""}
      </div>
      <div class="modal-body">${options.bodyHtml}</div>
      <div class="modal-foot">
        ${options.footNote ? `<span class="cap" style="margin:0">${esc(options.footNote)}</span>` : ""}
        <span class="sp"></span>
        ${actionsHtml}
      </div>
    </div>
  `;
  overlay.hidden = false;

  overlay.querySelectorAll<HTMLButtonElement>("[data-modal-act]").forEach((button) => {
    button.addEventListener("click", () => {
      const index = Number(button.dataset.modalAct ?? "-1");
      const action = options.actions[index];
      if (!action) return;
      void (async () => {
        const keep = await action.onClick?.(overlay);
        if (keep !== false) closeModal();
      })();
    });
  });

  // 点遮罩关闭：与点「取消」等价，不会触发任何破坏性动作。
  overlay.addEventListener("click", (event) => {
    if (event.target === overlay) closeModal();
  });

  overlayKeyHandler = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      closeModal();
    }
  };
  document.addEventListener("keydown", overlayKeyHandler);

  options.onMount?.(overlay);

  const buttons: HTMLButtonElement[] = [];
  overlay.querySelectorAll<HTMLButtonElement>("[data-modal-act]").forEach((b) => buttons.push(b));
  const focusIndex = options.actions.findIndex((a) => a.autofocus);
  const target =
    focusIndex >= 0 ? buttons[focusIndex] : buttons.length > 0 ? buttons[buttons.length - 1] : null;
  target?.focus();
}

/** 确认框：默认焦点在「取消」，且取消排第一。 */
export function confirmModal(options: {
  title: string;
  subtitle?: string;
  bodyHtml: string;
  confirmLabel: string;
  danger?: boolean;
  onConfirm: () => void | Promise<void>;
}): void {
  openModal({
    title: options.title,
    subtitle: options.subtitle,
    bodyHtml: options.bodyHtml,
    actions: [
      { label: "取消", tone: "quiet", autofocus: true },
      {
        label: options.confirmLabel,
        tone: options.danger ? "danger" : "primary",
        onClick: () => {
          void options.onConfirm();
        },
      },
    ],
  });
}

/* ------------------------------------------------------------------ */
/* 事件委托                                                            */
/* ------------------------------------------------------------------ */

export interface DelegatedAction {
  /** `data-act` 的取值。 */
  act: string;
  run: (node: HTMLElement) => void | Promise<void>;
}

/**
 * 在容器上代理 `data-act` 点击。
 *
 * 卡片会被整体重绘，逐个按钮绑定会随重绘漏绑；
 * 委托挂在容器上，重绘的是容器的子节点，监听器始终有效。
 */
export function bindActions(root: HTMLElement, actions: DelegatedAction[]): void {
  const table = new Map(actions.map((a) => [a.act, a.run]));
  root.addEventListener("click", (event) => {
    const target = (event.target as HTMLElement | null)?.closest<HTMLElement>("[data-act]");
    if (!target || !root.contains(target)) return;
    const act = target.dataset.act;
    if (!act) return;
    const run = table.get(act);
    if (!run) return;
    event.preventDefault();
    void run(target);
  });
}
