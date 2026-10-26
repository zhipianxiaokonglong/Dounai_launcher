/**
 * 本地安装纯逻辑测试。
 *
 * 用**已知向量**验证哈希与编码，而不是"跑通就算过"——
 * 哈希算错会导致后端校验失败，用户看到的是"哈希不匹配"这种
 * 极难排查的错误，所以正确性必须硬断言。
 */

import { describe, expect, it } from "vitest";
import {
  buildLocalInstallRequest,
  LocalInstallInputError,
  sha256Hex,
  toBase64,
} from "../src/localinstall.js";

const enc = new TextEncoder();

function buf(s: string): ArrayBuffer {
  const u8 = enc.encode(s);
  return u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength) as ArrayBuffer;
}

describe("sha256Hex", () => {
  it("空输入的哈希符合标准", async () => {
    expect(await sha256Hex(new ArrayBuffer(0))).toBe(
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    );
  });

  it("abc 的哈希符合标准向量", async () => {
    expect(await sha256Hex(buf("abc"))).toBe(
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
  });

  it("输出为 64 位小写十六进制", async () => {
    const h = await sha256Hex(buf("dounai"));
    expect(h).toMatch(/^[0-9a-f]{64}$/);
  });

  it("不同内容得到不同哈希", async () => {
    const a = await sha256Hex(buf("a"));
    const b = await sha256Hex(buf("b"));
    expect(a).not.toBe(b);
  });
});

describe("toBase64", () => {
  it("空数组编码为空串", () => {
    expect(toBase64(new ArrayBuffer(0))).toBe("");
  });

  it("abc 编码为 YWJj", () => {
    expect(toBase64(buf("abc"))).toBe("YWJj");
  });

  it("能处理超过 32KB 的数据（分块路径）", () => {
    // 0x8000 = 32768；构造 100KB 确保走多块
    const big = new Uint8Array(100 * 1024);
    for (let i = 0; i < big.length; i++) big[i] = i % 256;
    const b64 = toBase64(big.buffer);
    // 100KB → base64 长度约为 4/3 倍
    expect(b64.length).toBe(Math.ceil(big.length / 3) * 4);
    expect(b64).toMatch(/^[A-Za-z0-9+/]*=*$/);
  });

  it("超大输入不抛栈溢出", () => {
    // 5MB：如果用展开运算符会直接爆栈，这里应正常返回
    const huge = new Uint8Array(5 * 1024 * 1024);
    expect(() => toBase64(huge.buffer)).not.toThrow();
  });
});

describe("buildLocalInstallRequest", () => {
  it("构造出完整请求并带上真实哈希", async () => {
    const bytes = buf("fake-zip-content");
    const req = await buildLocalInstallRequest(bytes, "dounai-input", "core");
    expect(req.moduleId).toBe("dounai-input");
    expect(req.category).toBe("core");
    expect(req.version).toBe("local");
    expect(req.expectedSha256).toBe(await sha256Hex(bytes));
    expect(req.zipBase64).toBe(toBase64(bytes));
    expect(req.manifest).toEqual({ module_id: "dounai-input" });
  });

  it("去掉模块 id 两侧空白", async () => {
    const req = await buildLocalInstallRequest(buf("x"), "  mod-a  ", "optional");
    expect(req.moduleId).toBe("mod-a");
  });

  it("模块 id 为空时报错", async () => {
    await expect(buildLocalInstallRequest(buf("x"), "   ", "core")).rejects.toBeInstanceOf(
      LocalInstallInputError,
    );
  });

  it("模块 id 含非法字符时报错", async () => {
    for (const bad of ["a/b", "a\\b", "有中文", "a b"]) {
      await expect(buildLocalInstallRequest(buf("x"), bad, "core")).rejects.toBeInstanceOf(
        LocalInstallInputError,
      );
    }
  });

  it("模块 id 含 .. 时报错（防路径穿越意图）", async () => {
    await expect(buildLocalInstallRequest(buf("x"), "..", "core")).rejects.toBeInstanceOf(
      LocalInstallInputError,
    );
    await expect(buildLocalInstallRequest(buf("x"), "a..b", "core")).rejects.toBeInstanceOf(
      LocalInstallInputError,
    );
  });

  it("分类为空时报错", async () => {
    await expect(buildLocalInstallRequest(buf("x"), "mod-a", "  ")).rejects.toBeInstanceOf(
      LocalInstallInputError,
    );
  });
});
