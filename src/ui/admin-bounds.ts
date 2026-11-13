/**
 * 内嵌管理页的坐标计算。
 *
 * 子 WebView 是原生视图，位置相对主窗口客户区，不随网页滚动。容器若被滚出
 * 视口或窗口被缩得很小，直接把 `getBoundingClientRect()` 的结果下发会带负
 * 坐标或零尺寸，后端按 `admin_host::validate_mount_request` 一律硬拒。这里
 * 统一夹取，并明确告诉调用方「这一帧没有可用位置」而不是伪造一个尺寸。
 *
 * 抽成纯函数是为了能在无窗口的 CI 里测：桌面端才有的 WebView 路径，至少
 * 坐标这一段能被单测钉住。
 */

import type { AdminBounds } from "../bridge.js";

export interface ViewSize {
  width: number;
  height: number;
}

/** 后端要求宽高严格大于 0；小于一像素的可见区域当作没有位置。 */
export const MIN_SIDE = 1;

export function clampAdminBounds(rect: AdminBounds, view: ViewSize): AdminBounds | null {
  const parts = [rect.x, rect.y, rect.width, rect.height, view.width, view.height];
  if (!parts.every(Number.isFinite)) return null;
  const left = Math.max(0, Math.min(rect.x, view.width));
  const top = Math.max(0, Math.min(rect.y, view.height));
  const right = Math.max(0, Math.min(rect.x + rect.width, view.width));
  const bottom = Math.max(0, Math.min(rect.y + rect.height, view.height));
  const width = right - left;
  const height = bottom - top;
  if (!Number.isFinite(width) || !Number.isFinite(height)) return null;
  if (width < MIN_SIDE || height < MIN_SIDE) return null;
  return { x: left, y: top, width, height };
}
