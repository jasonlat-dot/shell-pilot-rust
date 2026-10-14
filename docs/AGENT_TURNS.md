# B3：生成轮次、停止和恢复

更新日期：2026-10-14。基础文本对话仍通过同进程 IPC/Channel，B4 已提供模型设置、AI 面板和可见历史，C2 已接入文字请求前自动整理。命令执行与聊天附件分别留给 D、E 阶段。

## 请求身份与幂等

`chatStream` 新增 `requestId`，表示一次用户提交；`streamId` 仅表示本次 Channel 订阅。前端 `App.tsx` 使用本条 assistant 消息的 UUID 作为 requestId，停止时也带上同一个 ID。

```json
{
  "requestId": "one-stable-request-id",
  "agentId": "model-profile-id",
  "userId": "default",
  "sessionId": "persistent-session-id",
  "terminalSessionId": "",
  "message": "解释错误"
}
```

一个 requestId 对应相同会话、消息和终端目标。复用 ID 改消息或终端会返回 `AGENT_REQUEST_CONFLICT`。请求 ID 最多 64 字节；订阅 ID 与请求 ID 分开，重新读取结果不复用旧取消令牌。

同一请求的处理：

| 已有状态 | 再次提交相同 requestId |
| --- | --- |
| running | 返回 AGENT_BUSY，不创建第二次模型调用 |
| completed | 返回已有 done/完整文本，不调用模型；旧模型配置、密钥或 SSH 终端不必继续存在 |
| cancelled / failed / interrupted | 返回原错误代码；通过历史查看部分文本，不自动重试 |

只有用户明确重新发送并使用新 requestId 才开始新生成。旧调用未提供 requestId 时仍兼容生成新 UUID；它不提供跨次重发幂等保证，新的调用应始终保存自己的 requestId。

## 数据库与最终状态

新增 `006_agent_turns.sql`，schema 6。`agent_chat_turns` 保存请求输入快照、状态、部分/完整文本、usage、事件引用、所属生成者及租约，不保存 API Key。既有会话、事件、输出和模型表继续保留。

新任务、用户事件和 running 轮次在同一 SQLite 写事务中建立。数据库唯一索引限制每会话最多一个 running 轮次，即使两个 Manager 同时调用也不能重复准入。单 Manager 的活动生成上限仍为 32。

完成、停止标记和恢复使用 SQLite 写事务排序：

- 停止先写入标记，完整响应随后到达：记录 cancelled，不提交完整 assistant，不发送成功 done。
- 完成先提交：完整 assistant 与 completed 记录一起保存；后到的停止返回 false，不把成功改成取消。
- 完成后的 Channel 接收失败：业务记录仍是 completed。重新读取相同请求获取已有结果，不重新调用模型。
- 生成期间用户消息、约束、任务或检查点改变：旧回复不得覆盖新版本，轮次记为 interrupted / AGENT_CONTEXT_CHANGED，保留已有原始记录。

模型必须返回 `finish_reason=stop` 和 `[DONE]`。EOF、错误 JSON、格式/usage 不合法、过滤、意外工具调用均不是成功。整个请求总超时包含读取和增量确认等待；HTTP 错误正文和第三方网络诊断不透传，避免回显提示词/密钥。

## 取消与异常退出

新的 stop_chat 请求带 `{ agentId, userId, sessionId, requestId }`。旧 requestId 的迟到停止不会取消下一轮。未提供 requestId 的旧接口仍表示“停止当前轮次”，新前端不使用这种宽泛停止方式。

停止首先检查用户与 Agent 归属，再取消对应的准备/生成令牌。已建立轮次时写入取消标记；不同 Manager 的生成者通过心跳观察标记并停止本地请求。true 表示取消信号或标记已发出，不保证供应商已停止计算/计费。

每个活动轮次每秒续租，租约为 30 秒。异常退出留下的 running 轮次在租约过期后，由下一次历史读取、收据读取或请求准入识别为 interrupted；不会立即抢占新鲜租约，也不会自动重发旧请求。重启后尚在租约宽限内的记录显示 running，最多等待剩余 30 秒后重新读取。

普通取消/错误保存当前已接收的部分文本。future 被丢弃时停止心跳、取消网络并安排中断记录写入；进程硬崩溃时租约负责最终恢复。部分文本在首个增量、累计新增达到 1024 字节、或后续增量检查到距上次保存超过 250ms 时保存；硬崩溃可能丢失尚未持久化的一小段。部分文本始终明确标记，不能冒充完整 assistant。

## 历史接口与前端隔离

| GET endpoint | 参数 | 返回 |
| --- | --- | --- |
| `agent/session_list` | agentId、userId、after（上一页最后 sessionId）、limit≤20 | Session[]，创建时间和 ID 稳定排序 |
| `agent/chat_history` | agentId、userId、sessionId、after（轮次 cursor）、limit≤20 | session、turns、hasMore、nextAfter |
| `agent/events` | agentId、userId、sessionId、after（原始 eventId）、limit≤50 | 原始事件，包括 B2 与内核记录 |

前端提供 `agentApi.sessions/history/events`。turns 包含 requestId、message、target、state、partialText、resultText、errorCode、usage、cursor 和毫秒时间戳。history 是当次状态快照，观察一个 running 轮次需重新读取包含该轮次的页；after 游标用于分页，不代表状态更新订阅。

进入下一次模型请求时只使用已完整提交的对话，失败/中断输入及部分文本仍保留为证据但不当作已完成历史。任务目标与用户约束每次重新注入；不静默裁掉完整历史来绕过预算。C2 已支持从已验证检查点和之后的完整轮次继续，自动整理较早前缀；原始历史和失败部分文本保持可查。准备阶段摘要与停止沿用同一 requestId 注册，只有摘要通过较低水位和版本/取消检查后才能提交和继续普通入轮次。最新里程碑见 AGENT_PROGRESS.md。

旧 B2 事件没有独立轮次，可通过 events 回查；不会自动伪造对应的 completed 轮次。B4 会将原生轮次历史接入可见面板，旧客户端显示缓存仍保留，不用缓存文本决定模型是否成功。

Channel 帧携带 requestId。前端校验关联及序号，重复帧不重复显示，缺帧/错误关联会取消并报错；调用结束、停止或切换会话后，迟到回调不会继续更新旧消息。Rust 结构化 text/done/error 直接保留原文，形如 {"content":...} 的 JSON 回复和代码不再二次解码；旧协议/工具事件继续沿用兼容解析。原生订阅 guard 在 future 被丢弃时移除自己的注册与取消记录，不清理其他会话。

## 针对性验证

Rust 目录：

```powershell
cargo test --offline --lib agent::
cargo test --offline --test agent_chat --test agent_models --test agent_context
cargo clippy --offline --lib --test agent_chat -- -D warnings
```

客户端目录：

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1
node --import tsx --test tests/agent-ipc.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts
node node_modules/typescript/bin/tsc --noEmit
```

本轮通过：6 项轮次/生命周期单元测试、15 项流式对话测试、18 项模型和 17 项内核回归，共 56 项 Rust 测试；原生 IPC 9 项、前端 32 项及 TypeScript 检查通过。测试覆盖事务排序、迟到停止、请求重放、跨 Manager/会话隔离、SQLite 关闭重开、future 取消、首个增量前后超时、HTTP/协议错误与传输结束后原文重读。

租约恢复测试通过修改测试数据库的过期时间模拟崩溃时间流逝，再实际关闭重开 SQLite；没有等待 30 秒，也没有抢占新鲜租约。所有模型请求是本地回环模拟服务，没有使用真实 API Key、用户服务器、完整 GUI 或安装包。
