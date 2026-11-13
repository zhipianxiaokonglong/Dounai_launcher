/**
 * 日志行的解析与过滤：全是纯函数，不碰 DOM。
 *
 * 抽出来的理由很实际——「哪一行算错误」决定了用户点「错误」筛选时看不看得到
 * 出问题的原因。这条规则必须能被测试钉住，而不是埋在渲染函数里靠肉眼验证。
 *
 * 行格式由 `diag_log::format_line` 决定：`2026-11-13T13:38:52Z [scope] 正文`。
 * 子进程的 stdout 也走同一条格式，所以来源可能是任意 scope 名。
 */

export type Level = "error" | "warn" | "ok" | "info";
export type LevelFilter = "all" | "warn" | "error";

export interface ParsedLine {
  /** 原始整行，复制与显示都以它为准。 */
  raw: string;
  /** 1 起的行号，按整份尾部计算；过滤后不变，便于和文件对照。 */
  index: number;
  /** 本地时间 `HH:MM:SS`；格式不符时为空串。 */
  time: string;
  /** 不是今天时填 `MM-DD`，否则为空串。 */
  day: string;
  scope: string;
  message: string;
  level: Level;
}

const LINE_RE = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2}:\d{2})Z \[([^\]]+)\] (.*)$/;

const ERROR_RE = /(error|failed|failure|panic|traceback|exception|失败|错误)/i;
const WARN_RE = /(warn|warning|retry|timeout|unhealthy|警告|超时|重试)/i;
const OK_RE = /(spawned|health 200|phase=running|started|succeeded|\bok\b)/i;

/**
 * 判定级别。顺序有意义：`failed to warn` 这类行同时命中多组，
 * 先判错误——漏报错误比多报错误的代价高。
 */
export function levelOf(line: string): Level {
  if (ERROR_RE.test(line)) return "error";
  if (WARN_RE.test(line)) return "warn";
  if (OK_RE.test(line)) return "ok";
  return "info";
}

/** 解析一行；不符合格式的行原样作为正文，级别仍按内容判定。 */
export function parseLine(raw: string, index: number, now: Date = new Date()): ParsedLine {
  const match = LINE_RE.exec(raw);
  if (!match) {
    return { raw, index, time: "", day: "", scope: "", message: raw, level: levelOf(raw) };
  }
  const date = match[1] ?? "";
  const time = match[2] ?? "";
  const scope = match[3] ?? "";
  const message = match[4] ?? raw;

  const pad = (value: number) => String(value).padStart(2, "0");
  const today = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
  return {
    raw,
    index,
    time,
    day: date === today ? "" : date.slice(5),
    scope,
    message,
    level: levelOf(raw),
  };
}

/** 级别与关键词过滤；关键词同时匹配来源与正文。 */
export function matchesFilter(row: ParsedLine, level: LevelFilter, query: string): boolean {
  if (level === "error" && row.level !== "error") return false;
  if (level === "warn" && row.level !== "error" && row.level !== "warn") return false;
  const needle = query.trim().toLowerCase();
  if (!needle) return true;
  return row.raw.toLowerCase().includes(needle);
}

/** 大小的人类可读写法，用于文件下拉。 */
export function humanBytes(size: number): string {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KiB`;
  return `${(size / (1024 * 1024)).toFixed(2)} MiB`;
}
