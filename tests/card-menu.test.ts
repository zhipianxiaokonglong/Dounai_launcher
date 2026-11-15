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
});
