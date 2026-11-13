/**
 * 环境页：回答「为什么起不来」。
 *
 * 这一页不做图表，只做可复制的文本与明确的判定——环境体检的价值
 * 完全建立在数据真实上，读不到就写读不到，不填占位值。
 */

import { isDesktop, purgeLocalData } from "../../bridge.js";
import { confirmModal, esc, toast } from "../dom.js";
import { icon } from "../icons.js";
import { describeError, refreshEnv, store } from "../store.js";
import type { ViewSlots } from "./modules.js";

export async function renderEnv(slots: ViewSlots): Promise<void> {
  renderHead(slots.head);
  renderBody(slots.body);
}

function renderHead(head: HTMLElement): void {
  const env = store.env;
  const when = store.envCheckedAt
    ? store.envCheckedAt.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })
    : null;
  let sub = "未检测";
  if (!isDesktop()) sub = "浏览器模式无法检测本机环境";
  else if (env) sub = `${when ? `上次检测 ${when} · ` : ""}${env.healthy ? "全部通过" : `${env.problems.length} 项待处理`}`;

  head.innerHTML = `
    <div>
      <h1>环境</h1>
      <div class="sub">${esc(sub)}</div>
    </div>
    <div class="sp">
      <button class="btn btn--sm" type="button" data-act="copy-report" ${env ? "" : "disabled"}>复制报告</button>
      <button class="btn btn--primary btn--sm" type="button" data-act="recheck">重新检测</button>
    </div>
  `;
  head.querySelector<HTMLButtonElement>('[data-act="recheck"]')?.addEventListener("click", () => {
    void recheck();
  });
  head.querySelector<HTMLButtonElement>('[data-act="copy-report"]')?.addEventListener("click", () => {
    void copyReport();
  });
}

function renderBody(body: HTMLElement): void {
  body.innerHTML = "";

  if (!isDesktop()) {
    body.innerHTML = `<div class="placeholder">
      <span class="ph-ico">${icon("env", 18)}</span>
      <b>浏览器模式没有本机信息</b>
      <p>环境检测读的是这台机器的运行时、显卡与数据根磁盘。请在桌面端打开。</p>
    </div>`;
    return;
  }

  const env = store.env;
  if (!env) {
    body.innerHTML = `<div class="placeholder">
      <span class="ph-ico">${icon("env", 18)}</span>
      <b>还没有检测过</b>
      <p>点右上角「重新检测」。检测只读注册表、PATH 与磁盘余量，不改任何配置。</p>
    </div>`;
    return;
  }

  const rows: string[] = [];
  const row = (dot: string, key: string, value: string, chip: string) => `
    <div class="envrow">
      <span class="dot ${dot}"></span>
      <span class="k">${esc(key)}</span>
      <span class="v" title="${esc(value)}">${esc(value)}</span>
      <span class="sp">${chip}</span>
    </div>`;

  for (const [label, runtime] of [
    ["Python", env.python],
    ["Node.js", env.node],
    ["uv", env.uv],
  ] as const) {
    rows.push(
      row(
        runtime.available ? "dot--run" : "dot--fail",
        label,
        runtime.detail || (runtime.available ? runtime.version : "未找到"),
        runtime.available
          ? '<span class="chip chip--run">就绪</span>'
          : '<span class="chip chip--fail">缺失</span>',
      ),
    );
  }

  for (const gpu of env.gpus) {
    const vram = `${gpu.vram_gb} GB · ${gpu.name}${gpu.truncated ? "（显存读数可能被截断）" : ""}`;
    rows.push(
      row(
        gpu.usable ? "dot--run" : "dot--warn",
        "显存",
        vram,
        gpu.usable
          ? '<span class="chip chip--run">够用</span>'
          : '<span class="chip chip--warn">偏紧</span>',
      ),
    );
  }

  rows.push(
    row(
      env.disk.ok ? "dot--run" : "dot--fail",
      "数据根盘",
      `${env.disk.path} · ${env.disk.free_gb} GB 可用`,
      env.disk.ok
        ? '<span class="chip chip--run">够用</span>'
        : '<span class="chip chip--fail">空间不足</span>',
    ),
  );

  const trust = store.trust;
  if (trust) {
    const keys = store.settings?.trust.builtin.length ?? 0;
    rows.push(
      row(
        "dot--run",
        "模块签名",
        trust.trusted_keys > 0 ? `${trust.trusted_keys} 个公钥参与判定` : "仅内置公钥",
        `<span class="chip chip--run">${keys > 0 ? "已内置 K1/K5" : "fail-closed"}</span>`,
      ),
    );
  }

  const envBox = document.createElement("div");
  envBox.className = "envlist";
  envBox.innerHTML = rows.join("");
  body.appendChild(envBox);

  if (env.problems.length > 0) {
    const problems = document.createElement("div");
    problems.className = "alert";
    problems.style.marginTop = "14px";
    problems.innerHTML = `<b>需要处理</b>${env.problems.map((p) => esc(p)).join("<br>")}`;
    body.appendChild(problems);
  }

  body.insertAdjacentHTML(
    "beforeend",
    `<p class="cap">每行都可点击展开：显存偏紧会给「换更小量化」的建议，但不会自动改你的配置。</p>

    <div class="grp" style="margin-top:22px">
      <span class="grp-name">维护</span><span class="grp-line"></span>
    </div>
    <div class="envlist">
      <div class="envrow">
        <span class="dot dot--idle"></span>
        <span class="k">清除本地数据</span>
        <span class="v" style="font-family:var(--sans);white-space:normal">清缓存、状态水位与模块文件；上游对话与记忆不在范围内</span>
        <span class="sp"><button class="btn btn--sm" type="button" data-act="purge">清除…</button></span>
      </div>
      <div class="envrow">
        <span class="dot dot--idle"></span>
        <span class="k">日志与诊断</span>
        <span class="v" style="font-family:var(--sans);white-space:normal">每个实例一份日志，2 MiB × 3 份自动轮转</span>
        <span class="sp"><button class="btn btn--sm" type="button" data-act="goto-logs">打开日志</button></span>
      </div>
    </div>`,
  );

  body.querySelector<HTMLButtonElement>('[data-act="purge"]')?.addEventListener("click", () => {
    askPurge();
  });
  body.querySelector<HTMLButtonElement>('[data-act="goto-logs"]')?.addEventListener("click", () => {
    window.dispatchEvent(new CustomEvent("dounai:navigate", { detail: "logs" }));
  });
}

async function recheck(): Promise<void> {
  try {
    await refreshEnv();
    const body = document.getElementById("pbody");
    const head = document.getElementById("phead");
    if (body && head) {
      if (store.env?.healthy) toast("环境正常", "ok");
      else if (store.env) toast(`${store.env.problems.length} 项待处理`, "error");
      renderHead(head);
      renderBody(body);
    }
  } catch (error) {
    toast(`检测失败：${describeError(error)}`, "error");
  }
}

async function copyReport(): Promise<void> {
  const env = store.env;
  if (!env) return;
  const lines = [
    `Dounai 启动器环境报告 ${new Date().toISOString()}`,
    `Python: ${env.python.available ? env.python.version : "缺失"} (${env.python.detail})`,
    `Node.js: ${env.node.available ? env.node.version : "缺失"} (${env.node.detail})`,
    `uv: ${env.uv.available ? env.uv.version : "缺失"} (${env.uv.detail})`,
    ...env.gpus.map((g) => `GPU: ${g.name} ${g.vram_gb}GB usable=${g.usable}`),
    `数据根盘: ${env.disk.path} 剩余 ${env.disk.free_gb}GB ok=${env.disk.ok}`,
    `整体: ${env.healthy ? "通过" : "待处理"}`,
    ...env.problems.map((p) => `问题: ${p}`),
  ];
  try {
    await navigator.clipboard.writeText(lines.join("\n"));
    toast("报告已复制到剪贴板", "ok");
  } catch (error) {
    toast(`复制失败：${describeError(error)}`, "error");
  }
}

function askPurge(): void {
  confirmModal({
    title: "清除启动器本地数据？",
    danger: true,
    confirmLabel: "清除",
    bodyHtml: `
      <div class="alert">
        <b>会先停掉全部托管实例，再删除文件</b>
        外部启动的实例停不掉，会被跳过并记入结果。
      </div>
      <p class="cap" style="margin:0">范围：下载与清单缓存、catalog 水位、zip 实例的包目录。用户源码目录永不删除，实例的数据目录按契约保留。</p>
    `,
    onConfirm: async () => {
      try {
        const result = await purgeLocalData({ caches: true, state: true, module_files: true });
        const bits = [
          `缓存 ${result.removed_caches.length} 项`,
          `状态${result.state_cleared ? "已清" : "未清"}`,
          `模块目录 ${result.removed_module_dirs.length} 个`,
        ];
        if (result.errors.length > 0) {
          toast(`${bits.join(" · ")}\n${result.errors.join("\n")}`, "error");
        } else {
          toast(`清除完成：${bits.join(" · ")}`, "ok");
        }
      } catch (error) {
        toast(`清除失败：${describeError(error)}`, "error");
      }
      await recheck();
    },
  });
}
