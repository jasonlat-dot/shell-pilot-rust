# B2：基础文本对话与桌面 IPC

更新日期：2026-10-14。

B2 接通就绪模型对应的 Agent 列表、创建会话、流式文本对话和停止操作；B3 已补齐请求幂等、持久轮次和恢复，见 [AGENT_TURNS.md](AGENT_TURNS.md)。桌面通过 invoke/Channel 调用同进程 Rust 服务，只有 Rust 模型客户端向模型供应商发 HTTP 请求。产品不新增监听端口或独立后端进程。

B4 已开放模型设置、文字 AI 面板与原生历史；API Key 安全存储见 [AGENT_MODELS.md](AGENT_MODELS.md)。C2 已在文字请求前接入普通模型自动整理与检查点恢复，保持现有 Channel 协议；工具执行和聊天附件仍待 D/E。本文接口以基础文本对话为范围，最新里程碑见 AGENT_PROGRESS.md。

## 控制接口与流式接口

| 前端调用 | 原生调用 | 返回 |
| --- | --- | --- |
| `agentApi.list()` | `builtin_request`，GET `agent/query_ai_agent_config_list` | `[{ agentId, agentName, agentDesc }]` |
| `agentApi.createSession(agentId)` | POST `agent/create_session` | `{ sessionId }` |
| `agentApi.stopChat(agentId, sessionId)` | POST `agent/stop_chat` | boolean，表示是否已向当前活动生成发出取消信号 |
| `agentApi.chatStream(body, receive, signal)` | `builtin_agent_subscribe` + Tauri Channel | `context / text / done / error` 事件，invoke 等待整个流完成 |
| `agentApi.contextStatus(agentId, sessionId)` | GET `agent/context_status` | 按用户/模型/会话隔离的最近输入测量与真实整理状态；只读，不调用模型 |

列表按 userId 查询，只返回配置中 ready=true 的模型；agentId 使用对应模型配置 ID，名称暂用模型 ID。就绪标志不代表已通过真实供应商探测。create_session 复核配置与用户归属，生成持久 SQLite Agent 会话。

请求体保留旧字段：

```json
{
  "agentId": "your-model-profile-id",
  "userId": "default",
  "sessionId": "created-session-id",
  "terminalSessionId": "",
  "message": "请解释这段报错"
}
```

不关联 SSH 终端时 terminalSessionId 传空字符串。关联终端时，原生适配器从存活终端查出 connectionId，并复核用户归属；不接受前端额外传入 connectionId/授权字段。任务目标固定，后续请求若改变目标会返回 `AGENT_TARGET_MISMATCH`，应新建会话。

流式聊天必须使用 Channel。通过普通 JSON 请求调用 `agent/chat_stream` 会返回 `AGENT_CHANNEL_REQUIRED`，不会走 HTTP 兼容接口。

## 事件与接收确认

Channel 每帧格式：

```json
{ "sequence": 1, "payload": { "event": "text", "content": "一个文本片段" } }
```

沿用前端 `parseStreamEvent`，保留 Unicode、空白、引号与代码里的字面量 `\n`。每处理一帧调用 `builtin_agent_ack`，当前订阅同时最多有一帧等待确认。15 秒未确认时停止该订阅。接收器异常会请求取消，并确认已收到的帧，避免清理过程持续等待。

成功结束发送 `{ event: "done", content: "完整文本" }`，失败使用 `{ event: "error", content, code }`。最后的错误帧 sequence=0，不等待确认。前端同时检查 invoke 结果与 done 事件，缺少 done 时不判成功。AbortSignal 调用 `builtin_cancel` 取消该 streamId，后续文本不再更新页面。

C5 新增 `{ event: "context", status }`，与文字帧共用 sequence/ACK 和当前订阅取消机制；`status.requestId` 必须匹配当前请求。`usage` 可为空，存在时包含 inputTokens、availableInputTokens、contextWindow、outputReserve、safetyReserve、compactPercent 和 source（estimated / providerCount）。占用是最近一次准备发送的完整输入快照，不是累计 usage，也不包含草稿或回复后的新增内容。

`phase` 为 idle / running / succeeded / failed / cancelled / interrupted；method 为 summary / remote 或 null。整理次数来自真实检查点数量，succeeded 必须关联已保存的 checkpointId。失败会提供固定 errorCode 与 fallback 标记，不回传供应商错误正文、提示或密钥。状态按 schema 11 持久保存；旧历史无测量时返回未知占用和实际检查点次数，生成中状态在租约过期后显示中断。只读状态不修改会话 revision，不触发摘要或恢复任务。细节及验证见 AGENT_PROGRESS.md 的 C5 记录。

这些事件仍属于基础文本对话；旧工具/子 Agent 事件解析保留，但本阶段不会生成工具执行事件。

## 模型流与会话记录

模型协议按 [OpenAI 流式响应文档中的 Chat Completions](https://developers.openai.com/api/docs/guides/streaming-responses) 实现 SSE：`stream=true`、`choices[0].delta.content`。字节解析允许 CRLF、注释和跨网络分片的 UTF-8，SSE data 行合并后再解码。

只有 `finish_reason=stop` 加明确的 `[DONE]` 才完成；单纯 EOF、截断、过滤、供应商错误 JSON 或意外工具调用均失败。支持可选 usage 数据，但不强制发送部分兼容供应商不接受的 stream_options。取消/总超时、TLS 校验、关闭重定向/重试及错误脱敏沿用 B1。

用户消息和可信任务/终端快照写入 A2 的事件库，B3 轮次表保存部分文本与明确状态。每次请求重新注入目标、目标任务和用户约束；任务快照不是执行授权。只有完整成功的 assistant 文本提交后才发送 done。生成中断不会把部分文本冒充完整 assistant；新版本上下文不会被旧回复覆盖。

同一会话只允许一个 running 轮次，数据库索引也约束跨 Manager 准入；单 Manager 最多 32 个活动生成。停止和工作区关闭会取消活动模型请求。新前端 stop_chat 携带 requestId，true 表示对应取消信号/标记已发出，不保证供应商已经中止计算或计费；已完成或没有活动生成返回 false。

## 当前边界与下一阶段

- 消息及完整回复最多 16000 UTF-8 字节；JSON 编码后的回复另限 60000 字节，保证日志和 Channel 帧有界。SSE 单行/单事件最多 64 KiB，总网络响应受 B1 maxResponseBytes 限制。
- 先检查完整历史与预算，不静默截断。C2 在接近触发比例或达到 200 条增量事件等压力时尝试有界整理；成功后使用检查点和近期完整轮次，保留原始日志。读取增量最多 256 条、单摘要最多 200 条；无法安全整理时明确报错，不伪造压缩成功。
- 历史回复只发送完整提交的轮次；独立追加、尚待回复的有效用户补充也会按顺序置于本次最新请求之前，防止整理期间新增约束漏送。失败/中断轮次的输入不自动重发，其记录与部分文本可通过 B3 历史接口回查；未知结果不伪造成功。
- 附件引用、已有工具记录及流式工具调用明确拒绝，避免在 E1/D1 前错误宣称支持它们。
- B3 已补齐轮次幂等、停止/完成排序、跨会话隔离及持久历史/租约恢复，具体实现和验证见 AGENT_TURNS.md。
- B4 添加模型设置和 AI 面板入口，B5 再完成前后端完整联验。尚未测试真实模型供应商或完整 GUI。

## 简单测试

Rust 项目目录：

```powershell
cargo test --offline --test agent_chat --test agent_models --test agent_context
cargo clippy --offline --lib --test agent_chat -- -D warnings
```

客户端目录：

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1
node --import tsx --test tests/agent-ipc.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts
node node_modules/typescript/bin/tsc --noEmit
```

本轮通过：Rust 8+18+17=43 项；原生 IPC 7 项；前端 27 项。Rust Clippy -D warnings、TypeScript 与针对性格式检查通过。桌面 Clippy 通过，报告既有聊天排序和旧进程测试警告，新 Agent 桥没有新增警告。

测试服务仅使用临时 `127.0.0.1` 模型端点和本地 SSH 协议夹具，不使用真实 API Key、用户 SSH 服务器、安装包或 GUI。完成记录及续接入口见 [AGENT_PROGRESS.md](AGENT_PROGRESS.md)。
