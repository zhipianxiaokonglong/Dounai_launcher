/**
 * 设置页：只放会改变行为的项。
 *
 * 公钥只做只读展示与撤销，不提供「信任任意公钥」的一键入口——
 * 登记新指纹必须经过确认框，把「你正在信任谁」显式说出来。
 */

import {
  isDesktop,
  refreshCatalog,
  revokeTrustedKey,
  confirmTrustedKey,
  setKeepVersions,
  type CatalogResponse,
} from "../../bridge.js";
import { confirmModal, esc, openModal, toast } from "../dom.js";
import { icon } from "../icons.js";
import { describeError, refreshSettings, store } from "../store.js";
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
let catalogResult: CatalogResponse | null = null;

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
  `;
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
      "只换令牌，不重建界面",
      `<div class="seg">${seg("dark", "深色")}${seg("light", "浅色")}${seg("auto", "跟随系统")}</div>`,
    ) +
    row(
      "界面密度",
      "紧凑为默认",
      `<div class="inputbox"><span>紧凑 · 13px 基准</span><span class="hint">固定</span></div>`,
    ) +
    `<p class="cap">深浅两套共用同一组语义令牌：主色只表示「可点 / 当前」，绿色只表示「进程健康」。切换不会丢失滚动位置与展开状态。</p>`
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

  const k1 = settings.trust.builtin.find((key) => key.key_id === "K1");
  const k5 = settings.trust.builtin.find((key) => key.key_id === "K5");
  const trusted = settings.trust.trusted;

  const trustedHtml =
    trusted.length === 0
      ? `<p class="cap" style="margin:0">还没有登记额外的发布者。TOFU 只在你确认过某个指纹后才会写入。</p>`
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
      "Catalog 公钥 K5",
      "签目录清单",
      `<div class="ctl--row" style="display:flex;gap:8px;align-items:center">
        <span class="fp">${esc(settings.k5_fingerprint)}</span>
        <button class="btn btn--sm" type="button" data-act="copy-fp" data-id="${esc(settings.k5_fingerprint)}">复制</button>
      </div>`,
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
      `<p class="cap" style="margin:0">A 类已验签可直接安装；B/C 类需逐次确认；D 类硬拒，不写入实例表，也不提供确认入口。</p>`,
    ) +
    `<p class="cap">指纹全量等宽展示而非截断：截断会让人误以为「差不多就行」，而指纹变更正是必须一眼看出来的东西。${
      k1 && k5 ? ` 内置 ${esc(k1.key_id)} / ${esc(k5.key_id)} 两把钥匙职责分离，互不复用。` : ""
    }</p>`
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
      "清理",
      "先停进程再删",
      `<div><button class="btn btn--sm" type="button" data-act="goto-env">到环境页清除本地数据…</button></div>`,
    )
  );
}

/* ---------------- 网络 ---------------- */

function networkHtml(): string {
  const entries = catalogResult?.entries.length;
  const status = catalogResult
    ? catalogResult.source === "unavailable"
      ? `<span class="chip chip--fail">不可用</span>`
      : `<span class="chip chip--run">${esc(catalogResult.source)} · ${entries} 条</span>`
    : "";
  return (
    row(
      "Catalog 源",
      "主源失败才回落",
      `<div class="inputbox" style="margin-bottom:6px"><span>sourceforge.net/projects/dounai-releases/files/catalog-v2.json</span><span class="hint">主</span></div>
       <p class="cap" style="margin:6px 0 0">内置源尚未发布，当前为空。可以手动填一个地址试拉取，结果只用于验证通道。</p>`
    ) +
    row(
      "手动拉取",
      "需在出网白名单内",
      `<div style="display:flex;gap:8px;align-items:center">
        <input id="catalog-url" class="input" style="flex:1" placeholder="https://…/catalog-v2.json" />
        <button class="btn btn--sm" type="button" data-act="fetch-catalog">拉取</button>
        ${status}
      </div>
      ${
        catalogResult?.reason
          ? `<p class="cap" style="margin:6px 0 0">原因：${esc(catalogResult.reason)}</p>`
          : ""
      }`
    ) +
    row(
      "出网白名单",
      "空集即不允许任何外部请求",
      `<p class="cap" style="margin:0">当前版本的下载与清单请求只允许白名单内的主机；白名单为空时，远程资源请求数为零。</p>`
    )
  );
}

/* ---------------- 诊断 ---------------- */

function diagnosticsHtml(): string {
  const env = store.env;
  return (
    row(
      "日志",
      "每实例一份",
      `<p class="cap" style="margin:0">启动器事件始终记录：拉起、退出码、health 与契约探测、停止。单文件超过 2 MiB 轮转，保留 3 份。</p>`
    ) +
    row(
      "子进程输出",
      "默认关",
      `<p class="cap" style="margin:0">模块 stdout/stderr 默认丢弃——输出里可能有你的对话内容。需要排障时用环境变量 <code>DOUNAI_LOG_CHILD_OUTPUT=1</code> 启动启动器。</p>`
    ) +
    row(
      "本地模块登记",
      "本次会话有效",
      `<div style="display:flex;gap:8px;align-items:center">
        <span class="chip ${store.devMode ? "chip--run" : "chip--stop"}">${store.devMode ? "已开启" : "未开启"}</span>
        <button class="btn btn--sm" type="button" data-act="goto-import">${
          store.devMode ? "登记本地目录" : "开启后登记目录"
        }</button>
      </div>`
    ) +
    row(
      "数据根",
      env?.disk.path ?? "未检测",
      `<p class="cap" style="margin:0">安装根是只读的程序目录，数据根放下载、实例与日志，两者必须在不同位置。</p>`
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

  form.querySelector('[data-act="fetch-catalog"]')?.addEventListener("click", () => {
    const input = form.querySelector<HTMLInputElement>("#catalog-url");
    void fetchCatalog(input?.value.trim() ?? "");
  });

  form.querySelector('[data-act="goto-env"]')?.addEventListener("click", () => {
    window.dispatchEvent(new CustomEvent("dounai:navigate", { detail: "env" }));
  });

  form.querySelector('[data-act="goto-import"]')?.addEventListener("click", () => {
    window.dispatchEvent(new CustomEvent("dounai:navigate", { detail: "import" }));
  });
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

async function fetchCatalog(url: string): Promise<void> {
  if (!url) {
    toast("请先填写 catalog 地址", "error");
    return;
  }
  try {
    const result = await refreshCatalog(url);
    catalogResult = result;
    if (result.source === "unavailable") {
      toast(`清单不可用：${result.reason ?? "未说明原因"}`, "error");
    } else {
      toast(`已拉取 ${result.entries.length} 条（${result.source}）`, "ok");
    }
  } catch (error) {
    toast(`拉取失败：${describeError(error)}`, "error");
  }
  const body = document.getElementById("pbody");
  if (body) renderBody(body);
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
