/**
 * 本地安装的纯逻辑（哈希、编码、请求构造）。
 *
 * 单独成文件的理由：这些是可以在 Node 里直接单测的纯函数，
 * 放进 UI 就只能靠浏览器验证——而浏览器自动化对 `crypto.subtle`
 * 这类异步密码学操作支持不稳（虚拟时间不会等待它完成）。
 * 抽出来之后可以用已知向量断言哈希正确性。
 */

/** 十六进制 SHA-256。 */
export async function sha256Hex(bytes: ArrayBuffer): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/**
 * ArrayBuffer → base64。
 *
 * 分块处理：`String.fromCharCode(...arr)` 在数组很大时会把每个元素
 * 当作实参压栈，几 MB 的模块包足以触发 "Maximum call stack size exceeded"。
 */
export function toBase64(bytes: ArrayBuffer): string {
  const view = new Uint8Array(bytes);
  const chunk = 0x8000;
  let binary = "";
  for (let i = 0; i < view.length; i += chunk) {
    binary += String.fromCharCode(...view.subarray(i, i + chunk));
  }
  return btoa(binary);
}

export interface LocalInstallRequest {
  moduleId: string;
  category: string;
  version: string;
  zipBase64: string;
  expectedSha256: string;
  manifest: Record<string, unknown>;
}

export class LocalInstallInputError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "LocalInstallInputError";
  }
}

/**
 * 由文件与用户输入构造安装请求。
 *
 * 校验放在这里而不是 UI 里，便于测试；也保证"模块 id 必填"
 * 这类规则不会因为某个调用点忘记检查而被绕过。
 */
export async function buildLocalInstallRequest(
  fileBytes: ArrayBuffer,
  moduleIdRaw: string,
  category: string,
): Promise<LocalInstallRequest> {
  const moduleId = moduleIdRaw.trim();
  if (!moduleId) {
    throw new LocalInstallInputError("请填写模块 id（需与 manifest 声明一致）。");
  }
  // 与 Rust 侧 module_dir 的字符集保持一致的子集，提前给出友好提示
  if (!/^[A-Za-z0-9._-]+$/.test(moduleId) || moduleId.includes("..")) {
    throw new LocalInstallInputError(
      `模块 id 只能包含字母、数字、点、下划线、连字符，且不能含 ".."：${moduleId}`,
    );
  }
  if (!category.trim()) {
    throw new LocalInstallInputError("请选择模块分类。");
  }

  const expectedSha256 = await sha256Hex(fileBytes);
  return {
    moduleId,
    category: category.trim(),
    version: "local",
    zipBase64: toBase64(fileBytes),
    expectedSha256,
    // 本地包没有配套签名；发布版严格策略会据此拒绝——这是预期行为
    manifest: { module_id: moduleId },
  };
}
