# `admin_ui` 声明

模块在 manifest 里声明自己的配置界面。启动器不自绘表单、不代理厂商配置、不存储密钥。

## 字段

`admin_ui` 是 manifest v2 的可选字段。结构以 [module-v2.schema.json](../contracts/module-v2.schema.json) 为准。

| 字段 | 说明 |
|---|---|
| `kind` | 模块类别标签 |
| `title` | 按钮标题 |
| `capabilities` | 界面声明的能力，仅作展示 |
| `web_ui.path` | 相对模块服务根的路径，必须以 `/` 开头 |
| `web_ui.label` | 按钮文案 |
| `web_ui.requires_running` | 为 true 时，模块未运行则按钮禁用 |
| `web_ui.embed` | 默认 true：用同窗口子 WebView 打开；声明 false 时开独立窗口 |
| `web_ui.min_size` | 窗口最小宽高 |
| `web_ui.placeholder` | 未就绪时的占位文案 |

绝对 URL 会被拒绝。可照抄的样例见 [omninexus.v2.json](../contracts/examples/omninexus.v2.json)。

## 渲染规则

1. 只渲染已安装模块的入口。模块未安装时按钮不出现。
2. `web_ui.requires_running` 为 true 且模块未运行时，按钮禁用。
3. `capabilities` 未列出的操作不渲染。
4. `admin_ui` 结构不合法时，只隐藏该模块的入口。模块本身照常显示。

## 用顶层窗口而不是 iframe

三个上游都设置了 `X-Frame-Options: DENY` 与 CSP `frame-ancestors 'none'`。这是防点击劫持的正确做法，但会让 iframe 内嵌被浏览器拒绝。

启动器用 Tauri 窗口加载该 URL。子 WebView 与独立窗口都是顶层窗口，不受 iframe 限制，模块不必放宽安全头。

实现补记（M4）：默认内嵌。启动器在主窗口里用 `Window::add_child` 挂一个原生子 WebView，它仍是顶层 WebView，不参与 DOM 布局，也不受 iframe 限制。坐标由界面计算、窗口缩放时重下发；切到别的视图时隐藏而不是销毁，所以表单里填了一半的内容不会丢。manifest 声明 `web_ui.embed = false` 的模块改开独立窗口，子 WebView 创建失败时同样自动退化，两种情形都不改变 URL、不重启模块服务。

## 密钥归属

密钥由各模块自己保管。启动器与悬浮窗都不保存密钥副本。

1. 模块自己提供 Web 管理界面。
2. 模块自己负责配置与密钥的存储。
3. 启动器只打开界面，不读写密钥值。

`admin_ui` 写在 manifest 内部，随 manifest 一起被签名覆盖。不允许在包外追加该字段，否则等于绕过签名。

警告：第三方 `shell` 模块的界面内容启动器无法验证。激活前，确认文案须披露这一点。不披露时，仿冒的配置页可能诱导用户输入密钥。
