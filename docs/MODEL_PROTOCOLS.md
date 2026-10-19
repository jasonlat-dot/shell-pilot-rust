# 模型协议策略

本次在原生模型层增加 `anthropicMessages`，保留 `openAiCompatible` 与 `openAiResponses`。数据库继续使用 schema 21；模型配置仍是 JSON，旧配置和加密 Key 不需要迁移。

## 分层与扩展

- `src/agent/provider/adapter.rs`：`ProviderAdapter` 策略接口及单一工厂。策略拥有认证头、配置归一化、请求编码/预算估算、完整响应解码与流解析器创建；能力查询供业务使用。
- `transport.rs` / `dispatch.rs`：共享 HTTP 生命周期，负责超时、取消、禁止重定向/自动重试、响应大小及 MIME 检查；不解释 Claude 的字段或工具片段。密钥头标记为 sensitive，供应商错误正文不回传。
- `protocol.rs`：共享内部消息、工具历史配对、工具名称/参数/结束原因校验及预算硬限制。`ResponseStream` 将供应商 SSE 归一为可见文本和最终 `Completion`；工具 JSON 片段只在完整响应校验后交给现有工具循环。
- `anthropic.rs` / `anthropic/stream.rs`：Anthropic Messages 编解码与事件状态机。`stream.rs` / `tool_stream.rs` 保留 Chat Completions 的文本/工具解析器。
- `responses.rs`：继续拥有 Responses 完整窗口、token 计数与远端整理。工厂通过窗口能力保留此专属路径，不能将供应商加密窗口摊平成普通聊天消息。
- 前端 `src/api/modelProtocols.ts`：穷尽协议目录驱动协议选项、路径、输出参数、Temperature 上限及推理控件；设置页只消费目录，不自行组装网络请求。

新增另一种无状态聊天协议时：新增配置枚举及独立策略/流解析器，在工厂注册，补齐前端目录和协议契约测试。Agent 主循环通过 `supports_tools()` 决定工具准入，无需再添加供应商判断；协议适配不赋予工具执行权限，所有调用仍经过原有 schema 校验、审批、目标核验与执行回执链路。具有独立持久上下文的协议还需要设计自己的上下文契约，不能只注册聊天策略便声称支持。

## Claude Messages 行为

- 端点：基础地址包含 `/v1/` 或代理前缀，相对路径通常是 `messages`。`x-api-key` 携带 Key，附带 `anthropic-version: 2023-06-01`；Key 的保存和保留/替换规则沿用原生仓库。
- 顶层 `system` 合并内部 system/developer 提示；对话只使用 user/assistant。工具定义转换为 `input_schema`，调用为 `tool_use`，结果为 user 下的 `tool_result`，相邻同角色内容合并。
- 图片使用原生 image/base64 source；预算按现有图片预留估算，不把 base64 字符串当普通文本计数。输出使用 `max_tokens`，Temperature 范围 0–1（具体模型还需支持该参数）。不发送 OpenAI 的 reasoning_effort/max_completion_tokens。
- 非流式及 SSE 共用完成校验；只有合法 stop reason 和 message_stop 才完成流。max_tokens/pause_turn/断流为未完成，不自动重放请求。usage 合并输入、缓存创建/读取及输出 token。
- 工具输入保留原始 JSON，包括重复键，使现有严格工具 schema 校验仍能拒绝歧义参数；不把部分 JSON 或思考片段当作可执行调用。
- 接近窗口时沿用本地模型摘要，支持自动和手动整理；不会调用 Responses 的计数或 compact API。

当前范围为**常规 Messages 模式**，显式禁用 thinking。扩展/自适应思考需要持久保存并回传签名内容块，本次未实现；不支持只能以思考模式工作的端点。若代理仍返回 thinking 与工具调用组合，会拒绝该轮工具，避免丢失签名继续执行。Bedrock / Vertex 专用认证、供应商服务端工具和 Claude 原生远端整理也不在本次范围内。

协议依据：[认证与版本](https://platform.claude.com/docs/en/api/overview)、[流事件与累计 usage](https://platform.claude.com/docs/en/build-with-claude/streaming)、[工具定义](https://platform.claude.com/docs/en/agents-and-tools/tool-use/define-tools)、[工具结果](https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls)。

## 验证范围

新增 `src/agent/provider/anthropic/tests.rs` 覆盖原生请求、图片估算、认证头、参数边界、usage、原始工具 JSON、工具片段隔离与不完整流；`tests/agent_anthropic.rs` 使用回环模型验证实际 HTTP、逐字节 UTF-8 SSE、工具授权/执行/回传以及断流不重试。前端新增协议切换及设置保存测试。

实际运行结果记录在 [AGENT_PROGRESS.md](AGENT_PROGRESS.md) 本次条目。回环测试不等同于真实 Claude Key、供应商兼容性或桌面视觉验收；不生成安装包。
