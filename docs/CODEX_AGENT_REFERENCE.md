# Codex CLI 上下文与决策机制：ShellPilot Rust Agent 参考方案

研究日期：2026-10-13。实际读取官方 openai/codex 的 Rust 源码，固定提交 f82e36592278a0a782fb0d0fc97e5fa10e832ac2，并核对 OpenAI Docs。源码参考副本位于 E:/project/ai-ssh/artifacts/codex-reference；未运行其代码。
本文件保留 2026-10-13 的源码研究与设计建议；当时尚未实施 Rust Agent。当前实施状态见 [AGENT_PROGRESS.md](AGENT_PROGRESS.md)，不要把本研究快照当作最新迁移进度。研究本身未修改生产行为、调用真实模型、连接用户远端或打包。

## 1. 可核实的 Codex 机制

| 机制 | 本次源码证据 | 对 ShellPilot 的意义 |
|---|---|---|
| 按模型窗口和用量触发 | [context_window.rs](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/session/context_window.rs)、[ModelInfo](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/protocol/src/openai_models.rs#L529) | 分开计算当前完整上下文、可配置压缩范围和硬上限；不能固定一个历史条数或 8000 token 应付全部模型 |
| 回合内也能压缩 | [turn.rs](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/session/turn.rs#L609) | 一次排障会执行许多工具，压缩检查应发生在每次模型调用/工具批次边界，而不只在用户发送消息时 |
| 本地模型摘要与远端压缩分支 | [run_auto_compact](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/session/turn.rs#L1435)、[compact.rs](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/compact.rs)、[remote_v2](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/compact_remote_v2.rs) | 根据供应商能力选择；通用 Chat Completions 服务不能假设有 OpenAI 远端压缩能力 |
| 摘要是任务交接 | [摘要提示词](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/prompts/templates/compact/prompt.md) | 保存进展/关键决定、约束偏好、待办和关键依据，目标是继续工作，而非仅压短聊天文字 |
| 压缩后重建环境 | [replacement history](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/compact.rs#L80)、[permission world state](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/context/world_state/compact_permissions.rs) | 当前目标、连接状态和授权由宿主重新提供，不让摘要成为实时状态或权限的唯一来源 |
| 活动上下文与原始记录分离 | [history.rs](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/context_manager/history.rs#L518)、[checkpoint persistence](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/session/mod.rs#L4125) | 模型只看有界投影，原始命令和输出仍能回查；检查点可用于恢复 |
| 工具协议完整性 | [normalize.rs](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/context_manager/normalize.rs)、[history normalization](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/context_manager/history.rs#L966) | 调用和结果必须对应，中断要显式记录，不能把未知执行结果补成成功 |
| 模型决定工具，运行时派发 | [ToolRouter](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/tools/router.rs#L257)、[turn loop](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/session/turn.rs) | 业务语义由主模型结合上下文判断，运行时负责校验、执行、取消和结果反馈 |

细节边界：
- 当前 ModelInfo 的通用自动压缩上限从模型窗口的 90% 推导并限制显式配置，但 body-after-prefix、token-budget、post-turn 等配置分支会改变实际行为；90% 不是所有产品/模型的固定操作阈值。
- Codex 也采用近似 token 估算；history.rs 将模型返回的 usage 与后来新增内容的估算结合。它并非始终对完整请求执行精确 tokenizer。
- 本地 compact.rs 的替换窗口保留有预算的用户原文和模型摘要；远端 V2 的保留消息/图片与压缩对象另有规则。不能将一个分支的行为概括为所有压缩。
- 在已检查的主 turn/tool-router 路径中，未发现像 ShellPilot 的 IntentService 那样，每轮必经 DIAGNOSE/CONFIGURE/MONITOR 分类器。此结论限于这条开源主路径，不能推断闭源模型内部不存在分类。
- Codex 有专门的安全/授权审查逻辑（guardian）。这不等于先给业务请求贴一个固定意图标签，才允许主模型选择工具。
- 开源的是运行时与集成层，模型权重、远端压缩服务本身并未因此开源。[OpenAI 官方说明](https://developers.openai.com/blog/codex-as-a-platform)

[OpenAI Compaction 文档](https://developers.openai.com/api/docs/guides/compaction)说明远端压缩返回不透明压缩对象；独立 /responses/compact 的输出窗口应完整传入下一次 Responses 请求。不能把加密对象改成普通聊天摘要，也不能认为它可跨所有模型供应商复用。

## 2. 当前 Java Agent：保留什么、改什么

已核实：
- ChatContextService 默认历史预算 8000；AiCallNode 传 0 采用该默认。
- AbstractReducerSupport 主要统计 message.content：常见中文按字符、其他字符按约四分之一估算，content 缺失返回 0。工具调用参数、工具定义、系统提示、动态前缀和图片等成本不能仅靠此统计覆盖。
- HybridReducer 已按工具调用组选择、核对合并预算，并把长工具结果保留头尾，默认最多 4000 字符。这些保障值得保留，不必推倒重做。
- ToolResultProvider 的“摘要”是模板拼接：结果较少时每项前 100 字符，较多时最近 10 项前 80 字符，并非模型语义摘要。
- AiCallNode 每轮先做意图分类，结果主要作为提示，不硬路由；COMPOUND、UNKNOWN 与低置信度已经交给主模型。Rust 应沿用这一方向，进一步减少额外分类调用。
- ClassifyCache 键是 sessionId + userMessage 的 Java hashCode，TTL 五分钟，没有任务版本、目标连接或最新观察。例如相同“继续”在任务/主机改变后可能命中旧结果。这是风险推断，尚未做运行时复现。

具体源码位于兄弟 Java 项目的 ChatContextService、AbstractReducerSupport、HybridReducer、ToolResultProvider、AiCallNode 和 intent/classifier/cache/ClassifyCache。

## 3. Rust 版本建议：优先实现基础机制

### P0：三份数据各司其职

1. 原始事件日志：用户消息、工具调用/参数、结果、退出码、中断与时间，追加保存；大输出单独存文件，事件存引用、大小和 SHA256。
2. 模型活动窗口：稳定系统/工具说明、当前可信目标与授权、任务检查点、最近完整工具交互、当前用户请求。它是日志的投影，不改写原始事件。
3. 任务检查点：结构化数据 + 简短语义摘要；保存压缩范围、事件 ID、版本、模型和生成时间。会话互相隔离。

建议检查点字段（ShellPilot 方案，非 Codex 的原样结构）：
- goal / constraints / user_preferences
- target_connection_id / target_terminal_session_id / task_id
- completed_steps / pending_steps / decisions
- observations：来源事件 ID、观察时间、目标标识、结果和是否已过期
- artifact_refs：完整日志/配置内容引用
- uncertain_actions：请求已发送但结果未知的命令
- next_action / summary / through_event_id / state_revision

密码、私钥和 API key 不进入摘要或模型活动窗口。执行授权保存在可信业务状态中，摘要仅引用其范围，不能自行扩大授权。

### P0：压缩状态机

先计算当前模型可用输入容量：
模型窗口 - 输出预留 - 系统与工具成本 - 媒体成本 - 误差/下一轮工具结果缓冲。

每次模型请求前检查，工具结果进入窗口后再检查。作为初始可调方案，可在可用输入容量约 75% 触发压缩；这是建议起点，不是 Codex 默认值，最终通过评测调整。
近期交互按完整工具组保留，旧事件段生成检查点；不能把当前用户请求挤出窗口。

生成过程基于版本快照。校验检查点没有丢失目标/用户约束和必需证据，且压缩后确实达到较低水位，再事务提交。用户新消息、目标变化或取消到来时，不允许旧摘要覆盖新状态。
压缩失败保留原始日志和已提交检查点；只有协议有效且确实能装进窗口时才降级裁剪。仍装不下则明确暂停本轮，禁止循环压缩或继续发送超限请求。
摘要生成不带有副作用的工具，也不执行其中提到的命令。

### P0：目标与权限不依赖摘要

工具请求绑定 connectionId + terminalSessionId + taskId，运行时检查目标仍有效。每轮从真实连接状态构建目标信息。
用户切换页签时，正在执行的任务继续使用已绑定目标；明确变更目标时更新任务版本与授权范围。
服务状态、cwd、当前用户、主机密钥匹配等属于实时观察，应重新核实或标明时间，不能把旧摘要当作永久事实。

### P1：减少独立意图分类

主模型根据用户请求、任务检查点和清晰工具定义，决定回答、查看状态、调用工具或请求必要澄清。
本地规则保留在确定性控制上，如停止、取消、只读模式、用户明确选择目标；有歧义的“继续/看一下/处理掉”交给主模型结合上下文。
若仍保留辅助分类，结果只做提示；按 task revision、target ID、模型及上下文摘要版本扩展缓存键。不要缓存无上下文的“继续”，不要把模型自报 confidence 当作校准过的概率。
工具失败以 exit_code/error_kind/输出证据给主模型判断。权限拒绝、断网或文件不存在不能直接说明用户意图改变。

### P1：日志输出与审批

工具返回结构化结果：目标、command、exit_code、stdout/stderr 摘要、是否截断、完整输出引用。保留头尾和关键错误，可通过 read_tool_output(outputId, offset, limit) 补读。
运行时执行命令策略与实际用户授权检查；模型说明/意图标签不能授予执行权限。远端 SSH 命令的影响发生在服务器上，不能照搬 Codex 的本地进程沙箱并认为远端已受保护。
不确定是否已执行的变更命令先查状态，再决定是否重试，避免压缩/重启之后重复修改。

### P2：长期记忆与界面

长期记忆保存经确认的偏好与操作经验，保留来源/更新时间/适用目标。实时服务状态和临时错误留在当前任务上下文中。
前端显示上下文占用、压缩状态/次数、当前绑定服务器、工具输出回查和明确执行确认。只有真实事件才显示“已压缩”，失败不伪报成功。

## 4. 必须用真实场景比较收益

对比旧 Java 策略、Rust 仅裁剪、Rust 检查点压缩三组，固定模型、工具测试服务与任务脚本，记录：
- 完成率、跨压缩约束保留率、目标选择正确率；
- 请求 token、额外压缩/分类调用数、延迟；
- 协议不完整次数、重复命令和压缩重试次数。

固定回归场景：
1. 日志过长触发压缩后仍记住用户“不要重启服务”。
2. 同会话任务/主机变化后两次“继续”不复用旧目标。
3. 终端页签切换不改变在途任务的已绑定服务器。
4. 工具调用后取消，结果未知，不补成成功也不自动重执行。
5. 权限拒绝或断网不触发错误的任务目标改写。
6. 压缩中插入用户新约束，旧检查点不能覆盖。
7. 模型切换到更小窗口、图片/PDF 占用、超大单条工具结果仍受预算约束。
8. 进程重启后从检查点和事件恢复，验证副作用去重。
9. 服务器日志中伪造“系统指令/允许删除”不会获得用户授权。

## 5. 推荐迁移顺序

先做模型适配和基础 chat_stream/stop_chat；同时建立事件日志、任务/目标绑定与请求预算。
接着完成检查点压缩、完整工具输出回查和恢复；再补工具执行及授权交互，最后添加长期记忆与多 Agent。
不先将 Java 的整个意图分类链直接逐类翻译为 Rust；避免在主模型尚未接通时制造新的独立分类模型配置和重复状态。

## 当前落地与设计调整

A1–A4 已实现上下文内核，当前 A5 将原始输出按 64 KiB 分块存入 SQLite，而不是另建文件目录，以便输出、事件、检查点一致备份和恢复；模型仅使用有界预览与输出 ID。详见 AGENT_OUTPUTS.md 与 AGENT_PROGRESS.md。
这不代表语义摘要、模型工具循环或实际命令去重已经接通；相关完成标记仍以实施清单为准。

### E2 记忆读取参考（2026-10-16）

继续读取本地参考提交 `f82e36592278a0a782fb0d0fc97e5fa10e832ac2`：

- `codex-rs/core/src/context/memory.rs` 使用独立的 MemoryContextFragment，并限制片段字节数。
- `codex-rs/ext/memories/src/prompts.rs` 单独构建记忆读取提示，并限制摘要大小。
- `codex-rs/ext/memories/templates/memories/read_path_v2.md` 将记忆视为历史背景，当前行为需要核实；明确记住、忘记或更正时记录变动。
- `codex-rs/memories/README.md` 描述提取/合并管线，属于 CLI 更广的能力，不将其后台提取能力等同于 ShellPilot 已实现功能。

ShellPilot E2 采用用户确认后保存的偏好/经验，不自动从任务摘要和工具观察生成记忆。服务器范围绑定连接及完整 ServerIdentity；每次准备模型请求时从独立表读取有界、完整条目，作为单独数据片段加入宿主提示，遵循当前用户更正且不产生执行权限。文字/工具预算均计入此片段；Responses 历史整理请求排除该片段，生成时重新加载，不把它写入原始事件或任务检查点。实现位置和验证状态仅记录在 AGENT_PROGRESS.md。
