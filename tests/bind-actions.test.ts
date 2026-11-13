/**
 * 委托监听的幂等性。
 *
 * 视图每次刷新都会重绘容器子节点、重新 bindActions；容器本身是常驻的。
 * 一旦重复注册监听器，一次点击就会触发多个处理函数——真机上表现为
 * 点一次「启动」发了四个启动请求，三个报「实例已在启动中」。
 */

import { describe, expect, it } from "vitest";
import { bindActions, type DelegatedAction } from "../src/ui/dom.js";

interface Click {
  type: string;
  handler: (event: unknown) => void;
}

/** 只保留 bindActions 真正用到的两个能力。 */
function fakeRoot(): { root: HTMLElement; clicks: Click[] } {
  const clicks: Click[] = [];
  const root = {
    addEventListener(type: string, handler: (event: unknown) => void) {
      clicks.push({ type, handler });
    },
    contains() {
      return true;
    },
  } as unknown as HTMLElement;
  return { root, clicks };
}

function clickOn(act: string): unknown {
  const node = { dataset: { act } } as unknown as HTMLElement;
  return {
    target: { closest: () => node },
    preventDefault() {},
  };
}

const actions = (runs: string[]): DelegatedAction[] => [
  {
    act: "start",
    run: () => {
      runs.push("start");
    },
  },
];

describe("bindActions", () => {
  it("重复绑定只留一个监听器", () => {
    const { root, clicks } = fakeRoot();
    const runs: string[] = [];
    for (let i = 0; i < 4; i += 1) bindActions(root, actions(runs));
    expect(clicks.length).toBe(1);

    clicks[0]!.handler(clickOn("start"));
    expect(runs.length).toBe(1);
  });

  it("重新绑定后用的是最新的处理函数", () => {
    const { root, clicks } = fakeRoot();
    const first: string[] = [];
    const second: string[] = [];
    bindActions(root, actions(first));
    bindActions(root, actions(second));

    clicks[0]!.handler(clickOn("start"));
    expect(first.length).toBe(0);
    expect(second.length).toBe(1);
  });

  it("没有登记的 act 不触发任何处理函数", () => {
    const { root, clicks } = fakeRoot();
    const runs: string[] = [];
    bindActions(root, actions(runs));
    clicks[0]!.handler(clickOn("uninstall"));
    expect(runs.length).toBe(0);
  });
});
