/**
 * 弹窗接力的回归：解析邀请码 → 确认框。
 *
 * openModal 的按钮回调返回 false 才阻止收尾关闭；解析成功后确认框会
 * 复用同一个 overlay 节点（内部先 closeModal 再重建）。如果回调仍返回
 * true，外层收尾就会把刚打开的确认框一起清掉——真机上表现为点了
 * “解析邀请码”之后弹窗直接消失、源也没有添加。
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { openModal } from "../src/ui/dom.js";

interface FakeButton {
  dataset: { modalAct?: string };
  addEventListener(type: string, handler: () => void): void;
  focus(): void;
  click(): Promise<void>;
}

function setupDom(): { overlay: Record<string, unknown>; buttons: FakeButton[] } {
  const buttons: FakeButton[] = [];
  const overlay = {
    hidden: true,
    html: "",
    firstChild: null,
    removeChild() {},
    listeners: {} as Record<string, Array<(event: unknown) => void>>,
    addEventListener(type: string, handler: (event: unknown) => void) {
      (this.listeners[type] ??= []).push(handler);
    },
    querySelectorAll() {
      return buttons;
    },
    get innerHTML(): string {
      return this.html;
    },
    set innerHTML(value: string) {
      this.html = value;
      buttons.length = 0;
      const count = (value.match(/data-modal-act=/g) ?? []).length;
      for (let index = 0; index < count; index += 1) {
        const handlers: Array<() => void> = [];
        buttons.push({
          dataset: { modalAct: String(index) },
          addEventListener(_type: string, handler: () => void) {
            handlers.push(handler);
          },
          focus() {},
          async click() {
            for (const handler of handlers) handler();
            // openModal 的 click 监听里用 void (async () => ...)() 收尾，
            // 等两个 microtask 让 await onClick 与关闭判断都跑完。
            await Promise.resolve();
            await Promise.resolve();
          },
        });
      }
    },
  };
  vi.stubGlobal("document", {
    getElementById(id: string) {
      return id === "overlay" ? overlay : null;
    },
    addEventListener() {},
    removeEventListener() {},
  });
  return { overlay: overlay as unknown as Record<string, unknown>, buttons };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("添加源弹窗接力", () => {
  it("接力打开确认框后，外层不再关闭新框", async () => {
    const { overlay, buttons } = setupDom();
    openModal({
      title: "添加下载源",
      bodyHtml: "",
      actions: [
        { label: "取消" },
        {
          label: "解析邀请码",
          onClick: () => {
            openModal({
              title: "确认信任这个源的 catalog 公钥",
              bodyHtml: "",
              actions: [{ label: "取消" }, { label: "确认并添加" }],
            });
            return false;
          },
        },
      ],
    });
    expect(overlay.hidden).toBe(false);

    await buttons[1]!.click();

    expect(overlay.hidden).toBe(false);
    expect(overlay.innerHTML).toContain("确认信任这个源的 catalog 公钥");
  });

  it("对照：回调返回 true 时确实关闭", async () => {
    const { overlay, buttons } = setupDom();
    openModal({
      title: "临时弹窗",
      bodyHtml: "",
      actions: [{ label: "关闭", onClick: () => true }],
    });
    await buttons[0]!.click();
    expect(overlay.hidden).toBe(true);
  });
});
