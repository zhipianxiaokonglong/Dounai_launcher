/**
 * 图标：内联 SVG，不用字体字形。
 *
 * 之前用 ▦ ◫ ◈ 这类几何字符，缺字体的机器上会渲染成方块（tofu）。
 * SVG 只依赖 stroke 与 currentColor，跟着文字颜色走，缩放也不糊。
 */

const PATHS: Record<string, string> = {
  brand: '<path d="M12 3l9 9-9 9-9-9z"/>',
  modules:
    '<rect x="3.5" y="3.5" width="7" height="7" rx="1.4"/><rect x="13.5" y="3.5" width="7" height="7" rx="1.4"/><rect x="3.5" y="13.5" width="7" height="7" rx="1.4"/><rect x="13.5" y="13.5" width="7" height="7" rx="1.4"/>',
  bundles:
    '<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/><path d="M4 7.5l8 4.5 8-4.5M12 12v9"/>',
  env: '<rect x="3" y="4" width="18" height="6" rx="1.5"/><rect x="3" y="14" width="18" height="6" rx="1.5"/><path d="M7 7h.01M7 17h.01"/>',
  logs: '<path d="M5 6h14M5 12h14M5 18h9"/>',
  settings:
    '<circle cx="12" cy="12" r="3"/><path d="M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3M5.2 5.2l2.1 2.1M16.7 16.7l2.1 2.1M18.8 5.2l-2.1 2.1M7.3 16.7l-2.1 2.1"/>',
  import: '<path d="M12 3.5v10m0 0l-4-4m4 4l4-4"/><path d="M4 15.5v4h16v-4"/>',
  box: '<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/><path d="M4 7.5l8 4.5 8-4.5"/>',
  alert: '<path d="M12 4l9 16H3z"/><path d="M12 10v4M12 17h.01"/>',
};

export function icon(name: string, size = 14): string {
  const body = PATHS[name] ?? PATHS.box;
  return `<svg viewBox="0 0 24 24" width="${size}" height="${size}" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">${body}</svg>`;
}
