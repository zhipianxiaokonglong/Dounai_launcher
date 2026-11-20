/**
 * 模块卡片的 active 交互回归。
 *
 * 用户反馈两个概念认知负担重：卡片底栏菜单里的「设为 active」永远指向
 * 隐含的 primary（第一个非外部实例），想切到第二个实例根本没有入口；
 * 而 primary 与 active 语义相近却本质不同。改法是让每个实例行自带
 * 「当前实例 / 设为当前」单选，实例级动作也下沉到行内的 ⋯ 菜单，
 * 卡片底栏只留模块级动作。这里钉住这些结构约束，防止回退。
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const source = readFileSync(resolve(process.cwd(), "src/ui/views/modules.ts"), "utf8");

describe("模块卡片的当前实例交互", () => {
  it("当前实例单选渲染在实例行里，带 radio 语义", () => {
    expect(source).toContain('class="chip chip--active active-pick active-pick--on"');
    expect(source).toContain('aria-pressed="true"');
    expect(source).toContain('class="chip chip--pick active-pick"');
    expect(source).toContain('aria-pressed="false"');
    // 两个态都绑在 switch-active 上，点到哪行就切哪行
    expect(source.match(/data-act="switch-active"/g)?.length).toBeGreaterThanOrEqual(2);
  });

  it("外部实例与 auxiliary 不显示切换控件", () => {
    expect(source).toContain('if (!isExternal(view) && view.instance.role !== "auxiliary") {');
    expect(source).toContain("外部/辅助实例也可能是 active，但没有切换控件");
  });

  it("卡片底栏不再有设为 active，也不再有实例级动作", () => {
    const foot = source.match(/function cardFootHtml\([\s\S]*?\n\}/)?.[0] ?? "";
    expect(foot).not.toContain("switch-active");
    expect(foot).not.toContain("data-act=\"detail\"");
    expect(foot).not.toContain("data-act=\"logs\"");
    expect(foot).not.toContain("data-act=\"restart\"");
    expect(foot).not.toContain("data-act=\"uninstall\"");
    expect(foot).toContain("data-act=\"toggle\"");
    expect(foot).not.toContain("const primary =");
  });

  it("实例级动作下沉到行内菜单，external 只给详情与日志", () => {
    expect(source).toContain("function instanceMenuHtml");
    const menu = source.match(/function instanceMenuHtml\([\s\S]*?\n\}/)?.[0] ?? "";
    expect(menu).toContain('data-act="detail"');
    expect(menu).toContain('data-act="logs"');
    expect(menu).toContain('data-act="restart"');
    expect(menu).toContain('data-act="uninstall"');
    // external 分支只 push 两条动作，不给重启/卸载/装依赖
    expect(menu).toContain("if (!isExternal(view)) {");
  });

  it("文案统一到「当前实例」，toast 与分组头都不再写裸 active", () => {
    expect(source).toContain("当前实例已切到");
    expect(source).toContain("当前实例 = ${activeInstance.instance.instance_id}");
    // 实例名 chip 与详情弹窗都不再出现英文 active 标签
    expect(source).not.toContain('chip chip--active">active<');
    expect(source).not.toContain('["active", view.active');
    expect(source).not.toContain("设为 active");
  });

  it("切换 active 前有确认弹窗，说明会停旧起新并自动回滚", () => {
    const fn = source.match(/async function makeActive\([\s\S]*?\n\}/)?.[0] ?? "";
    expect(fn).toContain("confirmModal");
    expect(fn).toContain("设为当前实例？");
    expect(fn).toContain("自动回滚");
    expect(fn).toContain("已经是当前实例");
  });
});
