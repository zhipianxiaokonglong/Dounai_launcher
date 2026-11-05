# OmniNexus 缺陷报告与改进建议

> 提交对象：OmniNexus 维护者
> 报告人：Dounai 总线与 UI 层（启动器 + 悬浮窗）
> 日期：2026-11-05
> 影响版本：`583200f`（本地实测），缺陷引入于 `7ba3f6d`

---

## 一、结论摘要

**用户在设置页配置的「默认提供方 / 默认模型」不会作用于对话，只会被会话初始化时的配置默认值永久遮蔽。**

具体表现：

| 场景 | 结果 |
|---|---|
| 在设置页把默认设为自定义厂商 `deepseek-flash` | `GET /v1/provider-defaults` **正确返回 deepseek** |
| 随后发起对话（不显式指定模型） | **实际使用 `openai:gpt-4o-mini`**，返回 `503 NO_PROVIDER_AVAILABLE` |
| 同一个请求**显式**指定 `deepseek` | **HTTP 200，1.3 秒正常回复** |

也就是说：**模型与密钥完全可用，配置也确实存进去了，但对话路径不读它。**

这不是 UI 层的问题。任何客户端（包括 OmniNexus 自带的 Web 界面）都会撞到。

---

## 二、复现步骤

### 2.1 前置条件

1. `config/default.yaml` 中 `gateway.default_model` 保持出厂值 `openai:gpt-4o-mini`
2. 该内置厂商**未配置**密钥（`OPENAI_API_KEY` 不存在）→ `available: false`
3. 通过 `POST /v1/custom-providers` 添加一个可用的自定义厂商（如 deepseek，带有效 key）
4. 通过 `PUT /v1/provider-defaults` 把默认设为该自定义厂商

### 2.2 复现

```bash
# ① 确认默认设置确实指向 custom/deepseek
curl http://127.0.0.1:8102/v1/provider-defaults
# → {"defaults":{"providerKind":"custom","providerId":"5d9bf380-...","providerName":"deepseek","model":"deepseek-flash"}}

# ② 不指定模型发一条对话
curl -X POST http://127.0.0.1:8102/v1/events \
  -H 'Content-Type: application/json' -H 'Accept: text/event-stream' \
  -d '{"event_id":"<uuid>","parts":[{"modality":"text","content":"你好"}],"metadata":{}}'
# → 503 {"code":"NO_PROVIDER_AVAILABLE",
#         "details":{"requested_model":"openai:gpt-4o-mini", ...}}
#                              ^^^^^^^^^^^^^^^^^^^^^^^^ 用的是配置默认，不是用户设的默认

# ③ 显式指定同一个厂商
curl -X POST http://127.0.0.1:8102/v1/events \
  -H 'Content-Type: application/json' -H 'Accept: text/event-stream' \
  -d '{"event_id":"<uuid>","parts":[{"modality":"text","content":"你好"}],
       "metadata":{"providerKind":"custom","providerId":"5d9bf380-...","model":"deepseek-flash"}}'
# → 200，正常流式输出
```

### 2.3 数据库旁证

```sql
SELECT model, provider_kind, model_name FROM sessions ORDER BY rowid DESC LIMIT 3;
```

实测输出：

| model | provider_kind | model_name | 说明 |
|---|---|---|---|
| `custom:5d9bf380-...:deepseek-flash` | custom | deepseek-flash | ③ 显式指定时正确 |
| `openai:gpt-4o-mini` | builtin | gpt-4o-mini | ② 不带模型时**错误落库** |
| `openai:gpt-4o-mini` | builtin | gpt-4o-mini | 同上 |

**每个新建会话都被写入了 `gateway.default_model`，与用户设置无关。**

---

## 三、根因定位

缺陷由**两处协同**造成，缺一不可。

### 3.1 写入侧：`sessions.ensure()` 用配置默认值兜底

`src/omninexus/sessions/service.py` L62–L72：

```python
record = SessionRecord(
    id=new_id,
    title=_clean_title(title_hint),
    model=model or self.config.gateway.default_model,   # ← L65 缺陷点
    provider_kind=provider_kind,
    provider_id=provider_id,
    model_name=model_name,
    prompt_profile=prompt_profile or self.config.prompts.default_profile,
    created_at=now,
    updated_at=now,
)
```

`orchestrator.py` L166–L171 调用时**只传 `model=None`，不传 `provider_kind/provider_id/model_name`**：

```python
session, created = await self.sessions.ensure(
    normalized.session_id,
    title_hint=text[:60] if text else "Image conversation",
    model=None,                 # ← 触发上面的 or 分支
    prompt_profile=None,
)
```

于是：
- `model` 落库为 `config.gateway.default_model`（`openai:gpt-4o-mini`）
- `provider_kind` / `provider_id` / `model_name` 全部为 `None`

### 3.2 读取侧：`_select_model()` 让 session 值优先于 defaults

`src/omninexus/orchestrator.py` L323 起，优先级为：

```
1. metadata 显式 providerKind/providerId/modelName   （explicit=True）
2. metadata.model（单字符串 ref）
3. session.provider_kind + provider_id + model_name  ← 命中这里
4. session.model
5. defaults（service.defaults() 的结果）              ← 永远走不到
```

第 3 条分支命中条件：

```python
if session.provider_kind and session.provider_id and session.model_name:
    return session.provider_kind, session.provider_id, session.model_name, False
```

由 3.1 可知这三个字段都是 `None`，**看起来不会命中**——但请注意：

**会话的 `model` 字段已被填入 `"openai:gpt-4o-mini"`，因此必然命中第 4 条分支：**

```python
if session.model:                                    # ← "openai:gpt-4o-mini" 非空
    kind, provider_id, model_name = self.providers.resolve_ref(session.model)
    return kind, provider_id, model_name, False      # → builtin / openai / gpt-4o-mini
```

`resolve_ref("openai:gpt-4o-mini")` 解析出 `builtin/openai/gpt-4o-mini`，直接返回。
**第 5 条的 `defaults` 被彻底绕过。**

### 3.3 为什么接口显示正确却不起作用

这是最容易误导排查的一点——**同一个 service，两条不同路径**：

| 路径 | 调用 | 结果 |
|---|---|---|
| `GET /v1/provider-defaults` | `service.defaults()` | 读存储 → `validate_target` 通过 → **返回 deepseek** ✓ |
| `POST /v1/events`（不带模型） | `_select_model(metadata, session, defaults)` | 命中 session 分支 → **用 openai** ✗ |

`defaults()` 本身是对的（`providers/service.py` L94–L99 确实先读存储再校验），
但它**算出来之后没有被使用**。

### 3.4 为什么 `validate_target` 通过却没生效

顺带说明一个易被误判的点：`validate_target("custom", "<id>", "deepseek-flash")` 返回 `True`
（该厂商 `enabled=true` 且模型存在），所以 `defaults()` 走的是"返回存储值"分支，不走回落分支。
**defaults() 完全正常，问题在于它的返回值被 `_select_model` 跳过了。**

### 3.5 同一缺陷的第二处表现：`session_availability`

`providers/service.py` L176 起：

```python
async def session_availability(self, session: SessionRecord) -> tuple[bool, str | None]:
    kind = session.provider_kind
    provider_id = session.provider_id
    model_name = session.model_name
    if not kind or not provider_id or not model_name:
        if not session.model:
            return False, "no_model"
        kind, provider_id, model_name = self.resolve_ref(session.model)   # ← 同样读 session.model
    ...
    if not provider.is_configured():
        return False, "provider_not_configured"                          # ← 返回这个原因
    return True, None
```

它同样以 `session.model` 为准。因此在**已存在的会话**上（`created=False`）继续对话时，
`orchestrator.py` L185–L193 会走到：

```python
if not created and not explicit:
    available, reason = await self.providers.session_availability(session)
    if not available:
        raise OmniError(ErrorCode.PROVIDER_UNAVAILABLE, ..., status_code=409,
                        details={"reason": reason})
```

**实测后果**：老会话会返回 `409 PROVIDER_UNAVAILABLE`，`reason=provider_not_configured`
——同样把用户引向"去配 OpenAI 密钥"这个错误方向。

这条与 3.1–3.3 是**同一根因的两处表现**：5.1 的修复（让 session 记录用户设定的默认）
可同时消除两者。

---

## 四、影响面

| 影响 | 说明 |
|---|---|
| **设置页"默认提供方/模型"形同虚设** | 只要会话是新建的，就永远用 `gateway.default_model` |
| **首次使用体验断裂** | 用户配好厂商、设好默认，一发消息就 503，提示"去配密钥"——而密钥其实已配好 |
| **`missing_api_key` 误导** | 错误信息指向 `openai:gpt-4o-mini`，让用户以为是自己没配 OpenAI |
| **所有客户端受影响** | Web 设置页、Dounai、任何第三方调用方 |
| **切换默认模型无效** | 用户改默认后，新建会话仍用旧配置默认 |

**不受影响**：显式传 `metadata.model` 或 `providerKind/providerId/modelName` 的调用方（绕开了缺陷）。

---

## 五、修复方案

### 5.1 推荐方案：写入侧传 defaults（治本，改动最小）

在 `orchestrator.py` L166 附近，先把 `defaults` 算好并传入 `ensure()`：

```python
defaults = await self.providers.defaults()
session, created = await self.sessions.ensure(
    normalized.session_id,
    title_hint=text[:60] if text else "Image conversation",
    model=None,
    prompt_profile=None,
    provider_kind=defaults.provider_kind,
    provider_id=defaults.provider_id,
    model_name=defaults.model,
)
```

注意：`defaults` 的获取需**提前到 `ensure()` 之前**（当前是在 L182，晚于 L166）。

**优点**：

- 一处改动同时修好"写入"和"读取"——session 从此记录的是**用户设定的默认**
- `_select_model` 的第 3 条件分支自然命中，返回正确的 custom/deepseek
- 不改 `_select_model` 的优先级语义（显式 > session > defaults 仍然成立）
- 后续会话沿用同一模型，行为一致

**注意事项**：

1. `sessions.ensure()` 已支持这三个关键字参数（L45 签名里有），无需改签名
2. `defaults()` 是异步且可能较慢（要校验厂商），但本来每轮都在 L182 调一次，提前调用不增加总次数
3. `model` 字段仍会落 `gateway.default_model`；建议**同时**让它与 `model_name` 一致，
   否则 `session.model` 与 `session.model_name` 会出现不一致（见 5.3）

### 5.2 备选方案：改 `_select_model` 优先级

把 `defaults` 提到 session 之前：

```python
# 若 session 只是"初始化默认值"而非用户显式选择，应让 defaults 优先
```

**不推荐**：session 的意义本就是"记住本次会话用的模型"，把 defaults 提前会破坏
"用户在会话中途切换模型后应保持"的语义（L223 附近正是靠 session 记住切换）。

### 5.3 附带建议：消除 `model` 与 `model_name` 的不一致

当前 `SessionRecord` 同时有 `model`（canonical ref 字符串）与
`provider_kind/provider_id/model_name`（拆分字段）。实测发现两者可能指向不同厂商：

```python
# L65 落的是 gateway.default_model
model = model or self.config.gateway.default_model
# 而 provider_kind/provider_id/model_name 是 None
```

建议在**写入时统一**：

```python
if model_name and provider_id:
    record.model = f"{provider_kind}:{provider_id}:{model_name}" \
                   if provider_kind == "custom" else f"{provider_id}:{model_name}"
```

这样两个数据源永远一致，`_select_model` 的两条 session 分支也不会给出不同答案。
长远看**建议只保留一组**（拆分字段），`model` 作为派生只读值，避免双写。

---

## 六、建议补充的回归测试

现状：`tests/` 里没有覆盖"新会话是否采用用户设置的默认模型"。

建议新增（最小充分集）：

```python
async def test_new_session_uses_configured_defaults_not_gateway_default(client, custom_provider):
    """新建会话必须采用 provider-defaults，而不是 gateway.default_model。"""
    # 1) 配置 gateway.default_model = "openai:gpt-4o-mini"（且该厂商无键）
    # 2) 添加可用的 custom 厂商，并 PUT /v1/provider-defaults 指向它
    # 3) POST /v1/events 时不带任何模型字段
    # 4) 断言：
    #    - 响应不是 NO_PROVIDER_AVAILABLE
    #    - sessions 表新行的 provider_kind == "custom"
    #    - sessions 表新行的 model_name == 默认模型
    #    - （反证）session.model 不得为 gateway.default_model
```

再补一条防"接口与对话不一致"的测试：

```python
async def test_provider_defaults_matches_actual_turn_model(client):
    """GET /v1/provider-defaults 的结果必须与实际对话使用的模型一致。"""
    stated = (await client.get("/v1/provider-defaults")).json()["defaults"]
    actual = ...  # 从会话记录或 SSE meta 帧取
    assert (actual["providerKind"], actual["providerId"], actual["model"]) == \
           (stated["providerKind"], stated["providerId"], stated["model"])
```

第二条尤其重要：它能一次性抓出本次这类"两条路径给出不同答案"的缺陷。

---

## 七、给维护者的补充说明：排查时容易被误导的三点

1. **`GET /v1/provider-defaults` 返回正确 ≠ 对话会用它对**
   必须比对"实际对话所用模型"（session 记录或 SSE `meta` 帧）与接口值是否一致。

2. **503 的 `requested_model` 是线索，不是结论**
   它显示 `openai:gpt-4o-mini` 时，不要立刻断定"用户没配 OpenAI"——
   本次案例里用户配的是 custom/deepseek，配置完全正常。

3. **`validate_target` 通过不代表生效**
   `defaults()` 校验通过并返回了正确值，但该返回值在后续 `_select_model` 里被跳过。
   排查时应跟到"最终 used model"，而不是止于 `defaults()`。

---

## 八、附：本次复现的完整证据链

| 环节 | 观测 | 结论 |
|---|---|---|
| 三层健康端点 | `/healthz`、`/v1/healthz`、`/api/health` 均 200 | 服务都活着 |
| `GET /v1/custom-providers` | 1 个 custom 厂商，`hasApiKey=true`，`enabled=true` | 厂商与密钥正常 |
| `GET /v1/models` | `custom:...:deepseek-flash` **available=True**；`openai:gpt-4o-mini` available=False | 可用模型确实存在 |
| `GET /v1/provider-defaults` | custom / deepseek / deepseek-flash | 配置已存 |
| `POST /v1/events` 不带模型 | 503，`requested_model=openai:gpt-4o-mini` | **缺陷触发** |
| `POST /v1/events` 显式指定 | 200，1.3s 正常输出 | 模型与密钥可用 |
| `POST /v1/events` 复用旧会话 | **409**，`reason=provider_not_configured` | 3.5 的第二处表现 |
| `sessions` 表最近 3 行 | 2 行为 `openai:gpt-4o-mini`，1 行为 deepseek | 写入侧证据 |

---

## 九、联系方式与后续

本报告基于本地实测（`583200f`），未修改 OmniNexus 仓库任何文件。

如需我提供可执行的最小复现脚本或补丁 PR，请告知；也可由我方在
Dounai 侧先做临时规避（每次请求显式读取 defaults），但**那只是绕过，
OmniNexus 自带 Web 界面仍会复现此问题**，因此建议在 OmniNexus 内修复。
