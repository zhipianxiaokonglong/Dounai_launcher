#!/usr/bin/env node
// 跨仓一致性检查：roles-v1.json 声明的端口、health 与契约探测路径，
// 必须与组装层和三个上游的源码一致。
//
// 用法：
//   node contracts/check-upstreams.mjs [--root <五仓共同上级目录>]
//   node contracts/check-upstreams.mjs --root ../upstreams   # CI
//
// 默认 root 是 contracts/ 的上两级，即本地工作区根目录。
// 缺少源码文件时直接失败，不把「检查不适用」当成通过。

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));

function flag(name) {
  const i = process.argv.indexOf(name);
  return i > -1 && process.argv[i + 1] ? process.argv[i + 1] : null;
}

const root = path.resolve(flag('--root') || path.join(here, '..', '..'));
const rolesPath = path.resolve(flag('--roles') || path.join(here, 'roles-v1.json'));
if (!fs.existsSync(rolesPath)) {
  console.error(`缺少角色契约：${rolesPath}`);
  process.exit(1);
}
const doc = JSON.parse(fs.readFileSync(rolesPath, 'utf8'));
const byId = Object.fromEntries(doc.roles.map((r) => [r.id, r]));

const report = [];
let failed = 0;

function check(label, ok, detail) {
  report.push(`${ok ? '通过' : '失败'}  ${label}${detail ? `：${detail}` : ''}`);
  if (!ok) failed += 1;
}

function read(rel) {
  const p = path.join(root, rel);
  if (!fs.existsSync(p)) {
    console.error(`缺少文件：${p}`);
    console.error('源码缺失时检查不能跳过，请确认五个仓都已检出。');
    process.exit(1);
  }
  return fs.readFileSync(p, 'utf8');
}

// 从 FastAPI 源码拼出完整的 GET 路径。APIRouter 带 prefix，FastAPI 实例不带。
function getPaths(src) {
  const prefixes = new Map([['app', '']]);
  for (const m of src.matchAll(/(\w+)\s*=\s*APIRouter\([^)]*?prefix\s*=\s*"([^"]+)"/g)) {
    prefixes.set(m[1], m[2]);
  }
  for (const m of src.matchAll(/(\w+)\s*=\s*FastAPI\(/g)) {
    prefixes.set(m[1], '');
  }
  const out = [];
  for (const m of src.matchAll(/@(\w+)\.get\(\s*"([^"]+)"/g)) {
    const prefix = prefixes.get(m[1]);
    if (prefix !== undefined) out.push(prefix + m[2]);
  }
  return out;
}

const upstreams = {
  input: 'Dounai_input/src/dounai_input/api.py',
  llm: 'OmniNexus/src/omninexus/api/routes.py',
  output: 'MoodVoice/src/emotional_tts/api.py',
};

for (const [id, file] of Object.entries(upstreams)) {
  const role = byId[id];
  if (!role || !role.health || !role.required_contract) {
    check(`${id} 在 roles-v1.json 中有数据面声明`, false, '缺少 health 或 required_contract');
    continue;
  }
  const paths = getPaths(read(file));
  check(`${id} 的 health 路径与源码一致`, paths.includes(role.health.path), role.health.path);
  check(
    `${id} 的契约探测路径与源码一致`,
    paths.includes(role.required_contract.probe.path),
    role.required_contract.probe.path,
  );
  check(
    `${id} 的探测方法与源码一致`,
    role.required_contract.probe.method === 'GET',
    role.required_contract.probe.method,
  );
}

const dataPlane = doc.roles.filter((r) => r.kind === 'data_plane').map((r) => r.id).sort();
const adapters = read('Dounai/src/upstream/adapters.ts');
const block = adapters.match(/DEFAULT_ENDPOINTS[^=]*=\s*\{([\s\S]*?)\}/);
check('组装层声明了 DEFAULT_ENDPOINTS', Boolean(block));
if (block) {
  const entries = [...block[1].matchAll(/(\w+)\s*:\s*"https?:\/\/[^"\s:]+:(\d+)"/g)]
    .map((m) => [m[1], Number(m[2])]);
  check(
    '组装层端点键与数据面角色一致',
    JSON.stringify(entries.map((e) => e[0]).sort()) === JSON.stringify(dataPlane),
    entries.map((e) => e[0]).join(', ') || '没有解析到端点',
  );
  for (const [role, port] of entries) {
    check(`${role} 的端口与 roles-v1.json 一致`, byId[role]?.port === port, String(port));
  }
}

// 正则一个都没匹配到时差异为空，会假通过。断言检查项数量堵住这个口子。
const expectedChecks = Object.keys(upstreams).length * 3 + dataPlane.length + 2;
check('检查项齐全', report.length === expectedChecks);

console.log(report.join('\n'));
console.log('');
console.log(`合计 ${report.length} 项，失败 ${failed} 项。`);
process.exit(failed > 0 ? 1 : 0);
