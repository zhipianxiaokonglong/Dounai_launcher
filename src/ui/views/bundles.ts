/**
 * 套装页：回答「一次装齐要装哪些东西」。
 *
 * 页面只做两件事：把套装定义摊开成可读的清单，并说明当前为什么不能
 * 在线安装。安装计划本身由 `planInstall` 计算——「最小安装原则」是
 * 硬逻辑（模型权重默认不装、core 不可勾除），界面不重复判断。
 */

import { planInstall, type InstallPlan } from "../../bundle.js";import { bindActions, esc } from "../dom.js";
import { icon } from "../icons.js";
import { store } from "../store.js";
import type { ViewSlots } from "./modules.js";

/** 当前选中的套装 id；切页不丢，避免每次回来都从第一个看起。 */
let selected: string | null = null;

export function renderBundles(slots: ViewSlots): void {
  if (!store.bundles.some((bundle) => bundle.bundle_id === selected)) {
    selected = store.bundles[0]?.bundle_id ?? null;
  }
  renderHead(slots.head);
  renderBody(slots.body);
}

function currentPlan(): InstallPlan | null {
  const bundle = store.bundles.find((item) => item.bundle_id === selected);
  if (!bundle) return null;
  return planInstall(bundle, store.manifests);
}

function renderHead(head: HTMLElement): void {
  head.innerHTML = `
    <div>
      <h1>套装</h1>
      <div class="sub">${store.bundles.length} 个套装 · 清单按最小安装原则生成</div>
    </div>
    <div class="sp">
      <button class="btn btn--sm" type="button" data-act="goto-import">登记本地目录</button>
    </div>
  `;
  bindActions(head, [
    { act: "goto-import", run: () => navigate("import") },
  ]);
}

function renderBody(body: HTMLElement): void {
  if (store.bundles.length === 0) {
    body.innerHTML = `
      <div class="placeholder">
        <span class="ph-ico">${icon("bundles", 18)}</span>
        <b>没有读到套装定义</b>
        <p>套装来自安装包内的 <code>bundles/</code> 目录。目录缺失或文件损坏时这里是空的，重新安装启动器可以恢复。</p>
        <div class="row" style="justify-content:center">
          <button class="btn btn--sm" type="button" data-act="goto-import">登记本地目录</button>
        </div>
      </div>
    `;
    bindActions(body, [{ act: "goto-import", run: () => navigate("import") }]);
    return;
  }

  const plan = currentPlan();
  body.innerHTML = `
    <div class="cards">
      ${store.bundles.map((bundle) => bundleCard(bundle.bundle_id)).join("")}
    </div>
    <div class="grp" style="margin-top:18px">
      <span class="grp-name">安装清单</span>
      <span class="grp-line"></span>
      <span class="grp-stat" id="plan-stat">${plan ? `${plan.steps.length} 项` : ""}</span>
    </div>
    <div id="plan-detail"></div>
  `;

  renderDetail(body);
  bindActions(body, [
    {
      act: "select",
      run: (node) => {
        const id = node.dataset.id ?? "";
        if (!id || id === selected) return;
        selected = id;
        renderBody(body);
      },
    },
    { act: "goto-import", run: () => navigate("import") },
  ]);
}

function bundleCard(bundleId: string): string {
  const bundle = store.bundles.find((item) => item.bundle_id === bundleId);
  if (!bundle) return "";
  const isOn = bundle.bundle_id === selected;
  const packs = bundle.model_packs.length;
  const modules = bundle.modules.length;

  return `
    <div class="card${isOn ? " card--on" : ""}" data-act="select" data-id="${esc(bundle.bundle_id)}"
         role="button" tabindex="0" aria-pressed="${isOn}">
      <div class="card-top">
        <div class="role-ico" aria-hidden="true">${icon("bundles", 13)}</div>
        <div style="min-width:0">
          <span class="card-name">${esc(bundle.name)}</span>
          <span class="card-id">${esc(bundle.bundle_id)}</span>
        </div>
        <span class="sp">${isOn ? '<span class="chip chip--active">已选中</span>' : ""}</span>
      </div>
      <p class="card-desc">${esc(bundle.description)}</p>
      <div class="meta">
        <span><b>${modules}</b> 个模块</span>
        <span><b>${packs}</b> 个模型包</span>
        ${packs === 0 ? '<span class="ok-text">不含模型权重</span>' : ""}
        ${bundle.requires_explicit_consent ? '<span class="warn-text">需显式确认</span>' : ""}
      </div>
    </div>
  `;
}

function renderDetail(body: HTMLElement): void {
  const host = body.querySelector<HTMLElement>("#plan-detail");
  if (!host) return;
  const plan = currentPlan();
  if (!plan) {
    host.innerHTML = "";
    return;
  }

  const steps = plan.steps
    .map(
      (step) => `
      <div class="envrow">
        <span class="k">${step.kind === "model_pack" ? "模型" : "模块"}</span>
        <span class="v">${esc(step.name)}</span>
        <span class="cap">${esc(step.id)}</span>
        <span class="sp"></span>
        ${step.locked ? '<span class="chip">不可勾除</span>' : ""}
        <span class="cap">${esc(step.reason)}</span>
      </div>
    `,
    )
    .join("");

  const recos = plan.recommendations
    .map(
      (item) => `
      <div class="envrow">
        <span class="k">推荐</span>
        <span class="v">${esc(item.name)}</span>
        <span class="cap">${esc(item.id)}</span>
        <span class="sp"></span>
        <span class="cap">${item.size_mb} MB</span>
      </div>
    `,
    )
    .join("");

  const warnings = plan.warnings.length
    ? `<div class="alert" style="margin-top:10px">
        ${plan.warnings.map((text) => `<div>${esc(text)}</div>`).join("")}
      </div>`
    : "";

  host.innerHTML = `
    <div class="envlist">
      ${steps || '<div class="envrow"><span class="k">清单</span><span class="v">这个套装没有列出安装项</span></div>'}
      ${recos}
    </div>
    ${warnings}
    <div class="alert" style="margin-top:10px">
      在线安装尚未接通：启动器还没有可用的模块下载源。改用「登记本地目录」登记已下载的模块，或等软件源发布后再试。
    </div>
    <div class="toolbar" style="margin-top:12px">
      <button class="btn btn--primary" type="button" disabled>开始安装</button>
      <span class="cap">按钮置灰的原因见上方说明。</span>
    </div>
  `;
}

function navigate(view: string): void {
  window.dispatchEvent(new CustomEvent("dounai:navigate", { detail: view }));
}
