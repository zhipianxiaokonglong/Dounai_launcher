import { describe, expect, it } from "vitest";
import { clampAdminBounds } from "../src/ui/admin-bounds.js";
import type { AdminBounds } from "../src/bridge.js";

const VIEW = { width: 1200, height: 800 };

function rect(x: number, y: number, width: number, height: number): AdminBounds {
  return { x, y, width, height };
}

describe("clampAdminBounds", () => {
  it("完全可见时原样返回", () => {
    const r = rect(232, 120, 900, 600);
    expect(clampAdminBounds(r, VIEW)).toEqual(r);
  });

  it("容器滚出视口时夹掉负坐标", () => {
    // 页面若出现滚动，容器上沿可能是 -40；子 WebView 只认窗口坐标，
    // 负坐标会被 admin_host::validate_mount_request 硬拒。
    expect(clampAdminBounds(rect(232, -40, 900, 600), VIEW)).toEqual(
      rect(232, 0, 900, 560),
    );
  });

  it("超出右边界与下边界时按可见部分裁", () => {
    expect(clampAdminBounds(rect(1100, 700, 400, 400), VIEW)).toEqual(
      rect(1100, 700, 100, 100),
    );
  });

  it("完全不可见时返回 null，不伪造尺寸", () => {
    expect(clampAdminBounds(rect(-500, 0, 100, 100), VIEW)).toBeNull();
    expect(clampAdminBounds(rect(0, 900, 100, 100), VIEW)).toBeNull();
  });

  it("零宽零高与非有限值都返回 null", () => {
    expect(clampAdminBounds(rect(0, 0, 0, 100), VIEW)).toBeNull();
    expect(clampAdminBounds(rect(0, 0, 100, 0), VIEW)).toBeNull();
    expect(clampAdminBounds(rect(Number.NaN, 0, 100, 100), VIEW)).toBeNull();
    expect(clampAdminBounds(rect(0, 0, Number.POSITIVE_INFINITY, 100), VIEW)).toBeNull();
  });

  it("窗口被缩到极小时仍不会给出越界坐标", () => {
    const tiny = { width: 320, height: 200 };
    const r = clampAdminBounds(rect(232, 120, 900, 600), tiny);
    expect(r).toEqual(rect(232, 120, 88, 80));
    expect(r!.x + r!.width).toBeLessThanOrEqual(tiny.width);
    expect(r!.y + r!.height).toBeLessThanOrEqual(tiny.height);
  });
});
