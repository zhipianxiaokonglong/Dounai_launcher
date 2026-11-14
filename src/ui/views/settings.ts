/**
 * 设置页：只放会改变行为的项。
 *
 * 公钥只做只读展示与撤销，不提供「信任任意公钥」的一键入口——
 * 登记新指纹必须经过确认框，把「你正在信任谁」显式说出来。
 */

import {
  enableDevMode,
  isDesktop,
  revokeTrustedKey,
  confirmTrustedKey,
  setChildOutput,
  setKeepVersions,
  storageFacts,
  type StorageFacts,
} from "../../bridge.js";
import { confirmModal, esc, openModal, toast } from "../dom.js";
import { icon } from "../icons.js";
import { describeError, refreshSettings, store } from "../store.js";
import { humanBytes } from "../loglines.js";
import { startTour } from "../tour.js";
import { currentThemeMode, setThemeMode, type ThemeMode } from "../theme.js";
import type { ViewSlots } from "./modules.js";

type SectionId = "appearance" | "trust" | "storage" | "network" | "diagnostics";

const SECTIONS: Array<{ id: SectionId; label: string }> = [
  { id: "appearance", label: "外观" },
  { id: "trust", label: "信任与公钥" },
  { id: "storage", label: "存储与清理" },
  { id: "network", label: "下载与网络" },
  { id: "diagnostics", label: "诊断" },
];

let active: SectionId = "appearance";

export async function renderSettings(slots: ViewSlots): Promise<void> {
  renderHead(slots.head);
  renderBody(slots.body);
}

function renderHead(head: HTMLElement): void {
  head.innerHTML = `
    <div>
      <h1>设置</h1>
      <div class="sub">launcher ${esc(__APP_VERSION__)} · schema v2</div>
    </div>
    <div class="sp">
      <button class="btn btn--sm" type="button" data-act="start-tour">走一遍引导</button>
    </div>
  `;
  // 放在页头而不是某个 tab 里：设置页默认停在「外观」，藏在末尾「诊断」
  // 分组的按钮基本没人找得到。页头在五个分组下都可见。
  head.querySelector('[data-act="start-tour"]')?.addEventListener("click", () => {
    startTour();
  });
}

function renderBody(body: HTMLElement): void {
  body.classList.add("pbody--flush");
  body.innerHTML = `
    <div class="sgrid">
      <div class="snav" role="tablist"></div>
      <div class="sform"></div>
    </div>
  `;

  const nav = body.querySelector(".snav");
  if (nav) {
    for (const section of SECTIONS) {
      const button = document.createElement("button");
      button.type = "button";
      button.role = "tab";
      button.textContent = section.label;
      if (section.id === active) button.classList.add("is-on");
      button.addEventListener("click", () => {
        active = section.id;
        renderBody(body);
      });
      nav.appendChild(button);
    }
  }

  const form = body.querySelector(".sform");
  if (!form) return;
  if (!isDesktop() && active !== "appearance") {
    form.innerHTML = `<div class="placeholder">
      <span class="ph-ico">${icon("alert", 18)}</span>
      <b>浏览器模式读不到这一组设置</b>
      <p>信任表、保留策略与下载源都写在启动器的数据根里，只有桌面端能读写。用启动器打开设置页可以查看和修改。</p>
    </div>`;
    return;
  }
  form.innerHTML = sectionHtml(active);
  bindSection(form);
}

/* ------------------------------------------------------------------ */

function sectionHtml(section: SectionId): string {
  switch (section) {
    case "appearance":
      return appearanceHtml();
    case "trust":
      return trustHtml();
    case "storage":
      return storageHtml();
    case "network":
      return networkHtml();
    case "diagnostics":
      return diagnosticsHtml();
  }
}

function row(label: string, hint: string, control: string): string {
  return `<div class="frow">
    <div class="lab">${esc(label)}${hint ? `<small>${esc(hint)}</small>` : ""}</div>
    <div class="ctl">${control}</div>
  </div>`;
}

/* ---------------- 外观 ---------------- */

function appearanceHtml(): string {
  const mode = currentThemeMode();
  const seg = (value: ThemeMode, label: string) =>
    `<button type="button" data-theme-set="${value}" class="${mode === value ? "is-on" : ""}">${label}</button>`;
  return (
    row(
      "主题",
      "深色、浅色或跟随系统",
      `<div class="seg">${seg("dark", "深色")}${seg("light", "浅色")}${seg("auto", "跟随系统")}</div>`,
    ) +
    row(
      "界面密度",
      "紧凑为默认",
      `<div class="inputbox"><span>紧凑 · 13px 基准</span><span class="hint">固定</span></div>`,
    ) +
    `<p class="cap">「跟随系统」取 Windows 的深浅设置。切换不会打断正在进行的操作，也不会改变模块状态。</p>`
  );
}

/* ---------------- 信任与公钥 ---------------- */

function trustHtml(): string {
  const settings = store.settings;
  if (!settings) {
    return isDesktop()
      ? `<p class="empty">读取设置失败，或尚未初始化信任表。重启启动器后重试。</p>`
      : `<p class="empty">浏览器模式没有信任表。</p>`;
  }

  const trusted = settings.trust.trusted;

  const trustedHtml =
    trusted.length === 0
      ? `<p class="cap" style="margin:0">还没有登记额外的发布者。你确认过某个指纹后，它才会出现在这里。</p>`
      : trusted
          .map(
            (key) => `
        <div class="inst" style="margin-bottom:6px">
          <div class="inst-row">
            <span class="dot ${key.revoked_at ? "dot--idle" : key.enabled ? "dot--run" : "dot--warn"}"></span>
            <span class="inst-name">${esc(key.label)}</span>
            <span class="chip ${key.revoked_at ? "chip--stop" : "chip--run"}" style="margin-left:auto">${
              key.revoked_at ? "已撤销" : key.enabled ? "生效中" : "已停用"
            }</span>
            <button class="btn btn--quiet btn--sm" type="button" data-act="revoke-key" data-id="${esc(key.fingerprint)}" ${
              key.revoked_at ? "disabled" : ""
            }>撤销</button>
          </div>
          <div class="fp" style="margin-top:4px">${esc(key.fingerprint)}</div>
        </div>`,
          )
          .join("");

  return (
    row(
      "模块签名公钥 K1",
      "签 zip 包，编译期内嵌",
      `<div class="ctl--row" style="display:flex;gap:8px;align-items:center">
        <span class="fp">${esc(settings.k1_fingerprint)}</span>
        <button class="btn btn--sm" type="button" data-act="copy-fp" data-id="${esc(settings.k1_fingerprint)}">复制</button>
      </div>`,
    ) +
    row(
      "官方源 catalog 公钥 K5",
      "签官方目录清单，编译期内嵌",
      `<div class="ctl--row" style="display:flex;gap:8px;align-items:center">
        <span class="fp">${esc(settings.k5_fingerprint)}</span>
        <button class="btn btn--sm" type="button" data-act="copy-fp" data-id="${esc(settings.k5_fingerprint)}">复制</button>
      </div>
      <p class="cap" style="margin:6px 0 0">这是内置官方源那一把。社区源各有自己的公钥，在「下载」页逐个显示与撤销。</p>`,
    ) +
    row(
      "已信任的发布者",
      "撤销后立即失效",
      `${trustedHtml}
       <div><button class="btn btn--sm" type="button" data-act="add-key">登记指纹…</button></div>`,
    ) +
    row(
      "判级规则",
      "A/B/C/D 四级",
      `<p class="cap" style="margin:0">A 类已验签可直接安装；B/C 类需要你逐次确认；D 类直接拒绝，也没有确认入口。</p>`,
    )
  );
}

/* ---------------- 存储 ---------------- */

function storageHtml(): string {
  const retention = store.settings?.retention;
  const keep = retention?.keep_versions ?? 2;
  const seg = [1, 2, 3]
    .map(
      (n) =>
        `<button type="button" data-keep="${n}" class="${keep === n ? "is-on" : ""}">${n}</button>`,
    )
    .join("");
  return (
    row(
      "保留版本数",
      "keep_versions",
      `<div class="ctl--row" style="display:flex;gap:8px;align-items:center">
        <div class="seg">${seg}</div>
        <span class="cap" style="margin:0">升级后旧版本留 ${keep} 份，回滚用</span>
      </div>`,
    ) +
    row(
      "数据目录策略",
      retention?.data_dir_policy ?? "永不自动删除",
      `<p class="cap" style="margin:0">实例的 <code>data/</code> 放用户数据，卸载与清理都不会碰它。要清必须单独确认。</p>`,
    ) +
    row(
      "源码目录策略",
      retention?.source_dir_policy ?? "永不自动删除",
      `<p class="cap" style="margin:0">目录实例指向你自己的文件夹。启动器只登记路径，卸载时不会删除文件。</p>`,
    ) +
    row(
      "磁盘占用",
      "解压后的大小",
      `<div id="storage-usage"><p class="cap" style="margin:0">正在统计…</p></div>`
    ) +
    row(
      "清理",
      "先停进程再删",
      `<div><button class="btn btn--sm" type="button" data-act="goto-env">到环境页清除本地数据…</button></div>`,
    )
  );
}

/* ---------------- 网络 ---------------- */

function networkHtml(): string {
  return (
    row(
      "下载源",
      "按源验签，不做全局优选",
      `<div><button class="btn btn--sm" type="button" data-act="goto-download">到「下载」页管理源…</button></div>
       <p class="cap" style="margin:6px 0 0">每个源带自己的 catalog 公钥与域名白名单。加源、启用停用、撤销公钥都在「下载」页做。</p>`
    ) +
    row(
      "出网白名单",
      "空集即不允许任何外部请求",
      `<p class="cap" style="margin:0">白名单由已启用源的域名逐个派生，不再有全局兜底列表；白名单为空时，远程资源请求数为零。</p>`
    )
  );
}

/* ---------------- 诊断 ---------------- */

function diagnosticsHtml(): string {
  const env = store.env;
  const onboarded = store.onboarding;
  return (
    row(
      "入门引导",
      onboarded?.completed ? (onboarded.skipped ? "上次是中途关掉" : "已走完") : "还没走过",
      `<div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap">
        <span class="chip ${onboarded?.completed && !onboarded.skipped ? "chip--run" : "chip--stop"}">${
          onboarded?.completed ? (onboarded.skipped ? "已跳过" : "已完成") : "未完成"
        }</span>
      </div>
      <p class="cap" style="margin:6px 0 0">引导是聚光灯，指着真实界面讲，不挡点击——可以边看边装、边改设置。想重看就点页头「走一遍引导」；自动弹出只发生在「没走过」且「一个实例都没装」时。</p>`
    ) +
    row(
      "日志",
      "每实例一份",
      `<p class="cap" style="margin:0">启动器事件始终记录：拉起、退出码、健康检查、停止。单个文件超过 2 MiB 轮转，保留最近 3 份。</p>`
    ) +
    row(
      "子进程输出",
      store.settings?.child_output ? "已开启" : "默认关",
      `<div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap">
        <span class="chip ${store.settings?.child_output ? "chip--run" : "chip--stop"}">${store.settings?.child_output ? "会写进日志" : "丢弃"}</span>
        <button class="btn btn--sm" type="button" data-act="child-output">${store.settings?.child_output ? "关闭捕获" : "开启捕获"}</button>
      </div>
      <p class="cap" style="margin:6px 0 0">模块 stdout/stderr 里可能有你的对话内容，默认丢弃。开启后只影响之后拉起的实例，已在跑的要重启才会接管；日志页也能直接切换。</p>`
    ) +
    row(
      "本地模块登记",
      "本次会话有效",
      `<div style="display:flex;gap:8px;align-items:center">
        <span class="chip ${store.devMode ? "chip--run" : "chip--stop"}">${store.devMode ? "已开启" : "未开启"}</span>
        <button class="btn btn--sm" type="button" data-act="dev-mode">${
          store.devMode ? "登记本地目录" : "开启本地目录登记"
        }</button>
      </div>`
    ) +
    row(
      "数据根",
      env?.disk.path ?? "未检测",
      `<p class="cap" style="margin:0">安装根是只读的程序目录，数据根放下载、实例与日志，两者必须在不同位置。</p>`
    ) +
    row(
      "install.json",
      store.settings?.install_note ? "已由启动器补写" : "与本次运行一致",
      `<p class="cap" style="margin:0">${
        store.settings?.install_note
          ? esc(store.settings.install_note)
          : "双根路径与随包 uv 的版本、哈希都记录在数据根的 state/install.json 里，启动时核对，不一致才会改写。"
      }</p>`
    ) +
    row(
      "开发者模式",
      "只进不退",
      `<p class="cap" style="margin:0">开启后可在「导入本地」登记任意目录为模块，重启启动器即失效。未开启时相关命令一律拒绝。</p>`
    )
  );
}

/* ------------------------------------------------------------------ */

function bindSection(form: Element): void {
  form.querySelectorAll<HTMLButtonElement>("[data-theme-set]").forEach((button) => {
    button.addEventListener("click", () => {
      const mode = button.dataset.themeSet;
      if (mode === "dark" || mode === "light" || mode === "auto") {
        setThemeMode(mode);
        const body = document.getElementById("pbody");
        if (body) renderBody(body);
      }
    });
  });

  form.querySelectorAll<HTMLButtonElement>("[data-keep]").forEach((button) => {
    button.addEventListener("click", () => {
      const value = Number(button.dataset.keep ?? "2");
      void saveKeepVersions(value);
    });
  });

  form.querySelector('[data-act="copy-fp"]')?.addEventListener("click", (event) => {
    const value = (event.currentTarget as HTMLElement).dataset.id ?? "";
    void navigator.clipboard
      .writeText(value)
      .then(() => toast("指纹已复制", "ok"))
      .catch((error: unknown) => toast(`复制失败：${describeError(error)}`, "error"));
  });

  form.querySelectorAll<HTMLButtonElement>('[data-act="revoke-key"]').forEach((button) => {
    button.addEventListener("click", () => {
      const fingerprint = button.dataset.id ?? "";
      confirmModal({
        title: "撤销这个发布者？",
        subtitle: fingerprint,
        danger: true,
        confirmLabel: "撤销",
        bodyHtml: `<p class="cap" style="margin:0">撤销后，由它签名的模块会退回 B/C 类判定，需要你逐次确认才能安装或更新。</p>`,
        onConfirm: async () => {
          try {
            const info = await revokeTrustedKey(fingerprint);
            toast(info.note, info.accepted ? "ok" : "info");
          } catch (error) {
            toast(`撤销失败：${describeError(error)}`, "error");
          }
          await reload();
        },
      });
    });
  });

  form.querySelector('[data-act="add-key"]')?.addEventListener("click", () => {
    openModal({
      title: "登记发布者指纹",
      subtitle: "sha256: + 64 位小写 hex",
      bodyHtml: `
        <div class="alert">
          <b>先核对再登记</b>
          指纹必须与发布者公开的完全一致。登记后由该密钥签名的模块会被判为 A 类，不再逐次确认。
        </div>
        <div class="field">
          <label for="key-fp">指纹</label>
          <input id="key-fp" class="input" style="width:100%" placeholder="sha256:…" />
        </div>
        <div class="field">
          <label for="key-label">名称</label>
          <input id="key-label" class="input" style="width:100%" placeholder="例如 dounai-official" />
        </div>
      `,
      actions: [
        { label: "取消", tone: "quiet", autofocus: true },
        {
          label: "登记",
          tone: "primary",
          onClick: async (root) => {
            const fp = root.querySelector<HTMLInputElement>("#key-fp")?.value.trim() ?? "";
            const label = root.querySelector<HTMLInputElement>("#key-label")?.value.trim() ?? "";
            if (!fp) {
              toast("请填写指纹", "error");
              return false;
            }
            try {
              const info = await confirmTrustedKey(fp, label || undefined);
              toast(info.note, "ok");
              await reload();
            } catch (error) {
              toast(`登记失败：${describeError(error)}`, "error");
              return false;
            }
            return true;
          },
        },
      ],
      onMount: (root) => {
        root.querySelector<HTMLInputElement>("#key-fp")?.focus();
      },
    });
  });

  form.querySelector('[data-act="goto-download"]')?.addEventListener("click", () => {
    window.dispatchEvent(new CustomEvent("dounai:navigate", { detail: "download" }));
  });

  form.querySelector('[data-act="goto-env"]')?.addEventListener("click", () => {
    window.dispatchEvent(new CustomEvent("dounai:navigate", { detail: "env" }));
  });

  // 存储占用要遍历数据根，几十毫秒到几百毫秒。先渲染占位，回来再填，
  // 不让整个设置页等一次全盘扫描。
  if (form.querySelector("#storage-usage")) void fillStorageUsage();

  form.querySelector('[data-act="dev-mode"]')?.addEventListener("click", () => {
    void openDevMode();
  });

  form.querySelector('[data-act="child-output"]')?.addEventListener("click", () => {
    const next = !(store.settings?.child_output ?? false);
    void setChildOutput(next)
      .then(async () => {
        await refreshSettings();
        toast(next ? "已开启子进程输出捕获，重启实例后生效" : "已关闭子进程输出捕获", "ok");
        const body = document.getElementById("pbody");
        if (body) renderBody(body);
      })
      .catch((error: unknown) => toast(`切换失败：${describeError(error)}`, "error"));
  });

  form.querySelector('[data-act="goto-import"]')?.addEventListener("click", () => {
    window.dispatchEvent(new CustomEvent("dounai:navigate", { detail: "import" }));
  });
}

async function fillStorageUsage(): Promise<void> {
  const host = document.getElementById("storage-usage");
  if (!host) return;
  let facts: StorageFacts;
  try {
    facts = await storageFacts();
  } catch (error) {
    host.innerHTML = `<p class="cap" style="margin:0">读不到占用：${esc(describeError(error))}</p>`;
    return;
  }
  const parts: Array<[string, number]> = [
    ["数据根", facts.data_bytes],
    ["解释器", facts.python_bytes],
    ["依赖缓存", facts.cache_bytes],
  ];
  host.innerHTML = `
    <div class="usage">
      ${parts
        .map(
          ([label, bytes]) =>
            `<span class="usage-i"><i>${esc(label)}</i><b>${esc(humanBytes(bytes))}</b></span>`,
        )
        .join("")}
      <span class="usage-i"><i>实例</i><b>${facts.instances}</b></span>
    </div>
    <p class="cap usage-path" title="${esc(facts.data_root)}">${esc(facts.data_root)}</p>
  `;
}

/**
 * 本地目录登记开关。
 *
 * 已开启就直接去导入页；没开启先真的调用 `enable_dev_mode`——
 * 以前这个按钮只是跳转，四个本地模块命令都要求 dev mode，
 * 于是「登记本地目录」这条路点到底也走不通。
 */
async function openDevMode(): Promise<void> {
  if (!store.devMode) {
    try {
      const info = await enableDevMode();
      store.devMode = info.enabled;
      toast("本地目录登记已开启，本次会话有效", "ok");
      await reload();
    } catch (error) {
      toast(`开启失败：${describeError(error)}`, "error");
      return;
    }
  }
  window.dispatchEvent(new CustomEvent("dounai:navigate", { detail: "import" }));
}

async function saveKeepVersions(value: number): Promise<void> {
  try {
    await setKeepVersions(value);
    await reload();
    toast(`保留版本数已设为 ${value}`, "ok");
  } catch (error) {
    toast(`保存失败：${describeError(error)}`, "error");
  }
}

async function reload(): Promise<void> {
  try {
    await refreshSettings();
  } catch (error) {
    toast(`读取设置失败：${describeError(error)}`, "error");
    return;
  }
  const body = document.getElementById("pbody");
  if (body) renderBody(body);
  window.dispatchEvent(new CustomEvent("dounai:settings-updated"));
}
