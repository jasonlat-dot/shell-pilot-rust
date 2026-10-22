# 模型协议策略

原生模型层通过策略工厂支持 `openAiCompatible`、`openAiResponses`、`anthropicMessages` 和 `googleGemini`。当前版本 **v0.2.5**，当前源码最新迁移为 026（渠道启用状态）；原生续接状态在 schema 22 引入，多模型渠道在 schema 24 引入，网络代理为第 25 号迁移。旧模型配置和加密 Key 保持原值，已有数据库启动时自动升级。已发布版本说明见 [v0.2.5](releases/v0.2.5.md)。

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

schema 23 的 agent_model_transitions 保存切换前事件边界。边界前的 assistant 和工具事件以明确标注的 JSON 历史数据传入，不形成新模型的可执行调用；原始事件、工具证据与对话界面不被重写。边界后的回复按新模型协议正常保存和回传原生状态。上下文用量保留上次快照并标记 usageStale，待下次测量替换；普通文本检查点继续使用。

usageStale 默认 false，兼容已存状态。显式切换在同一事务内保留统计、清除旧整理状态并撤销旧 observation owner，避免延迟回执覆盖；配置绑定刷新保留当前准备阶段 reporter 的身份。新请求 begin 保留上次 usage，Measured 才替换所有预算字段并清除标记。此缓存不参与实际请求预算，跨模型仍重新测量；没有测量的会话继续为未知。该显示状态随工作区持久保存，不增加迁移。

Responses 加密压缩窗口不能跨模型转发，切换时解除当前窗口引用，并恢复完整原始事件前缀；旧窗口作为历史记录保留。新请求仍受目标模型的上下文容量限制，不承诺较小窗口能容纳任意长历史。

前端允许生成期间选择下一轮模型，将意向与运行中模型分开，轮次和工具收尾后再调用原生 change_model；后端准入锁、revision 和忙碌校验仍然保留。按会话提交，不改变其他会话的选择或当前运行的审批、停止与工具归属。

运行中可编辑已保存的模型配置；当前轮次继续使用准备阶段固定的 RuntimeConfig。下一次用户请求在载入时若发现供应商/协议/模型/端点/凭据绑定变化，refresh_model_binding 复用相同的事务、忙碌及 CAS 校验，记录原生状态边界、解除远端加密窗口引用，再从保留的原始事件重建。普通文字检查点、任务、服务器身份和审批策略保留；旧思考、签名和密文不跨绑定重放。自动整理中使用已固定 RuntimeConfig 的重载不执行此刷新，避免运行中上下文被配置编辑改写。不增加 schema 或迁移。

回环测试覆盖三种协议间全部六种切换方向，分别验证文字和工具模式共 12 个场景：重开数据库后继续原 session、新请求包含旧历史、不携带旧协议状态、历史证据不会成为待执行工具。另有权限、CAS、忙碌拒绝、服务器目标、文本及远端检查点单测。未使用真实供应商凭据。

### Ollama 同端点多模型

SupplierKind::Ollama 使用独立供应商策略及现有 Chat Completions adapter，固定 `max_tokens`，默认省略强度，可选 `none / low / medium / high` 写入 `reasoning_effort`；不会因本地模型命名为 DeepSeek/Qwen 而发送云厂商的 thinking 字段。仅支持 Chat 协议，目录使用相同前缀的 `/models`。本地配置关闭 requireApiKey，代理认证可选 Bearer；地址、密钥、模型和供应商绑定继续隔离。没有 schema 迁移。

供应商策略提供可覆盖的 decode_chat / stream_chat，默认保持原解码器。Ollama 将官方 JSON/SSE 的 `reasoning` 映射到已有严格解码器，再以原始字段名保存原生续接；同模型后续工具请求回放 `reasoning`，可见回复与日志不包含思考。冲突、非字符串思考和未正常结束的流均拒绝；取消、体积及工具调用验证沿用原逻辑。

从同一 Ollama 地址的 DeepSeek 切到 Qwen 仍复用 change_model，保留同一 session、原始历史和审批边界，用可移植问答及工具记录重建新模型输入，不跨模型复用思考或运行缓存。专项回环测试在普通文字与启用工具两种模式下验证同端点切换、数据库重开、旧问题与回答进入新模型请求、旧原生思考不进入新请求。

应用窗口数值只用于预算，不能修改 Ollama 实际分配的 context；用户需按 `ollama ps` 的 CONTEXT 配置窗口、输出及安全预留。整理使用本地语义摘要，未接入原生 /api/chat、Responses、模型管理或服务控制。接口依据见客户端文档的 Ollama 章节及 [官方兼容层源码](https://github.com/ollama/ollama/blob/main/openai/openai.go)。


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

## 上游模型目录

`provider/catalog.rs` 通过独立 CatalogAdapter 与现有 ProviderAdapter 工厂组合，查询与对话参数、工具循环分离。OpenAI 两种协议共享 data/id 列表；Anthropic 使用 x-api-key + anthropic-version 和 after_id / has_more / last_id；Google 使用 x-goog-api-key 和 pageToken / nextPageToken，过滤 supportedGenerationMethods 中的 generateContent，规范化 models/ 前缀。Endpoint 均为校验后的基础地址拼接 models，不自动切换版本或追随上游 URL。

模型发现直接使用草稿连接字段，不要求已有模型 ID。KeyChange::Keep 按用户、渠道、修订解析加密凭据，Set 使用草稿，关闭鉴权不解密/发送；查询不会修改配置。保留现有单渠道 64 个模型及原存储限制。HTTP 禁止重定向/重试；全查询 30 秒 / 20 页 / 8 MiB / 10000 模型，取消或失败不提交部分结果，重复游标拒绝。目录不推断未声明的工具、图片、推理、上下文能力。

协议依据：[OpenAI List models](https://developers.openai.com/api/reference/resources/models/methods/list)、[Anthropic List Models](https://platform.claude.com/docs/en/api/models/list)、[Google models.list](https://ai.google.dev/api/models#method:-models.list)。真实供应商 / 中转是否开放目录及具体权限仍由用户查询时验证。

Google 模型目录对未声明能力（supportedGenerationMethods 缺失 / null / 空数组）的条目直接跳过，不推断其可对话性；仍只保留明确包含 generateContent 的有效记录。nextPageToken 缺失 / null / 空字符串均表示没有下一页。一个页面没有有效模型但有非空游标时仍继续分页；ID 规范化后跨页去重。错误的非空字段类型、有效条目缺少 models/ 前缀等继续报错并记录字段位置。此规则按用户完整中转返回确认，不自动变更 API 地址、鉴权或对话协议。


## DeepSeek 独立供应商与协议组合（2026-10-20）

供应商身份与 wire 协议分离：ModelConfig / CatalogRequest 可带 supplier，DeepSeek 保存为 deepseek，仍使用 openAiCompatible 或 anthropicMessages 作为 provider。旧 JSON 缺省 supplier 仍按原协议工作，通用供应商标签规范化为缺省身份，不迁移 schema 或按域名/模型 ID 识别品牌。原生 Agent 模型列表返回可选 supplier，前端配置卡片、筛选与两级模型选择器共用本地蓝色鲸鱼 SVG。

`provider/supplier.rs::SupplierPolicy` 组合原 ProviderAdapter：兼容策略完整沿用原校验，DeepSeek 策略集中处理允许的协议/思考档位、max_tokens、thinking 开关、模型目录与签名要求；HTTP、SSE、工具循环和语义摘要仍复用现有代码。默认思考不发覆盖字段，none 转 thinking disabled 并移除无效 effort 字段；low/high/max 启用思考并按协议发送 reasoning_effort 或 output_config.effort。DeepSeek Messages 的温度范围允许 0–2，通用 Anthropic 保持 0–1；思考模式下上游忽略温度。此次独立入口不开放 Responses，不能据此推断 DeepSeek 上游不支持该协议。

DeepSeek Chat 使用配置前缀下的 chat/completions 与 Bearer，Messages 使用 messages 与 x-api-key。官网占位地址分别为 https://api.deepseek.com/ 和 https://api.deepseek.com/anthropic/v1/，不写入空草稿。两种协议的目录均使用 OpenAI data/id 格式和 Bearer；仅 Messages 的完整后缀 /anthropic/v1/ 或 /anthropic/ 映射至同一 origin、保留代理前缀的 models，不剥离名字相似但不完整的路径段，不追随上游 URL、重试其他地址或发送真实用户凭据进行探测。已有目录的分页、超时/取消、体积、Trace 与密钥保护继续复用；DeepSeek name 元数据可作为可选展示名称。

DeepSeek 的 Messages 思考块允许无签名，JSON、SSE 与流式 JSON 回退走同一供应商规则；不会生成或伪造签名，通用 Anthropic 仍执行原严格规则。续接绑定增加向后兼容的可选 supplier，供应商变更后禁止原生思考/签名跨供应商重放；旧缺省绑定继续正常使用。本地摘要不带工具，不使用 Responses 计数/远端整理端点，失败保留原历史。

接口依据：[官方接入](https://api-docs.deepseek.com/)、[Anthropic 兼容](https://api-docs.deepseek.com/guides/anthropic_api/)、[思考模式](https://api-docs.deepseek.com/guides/thinking_mode/)、[models](https://api-docs.deepseek.com/api/list-models/)。本轮本地回环测试结果以 AGENT_PROGRESS.md 最新章节为准；真实供应商和桌面视觉验收分别记录，不把模拟测试标成真实联调完成。


DeepSeek 本轮最终定向验证：前端 64 项、Rust provider/目录/工具续聊/语义摘要 45 项、桌面原生 IPC 1 项通过；工具续聊覆盖两协议的 JSON/SSE 与四种思考设置，摘要覆盖两协议。TypeScript、Vite 资源构建、生产库严格 Clippy、新策略 Rustfmt 与 diff 检查通过。测试编译为降低本机内存，对本包测试临时关闭调试符号，未改生产配置。尚无真实供应商/中转和桌面视觉联调；未打包。详见 AGENT_PROGRESS.md 最新章节。


2026-10-20 默认地址补充：客户端空白地址在保存/查询边界解析为当前供应商/协议的官方默认值；输入框仍使用 placeholder，不自动回填。Google 默认使用 /v1beta；DeepSeek Chat 为 https://api.deepseek.com/v1，Messages 为 https://api.deepseek.com/anthropic/v1；Google 为 https://generativelanguage.googleapis.com/v1beta；GLM 见下节。显式地址和旧 /v1、/v1beta 配置保持原样，Rust URL 校验与相对路径规范化不变。本次前端相关 49 项、TypeScript 与 diff 检查通过，不改 Rust 生产代码；版本依据见 [Google API 版本说明](https://ai.google.dev/gemini-api/docs/api-versions)。

## GLM 供应商策略（2026-10-20）

`SupplierKind::Glm` 独立持久保存，复用 Chat Completions / Messages 的 wire adapter，策略注册于 `supplier.rs`。不增加 wire 协议枚举、不迁移 schema、不依赖模型名或域名猜测。前端供应商描述目录管理默认地址、协议选项、温度和推理档位，DeepSeek 的原有条件分支改为目录覆盖规则。

- Chat 默认 `https://open.bigmodel.cn/api/paas/v4` + `chat/completions`，Bearer；Messages 默认 `https://open.bigmodel.cn/api/anthropic/v1` + `messages`，x-api-key。自定义区域/代理前缀和凭据始终保留。
- normalize 固定输出 `max_tokens`，校验温度 0–1；显式推理仅支持 none/low/high/max。默认省略字段，none 转 thinking disabled 且移除 effort；其余转 thinking enabled，Chat 使用 reasoning_effort、Messages 使用 output_config.effort。模型支持范围由上游决定，不硬编码模型名单、不降档或重试。部分模型强制思考不可关闭，旧模型可能不支持 effort，应使用默认。
- GLM Messages 的 JSON/SSE 允许无签名 thinking，原字段保持原样；原生 continuation 绑定包含 supplier，切换到 DeepSeek/通用协议后不重放私有思考状态。思考/签名不进入可见回复或日志。通用 Anthropic 签名校验不变。
- 模型目录复用所选协议的原生 GET models、鉴权和分页；不自动剥离 GLM API 前缀、不跳转到其他推测目录。公开文档未保证统一模型目录；接口不支持时沿用错误/Trace 和手动模型 ID 维护，不制造内置模型列表。查询不保存草稿，不回显 Key。
- 两协议使用现有本地语义摘要，不携带工具，不使用 Responses 的计数/远端整理。本次入口只开放定向验收的 Chat 和 Messages。

依据：[智谱 OpenAI 兼容](https://docs.bigmodel.cn/cn/guide/develop/openai/introduction)、[Claude 兼容](https://docs.bigmodel.cn/cn/guide/develop/claude/introduction)、[思考续接](https://docs.bigmodel.cn/cn/guide/capabilities/thinking-mode)、[强度与模型限制](https://docs.bigmodel.cn/cn/guide/capabilities/thinking)。验证命令和真实接口边界见 AGENT_PROGRESS.md 本轮记录。

## Qwen 供应商策略（2026-10-20）

`SupplierKind::Qwen` 独立持久化，策略实现位于 `supplier/qwen.rs`，复用 Chat Completions / Messages adapter。默认客户端地址为国内公共端点 `https://dashscope.aliyuncs.com/compatible-mode/v1` 与 `https://dashscope.aliyuncs.com/apps/anthropic/v1`；显式业务空间域名、地域、代理和密钥保持原样，不通过模型 ID 推测协议。

`ReasoningEffort::Enabled` 表示仅启用思考，无档位；只由 Qwen 策略接受，其他供应商拒绝该值。默认省略思考字段；none / enabled 映射 Chat 的 enable_thinking=false/true 或 Messages 的 thinking.type=disabled/enabled，移除 effort 字段；low/medium/xhigh 同时启用思考并使用对应协议的 reasoning_effort / output_config.effort。固定 max_tokens，温度 `[0,2)`；不伪造 thinking_budget、budget_tokens，不发送 extra_body 包装，不全局强开 preserve_thinking，不按模型名硬编码支持列表或自动降档。

SupplierPolicy::prefers_streaming 默认 false，Qwen 为 true；完整回复入口 complete 通过 Box::pin 调用既有流式协议生命周期并收集结果，覆盖仅支持 SSE 的模型及工具规划、本地摘要场景。不会改其他供应商的传输方式；结束帧、超时、取消、体积上限、协议状态封装和审批边界复用原逻辑。Messages 接受百炼文档中的空思考签名，原内容保持不变；通用 Anthropic 严格规则不变。

`catalog/qwen.rs` 将已知完整推理后缀映射为同 origin / 代理前缀下的 api/v1/models，两协议目录均用 Bearer，解析 output.total/page_no/page_size/models[].model/name，控制分页和总量；自定义普通前缀保留且接受 OpenAI data/id。错误页、空洞分页或不完整列表失败，不静默返回部分结果。当前不接入 Responses 或 DashScope 原生生成协议。

依据：[百炼 Chat](https://help.aliyun.com/zh/model-studio/qwen-api-via-openai-chat-completions)、[Messages](https://help.aliyun.com/zh/model-studio/anthropic-api-messages)、[思考开关与流式限制](https://help.aliyun.com/zh/model-studio/deep-thinking)、[模型目录](https://help.aliyun.com/zh/model-studio/list-models)。本地回环验收、真实接口和桌面视觉分开记录，见 AGENT_PROGRESS.md 最新章节。
