// M0 契约校验器：零依赖，用本目录的 schema 校验样例与关键不变式。
// Node.js 18+。运行：node contracts/validate.mjs
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const read = (p) => JSON.parse(readFileSync(join(here, p), 'utf8'));
const HEX = 'a'.repeat(64);
const FP = `sha256:${HEX}`;
const SIG = `${'A'.repeat(86)}==`;
const TIME = '2026-11-10T00:00:00Z';

function typeOk(v, t) {
  if (Array.isArray(t)) return t.some((x) => typeOk(v, x));
  switch (t) {
    case 'object': return v !== null && typeof v === 'object' && !Array.isArray(v);
    case 'array': return Array.isArray(v);
    case 'string': return typeof v === 'string';
    case 'integer': return Number.isInteger(v);
    case 'number': return typeof v === 'number' && Number.isFinite(v);
    case 'boolean': return typeof v === 'boolean';
    case 'null': return v === null;
    default: return true;
  }
}

// 支持本批 schema 用到的关键字子集。完整校验仍应由 CI 用 ajv 复核。
function check(v, s, path, root, out) {
  if (!s || typeof s !== 'object') return true;
  if (s.$ref) {
    const name = s.$ref.startsWith('#/$defs/') ? s.$ref.slice(8) : null;
    const def = name ? root.$defs?.[name] : null;
    if (!def) { out.push(`${path}: $ref ${s.$ref} 无法解析`); return false; }
    return check(v, def, path, root, out);
  }
  let ok = true;
  const mark = (msg) => { out.push(`${path}: ${msg}`); ok = false; };

  if (s.type && !typeOk(v, s.type)) {
    mark(`类型应为 ${JSON.stringify(s.type)}`);
    return false;
  }
  if ('const' in s && JSON.stringify(v) !== JSON.stringify(s.const)) {
    mark(`值应为 ${JSON.stringify(s.const)}`);
  }
  if (s.enum && !s.enum.some((x) => JSON.stringify(x) === JSON.stringify(v))) {
    mark(`值不在枚举 ${JSON.stringify(s.enum)} 内`);
  }
  if (typeof v === 'string') {
    if (s.pattern) {
      try { if (!new RegExp(s.pattern).test(v)) mark(`不匹配 ${s.pattern}`); }
      catch (e) { mark(`pattern 非法：${e.message}`); }
    }
    if (s.minLength !== undefined && v.length < s.minLength) mark(`长度小于 ${s.minLength}`);
    if (s.maxLength !== undefined && v.length > s.maxLength) mark(`长度大于 ${s.maxLength}`);
  }
  if (typeof v === 'number') {
    if (s.minimum !== undefined && v < s.minimum) mark(`小于 ${s.minimum}`);
    if (s.maximum !== undefined && v > s.maximum) mark(`大于 ${s.maximum}`);
  }
  if (Array.isArray(v)) {
    if (s.minItems !== undefined && v.length < s.minItems) mark(`元素少于 ${s.minItems}`);
    if (s.maxItems !== undefined && v.length > s.maxItems) mark(`元素多于 ${s.maxItems}`);
    if (s.uniqueItems && new Set(v.map((x) => JSON.stringify(x))).size !== v.length) mark('元素重复');
    if (s.items) v.forEach((x, i) => {
      if (!check(x, s.items, `${path}[${i}]`, root, out)) ok = false;
    });
  }
  if (v !== null && typeof v === 'object' && !Array.isArray(v)) {
    const keys = Object.keys(v);
    if (s.required) for (const k of s.required) if (!keys.includes(k)) mark(`缺少必填字段 ${k}`);
    if (s.propertyNames) for (const k of keys) {
      if (!check(k, s.propertyNames, `${path}.${k}`, root, out)) ok = false;
    }
    for (const [k, sub] of Object.entries(s.properties || {})) {
      if (keys.includes(k) && !check(v[k], sub, `${path}.${k}`, root, out)) ok = false;
    }
    if (s.additionalProperties !== undefined) {
      for (const k of keys.filter((x) => !(x in (s.properties || {})))) {
        if (s.additionalProperties === false) mark(`不允许字段 ${k}`);
        else if (s.additionalProperties && typeof s.additionalProperties === 'object'
          && !check(v[k], s.additionalProperties, `${path}.${k}`, root, out)) ok = false;
      }
    }
  }
  if (s.allOf) for (const sub of s.allOf) {
    if (!check(v, sub, path, root, out)) ok = false;
  }
  if (s.anyOf) {
    const passed = s.anyOf.some((sub) => check(v, sub, path, root, []));
    if (!passed) mark('不满足任何 anyOf 分支');
  }
  if (s.oneOf) {
    const count = s.oneOf.filter((sub) => check(v, sub, path, root, [])).length;
    if (count !== 1) mark(`oneOf 命中 ${count} 个分支，应为 1 个`);
  }
  if (s.not && check(v, s.not, path, root, [])) mark('命中被禁止的结构');
  if (s.if) {
    const cond = check(v, s.if, path, root, []);
    if (cond && s.then && !check(v, s.then, path, root, out)) ok = false;
    if (!cond && s.else && !check(v, s.else, path, root, out)) ok = false;
  }
  return ok;
}

const report = [];
let failed = 0;
const expect = (name, ok, detail = '') => {
  report.push(`${ok ? '通过' : '失败'}  ${name}${detail ? `：${detail}` : ''}`);
  if (!ok) failed += 1;
};
const validates = (name, data, schema) => {
  const out = [];
  check(data, schema, '$', schema, out);
  expect(name, out.length === 0, out.slice(0, 3).join('；'));
  return out.length === 0;
};
const rejects = (name, data, schema) => {
  const out = [];
  check(data, schema, '$', schema, out);
  expect(name, out.length > 0, out.length ? out[0] : '校验器误放行');
};

const roles = read('roles-v1.json');
const moduleSchema = read('module-v2.schema.json');
const catalogSchema = read('catalog-v2.schema.json');
const sourceSchema = read('source-v1.schema.json');
const sourcesSchema = read('sources-v1.schema.json');
const trustSchema = read('trust-v1.schema.json');
const retentionSchema = read('retention-v1.schema.json');
const installSchema = read('install-v1.schema.json');
const depsSchema = read('deps-v1.schema.json');

report.push('== 角色枚举 ==');
const roleIds = roles.roles.map((r) => r.id);
expect('五个 role 且顺序固定', JSON.stringify(roleIds) === JSON.stringify(['input', 'llm', 'output', 'shell', 'auxiliary']), roleIds.join(','));
expect('封闭集且未知 role 硬拒', roles.closed === true && roles.unknown_role_policy === 'reject');
expect('备用端口段从 18101 起', roles.standby_port_base === 18101);
expect('schema 的 role 枚举与 roles-v1.json 一致', JSON.stringify(moduleSchema.properties.role.enum) === JSON.stringify(roleIds));
expect('编译期内嵌，不由 catalog 下发', roles.distribution === 'compiled_into_launcher');

const byId = Object.fromEntries(roles.roles.map((r) => [r.id, r]));
const probes = { input: '/v1/contract', llm: '/v1/contract', output: '/api/v1/contract' };
for (const [id, path] of Object.entries(probes)) {
  expect(`${id} 契约探测路径与上游实测一致`, byId[id].required_contract?.probe.path === path, byId[id].required_contract?.probe.path);
}
expect('llm 与 output 情绪标签一致且为 12 类',
  JSON.stringify(byId.llm.required_contract.expect.emotion_labels) === JSON.stringify(byId.output.required_contract.expect.emotion_labels)
  && byId.llm.required_contract.expect.emotion_labels.length === 12);
expect('四个单槽 role 各只有一个 active',
  ['input', 'llm', 'output', 'shell'].every((id) => byId[id].max_active === 1) && byId.auxiliary.max_active === null);
expect('shell 允许第三方', byId.shell.third_party_allowed === true);
expect('shell 第三方只能显式激活', byId.shell.activation_policy === 'explicit_only');
expect('shell 声明消费三个数据面端口',
  JSON.stringify(byId.shell.consumes) === JSON.stringify(['input', 'llm', 'output']));
expect('shell 保留手动切回', byId.shell.rollback === 'manual_switch_back');
expect('shell 不做契约探测', byId.shell.required_contract === null);
expect('output 可降级且降级模式已声明', byId.output.degradable === true && byId.output.degrade_modes.length === 2);

report.push('== 五份 v2 manifest ==');
const sampleFiles = [
  'dounai-input.v2.json', 'omninexus.v2.json', 'moodvoice.v2.json',
  'dounai-ui.v2.json', 'dounai-memory-reserved.v2.json'
];
const samples = sampleFiles.map((f) => ({ file: f, data: read(`examples/${f}`) }));
for (const { file, data } of samples) validates(`${file} 通过 module-v2.schema.json`, data, moduleSchema);

for (const { file, data } of samples) {
  const r = byId[data.role];
  expect(`${file} 的规范端口与 roles-v1.json 一致`,
    (r.port === null && data.ports.length === 0) || (data.ports.length === 1 && data.ports[0] === r.port));
  const healthPath = data.entry.health?.url ? new URL(data.entry.health.url).pathname : null;
  expect(`${file} 的 health 路径与 roles-v1.json 一致`, healthPath === (r.health?.path ?? null), String(healthPath));
  expect(`${file} 的 depends_on 与 roles-v1.json 一致`, JSON.stringify(data.depends_on) === JSON.stringify(r.depends_on));
  expect(`${file} 未自述启动器侧字段`,
    !['instance_id', 'source_kind', 'publisher_fp', 'trust', 'official'].some((k) => k in data));
  expect(`${file} 的 dir 实例可用空 files 清单`, Object.keys(data.files).length === 0);
}

report.push('== 负例必须被拒 ==');
const base = samples[1].data;
const clone = (patch) => JSON.parse(JSON.stringify({ ...base, ...patch }));
const signedWith = (algo) => clone({ signature: {
  algo, pubkey: `${'A'.repeat(43)}=`, key_id: 'K1', signed_at: TIME, value: SIG
} });
validates('module-v2 接受 Ed25519 算法标识', signedWith('Ed25519'), moduleSchema);
rejects('module-v2 拒绝名实不符的旧算法标识', signedWith('SHA-256 + Ed25519'), moduleSchema);
rejects('未知 role 硬拒，不降级为 auxiliary', clone({ role: 'planner' }), moduleSchema);
rejects('module_id 缺少 namespace', clone({ module_id: 'omninexus' }), moduleSchema);
rejects('变体塞进 version 串', clone({ version: '0.3.3-agent' }), moduleSchema);
rejects('签名段缺 pubkey', clone({ signature: { algo: 'Ed25519', key_id: 'k', signed_at: TIME, value: SIG } }), moduleSchema);
rejects('files 键含路径穿越', clone({ files: { '../evil.py': `sha256:${HEX}` } }), moduleSchema);
rejects('manifest 自述 instance_id', clone({ instance_id: 'x' }), moduleSchema);
rejects('auxiliary 缺 auxiliary_kind', clone({ role: 'auxiliary' }), moduleSchema);
rejects('非 shell 缺 {venv_python}', clone({ entry: { ...base.entry, command: 'python -m omninexus' } }), moduleSchema);
validates('shell 不带 runtime 仍通过', samples[3].data, moduleSchema);
validates('dir 实例 files 为 {} 仍通过', clone({ files: {} }), moduleSchema);

report.push('== 其余四份 schema 的可用性 ==');
const catalogSig = (patch = {}) => ({
  algo: 'Ed25519', key_id: 'K5', signer_fp: FP, signed_at: TIME, value: SIG, ...patch
});
const minCatalog = (patch = {}) => ({
  schema_version: 2, sequence: 1, generated_at: TIME, expires: TIME,
  sources: [{ id: 'github', kind: 'github', base_url: 'https://example.com', priority: 1 }],
  modules: [{
    module_id: 'dounai/omninexus', variant: 'stable', version: '0.3.3', role: 'llm',
    name: 'OmniNexus', publisher_fp: FP, sha256: FP, size: 1024,
    sources: [{ source_id: 'github', url: 'https://example.com/a.zip' }],
    signature_required: true, expires: TIME
  }],
  revoked: [],
  signature: catalogSig(),
  ...patch
});
validates('catalog-v2.schema.json 接受最小合法 catalog', minCatalog(), catalogSchema);
rejects('catalog 含未注册 role 被拒', minCatalog({
  modules: [{
    module_id: 'dounai/omninexus', variant: 'stable', version: '0.3.3', role: 'planner',
    name: 'OmniNexus', publisher_fp: FP, sha256: FP, size: 1024,
    sources: [{ source_id: 'github', url: 'https://example.com/a.zip' }],
    signature_required: true, expires: TIME
  }]
}), catalogSchema);

const installed = {
  schema_version: 1,
  active: { llm: 'dounai/omninexus@stable@main' },
  instances: {
    'dounai/omninexus@stable@main': {
      instance_id: 'dounai/omninexus@stable@main', module_id: 'dounai/omninexus',
      variant: 'stable', version: '0.3.3', role: 'llm', category: 'core',
      publisher_fp: FP, verdict: 'A', confirmed_at: TIME, installed_at: TIME,
      source_kind: 'zip', source_path: null, git: null, pinned: false, auto_update: true
    }
  },
  catalog_state: { sequence: 1, expires: TIME, last_seen_version: {} }
};
validates('source-v1.schema.json 接受 zip 实例', installed, sourceSchema);
rejects('source-v1 拒绝 auxiliary 出现在 active',
  { ...installed, active: { ...installed.active, auxiliary: 'x' } }, sourceSchema);
const dirInstance = JSON.parse(JSON.stringify(installed));
dirInstance.instances['local@agent'] = {
  instance_id: 'local@agent', module_id: 'tonyjh07/omninexus-agent',
  variant: 'stable', version: '0.2.0', role: 'llm', category: 'optional',
  publisher_fp: null, verdict: 'C', confirmed_at: TIME, installed_at: TIME,
  source_kind: 'dir', source_path: 'E:/work/OmniNexus-agent',
  git: { present: true, head: 'abc123', dirty: false, observed_at: TIME },
  pinned: false, auto_update: false
};
validates('source-v1.schema.json 接受 dir 实例（固定 C 类）', dirInstance, sourceSchema);
dirInstance.instances['local@agent'].auto_update = true;
rejects('source-v1 拒绝 C 类实例自动更新', dirInstance, sourceSchema);

validates('trust-v1.schema.json 接受 K1/K5 两钥', {
  schema_version: 1,
  builtin: [
    { key_id: 'K1', fingerprint: FP, purpose: 'module_signing', enabled: true, deletable: false },
    { key_id: 'K5', fingerprint: FP, purpose: 'catalog_signing', enabled: true, deletable: false }
  ],
  trusted: []
}, trustSchema);
rejects('trust-v1 拒绝信任源字段', {
  schema_version: 1,
  builtin: [
    { key_id: 'K1', fingerprint: FP, purpose: 'module_signing', enabled: true, deletable: false },
    { key_id: 'K5', fingerprint: FP, purpose: 'catalog_signing', enabled: true, deletable: false }
  ],
  trusted: [], source_id: 'github'
}, trustSchema);
validates('retention-v1.schema.json 接受默认三版', {
  schema_version: 1, keep_versions: 3, pinned: [],
  data_dir_policy: 'never_auto_delete', source_dir_policy: 'never_delete'
}, retentionSchema);
rejects('retention-v1 拒绝自动删 data_dir', {
  schema_version: 1, keep_versions: 3, pinned: [],
  data_dir_policy: 'delete', source_dir_policy: 'never_delete'
}, retentionSchema);

report.push('== M1 状态 schema ==');
const installState = {
  schema_version: 1,
  install_mode: 'per_machine',
  install_root: 'C:/Program Files/DounaiLauncher',
  data_root: 'C:/Users/me/AppData/Local/DounaiLauncher',
  uv_path: 'C:/Program Files/DounaiLauncher/bin/uv.exe',
  uv_sha256: FP,
  uv_version: '0.13.7',
  written_at: TIME
};
validates('install-v1 接受双根 install.json', installState, installSchema);
validates('install-v1 接受 POSIX 绝对路径', { ...installState, install_root: '/opt/dounai', data_root: '/home/me/.dounai', uv_path: '/opt/dounai/bin/uv' }, installSchema);
rejects('install-v1 拒绝相对 data_root', { ...installState, data_root: 'data' }, installSchema);
rejects('install-v1 拒绝未知 install_mode', { ...installState, install_mode: 'both' }, installSchema);
rejects('install-v1 拒绝不带前缀的哈希', { ...installState, uv_sha256: HEX }, installSchema);
rejects('install-v1 拒绝预发布版本号', { ...installState, uv_version: '0.13.7-beta.1' }, installSchema);
rejects('install-v1 拒绝非法 written_at', { ...installState, written_at: '2026-11-10 10:00:00' }, installSchema);
const { uv_sha256: _omitted, ...installWithoutHash } = installState;
rejects('install-v1 拒绝缺 uv_sha256', installWithoutHash, installSchema);

const depsState = {
  schema_version: 1,
  instance_id: 'dounai/omninexus@stable@main',
  kind: 'pyproject',
  source_path: 'pyproject.toml',
  extras: [],
  snapshot_sha256: FP,
  python_build_id: 'cpython-3.12.8-windows-x86_64-none',
  generated_at: TIME,
  offline_capable: true
};
for (const kind of ['uv_lock', 'requirements', 'pyproject']) {
  validates(`deps-v1 接受 ${kind}`, { ...depsState, kind }, depsSchema);
}
expect('deps-v1 的 kind 枚举与 module-v2 一致',
  JSON.stringify(depsSchema.properties.kind.enum)
  === JSON.stringify(moduleSchema.properties.runtime.properties.deps.properties.kind.enum));
rejects('deps-v1 拒绝未知 kind', { ...depsState, kind: 'poetry_lock' }, depsSchema);
rejects('deps-v1 拒绝绝对 source_path', { ...depsState, source_path: '/etc/requirements.txt' }, depsSchema);
rejects('deps-v1 拒绝穿越 source_path', { ...depsState, source_path: '../requirements.txt' }, depsSchema);
rejects('deps-v1 拒绝不带前缀的快照哈希', { ...depsState, snapshot_sha256: HEX }, depsSchema);
const { offline_capable: _omittedFlag, ...depsWithoutFlag } = depsState;
rejects('deps-v1 拒绝缺 offline_capable', depsWithoutFlag, depsSchema);
rejects('deps-v1 拒绝非法 generated_at', { ...depsState, generated_at: 'yesterday' }, depsSchema);

report.push('== M2 契约收紧 ==');
const envPatch = (env) => clone({ entry: { ...base.entry, env } });
validates('module-v2 接受大写环境变量名', envPatch({ OMNI_PORT: '{port}' }), moduleSchema);
rejects('module-v2 拒绝小写环境变量名', envPatch({ omni_port: '{port}' }), moduleSchema);
rejects('module-v2 拒绝键名含连字符', envPatch({ 'OMNI-PORT': '1' }), moduleSchema);
rejects('module-v2 拒绝 UV_ 前缀键', envPatch({ UV_CACHE_DIR: 'C:/evil' }), moduleSchema);
rejects('module-v2 拒绝覆盖 PATH', envPatch({ PATH: 'C:/evil' }), moduleSchema);
rejects('module-v2 拒绝覆盖 PYTHONPATH', envPatch({ PYTHONPATH: 'C:/evil' }), moduleSchema);

const depsPatch = (path) => clone({
  runtime: { ...base.runtime, deps: { ...base.runtime.deps, path } }
});
validates('module-v2 接受包内相对依赖路径', depsPatch('requirements/base.txt'), moduleSchema);
rejects('module-v2 拒绝绝对依赖路径', depsPatch('/etc/requirements.txt'), moduleSchema);
rejects('module-v2 拒绝 .. 段', depsPatch('../requirements.txt'), moduleSchema);
rejects('module-v2 拒绝嵌套 .. 段', depsPatch('locks/../evil.txt'), moduleSchema);
rejects('module-v2 拒绝反斜杠依赖路径', depsPatch('locks\\evil.txt'), moduleSchema);

validates('deps-v1 接受带命名空间的 instance_id', {
  ...depsState, instance_id: 'dounai/omninexus@stable@main'
}, depsSchema);
rejects('deps-v1 拒绝空段 instance_id', { ...depsState, instance_id: 'dounai//omninexus' }, depsSchema);
rejects('deps-v1 拒绝 . 段 instance_id', { ...depsState, instance_id: 'dounai/./omninexus' }, depsSchema);
rejects('deps-v1 拒绝 .. 段 instance_id', { ...depsState, instance_id: 'dounai/../omninexus' }, depsSchema);
rejects('deps-v1 拒绝超长 instance_id', { ...depsState, instance_id: 'a'.repeat(129) }, depsSchema);
rejects('deps-v1 拒绝含空格 instance_id', { ...depsState, instance_id: 'a b' }, depsSchema);

validates('install-v1 接受 UNC 程序根', {
  ...installState, install_root: '\\\\server\\share\\dounai'
}, installSchema);
rejects('install-v1 拒绝尾部空格路径', { ...installState, data_root: 'C:/Dounai/data ' }, installSchema);
rejects('install-v1 拒绝尾部点路径', { ...installState, data_root: 'C:/Dounai/data.' }, installSchema);
rejects('install-v1 拒绝控制字符路径', { ...installState, data_root: 'C:/Dounai/\u0001data' }, installSchema);

for (const [name, file, schema] of [
  ['install 状态样例', 'examples/state/install.json', installSchema],
  ['deps 状态样例', 'examples/state/deps.json', depsSchema]
]) {
  validates(`${name} 通过 ${schema.$id.split('/').pop()}`, read(file), schema);
}

report.push('== M8 多源契约 ==');
const sourcesExample = read('examples/state/sources.json');
const communitySource = {
  id: 'community-demo',
  name: 'Demo Community',
  kind: 'community',
  base_url: 'https://example.org/catalog',
  catalog_pubkey: `${'A'.repeat(43)}=`,
  catalog_fingerprint: FP,
  host_suffixes: ['example.org'],
  enabled: true,
  deletable: true,
  auto_update: false,
  group: 'community-demo',
  sequence: null,
  expires: null,
  priority: 3,
  added_at: TIME,
  revoked_at: null
};
const withSources = (list) => ({ schema_version: 1, sources: list });
const patchSource = (patch, index = 0) => {
  const list = sourcesExample.sources.map((s) => ({ ...s }));
  Object.assign(list[index], patch);
  return withSources(list);
};
const patchCommunity = (patch) => withSources([
  ...sourcesExample.sources.map((s) => ({ ...s })),
  { ...communitySource, ...patch }
]);

validates('catalog-v2 接受社区源自己的 key_id 与 signer_fp',
  minCatalog({ signature: catalogSig({ key_id: 'community-k1' }) }), catalogSchema);
rejects('catalog-v2 拒绝缺 signer_fp',
  minCatalog({ signature: { algo: 'Ed25519', key_id: 'K5', signed_at: TIME, value: SIG } }), catalogSchema);
rejects('catalog-v2 拒绝非法 signer_fp',
  minCatalog({ signature: catalogSig({ signer_fp: 'sha256:xyz' }) }), catalogSchema);
rejects('catalog-v2 拒绝 catalog 自述 pubkey',
  minCatalog({ signature: catalogSig({ pubkey: `${'A'.repeat(43)}=` }) }), catalogSchema);
expect('catalog-v2 的 key_id 已不再冻结为 K5',
  catalogSchema.properties.signature.properties.key_id.const === undefined
  && catalogSchema.properties.signature.required.includes('signer_fp'));

const installedWithSource = JSON.parse(JSON.stringify(installed));
installedWithSource.instances['dounai/omninexus@stable@main'].source_id = 'official-sf';
validates('source-v1 接受带 source_id 的实例', installedWithSource, sourceSchema);
validates('source-v1 接受不再写 sequence / expires 的 catalog_state', {
  ...installed, catalog_state: { last_seen_version: {} }
}, sourceSchema);

validates('sources-v1 接受内置源注册表样例', sourcesExample, sourcesSchema);
validates('sources-v1 接受社区源条目', patchCommunity({}), sourcesSchema);
rejects('sources-v1 拒绝源条目缺 catalog_fingerprint', (() => {
  const doc = patchCommunity({});
  delete doc.sources[1].catalog_fingerprint;
  return doc;
})(), sourcesSchema);
rejects('sources-v1 拒绝内置源 deletable: true', patchSource({ deletable: true }), sourcesSchema);
rejects('sources-v1 拒绝社区源 auto_update: true', patchCommunity({ auto_update: true }), sourcesSchema);
rejects('sources-v1 拒绝源条目自述 pubkey', patchCommunity({ pubkey: `${'A'.repeat(43)}=` }), sourcesSchema);
rejects('sources-v1 拒绝空 host_suffixes', patchCommunity({ host_suffixes: [] }), sourcesSchema);
rejects('sources-v1 拒绝公共后缀 com', patchCommunity({ host_suffixes: ['com'] }), sourcesSchema);
rejects('sources-v1 拒绝非 https base_url', patchCommunity({ base_url: 'http://example.org/catalog' }), sourcesSchema);
rejects('sources-v1 拒绝撤销公钥后仍启用的源',
  patchCommunity({ revoked_at: TIME }), sourcesSchema);
expect('源 id 唯一，样例只含已下发的官方主源（GitHub 镜像暂缓）',
  new Set(sourcesExample.sources.map((s) => s.id)).size === sourcesExample.sources.length
  && sourcesExample.sources.every((s) => s.kind !== 'official' || s.group === 'official')
  && !sourcesExample.sources.some((s) => s.id === 'official-gh'));
expect('allowlist 只由已启用源的 host 后缀派生', (() => {
  const enabled = sourcesExample.sources.filter((s) => s.enabled);
  const list = [...new Set(enabled.flatMap((s) => s.host_suffixes))].sort();
  return list.length === 2 && !list.includes('com') && list.includes('sourceforge.net');
})());
validates('sources-v1 接受逐源 last_error 记录', patchSource({
  last_error: '旧格式缺 signer_fp',
  last_error_at: TIME
}), sourcesSchema);
rejects('sources-v1 拒绝 last_error 类型错误', patchSource({ last_error: 7 }), sourcesSchema);
expect('样例源表公钥与 Rust 内嵌 K5 一致', (() => {
  const rust = readFileSync(join(here, '../src-tauri/src/state_v2.rs'), 'utf8');
  const pubkey = /BUILTIN_K5_PUBKEY_B64:\s*&str\s*=\s*"([^"]+)"/.exec(rust)?.[1];
  const fp = /BUILTIN_K5_FINGERPRINT:\s*&str\s*=\s*"([^"]+)"/.exec(rust)?.[1];
  return sourcesExample.sources.every((s) => s.catalog_pubkey === pubkey && s.catalog_fingerprint === fp);
})());

// 断言数量固定。误删或跳过断言时，不能只靠 failed 判绿。
const EXPECTED_ASSERTIONS = 131;
const count = report.filter((x) => x.startsWith('通过') || x.startsWith('失败')).length;
console.log(report.join('\n'));
console.log('');
console.log(`合计 ${count} 项，失败 ${failed} 项。`);
if (count !== EXPECTED_ASSERTIONS) {
  console.error(`断言数量应为 ${EXPECTED_ASSERTIONS} 项，实际 ${count} 项。增删断言后同步更新 EXPECTED_ASSERTIONS。`);
  process.exit(1);
}
process.exit(failed ? 1 : 0);
