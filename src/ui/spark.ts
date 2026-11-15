/**
 * 迷你趋势线：把采样点压成一条折线。
 *
 * 只做一件事——给概览资源表每行一条 52×14 的曲线。数值里有 null（首轮没有
 * 差分基准）时沿用上一个已知值：曲线中途断开会让人以为是采样丢了，而实际
 * 只是那一刻算不出百分比。
 */
export function sparklinePoints(
  values: (number | null)[],
  width = 52,
  height = 14,
): string {
  const known = values.filter((value): value is number => typeof value === "number");
  // 一个点连不成线，两点才是一条趋势；不足时返回空串，界面不画。
  if (known.length < 2) return "";

  const min = Math.min(...known);
  const max = Math.max(...known);
  const span = max - min || 1;
  const step = values.length > 1 ? width / (values.length - 1) : width;
  const points: string[] = [];
  let cursor = known[0] ?? 0;

  values.forEach((value, index) => {
    if (typeof value === "number") cursor = value;
    const ratio = (cursor - min) / span;
    // 上下各留 1px，线不会贴着边框被裁掉
    const y = height - 1 - ratio * (height - 2);
    points.push(`${(index * step).toFixed(1)},${y.toFixed(1)}`);
  });
  return points.join(" ");
}

/** 一整条 `<polyline>`，直接塞进 innerHTML。 */
export function sparklineSvg(values: (number | null)[]): string {
  const points = sparklinePoints(values);
  if (!points) return "";
  return `<svg class="spark" viewBox="0 0 52 14" width="52" height="14" role="presentation" aria-hidden="true" focusable="false"><polyline points="${points}" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" stroke-linecap="round"/></svg>`;
}
