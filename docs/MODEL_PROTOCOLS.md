# 模型协议策略

原生模型层通过策略工厂支持 `openAiCompatible`、`openAiResponses`、`anthropicMessages` 和 `googleGemini`。当前 schema 24（原生续接状态表在 schema 22 引入）；本轮 Google 接入不新增迁移。旧模型配置和加密 Key 保持原值，已有数据库启动时自动升级。

## 分层与扩展

- `src/agent/provider/adapter.rs`：`ProviderAdapter` 策略接口及单一工厂。策略拥有端点、流式请求准备、EOF 完成策略、认证头、配置归一化、请求编码/预算估算、完整响应解码与流解析器创建；能力查询供业务使用。
- `transport.rs` / `dispatch.rs`：共享 HTTP 生命周期，负责超时、取消、禁止重定向/自动重试、响应大小及 MIME 检查；不解释供应商的字段或工具片段。密钥头标记为 sensitive，供应商错误正文不回传。
- `protocol.rs`：共享内部消息、工具历史配对、工具名称/参数/结束原因校验及预算硬限制。`ResponseStream` 将供应商 SSE 归一为可见文本和最终 `Completion`；工具 JSON 片段只在完整响应校验后交给现有工具循环。
- `anthropic.rs` / `anthropic/stream.rs`：Anthropic Messages 编解码与事件状态机。`stream.rs` / `tool_stream.rs` 保留 Chat Completions 的文本/工具解析器。
- `continuation.rs`：按协议选择续接策略，只保留供应商实际返回的推理内容或签名，封装为不可从 IPC 构造的原生消息。`continuation_store.rs` 在原始事件范围旁保存状态，工具批次、最终回复和追加输入的中间回复与对应事件原子提交。
- `responses.rs`：拥有 Responses 完整窗口、工具调用、token 计数与远端整理。工厂通过窗口能力保留此专属路径，不能将供应商加密窗口摊平成普通聊天消息。
- 前端 `src/api/modelProtocols.ts`：穷尽协议目录驱动协议选项、路径、输出参数、Temperature 上限及推理控件；设置页只消费目录，不自行组装网络请求。

新增另一种无状态聊天协议时：新增配置枚举及独立策略/流解析器，在工厂注册，补齐前端目录和协议契约测试。Agent 主循环通过 `supports_tools()` 决定工具准入，无需再添加供应商判断；协议适配不赋予工具执行权限，所有调用仍经过原有 schema 校验、审批、目标核验与执行回执链路。具有独立持久上下文的协议还需要设计自己的上下文契约，不能只注册聊天策略便声称支持。

## Claude Messages 行为

- 端点：基础地址包含 `/v1/` 或代理前缀，相对路径通常是 `messages`。`x-api-key` 携带 Key，附带 `anthropic-version: 2023-06-01`；Key 的保存和保留/替换规则沿用原生仓库。
- 顶层 `system` 合并内部 system/developer 提示；对话只使用 user/assistant。工具定义转换为 `input_schema`，调用为 `tool_use`，结果为 user 下的 `tool_result`，相邻同角色内容合并。
- 图片使用原生 image/base64 source；预算按现有图片预留估算，不把 base64 字符串当普通文本计数。输出使用 `max_tokens`，Temperature 范围 0–1（具体模型还需支持该参数）。不发送 OpenAI 的 reasoning_effort/max_completion_tokens。
- 非流式及 SSE 共用完成校验；只有合法 stop reason 和 message_stop 才完成流。max_tokens/pause_turn/断流为未完成，不自动重放请求。usage 合并输入、缓存创建/读取及输出 token。
- 工具输入保留原始 JSON，包括重复键，使现有严格工具 schema 校验仍能拒绝歧义参数；不把部分 JSON 或思考片段当作可执行调用。
- 接近窗口时沿用本地模型摘要，支持自动和手动整理；不会调用 Responses 的计数或 compact API。

思考模式沿用供应商默认，不再固定发送 `thinking: disabled`。收到 `thinking` / `redacted_thinking` 时按原顺序保留文本、签名和密文，并与文本/工具块一起回传；SSE 拼接 thinking/signature 片段，校验结束后才交给工具循环。原生推理强度通过 output_config.effort 配置，不发送 OpenAI reasoning_effort，也不新增手动 thinking budget 配置。Bedrock / Vertex 专用认证、供应商服务端工具和 Claude 原生远端整理不在此范围内。

协议依据：[认证与版本](https://platform.claude.com/docs/en/api/overview)、[流事件与累计 usage](https://platform.claude.com/docs/en/build-with-claude/streaming)、[工具定义](https://platform.claude.com/docs/en/agents-and-tools/tool-use/define-tools)、[工具结果](https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls)。

## 验证范围

新增 `src/agent/provider/anthropic/tests.rs` 覆盖原生请求、图片估算、认证头、参数边界、usage、原始工具 JSON、工具片段隔离与不完整流；`tests/agent_anthropic.rs` 使用回环模型验证实际 HTTP、逐字节 UTF-8 SSE、工具授权/执行/回传以及断流不重试。前端新增协议切换及设置保存测试。

实际运行结果记录在 [AGENT_PROGRESS.md](AGENT_PROGRESS.md) 本次条目。回环测试不等同于真实 Claude Key、供应商兼容性或桌面视觉验收；不生成安装包。

## 推理与工具续接

| 配置的协议 | 自动保存/回传的供应商状态 | 适用入口 |
| --- | --- | --- |
| Chat Completions | assistant 的 `reasoning_content` | DeepSeek，以及返回同一字段的兼容模型 |
| Chat Completions | 原 tool call 的 `extra_content.google.thought_signature` | Gemini 的 OpenAI 兼容入口 |
| Messages | 完整 `thinking` / `redacted_thinking`、signature 与同轮文本和工具块 | Anthropic Messages 兼容入口 |
| Responses | 原顺序的 reasoning/encrypted_content、message、function_call 等完整输出项 | 支持 Responses 和 input_tokens 的端点 |

使用已保存的接口协议选择策略，解析实际返回的数据，不按 `deepseek`、`claude`、`gemini` 等模型名子串分支。普通响应没有这些字段时不补字段；不伪造签名或空思考。Gemini 原生 generateContent 不在本次协议范围内，代理必须完整保留其 OpenAI 兼容扩展字段。

Responses 工具使用原生 function 定义及 `call_id` / `function_call_output` 配对；工具定义也纳入发送前供应商 token 计数。工具片段只在 response.completed、完整输出、调用名称与参数校验后执行。任何错误、断流或取消都不自动重放工具。

原生状态绑定协议、完整端点、模型和凭据摘要，不向其他端点/凭据转发。恢复对话会从原始事件范围重建整条 assistant 回复，避免把正文和工具调用拆开；整理只移除已归入完整检查点的前缀，保留尾部状态。宿主生成的工具检查点作为明确标注的数据传入，不伪装成带签名的 assistant 响应。可见历史和诊断日志不包含思考原文、签名或密文；状态仅在本地 SQLite 原生表中保存，删除对话时一并删除。

旧版本已经丢弃的推理字段无法从正文恢复；本次升级不会编造这部分历史。如果供应商拒绝旧轮次，可新建对话，或先整理旧历史。模型/协议自身能力、显式思考参数支持范围仍以所用端点为准。

新增 `tests/agent_provider_continuations.rs` 覆盖四种状态的 JSON / 分块 SSE 两轮工具续聊、原始字段回传与日志隔离；原生单测覆盖普通模型不加字段、IPC 拒绝状态注入、端点/模型/凭据隔离、数据库重开、历史重建与整理后的尾部状态。均使用回环模型、临时数据库和模拟工具，没有调用真实供应商或 SSH。

协议依据：[DeepSeek 思考模式](https://api-docs.deepseek.com/guides/thinking_mode/)、[Claude 思考块与工具续接](https://platform.claude.com/docs/en/build-with-claude/thinking)、[Gemini thought signatures](https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures)、[OpenAI reasoning](https://developers.openai.com/api/docs/guides/reasoning)。


## 同一会话切换模型

Manager.change_model 在会话执行锁内检查新模型可用性、用户归属和 revision；原子更换 agent_id，保留 session/task/scope、SSH 身份、项目关系及审批策略。运行中的 turn、准入的执行或扩展调用、写入中的输出和悬挂工具调用会阻止切换。

schema 23 的 agent_model_transitions 保存切换前事件边界。边界前的 assistant 和工具事件以明确标注的 JSON 历史数据传入，不形成新模型的可执行调用；原始事件、工具证据与对话界面不被重写。边界后的回复按新模型协议正常保存和回传原生状态。上下文用量清零待下次实际测量，普通文本检查点继续使用。

Responses 加密压缩窗口不能跨模型转发，切换时解除当前窗口引用，并恢复完整原始事件前缀；旧窗口作为历史记录保留。新请求仍受目标模型的上下文容量限制，不承诺较小窗口能容纳任意长历史。直接编辑同一模型的端点或凭据仍执行绑定隔离。

回环测试覆盖三种协议间全部六种切换方向，分别验证文字和工具模式共 12 个场景：重开数据库后继续原 session、新请求包含旧历史、不携带旧协议状态、历史证据不会成为待执行工具。另有权限、CAS、忙碌拒绝、服务器目标、文本及远端检查点单测。未使用真实供应商凭据。


## Messages 原生 effort

ModelConfig.reasoningEffort 复用已保存的可选强度配置。Messages 策略接受 low / medium / high / xhigh / max 并写入 output_config.effort；不伪造 thinking、budget_tokens 或 OpenAI reasoning 字段。默认 null 省略 output_config，旧配置行为不变。协议层拒绝 none / minimal，且仍要求 capabilities.reasoningEffort；非 Messages 策略继续拒绝新增的 max 档位。

前端协议目录共用选项和提示，模型设置与输入框均可保存。具体模型或代理的支持范围由供应商校验，不按模型 ID 子串猜测，也不自动重试、切换模型或降低档位。强度不会改变续接状态的归属与签名保存；JSON 和分块 SSE 工具轮次都使用同一策略编码。最大输出仍是思考和可见回复的总预算。

依据：[Claude Effort](https://platform.claude.com/docs/en/build-with-claude/effort)。本次不增加迁移、不改变默认强度、不新增关闭思考或手动预算配置。

验证：Messages 单测 9 项、协议续聊集成 6 项通过，包括全部五档的配置保存后读取、JSON / 分块 SSE 两轮工具调用、原样签名回传，以及三种协议间同会话切换。沙箱权限阻止初次集成测试的回环网络和 Windows 凭据访问，授权环境重跑通过；未使用真实供应商，未做打包或提交。


## 多模型渠道与配置身份

schema 24 增加 agent_model_channels 和 agent_model_channel_members，旧配置无需批量重写。渠道编辑在同一事务内验证版本、保存公共参数/密钥和模型成员；最多 32 个渠道、每渠道 64 个启用模型，总配置（含移除后保留的身份）上限 4096。相同模型 ID 可属于不同渠道，运行时始终使用用户范围内独立的稳定 profile ID。

原有单模型 ID 在升级后保持不变；排序和移除/重加不产生新身份。移除模型设为 inactive，模型目录及新请求排除它，历史不删除。删除渠道会原子删除全部成员配置和名称，不删除对话记录。密钥始终使用原 profile ID 的 DPAPI 熵加密，渠道保存时统一应用；HTTP 请求选择的实际模型名仍是单个 config.model。

独立的输入框强度更新只允许改变成员的 reasoningEffort 和对应能力位，禁止绕过渠道编辑器修改公共端点、名称或密钥；它同时推进渠道修订版本，防止过期渠道编辑覆盖新设置。跨模型切换继续沿用会话锁、revision 校验和历史模型边界，签名和密文仍不会跨模型转发。

验证：Rust 全量 453 项通过、6 项既有忽略，生产库严格 Clippy 通过；客户端原生库 59 项和前端 530 项通过。渠道集成测试使用临时数据库、测试密钥和回环模型，验证两个实际模型名、共享密钥的请求头、轮换/清除、同会话历史续聊和旧配置重开。未调用真实供应商、未打包或提交。


## Google GenerateContent（2026-10-20）

`google.rs` 为独立 ProviderAdapter，不向 Agent 循环增加供应商分支。基础地址保留官方 /v1beta 或代理前缀，相对路径 models，实际模型 ID 作为安全 URL 段追加；生成与流式分别用 :generateContent 和 :streamGenerateContent?alt=sse。模型 ID 限定为本地安全标识，可规范去掉 models/ 前缀；不接受完整 URL/查询或任意路径。API Key 由原生仓库加密保存，只通过 sensitive 的 x-goog-api-key 发送。

内部 system/developer 合并为 systemInstruction；用户/模型/工具映射到 contents 的 user/model、functionCall 和 functionResponse，工具参数仍经过既有白名单、闭合 schema、审批和执行边界。FunctionDeclaration 使用 parametersJsonSchema 保留既有 JSON Schema 约束；Google 原生 args 在转成 Value 前使用共享 strict_json 拒绝重复字段，不能把模糊参数归一化后派发。图片使用 inlineData，预算按现有图片预留估算，不按 base64 字符串长度计费。

generationConfig 包含 maxOutputTokens、candidateCount=1 及显式温度；显式 minimal/low/medium/high 转为 thinkingConfig.thinkingLevel。默认省略思考参数；不猜模型代际、不伪造预算或签名、不自动重试降档。thought 文本不进入可见回复，保留 model parts 原始顺序与 thoughtSignature，回复/工具 ID 与结果正确配对（供应商无 ID 时使用本地主机 ID，回传不编造供应商 ID）。原生状态沿用数据库的绑定、保存、切换及删除机制，不写入诊断正文。

SSE 使用共享字节帧解析，支持跨 UTF-8 字节/行分块；Google 无 [DONE]，仅完整 EOF、明确 STOP 和无未闭合 SSE 帧时成功。保留结束帧之后的 usageMetadata；截断、过滤、MAX_TOKENS、未知工具或异常参数不能触发执行。共享 HTTP 超时/取消/响应限制、禁止重定向与禁止自动重试保持。Google 整理使用本地摘要及现有检查点 CAS，不调用 Responses 计数或远端 compact。

范围是 Gemini Developer API 的 GenerateContent；旧 Gemini OpenAI 兼容配置不变。Vertex OAuth、Live/Interactions、原生文件上传、托管搜索/代码执行不属于此次接入。使用者仍须提供端点支持的模型 ID 和凭据。依据：[GenerateContent API](https://ai.google.dev/api/generate-content)、[思考配置](https://ai.google.dev/gemini-api/docs/generate-content/thinking)、[思考签名](https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures)。


本轮定向验证：Rust 46 项通过（供应商单测 24、Google HTTP/摘要/持久化 5、渠道 6、四协议续接 7、Anthropic 传输 3、严格工具参数 1）；四协议切换覆盖全部 12 个方向的文字/工具共 24 场景。前端 48 项、TypeScript / Vite、桌面原生 tests 编译和生产库严格 Clippy 通过。临时数据库与回环服务，无真实 Google Key/用户 SSH；实际桌面视觉及真实供应商联调未完成，未打包或提交。完整结果与初轮修正记录见 AGENT_PROGRESS.md。
