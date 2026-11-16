/**
 * 旧版 v1 清单补齐向导。
 *
 * 钉住三件事：v1 候选不能被判成「不可登记」，用户填的四项要原样送回后端，
 * 以及生成物不给信任相关字段留输入框——「补字段」不能变成把本地目录
 * 伪装成官方签名包的通道。
 */

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import type { ScanCandidate, V1Preview } from "../src/bridge.js";
import { classifyCandidates, readV1Options, v1Form } from "../src/ui/views/import.js";

function candidate(over: Partial<ScanCandidate>): ScanCandidate {
  return {
    path: "E:/src/dounai-input",
    module_id: "dounai-input",
    name: "输入接入层",
    version: "0.2.0",
    category: "core",
    signed: false,
    registered: false,
    problem: null,
    v1: false,
    instance_module_id: null,
    ...over,
  };
}

function preview(): V1Preview {
  return {
    path: "E:/src/dounai-input",
    source_sha256: "a".repeat(64),
    options: {
      module_id: "local/dounai-input",
      role: "input",
      variant: "stable",
      ports: [8101],
      deps_kind: "pyproject",
      deps_path: "pyproject.toml",
      command: "{venv_python} -m dounai_input serve",
      auxiliary_kind: null,
    },
    manifest: {},
    manifest_text: "{}",
    warnings: ["源码目录只读", "首次启动重建虚拟环境"],
    problem: null,
  };
}

function fakeRoot(values: Record<string, string>): HTMLElement {
  return {
    querySelector: (selector: string) => {
      const id = selector.replace("#", "");
      return values[id] === undefined ? null : { value: values[id] };
    },
  } as unknown as HTMLElement;
}

describe("候选分类", () => {
  it("v1 清单单独成组，既不批量登记也不算不兼容", () => {
    const groups = classifyCandidates([
      candidate({ problem: "旧版 v1 manifest（缺 schema_version: 2）" }),
      candidate({ v1: true, problem: "旧版 v1 manifest（缺 schema_version: 2）" }),
      candidate({}),
      candidate({ problem: "签名段不完整" }),
    ]);
    expect(groups.pending).toHaveLength(1);
    expect(groups.v1Pending).toHaveLength(1);
    expect(groups.blocked).toHaveLength(2);
  });

  it("已登记的实例不再出现在任何待办组", () => {
    const groups = classifyCandidates([
      candidate({ registered: true, v1: true, problem: "旧版", instance_module_id: "local/dounai-input" }),
    ]);
    expect(groups.pending).toHaveLength(0);
    expect(groups.v1Pending).toHaveLength(0);
    expect(groups.blocked).toHaveLength(0);
  });
});

describe("向导字段", () => {
  const base = {
    "v1-module-id": "local/dounai-input",
    "v1-role": "input",
    "v1-variant": "stable",
    "v1-ports": "8101",
    "v1-deps": "pyproject",
    "v1-deps-path": "pyproject.toml",
    "v1-command": "{venv_python} -m dounai_input serve",
    "v1-aux": "tooling",
  };

  it("读回用户拍板的四项，端口只收合法整数", () => {
    const options = readV1Options(fakeRoot({ ...base, "v1-ports": "8101, 99999, x" }));
    expect(options.module_id).toBe("local/dounai-input");
    expect(options.role).toBe("input");
    expect(options.variant).toBe("stable");
    expect(options.ports).toEqual([8101]);
    expect(options.deps_kind).toBe("pyproject");
    expect(options.command).toBe("{venv_python} -m dounai_input serve");
  });

  it("总装角色不写依赖，旁路角色才带 auxiliary_kind", () => {
    const shell = readV1Options(fakeRoot({ ...base, "v1-role": "shell", "v1-ports": "" }));
    expect(shell.deps_kind).toBeNull();
    expect(shell.auxiliary_kind).toBeNull();
    expect(shell.ports).toEqual([]);

    const auxiliary = readV1Options(fakeRoot({ ...base, "v1-role": "auxiliary" }));
    expect(auxiliary.auxiliary_kind).toBe("tooling");
    expect(readV1Options(fakeRoot(base)).auxiliary_kind).toBeNull();
  });

  it("信任相关字段没有输入框，改不了 C 类判定", () => {
    const html = v1Form(preview());
    for (const id of ["v1-category", "v1-signature", "v1-files", "v1-publisher-fp", "v1-verdict"]) {
      expect(html).not.toContain(`id="${id}"`);
    }
    expect(html).toContain("源码目录不会被修改");
    expect(html).toContain("重建虚拟环境");
    expect(html).toContain("local/dounai-input");
  });

  it("预览里展示的启动命令是转换后的占位符形式", () => {
    expect(v1Form(preview())).toContain("{venv_python} -m dounai_input serve");
  });
});

describe("本地目录的系统选择器", () => {
  const bridge = readFileSync(resolve(process.cwd(), "src/bridge.ts"), "utf8");
  const view = readFileSync(resolve(process.cwd(), "src/ui/views/import.ts"), "utf8");

  it("路径输入框旁边给出选择目录按钮", () => {
    expect(view).toMatch(/data-act="pick-dir"/);
    expect(view).toContain("选择目录…");
    expect(view).toMatch(/\{ act: "pick-dir", run: \(\) => pickDir\(\) \}/);
  });

  it("对话框按目录模式打开，选完写回输入框", () => {
    expect(bridge).toMatch(/export async function pickDirectory/);
    expect(bridge).toMatch(/directory: true/);
    expect(view).toMatch(/getElementById\("import-root"\)/);
    expect(view).toMatch(/await pickDirectory\(\)/);
  });
});
