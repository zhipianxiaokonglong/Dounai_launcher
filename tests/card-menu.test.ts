/**
 * 卡片「⋯」菜单的定位约束。
 *
 * 2026-11-15 真机发现：面板按按钮右边缘向左展开，而动作区实际类名是
 * .mcard-acts。早先写的 .card-foot .menu 从未命中，按钮没贴右，面板
 * 左边缘越出卡片，再被 .mcard 的 overflow: hidden 裁掉，菜单项每个
 * 只剩右半截；向上展开的部分同样被裁。这里钉住选择器与解除裁剪的条件，
 * 免得改类名时旧病复发。
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const css = readFileSync(resolve(process.cwd(), "styles/app.css"), "utf8");
const source = readFileSync(resolve(process.cwd(), "src/ui/views/modules.ts"), "utf8");

describe("card more-menu layout", () => {
  it("menu pushes itself right without depending on a parent class", () => {
    const rule = css.match(/\.menu\s*\{([^}]*)\}/)?.[1] ?? "";
    expect(rule).toContain("margin-left: auto");
  });

  it("no dead .card-foot selector remains", () => {
    // 注释里提到旧类名不算残留。先剥掉块注释，再看有没有真选择器。
    const rulesOnly = css.replace(/\/\*[\s\S]*?\*\//g, "");
    expect(rulesOnly).not.toMatch(/\.card-foot/);
  });

  it("panel is right-anchored, opens upward, and can flip below", () => {
    const rule = css.match(/\.menu-body\s*\{([^}]*)\}/)?.[1] ?? "";
    expect(rule).toContain("right: 0");
    expect(rule).toContain("bottom: calc(100% + 4px)");
    expect(css).toContain('.menu[data-dir="down"] .menu-body');
  });

  it("only the card with an open menu lifts clipping", () => {
    expect(css).toMatch(/\.mcard\s*\{[^}]*overflow: hidden/);
    expect(css).toMatch(/\.mcard--menu-open\s*\{[^}]*overflow: visible[^}]*z-index: 6/);
  });

  it("opening one menu closes the others", () => {
    // 多个浮层能同时展开时，相邻卡片的面板会叠在一起
    expect(source).toContain("function closeAllMenus");
    expect(source).toMatch(/if \(other !== menu\) other\.open = false/);
  });

  it("flips below using the clipping container, not the viewport", () => {
    // .pbody 才是裁剪边界：卡片贴顶时按视口算会误判成「上方空间充足」
    expect(source).toContain("function clipRect");
    expect(source).toMatch(/sr\.top - clip\.top/);
    expect(source).toMatch(/clip\.bottom - sr\.bottom/);
  });

  it("panel scrolls inside itself when neither side fits", () => {
    expect(css).toMatch(/\.menu-body\s*\{[^}]*overflow-y: auto/);
    expect(source).toContain('panel.style.maxHeight');
  });

  it("refreshes the action table on every render even after other views bind the same body", () => {
    // 所有视图共用 #pbody，各自 bindActions 会覆盖同一张动作表。WeakSet 只能
    // 守卫那几个额外监听；若它连 bindActions 一起跳过，切回模块页时 start /
    // switch-active 就没注册，表现为按钮点了没反应。
    const fn = source.match(/function bindBodyActions\(body: HTMLElement\): void \{([\s\S]*?)\n\}/)?.[1] ?? "";
    const bindAt = fn.indexOf("bindActions(body, [");
    const guardAt = fn.indexOf("if (boundBodies.has(body)) return;");
    expect(bindAt).toBeGreaterThanOrEqual(0);
    expect(guardAt).toBeGreaterThan(bindAt);
  });
});
