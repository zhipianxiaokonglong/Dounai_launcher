/**
 * 引导气泡的定位与挖洞。
 *
 * 这些断言守的是「气泡会不会跑到屏幕外」和「洞会不会套错地方」：算错了
 * 界面不崩，只是气泡压在目标上、或者一半露在视口外，用户既读不到说明
 * 也看不清被指的按钮。四种贴边情况与视口夹取都在这里钉住。
 */

import { describe, expect, it } from "vitest";
import { centerPop, holeOf, placePop, viewport, type Rect } from "../src/ui/tour.js";

const VIEW = { width: 1200, height: 800 };
const POP = { width: 344, height: 200 };

function rect(left: number, top: number, width: number, height: number): Rect {
  return { left, top, width, height };
}

describe("placePop", () => {
  it("目标在中间时贴下方", () => {
    const placed = placePop(rect(500, 300, 120, 32), POP, VIEW);
    expect(placed.side).toBe("bottom");
    expect(placed.top).toBe(300 + 32 + 12);
    // 水平居中于目标
    expect(placed.left).toBe(500 + 60 - 172);
  });

  it("目标贴底时翻到上方", () => {
    // 底部状态栏：下方只剩 0px，装不下 200 高的气泡。
    const placed = placePop(rect(0, 770, 1200, 30), POP, VIEW);
    expect(placed.side).toBe("top");
    expect(placed.top).toBe(770 - 12 - 200);
  });

  it("侧栏那种窄高目标贴到右侧", () => {
    // 左侧栏：下方有空间但气泡是 344 宽，右移更符合阅读顺序。
    const placed = placePop(rect(0, 100, 232, 44), POP, VIEW);
    expect(placed.side).toBe("bottom");
    // 下方确实够的时候仍走 bottom，不强行右移——说明顺序是 down 优先。
    const tall = placePop(rect(0, 100, 40, 600), POP, VIEW);
    expect(tall.side).toBe("right");
    expect(tall.left).toBe(40 + 12);
  });

  it("首选侧装不下时退到能装下的一侧", () => {
    // 显式要求上方，但目标已经贴顶，上方没有空间。
    const placed = placePop(rect(500, 0, 120, 30), POP, VIEW, "top");
    expect(placed.side).toBe("bottom");
  });

  it("四周都装不下时仍留在视口内", () => {
    // 一个几乎铺满视口的目标：没有任何一侧放得下。
    const placed = placePop(rect(10, 10, 1180, 780), POP, VIEW);
    expect(placed.top).toBeGreaterThanOrEqual(12);
    expect(placed.left).toBeGreaterThanOrEqual(12);
    expect(placed.top + POP.height).toBeLessThanOrEqual(VIEW.height - 12);
    expect(placed.left + POP.width).toBeLessThanOrEqual(VIEW.width - 12);
  });

  it("目标贴右边缘时气泡不溢出", () => {
    const placed = placePop(rect(1150, 300, 48, 32), POP, VIEW);
    expect(placed.left + POP.width).toBeLessThanOrEqual(VIEW.width - 12);
  });

  it("目标贴左边缘时气泡不溢出到负值", () => {
    const placed = placePop(rect(2, 300, 20, 32), POP, VIEW);
    expect(placed.left).toBeGreaterThanOrEqual(12);
  });
});

describe("冷启动视口与居中", () => {
  /** 造一组候选值；没给的按「都正常」补齐。 */
  function inputs(over: {
    layout?: { width: number; height: number };
    visual?: { width: number; height: number } | null;
    outer?: { width: number; height: number };
    screen?: { width: number; height: number };
  }) {
    return {
      layout: over.layout ?? { width: 1200, height: 800 },
      visual: over.visual === undefined ? { width: 1200, height: 780 } : over.visual,
      outer: over.outer ?? { width: 1230, height: 840 },
      screen: over.screen ?? { width: 1920, height: 1040 },
    };
  }

  it("尺寸正常时取最小的真实视口", () => {
    // 正常环境下 visualViewport 略小于 innerHeight（差的是滚动条/边框）。
    expect(viewport(inputs({}))).toEqual({ width: 1200, height: 780 });
  });

  it("layout viewport 远大于窗口时靠其他候选兜住", () => {
    // WebView 冷启动实测到过 innerHeight 上万像素；四个候选里三个是可信的。
    const huge = { width: 1200, height: 13000 };
    expect(viewport(inputs({ layout: huge, visual: huge }))).toEqual({
      width: 1200,
      height: 840,
    });
  });

  it("没有 visualViewport 时仍能钳住", () => {
    expect(
      viewport(inputs({ layout: { width: 1200, height: 13000 }, visual: null })),
    ).toEqual({ width: 1200, height: 840 });
  });

  it("窗口比屏幕小得多时也不会落到窗口外", () => {
    // 屏幕 1040、窗口只有 600 高：只看屏幕会算出 top=420，气泡底 620 出界。
    // 取 outer(640) 与 visual(600) 的最小值才对。
    const view = viewport(
      inputs({
        layout: { width: 1200, height: 13000 },
        visual: { width: 1200, height: 600 },
        outer: { width: 1230, height: 640 },
      }),
    );
    expect(view.height).toBe(600);
    const at = centerPop(POP, view);
    expect(at.top + POP.height).toBeLessThanOrEqual(view.height - 12);
  });

  it("气泡居中且完整落在视口内", () => {
    expect(centerPop(POP, VIEW)).toEqual({ top: 300, left: 428 });
  });

  it("气泡比视口还大时退到边距，不出现负坐标", () => {
    const at = centerPop({ width: 400, height: 900 }, { width: 320, height: 400 });
    expect(at.top).toBe(12);
    expect(at.left).toBe(12);
  });
});

describe("holeOf", () => {
  it("四周留出呼吸位", () => {
    const hole = holeOf(rect(100, 100, 200, 40), VIEW, 6);
    expect(hole).toEqual({ left: 94, top: 94, width: 212, height: 52 });
  });

  it("贴边时不出现负坐标", () => {
    const hole = holeOf(rect(0, 0, 50, 20), VIEW, 6);
    expect(hole.left).toBe(0);
    expect(hole.top).toBe(0);
    // 左侧的呼吸位被裁掉了，但宽度仍按目标的右侧算，不会跟着缩水
    // ——否则贴左栏的目标会被洞切掉一半。
    expect(hole.width).toBe(62);
  });

  it("目标超出视口时洞被裁到视口内", () => {
    const hole = holeOf(rect(1150, 760, 200, 100), VIEW, 6);
    expect(hole.left + hole.width).toBeLessThanOrEqual(VIEW.width);
    expect(hole.top + hole.height).toBeLessThanOrEqual(VIEW.height);
  });
});
