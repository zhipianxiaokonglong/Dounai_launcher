/**
 * 主题：深色 / 浅色 / 跟随系统。
 *
 * 只改 `<html data-theme>`，不动任何 DOM 结构与样式规则——
 * 切换是换令牌值，不是重建界面（否则滚动位置、展开状态、焦点都会丢）。
 */

export type ThemeMode = "dark" | "light" | "auto";

const STORAGE_KEY = "dounai.theme";
const media = window.matchMedia("(prefers-color-scheme: dark)");

let mode: ThemeMode = "auto";

function readStored(): ThemeMode {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (raw === "dark" || raw === "light" || raw === "auto") return raw;
  } catch {
    // 存储不可用（隐私模式/被策略禁用）时退回跟随系统，不阻断启动。
  }
  return "auto";
}

function store(value: ThemeMode): void {
  try {
    window.localStorage.setItem(STORAGE_KEY, value);
  } catch {
    // 同上：写不进去就只在本次会话生效。
  }
}

/** 当前模式（auto 表示跟随系统）。 */
export function currentThemeMode(): ThemeMode {
  return mode;
}

/** 实际生效的配色。 */
export function resolvedTheme(): "dark" | "light" {
  if (mode === "auto") return media.matches ? "dark" : "light";
  return mode;
}

function paint(): void {
  document.documentElement.dataset.theme = resolvedTheme();
}

/** 切换模式并落盘。 */
export function setThemeMode(next: ThemeMode): void {
  mode = next;
  store(next);
  paint();
}

/**
 * 初始化：读存储值、上色，并在跟随系统时响应系统主题变化。
 *
 * 监听器常驻：即使当前是手动模式，系统主题变化也只是重新上色一次，
 * 结果与当前值相同，没有副作用。
 */
export function initTheme(): void {
  mode = readStored();
  paint();
  media.addEventListener("change", () => {
    if (mode === "auto") paint();
  });
}
