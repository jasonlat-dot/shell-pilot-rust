# B1：Rust 模型配置与适配器

本阶段完成模型配置、安全保存 API Key、请求预算与 OpenAI 兼容 Chat Completions 协议适配。它是桌面应用内部的 Rust 库，不启动后端监听端口。这里的 API 地址是模型服务地址。

B4 已接通前端“设置 → 模型设置”和基础文本 AI 面板，使用本适配器的加密配置；使用说明见 [模型设置教程](../../ai-ssh-terminal-client/docs/agent-model-settings.md)。B2 基础文本 IPC/Channel 见 [AGENT_CHAT.md](AGENT_CHAT.md)，B3 持久轮次与恢复见 [AGENT_TURNS.md](AGENT_TURNS.md)。C2 已在文字请求前接入普通模型自动整理；工具执行与聊天附件仍未开放。

## 文件与入口

| 文件 | 作用 |
| --- | --- |
| `src/agent/provider/config.rs` | 配置、能力声明、URL、参数与密钥输入校验 |
| `src/agent/provider/repository.rs` | 按用户保存模型配置、版本检查、原生密钥解密 |
| `src/agent/provider/protocol.rs` | 文本/函数工具请求、预算与响应解析 |
| `src/agent/provider/transport.rs` | 原生 HTTP 调用、取消、超时、响应大小限制 |
| `migrations/005_agent_models.sql` | 模型配置表，数据库 schema 5 |
| `tests/agent_models.rs` | 临时数据库、Windows DPAPI、本地模拟模型测试 |

`ModelRepository::save(id, user, expected_revision, update)` 创建或更新配置。创建时 revision 为 0；更新/删除传入最近读取的 revision，版本冲突返回 `Stale`。`get`/`list` 只返回公开配置，模型列表按用户隔离，每个用户最多 32 项。

配置显示名称与内部 ID 分离：schema 7 的 `agent_model_names` 保存名称，旧配置没有名称记录时回退到原 ID。`save_named(id,user,revision,update,Some(name))` 在同一事务里保存名称和配置；原有 `save` 调用保留显示名称。改名不改变 ID、密钥加密绑定或会话所属配置。名称去除两端空白，限 1–64 个非控制字符。IPC POST `agent/models` 可携带顶层 `displayName`；响应返回 `id` 与 `displayName`，Agent 列表显示配置名称并额外返回 `modelName`。

`runtime(id, user)` 只供原生代码获取运行配置，`ModelClient::new(runtime)` 创建模型客户端。数据库方法是同步方法，后续 IPC 主机应放在阻塞工作线程中调用。不要把 `RuntimeConfig` 传回前端或放进 Agent 事件日志。

## 配置示例与参数

下面是 `ModelConfigUpdate` 的 JSON 形状，用于理解后续设置接口。窗口与能力是示例值，必须按实际供应商、模型填写；程序不会根据模型名称猜测上下文窗口。

```json
{
  "config": {
    "provider": "openAiCompatible",
    "baseUrl": "https://model.example/v1/",
    "completionsPath": "chat/completions",
    "model": "your-model-id",
    "contextWindow": 8192,
    "outputReserve": 1024,
    "safetyReserve": 256,
    "compactPercent": 75,
    "outputTokenParameter": "maxCompletionTokens",
    "capabilities": {
      "toolCalls": true,
      "developerRole": false,
      "temperature": true,
      "reasoningEffort": false,
      "vision": false
    },
    "temperature": null,
    "reasoningEffort": null,
    "timeoutSeconds": 120,
    "maxResponseBytes": 1048576,
    "requireApiKey": true
  },
  "apiKey": { "action": "keep" }
}
```

| 参数 | 效果与边界 |
| --- | --- |
| `baseUrl`、`completionsPath` | 基础 URL 与相对路径拼接；保留代理前缀，如 `/proxy/v1/`。路径允许一个开头 `/`，仍相对基础前缀拼接。拒绝跨站 URL、`..`、查询串、片段及编码路径；基础 URL 不接受用户名/密码或查询串。 |
| `model` | 供应商实际支持的模型 ID，去除两端空白；不是客户端显示名称。 |
| `contextWindow` | 模型上下文窗口，范围 256–16777216 token。 |
| `outputReserve`、`safetyReserve` | 输出与安全预留；输出必须大于 0，两者之和必须小于窗口。 |
| `compactPercent` | 可用输入预算的整理触发比例，1–99；默认 75。C2 据此触发摘要，提交前须降至其 80% 的较低水位（默认 60%）。前端高级参数可编辑。 |
| `outputTokenParameter` | `maxCompletionTokens` 发 `max_completion_tokens`；兼容供应商需要旧字段时用 `maxTokens`，一次只发送一种字段。 |
| `capabilities` | 明确声明函数工具、developer 角色、temperature、reasoning 参数等支持情况。声明是配置，不代表已通过真实服务探测。当前只处理文本；vision 标记留给 E1。 |
| `temperature` | `null` 表示不发送；设置时必须支持该参数，值在 0–2 之间。 |
| `reasoningEffort` | `null` 表示不发送；可配置 `none/minimal/low/medium/high/xhigh`，需要能力开关。具体模型接受哪些值需按供应商文档选择。 |
| `timeoutSeconds` | 整次请求总时限，包含发请求和读取响应，1–600 秒；连接时限最多 30 秒。 |
| `maxResponseBytes` | 成功响应正文上限，1024–8388608 字节；按实际读取字节检查，不仅依赖 Content-Length。 |
| `requireApiKey` | 为 true 且无密钥时，允许保存草稿但 `ready=false`，不能发模型请求；无认证的本地模型可设 false。 |

预算计入序列化后的完整消息、工具定义、参数和消息开销；超限直接报 `BudgetExceeded`，不悄悄裁掉消息。UTF-8 估算仍是启发式，不是精确 tokenizer；响应 usage 会返回实际 token 统计，C2 在普通发送之前尝试有界自动整理，实际发送仍经过完整硬上限校验；持续 usage 校准尚未实现。

## API Key 的保存和修改

`apiKey` 接受三个动作：

```json
{ "action": "keep" }
{ "action": "set", "value": "example-only-not-a-real-key" }
{ "action": "clear" }
```

省略该字段默认 keep。keep 保留原密钥，set 明确替换，clear 删除。已有密钥时更改基础 API URL 必须同时明确 set 或 clear，避免把旧供应商密钥自动发往新地址。改变同一地址下的模型 ID 可以保留密钥。

Windows 复用现有 DPAPI，加密范围绑定当前 Windows 账户以及用户/配置 ID。SQLite 中只有密文，公开配置只返回 `apiKeyConfigured` 和 `ready`；密钥类型不支持序列化，Debug 会脱敏。真正发请求时才在原生代码中解密并放入 Bearer 请求头，不放在 URL、请求 JSON 或 Agent 事件中。

`ready` 表示所需配置/密钥是否存在，不代表服务可连接或密文一定能解密。解密损坏密文或复制到不同配置的密文时返回 `Credentials`。数据库复制到另一 Windows 账户/电脑后可能需要重新填写密钥。非 Windows 平台当前返回安全保存不支持的错误，不回退为明文；跨平台凭据存储需要后续专门实现。

模型服务可使用 HTTP(S)。HTTPS 使用系统信任根并验证证书，不提供跳过证书校验选项；HTTP 会明文传输请求，应仅用于你明确选择的本地/受控模型服务。回环地址直接连接，不使用环境 HTTP 代理。

## 协议与错误

协议参考 [OpenAI Chat Completions 创建接口](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create)。B1 的 complete 接口实现非流式请求；B2 新增基础文本 stream，见 [AGENT_CHAT.md](AGENT_CHAT.md)。当前没有 Responses、Anthropic 原生协议或远端压缩分支。

支持文本 system/developer/user/assistant/tool 消息与 function 工具定义。函数调用参数必须是 JSON 对象，调用 ID 唯一，历史中的每次调用必须有结果；未定义工具、重复/孤立结果、未完成工具组均在发请求前拒绝。这里只校验协议，不执行 SSH 工具、不授予工具权限；执行参数 schema、目标和确认策略仍属于 D1–D3。

成功返回一个 assistant choice，接收 `stop` 或 `tool_calls` 结束原因。`length`、`content_filter` 等不被当成完整成功；空 choices、损坏 JSON、未声明工具、错误调用参数或矛盾 usage 返回错误。原始 provider 错误正文不透传，只返回状态码，避免它回显密钥或提示词。网络错误也不附带 URL 或第三方诊断正文。

客户端关闭重定向和自动重试；取消后不继续读取响应。取消/超时只代表本地请求停止，不保证供应商已经中止计费或计算。B2/B3 已实现 SSE、结束标记、会话互斥及持久轮次恢复；`complete` 仍是非流式接口，不能把它当作 `chat_stream`。

## 运行简单测试

在 Rust 项目目录执行：

```powershell
cargo test --offline --test agent_models --test agent_context --test agent_kernel --test agent_outputs
cargo clippy --offline --lib --test agent_models -- -D warnings
```

首次新增 reqwest 依赖尚未下载时先执行 `cargo fetch`，之后可以离线测试。所有模型协议请求都发往测试临时启动的 `127.0.0.1` 服务；测试只用假密钥，不要求真实 API Key，也不连接用户 SSH 服务器。

Windows 本轮结果：模型测试 18 项 + 原 Agent 内核 40 项，共 58/58 通过；桌面原生集成另有 5/5 通过。包括 schema 4 → 5、旧会话保留、密文重启/篡改/跨配置拷贝、权限/版本、函数调用、错误脱敏、重定向阻止、取消、超时和普通/分块响应大小。静态检查通过。具体续接记录见 [AGENT_PROGRESS.md](AGENT_PROGRESS.md)。
