# Rust SSH Agent 实施清单与续接入口

## 当前状态总览（2026-10-21，以本节覆盖下方历史阶段描述）

### 运行中编辑配置与选择下一轮模型（2026-10-21，计划随 v0.2.4 发布）

- 前端将实际会话模型与下一轮选择分离；运行中可以选择模型、编辑渠道及推理强度，本轮请求、工具、审批和停止继续使用原模型与配置。原轮次收尾后按会话切换，保留 session、历史、草稿/队列、项目、SSH 身份与审批边界；后台本地会话同样提交，其他会话不受影响。
- 连续选择以最后一次为准，可取消或恢复原模型；失败显示行内提示并暂停新消息，支持明确重试。切换落库但回执丢失时核对实际归属，重试和取消均不重复发送轮次或工具。客户端卸载/StrictMode 清理不会让旧异步结果恢复状态；所有尚未确认的模型保存阻止安装更新。运行中的渠道保留删除保护，MCP/Skills 运行期限制不变。
- 原生 refresh_model_binding 复用切换事务与 CAS/忙碌保护；同一配置的模型 ID、协议、地址或凭据变化后，下次新请求重建可移植历史，旧思考/签名/密文不跨绑定。普通文字检查点保留，远端检查点解除引用后恢复原始历史。运行中的固定配置与自动整理不被新配置覆盖，无 schema 或版本修改。
- 验证：前端 199 项定向测试、TypeScript 和 Vite 生产构建通过；Rust Responses 25 项、工具续聊 12 项、摘要 21 项、模型切换单元 2 项及生产库严格 Clippy 通过，合计 259 项。新增覆盖生成中选择、后台提交、切换失败/取消、丢失回执、历史加载与切换竞争、配置编辑后跨协议及凭据重建、自动整理中固定配置。仅使用临时数据库和回环服务，未调用真实供应商或验收实际桌面视觉。

### Grok / xAI 独立供应商（2026-10-21，计划随 v0.2.4 发布）

- 新增 SupplierKind::Grok 与 supplier/grok.rs，支持 Responses（前端默认）及 Chat Completions；不开放 xAI 已弃用的 Messages。使用 Bearer，目录复用同一代理前缀的 models。
- 默认省略推理参数，可选 low/medium/high/xhigh；分别使用 reasoning.effort / reasoning_effort，不发送 thinking 开关，不根据模型 ID 猜测能力。Chat 固定 max_completion_tokens，Responses 固定 max_output_tokens；Temperature 可选 0–2。
- Responses 原样保留加密推理、工具调用及原生窗口；供应商/模型/协议/端点/凭据绑定继续隔离，诊断与可见输出不泄露思考。支持显式开启远端 compact，Chat 沿用本地语义摘要。
- xAI 未公开 OpenAI 的 responses/input_tokens 接口，供应商策略选择本地输入估算，覆盖完整 input、instructions、工具与图片，状态标记 estimated；其他 Responses 仍使用 providerCount。加密内容的本地估算偏保守，不承诺精确 token 数。
- 前端加入 Grok 图标、配置/筛选/多模型选择/推理目录；现有地址、密钥、多模型和置顶流程不变。更新配置文档并补齐原生 IPC、策略、目录、工具续聊与上下文整理测试。
- 不访问真实上游、SSH 或正式工作区；版本与 schema 不变，本轮不打包或发布。真实接口及桌面视觉尚未验收。
- 验证：前端 82 项、Rust provider 45 项/目录 19 项/续聊 12 项/摘要 21 项/Responses 24 项、桌面 Grok IPC 1 项，共 204 项定向测试通过。Grok 续聊内部覆盖两协议、JSON/SSE 及五种强度设置共 20 个场景；Responses 验证无 input_tokens 请求、本地估算来源、远端整理、历史保留与整理后续聊。TypeScript、Vite 生产构建和 Rust 生产库严格 Clippy 通过。

### Kimi / Moonshot 独立供应商（计划随 v0.2.4 发布）

- SupplierKind::Kimi 与 supplier/kimi.rs 提供独立策略，复用 Chat Completions / Messages adapter；不根据模型 ID 或域名推断供应商，不开放 Responses 或 Kimi Code 订阅专用接口。
- SupplierPolicy.headers 默认保持原协议鉴权，Kimi 覆盖为两协议 Bearer，目录同样使用 Bearer。Kimi Messages 端点为 /anthropic/v1/messages，目录仅映射完整已知后缀至同 origin / 代理前缀下的 /v1/models；其他自定义地址保留。
- 使用 max_tokens，Temperature 使用上游默认并原生拒绝显式值。Chat 默认不发送，none/enabled 只控制 thinking.type，low/high/max 只发送 reasoning_effort；Messages 仅支持默认或 output_config.effort 的 low/high/max。无虚构思考预算、keep 参数或自动降档重试，模型实际能力由上游验证。
- 保留 reasoning_content、Messages 思考块/原始签名及工具调用顺序；允许 Kimi 返回无签名块，通用 Anthropic 规则不变。供应商、协议、端点、模型与凭据绑定隔离沿用原生续接实现；思考不进入可见输出与日志。两协议沿用本地语义摘要。
- 前端描述目录新增 Kimi、本地 MIT 图标和协议级 overrides；多模型、搜索/供应商筛选、置顶、输入框推理、查询/取消/已存密钥继续复用组件。Kimi 温度控件禁用，跨供应商切换清除温度，Chat 切换至 Messages 清除不适用的开关。
- 本轮仅测试临时数据库、合成密钥与回环服务，不读取或修改本机正式工作区、不调用真实上游或 SSH；当前版本号仍为 0.2.3，schema 24，无新增迁移，不打包或发布。测试结果见下方补充。
- 验证：前端模型/供应商/设置/选择器/推理/查询 78 项，Rust provider 单元 42 项、模型目录 18 项、工具续聊 11 项、摘要 20 项，桌面 Kimi IPC 1 项通过，合计 170 项定向测试。Kimi 工具续聊覆盖 Chat 默认/none/enabled/low/high/max 与 Messages 默认/low/high/max，分别走 JSON 与分片 SSE，共 20 个场景；校验 Bearer、思考原文回放、工具执行一次及日志隔离。TypeScript、Vite 构建、生产库严格 Clippy 通过。真实 Moonshot/中转与桌面截图未验收。

### 待随 v0.2.4 发布：单条模型配置异常隔离

- 新增 StoredConfig 错误区分“已存配置不可解析/不受支持/参数无效”和真正数据库故障。ModelRepository.list 仅跳过前者，SQL/锁/读取错误继续返回；正常模型继续出现在 Agent 选择列表，异常模型按 ID 直接访问时明确拒绝。
- 新增 channel_report 返回 items / issues，经 agent/model_channel_report IPC 提供给模型设置；每条异常只返回渠道 ID、名称及固定安全提示，不回显原始 JSON 或密钥。读取过程不写数据库，不改变 revision、原始配置或成员关系。
- 渠道检查所有活动成员，任一成员异常时暂停整个渠道编辑，避免保存部分成员误删异常配置。同渠道其余健康模型仍可用于对话；其他渠道照常编辑。异常项恢复后刷新即可消除提示；全部异常不显示“还没有模型配置”。
- 前端设置/IPC 68 项、Rust 模型渠道 8 项、桌面原生 IPC 1 项通过；生产库严格 Clippy、TypeScript 与 Vite 构建通过。桌面 IPC 覆盖报告结构、原渠道列表兼容及用户隔离，Rust 覆盖未知供应商/字段、损坏 JSON、非法参数、非首成员异常、原记录不变与真实 SQL 故障不被吞掉。测试使用临时数据与本机模拟接口，不修改用户正式工作区。没有修改已安装的 v0.2.2 程序，不承诺旧二进制可读取新配置；当前应用版本号及 schema 未变更。
- 补充回归：模型存储 agent_models 22 项、模型目录 agent_model_catalog 17 项通过；本轮合计 116 项定向测试通过，未运行全量供应商/桌面测试或进行截图验收。

- 当前版本 v0.2.2，数据库 schema 24；前端 `master`，Rust `main`。A/B/C、D1–D5、E1–E3、E5/E6 已完成；E4 用户取消，不做旧 Java 数据导入。旧章节中的 test-rust-ui、schema 19/21、“下一项 E1/E2”和当时未提交状态仅为历史记录。
- 本轮 Qwen 接入前本地基线：前端 `master/185baf0`，Rust `main/95291f7`，GLM 接入已在两仓库提交。本轮未重新 fetch，不推断远端状态；下方 328742e / 0c05773 及当时领先/落后 0/0 为前次同步快照。支持多模型渠道、筛选/搜索/置顶/创建顺序、会话内模型切换、失败原因持久保存与恢复；数据库 schema 22/23/24 分别用于原生续接、失败/切换、多模型渠道。
- 已接入 Chat Completions、Responses、Anthropic Messages、Google Gemini GenerateContent；四者支持对话和工具续聊（Google 完成本地定向验收，真实供应商待配置后联调）。Responses 使用其原生计数/远端整理能力，其他协议使用本地语义摘要；推理/签名原样续接并按协议/模型/接口/凭据隔离。此前“Messages 仅常规模式”“Responses 不支持工具”“Gemini 仅兼容入口”属于旧阶段边界，Google 原生接入进度见下项。
- 日志当前为 `logs/agent/YYYY-MM-DD/<traceId>.log`（逐行 JSON），默认 INFO / 7 天，级别和保留天数可即时修改；包含用户与助手正文。旧 .json/.jsonl 不批量改名，仅兼容过期清理。Windows 调试发送栈溢出已用 Box::pin 隔离状态机并完成小栈回归。
- 既有阶段验证记录分别见对应最新功能文档，历史 E6 验收不能替代新供应商的真实接口/桌面验收。v0.2.2 升级与发布说明已准备；签名安装包、覆盖安装和在线更新须以实际产物独立验收，当前不打包。
- 最新工作：模型输出回读改为通常 4 KiB、单页最大 16 KiB；省略 limit 时原生实际补 4096，超过上限的正整数限制到 16384，主任务/子任务共用规则，并补齐结果 JSON 和历史事件的封装额度。Qwen 接入已提交，真实供应商/桌面视觉仍待独立联调。详见下节。

### 模型输出回读默认 4 KiB、单页最大 16 KiB（2026-10-20）

- [x] `tool_session.rs` 将常规读取量与硬上限分为 DEFAULT_OUTPUT_PAGE_BYTES=4096 / MAX_OUTPUT_PAGE_BYTES=16384。模型提示通常使用 4096；缺省 limit 时在严格 JSON 解析后、schema 校验前实际补 4096。合法小页保留，过大正整数封顶 16384，零/负数/小数/null/字符串、非对象和其他非法参数继续拒绝，不改输出 ID 或 offset。schema 保持既有闭合必填参数约定，主任务与子任务复用同一原生归一化。
- [x] `ToolExecutor::result_byte_limit` 为可信宿主提供有硬上限的结果额度覆盖；仅 read_tool_output 使用 132 KiB，以容纳 16 KiB 原文、精确 bytesHex 和 JSON 转义，其他工具仍沿用 Limits::result_bytes（默认 8 KiB）。Child 转发宿主额度。ToolLoop 对任何覆盖继续强制 132 KiB 上限；ToolResult 事件上限为 192 KiB 容纳再次转义，其他事件仍为 64 KiB。
- [x] 输出回读仍限当前 user/session/task 的已关联输出，按 nextOffset/eof 分页；不执行 SSH、不额外请求命令批准、不占工具执行次数，但后续模型请求仍计入轮数。初次命令摘要每路约 384 字节、原始捕获每路 16 MiB、UI 每页 64 KiB 自动读取、上下文硬边界和原始证据保留机制不变；没有增加前台设置或数据库迁移。
- [x] **76 项相关测试通过**：output_argument_tests **2**（默认、上限、非法参数保留及 16 KiB 全零二进制在两次 JSON 转义后的额度校验），agent_context **19**，agent_outputs **17**，agent_tool_loop **14**，agent_tool_session **24**。新增工具循环测试覆盖大回读封装成功与不可信超大额度仍被硬上限拒绝；普通工具 8 KiB 限制测试保留。子任务集成覆盖缺省 4 KiB、16 KiB 请求、64 KiB 请求封顶及小页，逐字节核对二进制结果与 offset，无重复 SSH，免费回读与模型轮数分别计数。
- 集成测试初轮仅有原 512 字节截断断言不再成立，已调整为实际请求大小、连续 offset 和 EOF；新增 UTF-8 测试断言类型问题也已修正。最终相关测试全通过，生产库严格 Clippy `-D warnings` 与两个仓库 diff 检查通过；本轮前端只有使用文档修改，不重复执行前端构建。
- 已同步 E3_EXTENSIONS.md 及客户端 agent-model-settings.md，旧阶段 512 字节记录保留为历史。只用临时数据库、测试密钥和回环 SSH/模型服务；未连接用户服务器或真实供应商，未打包、提交或推送。版本仍为 0.2.2 / schema 24，需完全退出托盘后重启 npm run tauri dev 编译原生改动。

### Qwen / 通义千问独立供应商（2026-10-20，本地定向验收）

- [x] Rust `SupplierKind::Qwen` 与 `supplier/qwen.rs` 实现独立策略，复用 Chat / Messages adapter。固定 max_tokens、温度 `[0,2)`；默认不覆盖上游，none/新增 enabled 仅发送思考开关，low/medium/xhigh 按协议发送强度。enabled 在其他供应商上原生校验拒绝；不猜模型名称，不伪造 token 预算，不全局设置 preserve_thinking，不降档/自动重试。
- [x] `SupplierPolicy::prefers_streaming` 默认 false，Qwen true；完整回复入口通过 Box::pin 复用流式协议生命周期，覆盖仅支持 SSE 的模型、工具规划与本地语义摘要。取消、超时、体积、结束帧校验及审批边界保留。Messages 接受官方空签名思考并原样续接，通用 Anthropic 严格规则保留；供应商/协议/模型/地址/凭据绑定隔离，私有思考不进入回复或日志。
- [x] `catalog/qwen.rs` 将完整 `/compatible-mode/v1/`、`/apps/anthropic/v1/`、`/apps/anthropic/` 后缀映射到同 origin / 代理前缀下的 api/v1/models；两协议目录使用 Bearer，读取 output.models[].model/name，并按 page_no/page_size/total 分页。自定义普通前缀及 OpenAI data/id 列表保留；失败响应、畸形页与空洞分页不返回部分成功列表，仍遵守既有取消、超时/总量、日志与凭据边界。
- [x] 前端供应商描述目录、模型设置/选择器/筛选/输入框推理复用现有组件，新增本地紫色 Qwen SVG 并保留 MIT 许可。多模型、编辑恢复、已存 Key、代理和稳定模型 ID 保留；默认 Chat 为 https://dashscope.aliyuncs.com/compatible-mode/v1，Messages 为 https://dashscope.aliyuncs.com/apps/anthropic/v1。温度输入通过可选 step 配置支持 0.01、最高 1.99，其他供应商仍使用原步长。未接入 DashScope 原生生成协议或 Responses。
- [x] 前端定向 **81 项通过**，TypeScript 和 Vite 资源构建通过；覆盖供应商/双协议/默认地址、思考开关与档位隔离、渠道保存恢复/Key、并且筛选、图标、模型选择、发现取消与代理保留。
- [x] Rust 定向 **84 项通过**：provider **38**、目录 **17**、工具续聊 **10**、摘要 **19**。Qwen 续聊涵盖两协议 × 两种调用入口 × 默认/none/enabled/low/medium/xhigh 共 **24 个场景**，全部实际传输 SSE，验证原始思考/空签名回放、工具执行一次、私有状态不泄露；两协议摘要通过 SSE 收集且无工具、原历史不变。生产库严格 Clippy 通过。
- [x] 桌面原生 IPC **1 项通过**：qwen_supplier_ipc_preserves_channels_thinking_toggle_and_saved_key_catalog，真实 dispatch 保存两种协议的多模型渠道，DTO 保留 Qwen，enabled 与 xhigh 可保存，查询复用加密测试 Key，目录路径映射及 Bearer 正确，编辑保留成员 ID / Key。测试对桌面本包临时关闭调试符号，未改 Cargo 配置。
- 使用官方百炼文档、合成响应、回环服务和一次性测试数据；未访问用户真实百炼 Key 或 SSH，真实云端/中转和实际桌面视觉未验收。版本仍为 0.2.2 / schema 24；本轮未提交、推送、打包或发布。包含原生代码，需完全退出托盘后重新运行 npm run tauri dev。配置方法与官方来源见两仓库模型文档。

### GLM / 智谱独立供应商（2026-10-20，本地定向验收）

- [x] Rust `supplier.rs` 新增 `SupplierKind::Glm` 与独立 SupplierPolicy，复用 Chat / Messages adapter；保持独立身份持久化和续接绑定，不增加协议枚举或数据库迁移。输出 max_tokens、温度 0–1；默认不覆盖思考参数，显式 none/low/high/max 按协议编码；不猜模型名称，不自动降档或重试。GLM Messages 允许无签名 thinking，原始思考/签名只进入原生续接，不进入可见正文和诊断日志，通用 Anthropic 严格规则保留。
- [x] 前端 `models.ts` / `modelProtocols.ts` / `ModelSupplierIcon.tsx` 增加 GLM / 智谱供应商和本地 Zhipu SVG；供应商特有默认地址、能力和档位通过描述目录覆盖。设置/筛选/模型菜单/当前模型/输入框推理共用元数据，支持多模型、编辑恢复、并且搜索、置顶和已存 Key 复用。Chat 默认 https://open.bigmodel.cn/api/paas/v4，Messages 默认 https://open.bigmodel.cn/api/anthropic/v1；显式代理地址保留，空草稿不回填。
- [x] 查询上游模型保留所选协议的前缀/鉴权/分页；GLM 未记录官方统一模型目录，不跨协议猜地址，不伪造模型列表。上游缺失目录时显示现有 Trace 错误，手动模型维护仍可用。本次独立入口仅开放 Chat / Messages；Responses 未接入此供应商策略。
- [x] 前端定向 **77 项通过**：model-protocols、model-compaction-ui、model-discovery、model-supplier-icon、model-picker、composer-reasoning、model-endpoints、model-channels；覆盖新建空地址、已存配置恢复/保存/密钥保留、供应商与名称并且筛选、图标、模型选择、思考和查询取消。TypeScript、Vite 资源构建通过。
- [x] Rust 定向 **74 项通过**：provider 单测 **32**（GLM 新增 3）、agent_model_catalog **15**、agent_provider_continuations **9**、agent_compact **18**。GLM 续聊 1 项包含两协议 × JSON/SSE × 默认/none/low/high/max 共 **20 个场景**，每场景实际运行工具循环并校验思考原样回传、工具仅执行一次、日志与回复不含私有思考；摘要覆盖两协议、无工具和保留原日志。生产库严格 Clippy `-D warnings` 通过。
- [x] 桌面原生 IPC **1 项通过**：`glm_supplier_ipc_roundtrips_channels_choices_and_saved_key_discovery` 经真实 dispatch 保存两种协议的多模型渠道，DTO 带 GLM 身份，目录复用加密测试 Key，编辑保留成员 ID/Key，代理前缀及 Bearer/x-api-key 正确。临时对本包测试关闭调试符号以降低编译内存，不改 Cargo 配置。
- 接口依据见两仓库 MODEL_PROTOCOLS / agent-model-settings 文档链接。本轮初次单测发现测试夹具缺少 outputTokenParameter，已补齐；受限环境 DPAPI / 回环网络 / rustfmt / Clippy 配置访问受限后以同命令本机权限复验通过，未修改生产安全边界。仅使用合成响应、一次性数据库和测试 Key，未访问用户真实 GLM Key 或 SSH。
- 版本仍为 v0.2.2 / schema 24；源码与本进度更新未提交、推送、打包或发布。原有 icon.icns 本地修改保持字节一致；包含原生改动，需要完全退出托盘后重启 `npm run tauri dev` 编译。真实 GLM/中转请求与桌面视觉尚未验收，不能用本地模拟结果替代。

### 仓库同步与当前续接状态（2026-10-20）

- [x] 客户端从 `84754ff` 快进至 `328742e`，Rust 从 `1e0d8fa` 快进至 `0c05773`，各同步 4 个远端提交；未切换分支或创建合并提交。客户端原有 `src-tauri/icons/icon.icns` 本地修改先备份、同步后恢复并校验字节一致；同步后保留该修改，本次另更新本进度文档。
- [x] 核对新增能力：Google Gemini 原生 GenerateContent 与工具续聊；上游模型查询、搜索/逐个添加/全部添加及手动维护；独立模型目录 Trace 日志；DeepSeek 供应商策略支持 Chat Completions / Messages，保留供应商身份、思考配置和续接隔离。基础地址可留空解析官方默认值，也可选择已保存地址，Google 默认 /v1beta，其余当前默认 /v1。
- [x] 核对终端交互：常驻操作收进顶部“终端”菜单，按激活窗格确定目标；保留选区复制、粘贴和添加到对话入口。SSH 断线时可在当前终端按普通键手动重连，消费触发键并防止重复请求；连接等待提示随成功、失败、超时或取消关闭。菜单当前只显示字号与 Ctrl + 鼠标滚轮提示，不再提供增减/重置字号项。
- 版本仍为 v0.2.2 / schema 24。客户端标签 `v0.2.2` 指向 `ab26468`，当前 HEAD 比标签多 5 个提交；最新源码能力不等同于该标签安装包的能力。后续发布按实际选定的源码和版本打包，包级签名、覆盖安装与在线更新验收单独记录，不从标签推断 GitHub Release 状态。
- 验证沿用各功能原有记录：DeepSeek 前端 64、Rust 定向 45、原生 IPC 1；最新默认地址/模型查询/地址建议相关 55；终端菜单及断线重连分别有对应定向记录。以上是历史功能验证结果，本次仅核对 Git、代码与文档，未重新运行测试、类型检查、构建、真实供应商或 SSH 请求。
- 后续按用户需求继续优化；新供应商/中转真实接口、实际 WebView 视觉与新安装包升级尚需独立验收。A/B/C、D1–D5、E1–E3、E5/E6 的既有完成状态保留，E4 用户取消，不恢复旧 Java 数据导入任务。本次未提交、推送、打包或发布。

### Google 默认基础地址恢复 v1beta（2026-10-20）

- [x] 前端 master `api/modelProtocols.ts::modelSuppliers.google` 将默认提示恢复为 https://generativelanguage.googleapis.com/v1beta；空白地址保存、查询上游模型及建议菜单官方默认共用该目录。OpenAI / Anthropic / DeepSeek 默认值保持 /v1，已有非空 v1 / v1beta / 中转地址不改写；无需迁移已存配置。本节覆盖此前“全部统一 /v1”的 Google 部分。
- [x] 更新 `tests/model-protocols.test.ts`、`tests/model-compaction-ui.test.mjs` 的版本断言，包含显式 v1 保留验证；连同查询/地址建议相关回归 **55 项通过**。TypeScript 与两仓库 diff 检查通过；客户端 `docs/agent-model-settings.md` 和 `docs/MODEL_PROTOCOLS.md` 同步默认版本说明。未访问真实上游/SSH、未重新运行无 Rust 生产改动的原生测试，未打包或提交。

### 地址建议滚动条缩窄（2026-10-20）

- [x] 前端 master `components/ModelEndpointInput.css` 为地址建议菜单单独设置 3px 滚动条，较全局 6px 缩小一半，保留现有滑块颜色、圆角及滚动行为。仅局部 CSS 调整，两仓库 diff 检查通过；未重复运行逻辑测试或实际视觉验收，未打包或提交。

### 地址建议字重与浮层阴影（2026-10-20）

- [x] 按用户后续反馈，前端 master `components/ModelEndpointInput.css` 将建议列表文字加重到 500、已选地址 600；边框改用主题 line-strong，在既有主题菜单阴影上叠加柔和近层阴影，强化下拉框层次。自定义字体继续生效，本次字重以局部样式为准。仅样式修改，两仓库 diff 检查通过；未重复运行交互/类型/Rust 测试或实际视觉验收，未打包或提交。

### 地址建议列表字体优化（2026-10-20）

- [x] 前端 master `components/ModelEndpointInput.css` 移除地址文字写死的 12px Consolas/Courier New 等宽字体，改用应用 13px 正文字体，并跟随外观设置中的自定义字体/常规字重；分组标题调整为 12px，选项统一字体继承、36px 最小行高和选中浅色高亮。仅调整下拉列表排版，地址输入和选择逻辑保持。
- [x] 现有地址建议交互定向 **6 项通过**，两仓库 diff 检查通过；不新增纯样式断言，未重新运行无代码改动的类型/Rust/全量回归，未完成真实 WebView 视觉验收，未打包或提交。保留原有未提交修改。

### 模型基础地址建议 UI（2026-10-20，本地定向验收）

- [x] 前端 master `components/ModelEndpointInput.tsx/.css` 提供可编辑地址建议控件，`ModelSettings.tsx` 接入并关闭表单/字段原生自动补全，替代 WebView“保存的信息”弹层。区分官方默认与已保存地址，沿用主题、高亮、图标、选中勾、长地址省略、Portal 与视口避让；点击选择、方向键循环/回车、Esc/Tab/外部点击/滚动关闭，输入法组合不拦截。菜单内部滚动不影响表单；保存中禁用。
- [x] `state/modelEndpoints.ts` 从现有模型配置生成建议，不读取浏览器历史或另存未保存草稿；地址去重，最多 8 个已保存地址，输入筛选。排除无效及含用户名/密码/查询参数/片段的记录。选择官方默认清空字段，由已有 `resolvedModelBaseUrl` 解析，选择已保存地址使用原值；不自动改写用户输入或密钥。
- [x] `tests/model-endpoints.test.mjs` 新增 **6 项通过**，覆盖建议来源/去重/过滤/上限、留空默认与保存地址选择、原文输入、键盘/输入法、浮层位置和关闭/禁用；连同表单、发现、协议相关回归合计 **55 项通过**，TypeScript 与 diff 检查通过。客户端 `docs/agent-model-settings.md` 同步说明。
- 仅前端代码与测试/说明和本进度记录调整，保留已有 DeepSeek 与用户图标变更；Tabbit tabs/diagnose 启动器均无输出退出（exit 1），未完成真实浏览器/WebView 视觉与原生补全抑制验收。未访问真实上游/用户 SSH，未重新运行 Rust 回归、打包或提交推送。

### 模型基础地址留空默认值与 v1 统一（2026-10-20）

- [x] 前端 `api/modelProtocols.ts::resolvedModelBaseUrl` 作为保存/模型查询的唯一地址解析入口：空或仅空格使用当前供应商/协议的 placeholder，非空自定义地址优先且保留版本/代理前缀；不回填表单草稿。`ModelSettings.tsx` 移除空地址必填限制并在保存时解析，`ModelCatalogInput.tsx` 查询时复用解析；地址下方提示改为“留空使用默认地址，也可填写中转地址”。
- [x] OpenAI、Anthropic、Google、DeepSeek Chat 与 Messages 的默认提示全部统一到 /v1 结尾；DeepSeek 分别为 https://api.deepseek.com/v1 和 https://api.deepseek.com/anthropic/v1，Google 使用官方稳定版 /v1，已有 /v1beta 或自定义路径不改写。Google 路径提示改为按 API 版本填写，避免继续要求默认 /v1beta。
- [x] `model-protocols.test.ts`、`model-compaction-ui.test.mjs`、`model-discovery.test.mjs` 定向 **49 项通过**，覆盖各供应商/协议留空/空格默认、保存与查询一致、不回填、代理优先、查询取消与密钥开关；TypeScript 与两个仓库 diff 检查通过。客户端 `docs/agent-model-settings.md` 同步最新用法。
- 仅前端 master 代码、相关测试与说明/进度记录调整，无 Rust 生产代码修改；保留已有 DeepSeek 改动和用户图标变更。未访问真实上游或用户 SSH、未重新执行原生回归、构建安装包或提交推送。

### DeepSeek 独立供应商接入（2026-10-20，本地定向验收完成）

- [x] `provider/supplier.rs::SupplierPolicy` 将供应商身份与 wire 协议组合，`ModelConfig`/`CatalogRequest` 增加可选 supplier，DeepSeek 保留独立身份，旧 JSON 与原协议 ID 不变，无 schema 迁移；`chat.rs::AgentConfig`、请求/目录日志和续接 Binding 携带身份。默认 Chat Completions，可选 Messages；专属 max_tokens、思考开关/档位、目录策略和无签名思考块规则集中在策略，不复制 HTTP/SSE、工具循环或摘要路径。通用 Anthropic 签名与温度限制不放宽，DeepSeek 不开放当前 Responses 专用流程。
- [x] 前端 master `api/modelProtocols.ts` / `models.ts`、`ModelSettings.tsx`、`ModelSupplierIcon.tsx`、`ModelPicker.tsx`、`ComposerReasoningControl.tsx`、`modelChannels.ts` 接通供应商选择、双协议、保存/编辑/筛选、本地蓝色鲸鱼图标和聊天推理档位；模型查询参数包含 supplier，`ModelCatalogInput` 供应商变化时取消旧查询。官网地址仅 placeholder，用户地址保留，旧渠道不按域名或名称自动转换。
- [x] 后端定向 **45 项通过**：provider 单测 **29**（含 supplier 新增 **5**，覆盖旧配置、合法组合、双协议参数、保存重开/绑定隔离、签名保护及完整路径段映射）；`agent_model_catalog.rs` **14**（含新增 DeepSeek 双协议 Bearer/代理前缀/只读查询）；`agent_provider_continuations.rs::deepseek_dual_protocol_tools_streaming_and_thinking_roundtrip` **1**（两协议 × JSON/SSE × 默认/none/high/max 共 **16** 场景，含无签名思考原样续接、真实工具循环、日志不泄露思考）；`agent_compact.rs::deepseek_both_protocols_use_local_semantic_compaction_without_tools` **1**（两协议语义摘要、max_tokens、无工具、原日志保留）。生产库严格 Clippy（-D warnings）通过。
- [x] 客户端 `backend_ipc_tests.rs::deepseek_supplier_ipc_roundtrips_channels_choices_and_saved_key_discovery` **1 项通过**：两种协议经真实 IPC 保存多模型渠道，模型列表 DTO 返回 DeepSeek 身份，配置/列表不含测试 Key，编辑留空复用已保存加密 Key 查询目录，Messages 目录映射与 Bearer 头正确；仅回环服务器与一次性数据目录，没有用户 SSH。
- [x] 前端相关 **64 项通过**（供应商/表单/图标/选择/发现 **59** + 聊天推理 **5**），TypeScript / Vite 资源构建、new supplier 模块 Rustfmt 与两个仓库 diff 检查通过；未重新执行无生产改动的终端等无关测试。`docs/MODEL_PROTOCOLS.md` 和客户端 `docs/agent-model-settings.md` 已同步配置方法与验证边界。
- 本机原生编译遇到内存压力，最终串行完成；provider 库单测与桌面 IPC 单测仅对本包使用命令行 profile.test.package.<包名>.debug=0 降低内存，不改 Cargo 配置或停止用户应用。初轮旧压缩模拟服务仅理解 Chat 消息位置，已改为末条消息/内容块读取，最终两协议摘要验收通过。
- 本轮均为合成响应/测试 Key/回环服务；**未完成真实 DeepSeek/中转联调与实际桌面视觉验收**，未访问真实用户 Key 或 SSH、未全量回归、打包、提交或推送。前端 master / Rust main；包含原生实现，需重启 `npm run tauri dev` 重新编译后体验，不能仅刷新前端。

### 移除命令栏 Agent 单终端提示（2026-10-20）

- [x] 前端 master `TerminalCommandBar.tsx/.css` 移除“Agent 始终单终端”徽标与专用样式，保留命令目标选择、广播状态以及既有 Agent 终端绑定逻辑。更新 `tests/terminal-broadcast.test.mjs` 原提示用例，定向 **1 项通过**，覆盖 single/all/selected 三种模式下提示移除与广播徽标保留；diff 检查通过。只调整展示和进度记录，保留此前改动，未打包或提交。

### 终端菜单提示精简（2026-10-20，本地定向验收）

- [x] `RemoteTerminalView.tsx` 将粘贴主快捷键提示改为 Ctrl+V（macOS ⌘V），保持 Ctrl+Shift+V 原有兼容；删除放大/缩小/默认字号菜单动作与闲置 changeFontSize 函数。`TerminalMenu.tsx/.css` 字号区只展示当前字号和“Ctrl + 鼠标滚轮”提示，移除底部重复提示，沿用既有滚轮实现、字号约束及持久化/同步。
- [x] 更新 `tests/terminal-menu.test.mjs` 的平台粘贴提示与无字号命令断言，连同原复制/粘贴快捷键回归 **11 项通过**；TypeScript、diff 检查通过。`docs/operation-feedback.md` 同步说明；只改前端 master 及进度文档，不做真实 SSH/桌面联调或打包，保留此前菜单/断线重连改动。

### SSH 断线按任意键重新连接（2026-10-20，本地定向验收）

- [x] 前端 `RemoteTerminalView.tsx::handleTerminalKeyDown` 在当前可见、激活的断线 SSH 终端交互区消费普通按键并调用现有 `reconnectNow` 手动入口（含主动断开后保留页签）。进行中状态、同步 ref 互斥与 repeat 检查避免重复请求，按键不进入 xterm 输入队列，失败后下一次独立按键可重试；本地终端、仍存活的后端传输恢复、隐藏/非激活窗格、聊天与菜单控件不触发，保留复制/粘贴组合键、Tab/Escape 和 IME 组合行为。
- [x] `RemoteTerminalView.tsx` 调整初始化和状态变化聚焦：断线 SSH 仍可获得按键焦点，状态变化不抢聊天、其他窗格或终端控件焦点。断线原因后追加“在终端内按任意键重新连接”，重连中显示进度；`state/remoteTerminal.ts` 清理旧“点击重新连接”文案，`App.tsx::cancelTerminalReconnect` 同步取消后的引导。`docs/operation-feedback.md` 更新使用说明，既有顶部菜单改动完整保留。
- [x] `tests/terminal-reconnect-key.test.mjs` **新增 6 项通过**：真实手动入口/连按互斥、失败解锁重试、终端与目标范围、修饰键/快捷键/IME 隔离、断线与重连文案、断线焦点及外部输入保护；与复制快捷键/菜单/分屏/连接通知直接关联回归合计 **42 项通过**，TypeScript 与 diff 格式检查通过。分屏原焦点断言同步为允许断线 SSH 聚焦，未扩大到无关全量验证。
- 仅前端 master 代码与本进度记录修改，无 Rust 生产代码变更；未访问用户 SSH 或真实上游 API、未做实际桌面联调，未打包、提交或推送。

### 顶部终端菜单收纳（2026-10-20，本地定向验收）

- [x] 按用户明确选择，在应用顶部添加“终端”菜单，操作当前终端；前端 `Shell.tsx` / `TerminalMenu.tsx/.css` 提供当前终端名称、文本/字号/连接分组、禁用原因、快捷键提示、键盘导航及点击外部关闭，沿用浅色/深色主题变量。复制、粘贴、添加到对话、字号增减/默认、重连/取消与断开/关闭收纳到菜单，`RemoteTerminalView.tsx` 移除整排常驻工具栏，保留选区附近的添加入口与终端内剪贴板反馈/手动粘贴兜底框。
- [x] `state/terminalActions.ts` 与 `TerminalActionsContext.tsx` 独立管理当前终端动作，`main.tsx` 挂接稳定 Provider、`App.tsx` 传入会话标题；仅可见且激活窗格注册，无目标时显示空态。所有者校验阻止旧菜单调用新目标或旧窗格清理新目标，分屏切换后迟到剪贴板结果不再粘入旧窗格；终端运行时和 xterm 初始化不因菜单开关重建。原终端快捷键、无选区 Ctrl+C 与 Ctrl+滚轮不增加全局拦截，字号仍本地保存并同步现有终端。
- [x] `tests/terminal-menu.test.mjs` 新增 **6 项通过**：动作所有者/陈旧调用/禁用状态、真实注册配置与本地/SSH 差异、离线/重连/取消、字号限制/持久化/广播、异步粘贴焦点与运行时竞态、键盘循环/Escape/Tab。直接相关快捷键/选区/分屏/主题/顶部栏/对话文本回归 **41 项通过**，TypeScript 与 Vite 前端资源构建通过；构建仍有体积提示，不影响成功。`docs/operation-feedback.md` 同步使用说明，diff 格式检查通过。
- 仅前端 master 代码与本进度文档修改，没有 Rust 生产代码变更；Tabbit CLI 当前无可用浏览器连接（exit 69），未做实际桌面视觉验收，未访问用户 SSH/真实 API、未全量回归、未打包安装程序或提交推送。

### Google 模型目录空能力与空分页兼容（2026-10-20，本地定向验收）

- [x] 用户提供完整中转返回后明确批准这两条规则，覆盖上一节“仅日志、不兼容”的当时限制。Rust `provider/catalog.rs::Google::decode` 对 supportedGenerationMethods 缺失 / null / 空数组跳过当前条目，保留后面的 generateContent 有效条目；nextPageToken 缺失 / null / 空字符串作为没有下一页。有效条目的 models/ 前缀、ID 校验、非空错误字段类型校验、原生鉴权、URL 与分页/大小/取消限制不变，不猜测未知能力。
- [x] `provider/catalog/diagnostics.rs` 添加每页一次的 DEBUG `model.catalog.entries_skipped` 汇总缺失/空能力条目数量，不逐模型刷日志，不记录正文；原解析失败定位与 Trace 保留。`tests/agent_model_catalog.rs` 新增完整混合返回的 7 模型去重回归、空能力页仍按游标前进、空分页标记结束及错误标记拒绝；原日志错误夹具和客户端原生 IPC 负例改为错误字符串类型，继续验证真正的协议错误。
- [x] 最终源码的 `tests/agent_model_catalog.rs` 定向 **13 项通过**（原目录 / 日志回归 11 + 新兼容场景 2）：用户混合返回提取 7 个唯一模型、null / 缺失 / 空数组能力跳过、空能力页仍继续下一页、缺失 / null / 空字符串游标结束、错误类型与非对象模型记录仍拒绝、跳过汇总及原 Trace 脱敏正常；生产库严格 Clippy（-D warnings）、新增 Rustfmt / diff 格式检查通过。
- [x] 桌面 `cargo check --offline --tests -j1` 通过；只修改 Rust 查询解码、日志汇总、相关测试夹具和说明，不改前端交互或请求地址。验证曾等待开发应用构建锁，未停止用户进程；未再次运行无生产改动的桌面 IPC 运行时测试，未访问真实上游 / 用户 SSH、未全量回归或打包、未提交推送，保留既有未提交改动。

### 模型查询独立日志与按钮命名（2026-10-20，本地定向验收）

- [x] 按用户澄清，本轮仅增加查询日志，**不兼容中转返回格式、不改 Google 模型名称前缀或 supportedGenerationMethods 的解析要求**。`provider/catalog/diagnostics.rs` 抽离固定元数据日志；`catalog.rs` 接入 GET 地址 / 分页 / 鉴权方式、HTTP 状态 / 媒体类型、DEBUG 结构类型和 ERROR JSON 路径 / 预期 / 实际类型。成功记 INFO，取消 WARN，失败 ERROR；使用现有全局级别、日期目录和保留设置。分页游标脱敏，不记录密钥、原始请求头或响应正文。
- [x] 客户端 `backend_ipc.rs::dispatch_json_request` 为 agent/model_catalog 创建独立 Trace，覆盖凭据预检、所有分页及结束 / 失败 / 取消；复用 IPC requestId 写入 `<日期>/<traceId>.log`，失败 Toast 附带 Trace ID。其他 JSON 请求不增加日志，聊天既有 Trace 不变；Box::pin 隔离诊断状态机。
- [x] `ModelCatalogInput.tsx` 初始按钮改为“查询上游模型”。查询仍跟随当前“使用 API Key”开关，输入新值优先、编辑留空复用已存密钥，关闭不发送；保留原供应商鉴权头，不额外增加跨协议凭据或重试。
- [x] `tests/agent_model_catalog.rs` 定向 **11 项通过**（原目录回归 8 + 新日志契约 3），覆盖分页同 Trace、实际鉴权元数据、密钥 / 游标 / 正文不落盘、截图同类 null 字段和裸名称前缀的精确错误位置、INFO / WARN / ERROR 过滤与取消；确认仍拒绝原不接受的格式。前端查询行为 **9 项通过**，TypeScript、生产库严格 Clippy（-D warnings）、新增 Rustfmt / diff 检查通过。
- [x] 客户端原生 IPC 定向 **2 项通过**：原未保存草稿查询不写配置 / 404 反馈回归；新增 `model_catalog_json_request_logs_trace_and_exposes_id_for_failure` 核对真实 JSON 入口一次开始 / 一次失败、所有事件同 Trace、Toast 带定位 ID、字段错误路径准确、预取消记 WARN 且不误报完成，测试 Key / 响应内容不落盘。
- 本轮仅日志与按钮命名，不放宽响应解析、不改用户 API 地址或密钥配置；未发送真实 API 请求、未执行用户 SSH、未全量回归或打包、未提交推送。前端 master / Rust main；包含原生代码，需开发应用重新编译后生效，保留原有改动（含用户图标文件）。

### 上游模型发现与点击维护（2026-10-20，本地定向验收）

- [x] Rust `provider/catalog.rs` 定义独立目录适配接口，`ProviderAdapter::catalog` 挂接 OpenAI / Anthropic / Google 策略，Chat Completions 和 Responses 共用 OpenAI 目录。GET 基础地址 + models，保留版本与代理前缀；原生鉴权请求头复用，Anthropic 游标 / Google pageToken 分页，Google 仅保留 generateContent 并去除 models/ 前缀。HTTP 禁止重定向和自动重试，整个查询 30 秒 / 20 页 / 8 MiB / 10000 模型上限，重复游标报错，不返回部分查询假装完整。
- [x] `provider/config.rs` 提取已有基础 URL 校验；`repository/channels.rs::catalog_client` 使用草稿密钥或按用户和渠道修订读取已保存的加密 Key，关闭鉴权不解密/发送。查询独立于模型 ID / 参数是否填完，不保存草稿、创建渠道或修改历史；支持原始模型已被移除后的渠道根凭据。客户端 `backend_ipc.rs` 增加 agent/model_catalog 原生入口及 404 / 鉴权 / 超时提示，不把原始上游正文回传界面。
- [x] 客户端 master：`ModelCatalogInput.tsx/.css`、`modelSelection.ts`、`api/models.ts` 接入查询与重新查询、搜索、点击单个添加、已选标签 × 移除、全部添加与手动填写折叠入口，`ModelSettings.tsx` 传入当前草稿和已存渠道身份。保留原单渠道 64 模型上限，超限 Toast 且整批不修改，不截断或覆盖已选；更改供应商/地址/密钥/鉴权或关闭弹窗取消旧请求并丢弃迟到结果。
- [x] 前端定向共 **45 项通过**（新模型查询/选择行为 9、原设置/渠道/协议 36），TypeScript 与 Vite 资源构建通过（保留既有大资源块提示）；搜索回车不会误提交外层配置。Rust 新目录回环契约 **8 项通过**，共享基础 URL 校验回归 **1 项通过**，生产库严格 Clippy（-D warnings）、新增模块 Rustfmt / diff 格式检查通过。新测试树遍历器初轮错误已修正后通过。
- [x] 客户端 `src-tauri/src/backend_ipc_tests.rs::model_catalog_ipc_queries_unsaved_draft_without_persisting_and_reports_errors` 定向 **1 项通过**（原生测试模块编译通过）：未保存草稿查询返回可选模型、配置列表保持为空、404 提示手动填写、未知供应商拒绝。原生入口 Box::pin 隔离查询 Future，不增加聊天状态机的内联存储。
- 实际 GUI 视觉验收未完成：Tabbit tabs / diagnose 返回 exit 69。全部验证仅用临时库、模拟数据/Key 和回环接口，不调用真实模型 API 或用户 SSH，未全量回归、未打包、未提交或推送。前端保持 master、Rust main，包含原生变更，开发应用需重新编译后使用。

### 输入框当前模型使用 Google G（2026-10-20，最终图标范围）

- [x] 按最新截图，客户端 `ModelPicker.tsx` 输入框当前模型图标使用供应商默认展示（Google 四色 G）；只有下拉列表的具体 Gemini 模型行使用 model 展示（蓝色星形）。供应商分组与配置卡片保持四色 G。
- [x] 菜单定向 **5 项**及相关 diff 格式检查通过；同步客户端说明。仅前端展示，不改模型配置或协议；未全量回归、未打包或提交。

### Google 供应商 / Gemini 模型图标区分（2026-10-20，覆盖上一轮图标范围）

- [x] 按用户澄清，供应商分组与配置卡片保留四色 Google G，具体模型行及输入框当前模型恢复原蓝色 Gemini 星形。客户端 `ModelSupplierIcon.tsx` 新增 supplier/model 展示类型，`ModelPicker.tsx` 仅为模型位置指定 model，不影响供应商与其他协议。
- [x] 图标/模型菜单定向 **9 项**、TypeScript、相关 diff 格式检查通过；更新客户端说明。仅前端和文档，未全量回归、未 GUI 视觉验收、未打包或提交，保留其他改动。

### Google 供应商图标改为四色 G（2026-10-20）

- [x] 客户端 master：`src/components/ModelSupplierIcon.tsx` 用本地四色 G SVG 替换 Gemini 星形；模型渠道菜单与配置卡片等共用位置同步生效，保持尺寸、装饰性无障碍属性与原有主题容器，不增加依赖或图片网络请求。
- [x] 图标/菜单定向 **8 项通过**，TypeScript 与相关 diff 格式检查通过。首次旧断言固定 path 的属性顺序，改为按实际 SVG 属性匹配后通过；未全量回归、未实际桌面视觉验收、未打包或提交。仅前端和文档修改，保留已有开发改动。

### Google 首次中转请求 404 排查（2026-10-20，只读定位）

- [x] Trace `cafe7dc4-83eb-47d6-beae-95b00d2a9f46`：11:23:39，googleGemini / gemini-3.8-flash，api-direct.loveapi.top 在约 504ms 返回 HTTP 404，上游 requestId 为 `62fd1ceb-23d5-4987-bc44-16900014e33e`，未开始工具执行。错误日志未给出可用的供应商 code/type，不能仅据此断定模型不存在。
- [x] 只读查询该配置的非敏感字段（revision 1）：baseUrl 仍为 `https://api-direct.loveapi.top/v1/`，相对路径 models。当前代码实际构造 `/v1/models/gemini-3.8-flash:streamGenerateContent?alt=sse`；Google 官方原生端点前缀为 `/v1beta/models/...`。models 作为本程序相对路径是正确的，优先核对中转的原生基础地址是否应改为 /v1beta/，以及该中转是否开放对应模型与原生协议。
- [ ] 中转具体路由/模型可用性尚未证实，未将路径差异标记为唯一根因或已修复。未改用户配置、不读取/输出密钥、不发真实 API 请求；仅记录定位结果，没有产品代码改动或测试。

### SSH 连接等待提示按请求结果关闭（2026-10-20）

- [x] 根因：客户端 `src/App.tsx::selectHost` 使用固定 60 秒 Toast，成功/失败后新增通知但未移除等待通知；悬停还会延迟其计时。改为 `src/state/notifications.ts::beginNotice` 为每个操作返回独立关闭函数，`NotificationToast.tsx` 支持 durationMs=0 由操作收尾。成功、失败、取消和超时通过 finally 关闭，不受鼠标悬停影响；重复点击不新增第二条计时等待通知，同名并发请求也不共用通知 ID。
- [x] `src/state/sshConnectAttempt.ts::open` 使用连接配置 connectTimeout，缺省 10 秒（原生范围 1–300 秒），对 IPC/连接请求添加前端期限与取消竞速；到期取消原 requestId，立即释放连接中状态并显示服务器名及超时/失败原因。正常返回清理计时器与监听，用户取消保持 AbortError、不误报失败；迟到成功会话关闭且不注册页签，原 backendUrl/requestId 始终保持。首次和重连入口都传入各自连接配置的超时值。
- [x] `tests/ssh-connection-notice.test.mjs` 执行真实 App 函数、SshConnectAttempt 和 Toast 源码，**11 项通过**：成功、服务端失败、自定义/默认超时、取消、迟到清理、同名并发、取消后立即重试、手动关闭、持久/普通 Toast 计时及重复点击。`tests/toast-errors.test.mjs` **3 项通过**，TypeScript、相关 diff 格式检查通过。测试使用模拟 API 和虚拟时钟，无真实 SSH；未全量回归、未实际桌面视觉验收、未运行 Rust 测试或打包。
- [x] 更新客户端 `docs/operation-feedback.md`；前端改动保留在 master，本轮没有 Rust 生产代码修改。保留此前 Google 接入及用户已有 Cargo.toml / 图标等修改，未提交或推送。

### Google Gemini 原生协议接入（2026-10-20，本地定向验收）

- [x] Rust `src/agent/provider/google.rs` 新增独立 GoogleGemini 策略，`adapter.rs` 注册工厂和通用 endpoint / prepare_stream / finish_on_eof 钩子。共享 HTTP 层只调用策略，不向 Agent 主循环增加 Google 分支；x-goog-api-key、模型方法 URL、generationConfig、systemInstruction、contents、inlineData、functionDeclarations / functionResponse 均在适配器封装。共享超时、取消、大小/预算、权限和未知结果规则不变；无新依赖或数据库迁移，schema 24。
- [x] `continuation.rs` 保存原生 model parts，严格绑定协议/模型/接口/密钥；保留 thoughtSignature / thought 顺序，不泄露到可见回复或诊断。JSON / 跨字节 SSE、工具续聊、结束后 usageMetadata、STOP 与截断检查接通。`strict_json.rs` 从既有工具 schema 提取重复键拒绝逻辑，Google 解码和 `tool_loop/schema.rs` 复用，不在参数归一化前丢弃歧义。`protocol.rs` 图片预算支持 inlineData。
- [x] `repository/channels.rs` 用策略归一化后的模型 ID 做成员键和唯一检查，支持 models/xxx 前缀并规范为 xxx；同一渠道不重复保存别名，编辑、换序和重开保持配置 ID。凭据继续加密保存，旧供应商参数与旧配置不自动转换。
- [x] 前端 master：`src/api/models.ts` / `modelProtocols.ts` 新增 googleGemini 与 Google 兼容目录、地址占位提示及原生输出参数；`agent.ts` 引用统一协议类型。`ModelSupplierIcon.tsx` 本地 Gemini 标志用于配置卡片、供应商和模型菜单；`ModelSettings.tsx` 说明模型 URL 自动拼接。多模型渠道、筛选、模型切换、输入框强度沿用现有流程。默认不发送 thinkingConfig；显式 minimal/low/medium/high 映射 thinkingLevel，较早模型/不支持档位的端点保持默认，不自动重试或降档。
- [x] 前端定向 **48 项通过**：协议目录、供应商 SVG / Select、渠道保存/恢复/筛选、强度控件和宽窄模型菜单；TypeScript 与最终 Vite 资源构建通过（保留既有大资源块提示）。最终路径提示也由目录驱动，设置页没有新增 Google 供应商条件分支。
- [x] Rust 定向共 **46 项通过**：供应商单测 24（含 Google 7、Anthropic、OpenAI 工具流与原生绑定），Google 回环集成 5、多模型渠道 6、四协议原生续接 7、Anthropic HTTP/SSE 3、工具 schema 重复键回归 1。Google 覆盖 HTTP/SSE、429/大小/截断/超时/取消、共享加密 Key 重开后两模型的实际 URL/请求头、本地摘要失败不提交/成功提交/后续聊天；四协议切换覆盖 12 个方向的文字/工具共 24 个回环场景，原样签名回传且日志不泄露。
- [x] 最终桌面原生 `cargo check --offline --tests -j1`、生产库严格 Clippy (`-D warnings`)、新增模块 Rustfmt 与两仓库 diff 格式检查通过。初次前端断言仍按两供应商、Google 测试夹具方法写错，更新后通过；新增 Rust 图片夹具遗漏 detail / 渠道测试误读私有 RuntimeConfig.api_key 的编译错误已修正，凭据验证改为配置/解密成功及实际回环请求头，没有放开生产密钥访问。未将这些初轮错误记作通过。
- 本地测试仅用临时数据库、测试 Key、回环模型与既有测试执行器，不访问真实 Google/用户 SSH。按 Tabbit 技能尝试浏览器可用页面查询返回路由不可用，未标记实际 GUI 视觉验收完成。Vertex OAuth、Interactions/Live、Google 原生文件上传/托管工具不在本次范围；PDF 仍使用既有文字提取。同步 `MODEL_PROTOCOLS.md`、`CODEX_AGENT_REFERENCE.md` 的历史快照说明、客户端 README 和模型配置文档。未全量回归、未打包或提交推送；退出托盘旧进程并重新运行 npm run tauri dev 载入新 Rust 库。

### 请求日志后缀调整为 .log（2026-10-19）

- 按用户最新要求，新请求日志使用 `<traceId>.log`，仍逐行追加 JSON 对象。七天保留策略同时兼容 `.log`、旧 `.json` 和 `.jsonl`；既有文件不批量改名。同步两仓库日志路径测试和当前使用说明。

### 请求日志后缀与默认七天保留（2026-10-19）

- [x] 按用户要求新 Trace 文件后缀改为 `.json`，内容仍为逐行追加的 JSON 对象；同步生产路径、原生/后端测试和使用文档，不批量改名旧 `.jsonl`。
- [x] 桌面设置增加独立的日志保留时间，默认 7 天，旧 workspace.json 自动使用默认值；后端校验 1–3650 天，界面提供常用档位。保存后原子更新运行实例并立即清理，不重启 SSH，也不保存数据目录/附件设置的未提交草稿。
- [x] 启动时和保存日志设置后清理，运行期间每小时在阻塞工作线程中清理；停止工作区后终止周期任务。保留最近 N 个本地自然日，兼容过期 `.json` 与 `.jsonl` Trace 文件；只处理指定日志根目录内的合法日期子目录，跳过链接/junction、未知文件和嵌套目录，不递归清理，不影响数据库、附件和 backend.log。删除失败报告标准错误，不阻止聊天。
- [x] **19 项定向检查通过**：诊断/保留边界与未知文件保护 5、宿主设置 5、前端日志设置 6、原生正文落盘/回放/小栈 2、失败部分回复 1；前端 TypeScript 检查通过。删除检查只使用临时目录，未启动真实桌面或清理实际用户日志，未全量回归、未打包或提交推送。需重新编译并启动桌面进程。

### 显式 Trace ID 的对话正文日志（2026-10-19）

- [x] 按用户要求，新增同步 `Trace::conversation_message`：直接追加 user/assistant、connectionName、实际 modelId、state、message 与原 requestId/traceId，不为正文增加异步包装。正文写入现有日期/Trace JSONL 文件，普通对话连接名为 null；元数据诊断继续脱敏，配置密钥、系统提示词与原始工具输出不额外记录。
- [x] 普通和工具对话在新回合准入后记录用户文本，回复持久化后记录最终或部分助手文本，使用本轮模型名及接收时连接名称。流式增量不逐片记录，历史回放不重复写正文；失败/取消标记真实状态。日志不替代 SQLite，写文件失败不改变对话结果。INFO 正文按现有日志级别过滤，设置页明确说明正文在本机明文保存。
- [x] 定向 **12 项通过**：诊断 4、连接名/实际模型名 1、原生小栈/完整对话及正文回放 2、失败部分回复及错误脱敏 1、日志设置 UI 4。连接夹具初次因缺少 known_hosts 被拒，补充仅用于模拟连接的 strict_host_key_check=false 后通过；未修改生产校验。保留 1 MiB 线程栈与 Future 小于 32 KiB 的回归保护。
- 同步两仓库日志说明和客户端设置文案。仅使用临时库/回环模型，未调用真实模型或用户 SSH、未全量回归、未 GUI 验收或打包、未提交推送。需重启桌面开发进程重新编译后生效。

### 修复诊断包装后的 Windows 调试版发送栈溢出（2026-10-19）

- [x] 复现发送消息时的 `STATUS_STACK_OVERFLOW`：原生定向回归在默认测试线程栈上同样崩溃；仅为测量临时扩大测试线程栈后，确认 IPC 聊天 Future 为 798648 字节。异步诊断包装内联保存下游状态机，调试构建的临时移动进一步消耗栈空间。
- [x] 客户端 `backend_ipc.rs` 与 Rust 的聊天、桌面聊天、流式/工具流式/非流式模型入口使用 `Box::pin` 隔开状态机存储。保留原 Trace 作用域、审批、取消及错误传播；不增加模型请求，也不修改应用线程栈配置。
- [x] 定向验证 **5 项通过**：原生 Future 小于 32 KiB 的大小回归；显式 1 MiB 线程栈上通过真实 IPC 消息函数完成普通/工具模式两次流式对话，并核对各请求仅一条开始事件、模型完成及最终完成日志；诊断作用域传播、脱敏/级别、取消/中断 3 项。使用临时 SQLite 和回环模拟模型，不使用真实凭据、用户 SSH 或生产数据。
- 未全量回归、未运行实际桌面 GUI、未打包。两仓库均有原生源码修改，需要重新执行 `npm run tauri dev` 编译；未提交或推送，保留原有图标改动。

### 请求全链路本地诊断日志（2026-10-19）

- [x] Rust main：新增 `src/diagnostics.rs` 统一结构化日志与 task-local Trace，复用 requestId，按本地年月日 / traceId.jsonl 追加落盘。默认 INFO；支持 DEBUG / INFO / WARN / ERROR 原子级别切换。请求开始、正常完成、失败、取消和 future 中断分别记录，不把取消记成成功；并发请求隔离，统一阻塞任务入口传播 Trace。
- [x] `src/agent/chat.rs`、`chat/execution.rs`、`tool_loop.rs`、`approvals.rs`、`approvals/reviewer.rs`、`extensions/approval.rs`、`context_status.rs`、`compact.rs`、`compact/auto.rs`、`steering.rs` 接入发送、引导输入、模型轮次、自动/人工审批、工具执行/结果入库、上下文整理及请求结束。独立人工审批通过 `repository.rs` 的工具事件 / user_event_id 找回原请求 Trace，覆盖异步 IPC 决策；不逐 token 或逐轮询刷日志。
- [x] `src/agent/provider/diagnostics.rs` 统一模型计时、用量、结构与 modelCallId；`transport.rs`、`dispatch.rs`、`responses.rs` 在读取上游错误正文前保留 HTTP 状态与安全供应商请求 ID，随后仅保存限定错误类型/代码及分类。DEBUG 不保存原始消息、提示词、思考、命令参数、输出或密钥；原始聊天证据仍由既有会话历史查询。本项补齐后续排查信息，未声称已复现/修复此前 DeepSeek 报错根因。
- [x] 前端 master：`src-tauri/src/embedded_backend.rs` 持久化日志级别并实时更新运行实例；`backend_ipc.rs` 从桌面请求预检开始包裹同一 Trace。`src/components/RustBackendSettingsDialog.tsx` / `WorkspaceSettings.css` 在“本地工作区”新增诊断日志级别及目录入口；修改级别自动保存，不重启 SSH，不覆盖其他未保存草稿。`src/App.tsx` 失败 Toast 附带 Trace ID。`src-tauri/capabilities/default.json` 仅为专用日志目录增加打开权限。
- [x] 定向集成测试通过 **3 项**：`tests/agent_chat.rs` 模拟 HTTP 400 保留状态与分类、不泄露原文；`tests/agent_tool_loop.rs` 工具执行和后续模型调用共用 Trace；`tests/agent_approvals.rs` 独立人工批准回连原 Trace。均使用本地模拟服务/测试 SSH，无真实模型 API 或用户服务器请求。前端 `tests/diagnostic-settings-ui.test.mjs` **4 项通过**，覆盖即时保存/失败恢复/目录操作/权限；TypeScript、Vite 资源构建及 Rust library Clippy (`-D warnings`) 通过。核心日志单测此前 **3 项通过**；原生库类型检查此前通过。按用户最新要求仅更新文档并提供 commit note，停止尚未完成的核心单测重复复核和桌面配置定向单测编译，不计为通过；未影响用户开发进程。
- [x] 新增 `docs/REQUEST_DIAGNOSTICS.md`，同步客户端 `docs/operation-feedback.md` / `docs/agent-model-settings.md`。桌面日志位于应用本地目录 `logs/agent/YYYY-MM-DD/<traceId>.jsonl`，独立服务位于数据库目录下的同名子目录；跨午夜 Trace ID 不变。不自动上传或清理。未全量回归、未实际视觉验收、未提交推送或打包；保留已有改动（含非本项的图标文件）。新 Rust 代码首次使用需重新启动开发应用，此后仅切换级别即时生效。

### DeepSeek 最新服务器对话失败排查（2026-10-19，仅定位阶段，未修复）

- [x] 只读核对本地会话 `e83f53f1-a10e-4446-acd1-66c06480b4c0`：项目“凯平哥”、模型 `deepseek-flash`、Chat Completions。北京时间 17:10:47 开始，17:11:03 失败，保存错误为 `MODEL_HTTP_ERROR`；`ssh_execute` 已完成且退出码为 0，工具结果事件先于失败事件，失败发生于工具结果返回后的模型继续请求。
- [x] 核对 `src/agent/provider/dispatch.rs` / `transport.rs` 和 `src/agent/chat.rs`：非成功 HTTP 返回仅临时保留状态码，持久会话归并为 `MODEL_HTTP_ERROR`，原始响应状态码与错误正文未持久保存，现有日志不能进一步区分 400 / 429 / 5xx。最新模型配置修订为 5、更新时间 17:18:52，晚于报错；本地留存信息不能完整恢复当时配置。
- [x] 经用户分两次明确授权，使用当前已存 DeepSeek 凭据向原官方地址发起 3 次非流式及 3 次流式合成请求，均 HTTP 200；仅合成工具回合，不执行 SSH、不发送真实命令输出、不修改应用数据、不输出密钥。首个合成响应没有非空 `reasoning_content`，遗漏该字段仍成功，故不能将本次真实失败断定为缺少思考回传。官方文档虽要求思考工具回合回传该字段，但当前探测未复现这个错误。
- [ ] 原始 HTTP 失败的具体状态/上游原因仍未证实，旧请求丢失的信息不能由新代码补回。后续已完成安全 HTTP 状态日志、Trace ID 界面提示及供应商原生推理续接；再出现失败可按新 Trace 定位，但不能据此追认本次原始根因或声称其已复现修复。
- 本轮仅排查并记录进度，未改产品代码、未重发用户请求或远端命令，未运行全量或打包。

### 模型地址编辑与全局 Toast 错误反馈（2026-10-19）

- [x] 前端 master / Rust main：移除 `ModelSettings.tsx` 和 `src/agent/provider/repository.rs` 的“改地址必须重填/清除密钥”限制。修改地址并留空 API Key 使用 `KeyChange::Keep` 保留原加密密钥；显式替换/清除、版本校验、密钥不回显及 Responses 旧窗口失效机制保留。未改 MCP 服务凭据的目标绑定规则。
- [x] 客户端 `src/state/useToastError.ts` 提供复用错误状态和外部错误通知桥接。模型、工作区、工具/MCP/Skills、SSH 表单/操作、记忆、项目/历史编辑删除、常用命令、下载、聊天输入、更新和外观字段校验统一使用现有全局 Toast，移除对应页内错误条。保留输入、无效字段标记及独立重试入口；主动操作重复失败可再提示，工具状态轮询抑制相同错误。终端断连/读取重试、附件资源失败、工具执行证据、队列及传输状态继续在原位展示。
- [x] 验证仅覆盖改动与直接影响范围：前端相关组件/Toast **152 项通过**，TypeScript、Vite 资源构建、diff 格式检查通过。Rust 定向 **2 项通过**：`agent_models::encrypted_keys_survive_restart_and_explicit_url_edits_keep_credentials` 验证地址修改后密文保留与可解密、随后可清除；`agent_responses::endpoint_model_protocol_and_key_changes_never_reuse_an_opaque_window` 验证地址/模型/协议/密钥变化仍不复用旧压缩窗口。初轮部分前端断言仍检查旧内联错误，适配 Toast 后全部通过。Rustfmt 检查发现库文件此前已有无关排版差异，未扩散修改；本次测试代码按格式整理。
- [x] 同步客户端 `docs/operation-feedback.md` / `docs/agent-model-settings.md`。未全量回归、未实际视觉验收、未提交推送或打包，保留已有未提交改动。含 Rust 库变更，桌面测试需重启 `npm run tauri dev` 重新编译，单独刷新前端不足以更新服务端校验。

### 模型设置：容量缩写与紧凑卡片（2026-10-19）

- [x] 客户端 master：`src/components/ModelSettings.tsx` 使用十进制 K/M 容量缩写，如 `200K tokens` / `256K tokens`，去除“上下文”前缀，完整数值保留于 title；协议和容量使用蓝/紫柔和标签高亮。`.css` 将卡片宽度限制为 360px 并收紧内边距、行距及底部操作区，保留窄屏适配。
- [x] 定向展示/删除 **2 项通过**，TypeScript 检查通过；展示断言首次因 JSX 子节点额外空格失败，统一字符串后复测通过。未实际视觉验收，未运行全量或打包，保留已有改动。

### 模型设置：卡片信息统一标签风格（2026-10-19）

- [x] 客户端 master：`src/components/ModelSettings.tsx` / `.css` 去除配置卡片上重复的供应商兼容文字；协议、上下文及 URL 均复用中性 `Badge` 标签样式，移除原大块信息底色，协议与上下文一行可换行、URL 独立一行并支持长地址省略。供应商选择菜单名称保持不变。
- [x] `tests/model-compaction-ui.test.mjs` 相关展示/删除 **2 项通过**，TypeScript 和相关文件 diff 格式检查通过；未实际视觉验收，未运行全量或打包，保留已有改动。

### 模型设置：配置卡片层级与操作统一（2026-10-19）

- [x] 客户端 master：`src/components/ModelSettings.tsx` / `.css` 调整配置卡片的名称、状态、协议及连接信息层级；名称 14px、状态与辅助信息 12px，状态圆点增至 7px，使用随主题变化的绿/琥珀色底。地址与上下文集中展示，操作区独立靠右，编辑/删除统一为图标加文字按钮；支持长名称、长地址与窄屏换行。
- [x] 仅验证卡片供应商展示和删除确认/失败重试/成功刷新 **2 项通过**；TypeScript 与相关文件 diff 格式检查通过。未实际视觉验收，未运行全量或 Rust 测试，未打包，保留原有未提交改动。

### 模型设置：Claude 品牌图标修正（2026-10-19）

- [x] 客户端 master：`src/components/ModelSupplierIcon.tsx` 将 Anthropic 字母标志替换为用户指定的 Claude 放射图标，使用暖橙色 `#D97757`；下拉菜单、当前值、配置卡片和删除摘要共用组件同步生效。图标沿用 Lobe Icons 本地 SVG，更新 `public/licenses/lobe-icons.txt` 标注。
- [x] 仅运行 `tests/model-supplier-icon.test.mjs`，SVG、菜单显示及文字选项键盘行为 **3 项通过**。未实际视觉验收，未运行全量、未打包，保留已有改动。

### 模型设置：删除确认弹窗排版（2026-10-19）

- [x] 客户端 master：`src/components/ModelSettings.tsx` / `.css` 为删除确认添加供应商品牌图标及名称/模型摘要卡片，统一标题、正文、底栏留白，调整圆角与阴影，去除弹窗容器默认轮廓；保留操作按钮键盘焦点提示、原确认及错误重试流程。
- [x] 仅验证直接相关范围：`tests/model-compaction-ui.test.mjs` 中删除确认/失败重试/成功刷新 **1 项通过**；TypeScript 检查通过。未实际界面视觉验收，未运行全量或 Rust 测试，未打包。保留已有未提交改动。

### 模型设置：供应商图标与精简排版（2026-10-19）

- [x] 客户端 master：供应商下拉仅显示品牌图标＋“OpenAI 兼容 / Anthropic 兼容”，移除说明列；已配置卡片以对应品牌图标替代机器人头像。精简重复的页面/协议/密钥描述，调整头像、名称、标签及操作区间距，沿用设置主题。
- [x] 代码：`src/components/ModelSupplierIcon.tsx` 内置 OpenAI / Anthropic SVG，随主题文字着色；`Select.tsx` / `.css` 新增通用可选图标渲染，原文字型选项行为不变；`ModelSettings.tsx` / `.css` 与 `src/api/modelProtocols.ts` 统一供应商展示。图标来自 Lobe Icons，MIT 许可证包含于 `public/licenses/lobe-icons.txt`，无额外运行时依赖或图标网络请求。
- [x] 本次直接相关验证：`tests/model-supplier-icon.test.mjs`、`tests/model-compaction-ui.test.mjs`、`tests/model-protocols.test.ts` 共 **25 项通过**，覆盖实际 SVG、菜单选项和当前值、纯文字 Select 键盘操作、供应商/协议/地址/密钥/整理参数保存；TypeScript 与 Vite 资源构建通过。未进行实际界面视觉验收，未运行全量或 Rust 回归、未提交或打包，保留已有工作区改动。

### 模型配置：基础地址改为占位提示（2026-10-19）

- [x] 客户端 master：`src/components/ModelSettings.tsx` 新建配置的 API 基础地址保持空值；`src/api/modelProtocols.ts` 供应商目录仅提供 placeholder，不再在切换供应商/协议时自动填入网址。已有配置及用户输入地址保留，未填写真实地址时明确提示并阻止保存。
- [x] 只验证相关范围：供应商/协议切换、新建地址提示与保存、已有配置恢复 **5 项定向测试通过**；TypeScript 检查通过。同步客户端 `docs/agent-model-settings.md`；未运行全量、未修改 Rust 产品代码、未打包。

### 模型设置：配置概览与供应商弹窗（2026-10-19）

- [x] 客户端 master：`src/components/ModelSettings.tsx` / `.css` 改为配置卡片列表；无配置时展示空状态，新建和编辑在独立弹窗完成。卡片展示名称、模型、供应商、协议、地址和状态，支持编辑、确认删除与刷新；保存成功关闭并更新列表，失败保留草稿，取消丢弃，保存时禁止关闭。
- [x] `src/api/modelProtocols.ts` 以供应商目录组织协议：OpenAI / 兼容服务下选 Chat Completions 或 Responses，Anthropic / Claude 展示原生 Messages。旧配置按原协议恢复供应商，官网默认地址切换、自定义代理地址保留，沿用已有密钥保留/替换与稳定配置 ID；不改 Rust 代码或数据库。弹窗复用现有 Modal/Select/数字控件、设置主题和焦点管理，宽窄屏分栏、固定底栏与内部滚动。
- [x] 按用户要求只验证本次及直接影响范围：`tests/model-compaction-ui.test.mjs` 与 `tests/model-protocols.test.ts` 共 **21 项通过**，覆盖新建/编辑/取消/删除、失败恢复、供应商、密钥、原有整理比例与远端整理参数保存；TypeScript、Vite 资源构建通过。实际界面视觉验收未完成，未运行全量或 Rust 回归、未提交或打包，保留已有未提交改动。使用说明见客户端 `docs/agent-model-settings.md`。

### Tauri 开发启动：Cargo 路径发现修复（2026-10-19）

- [x] 修复用户 `npm run tauri dev` 报 `cargo metadata ... program not found`：Rust 已安装但终端 PATH 缺失 Cargo。客户端 master 的 `scripts/node-runtime.mjs` 新增 Cargo 环境发现，Tauri dev/build 优先使用现有可运行 Cargo，再检查 `CARGO_HOME/bin` 与用户 `.cargo/bin`；只调整子进程 PATH，保留 Node、npm、Rustup 等环境，失败明确提示。未改系统 PATH、未安装工具链。
- [x] 按用户最新要求只做相关验证：`tests/node-runtime.test.mjs` **10/10 通过**；模拟 PATH 不含 Cargo 后，实际 `cargo metadata --no-deps --format-version 1 --offline` 成功；系统旧 Node 启动 `node scripts/node-runtime.mjs tauri dev --help` 成功。未启动 GUI、未重新编译 Rust、未跑全量或打包。README 同步启动说明；保留两个仓库已有未提交改动。

### Claude Messages 协议与供应商策略（2026-10-19，已接通并完成本地回归）

- E4 已按用户明确要求取消，不再实施；保留历史记录，不把取消标成迁移完成。
- Rust main / 前端 master：前端按用户最新明确要求直接在 master 修改，覆盖原 test-rust-ui 约定；保留未提交改动，未提交或推送。此前为续接快进过本地 test-rust-ui 到 148c11c，目前已切回 master。
- [x] 已接通 Anthropic Messages：`src/agent/provider/adapter.rs` 为策略接口及单一工厂，`transport.rs` / `dispatch.rs` 共用 HTTP 生命周期，`anthropic.rs` / `anthropic/stream.rs` 实现原生认证、文字/图片、工具编解码和 SSE。`config.rs` / `protocol.rs` 提供共享预算/配对/完成校验；`tool_loop.rs`、`tool_session.rs`、`chat/execution.rs`、`compact/auto.rs` 按能力选择，不向 Agent 主循环堆供应商分支。
- [x] 前端 `src/api/modelProtocols.ts` 统一协议目录，`ModelSettings.tsx` 新增 Claude 选项、messages 路径、max_tokens 与 0–1 Temperature；`ComposerReasoningControl.tsx` 显示常规模式。Key 仍由原生仓库加密保存，schema 21 无迁移。
- 边界：当前仅常规 Messages，显式禁用 thinking；扩展/自适应思考签名历史、Bedrock/Vertex 专用认证与供应商服务端工具未实现。Responses 窗口/计数/远端整理保持专属路径；Claude 使用本地摘要整理。架构与扩展方法见 [MODEL_PROTOCOLS.md](MODEL_PROTOCOLS.md)。
- [x] 用户最新要求测试，覆盖此前“不用验证”要求：Rust 全目标 **428 项通过、0 失败、6 项既有忽略**；原生库 **50 项**、前端 **439 项**通过；Rust 生产库严格 Clippy、TypeScript 与 Vite 构建通过。Rust 忽略项为 5 项需真实配置的 E6 测试及 1 项需 POSIX 的 Shell 测试，本次没有新增忽略项。新增 `provider/anthropic/tests.rs`（8 项）、`tests/agent_anthropic.rs`（3 项）及前端 `tests/model-protocols.test.ts` / `model-compaction-ui.test.mjs`（3 项）；覆盖认证、原生请求、图片估算、流片段、工具授权/执行/回传与断流不重试，旧 OpenAI / Responses 回归通过。
- 验证记录：后端 `target/claude-regression.txt`（24 个测试目标汇总）；前端 `src-tauri/target/claude-ui-tests.txt`。最初工具集成用例未启用流式上下文却模拟 SSE，2/3 通过；补上与桌面一致的 `LoopContext` 后 3/3 通过，最终已包含在 428 项全量结果内。Windows 并行链接内存不足，停止过时构建并改为串行；不把中断构建算测试成功。原生计数采用前一次完整 50/50、exit 0 的结果；串行脚本后来重复启动的原生运行已停止，不将该次部分记录计作成功。原生首次缺失依赖和后端说明资源，按锁文件及现有资源脚本补齐后 50/50 通过。
- 使用临时库、测试 Key 与本地回环模型，不调用真实 Claude 或用户 SSH；未做桌面视觉验收、未生成安装包。前端使用现有兼容 Node；真实 Claude 供应商与具体模型兼容性仍需配置后联调，历史 E6 验收不代表本次真实模型验收。

### 最新仓库与任务状态同步（2026-10-19）

- [x] 已 fetch Rust github 与客户端 origin，并核对跟踪分支：两边领先/落后均为 0/0，无需合并或拉取代码。Rust 当前 main/fd7e46a（github/main）；客户端当前 master/148c11c（origin/master），版本均为 v0.2.1。客户端旧 test-rust-ui 仍为 55d907e/v0.2.0；本次仅记录实际分支，没有切换分支或修改 AGENTS 约定。
- [x] 对照实际源码、发布说明与现有验收记录：A/B/C、D1–D5、E1–E3、E5、E6 已完成，E4 继续暂缓，schema 21。E6 按 2026-10-18 验收记录完成，不将此前各历史段落里的“下一项 E3/E6”当作当前任务游标。
- 最新 v0.2.1 已接入空闲对话主动压缩及取消（Rust chat/manual_compact.rs、客户端 AgentContextStatus/原生 compact_context）、托盘隐藏/单实例恢复、窗口边缘缩放、SSH 三步引导、正式名称和 com.jasonlat.shellpilot 数据目录迁移；细节见两端 docs/releases/v0.2.1.md。
- 同步前两个工作区均干净，本次只更新本进度文件；未修改产品代码，未运行测试/类型检查/构建/真实请求，未提交、推送或打包。后续按用户需求继续产品优化，E4 不自动启动。

### E6：回归及真实环境验收完成（2026-10-18）

- [x] 后端全目标 410 项、原生 57 项、前端 415 项通过，显式 POSIX Shell 隔离测试通过；两端严格 Clippy、TypeScript 与 Vite 构建通过。真实 SSH / 登录与 sudo SFTP / MCP / Skills 5 项重试通过，另完成 Baidu MCP 固定公开查询。
- [x] 当前模型 gpt-6-luna / low 的日志场景 A/B 各 5 次，共 10/10 完成；2 次首轮摘要拒绝经固定原因反馈恢复，全部回执重放无重复执行。E5 原始正式失败统计保留，不以补测覆盖。
- [x] 用户明确确认剩余 GUI 范围“ok了 就当通过了”，E6 据此勾选；已抽查项、未完整自动覆盖的窄屏矩阵与原始证据见 [E6 验收记录](../evals/e6/results/2026-10-18/REPORT.md)。E4 仍暂缓，当前不打包，后续按用户需求处理产品优化。

### E5 失败回查：摘要拒绝诊断与重试反馈（2026-10-18）

- [x] 回查 A/log-injection/第 3 次失败：两次模型响应都有 usage，但旧记录没有具体摘要拒绝原因，原始一次不能追溯断言同因。新增无正文的摘要长度/数量/引用诊断及原生固定拒绝码，先独立诊断复测 10/10 完成，捕获 2 次首轮无效保留事实被拒后重试成功。
- [x] 确定性复现首轮摘要把超长原始背景全文作为事实引用时，第二轮缺少拒绝原因反馈。修复普通文本/工具回合自动整理重试，传递宿主固定拒绝码和纠正规则，保留 512 字节引用、来源、证据、预算校验与最多两次模型尝试。测量桥保留实际 finish_reason，诊断不记录被拒模型正文。
- [x] 相关 **109 项回归**及严格 Clippy 通过；修复后同配置日志场景 A/B 各 5 次，**10/10 完成**，全部有检查点、原目标只读查询及干净重放，无重复执行。该批恰好均首轮摘要合格，不用小样本宣称反馈改善概率；反馈路径由确定性回归验证。原始正式 A 17/18、B 18/18 不改，E6 重复压力验收保留。见 [回查记录](../evals/e5/results/compaction-investigation-2026-10-18/REPORT.md)。

### E5：完整同模型 A/B 评测完成（2026-10-18）

- [x] 按用户继续完成 E5 的要求，使用当前 gpt-6-luna / medium 冻结快照及同一修复后实现，全新运行 6 类任务 × 两种整理策略 × 3 次重复，共 36 场景，不继承旧模型成功记录。A 为 99%、B 为 60%；两组统一 16384 窗口、8192 输出及 512 安全预留。103 个源文件与实际执行文件运行前后指纹一致。
- [x] 正式首次结果 A 17/18、B 18/18；事实 38/39 与 39/39，目标任务核实 11/12 与 12/12，全部 36 次回执重放通过，无重复 inventory 执行。A/B 共 127 次模型请求均返回 usage，总 token 192588 / 160628，完成延迟中位数 11.077 / 8.5025 秒。本组 B 的总 token 少 16.6%、中位完成延迟少 23.2%；小型压力任务不能证明通用最优或统计显著优势。
- [x] A 日志场景第 3 次重复出现 AGENT_COMPACTION_LIMIT，未提交检查点且未执行日志查询。保持同一模型/代码的独立补测通过（1 个检查点及原目标查询），原始失败保留在正式统计，不用补测覆盖。登记 E5-LATE-COMPACTION-01，根因本次未确定，E6 继续重复压力验收；评测完成不代表该风险不存在。
- [x] 增加完整汇总校验，拒绝混合配置、选择性复测及重复/缺失场景；从逐项记录重算指标。统计校验 **4 项通过**；生产实现沿用此前相关 **97 项回归、严格 Clippy 通过**版本。原始 JSON/JSONL、模型/代码/执行文件指纹、复核索引与独立补测均保存于 [完整报告](../evals/e5/results/full-ab-2026-10-18/REPORT.md)。
- E5 勾选完成，E4 暂缓，下一项 E6。没有修改产品默认阈值或客户端配置，工具仍为回环模拟；实际 GUI、第三方 MCP 与用户 SSH 验收属于 E6，当前不打包。

### E5：当前配置定向复测与容量修复（2026-10-18）

- [x] 用户切换当前配置为 gpt-6-luna / medium，并要求成功项不重跑、失败项尝试修复后复测。新增 --retry-from，按 case/分组/重复编号采用最新记录，跳过完整成功项；整理触发但无检查点及缺失场景仍纳入复测。每批冻结只读配置快照，异常指纹变化保留部分报告与 excluded 结果。
- [x] 旧批次保存 30 项后因配置变化停止；新配置工具预检 2/2 通过，首轮定向复测 24/29 通过。修复托管工具批次过早执行硬容量校验的问题：本地批次只校验协议，实际模型发送前仍严格检查容量与协议；审批/目标/执行次数保护不变。整理前先本地寻找来源和替换投影都可容纳的完整前缀，不适合的候选不消耗模型次数，模型尝试仍最多两次。
- [x] 修复后只复测残留 5 项，5/5 通过；最新 36 项任务状态通过（31 项为当前模型验证，5 项按要求沿用旧模型成功结果）。B 组 12 个长历史场景均有有效检查点，回执重放不新增调用。不同模型及修复前后数据不混作同模型同代码版本的 A/B 性能结论。
- 验证：相关回归 **97 项通过**（工具循环 12、工具会话 24、扩展 29、整理 25、计分/复测 7），严格 Clippy 通过。当前模型各次实际尝试共 120 次请求，usage 完整，输入 320270 / 输出 7210 / 总量 327480 tokens。没有修改客户端配置、调用真实用户 SSH/MCP 或运行 GUI 验收；未提交、推送或打包。
- 定向复测已闭环，旧失败及新通过证据全部保留：[复测报告](../evals/e5/results/RETRY-RESULTS-2026-10-18.md)。E4 暂缓；按用户要求未重跑旧成功项，完整统一模型/代码版本 A/B 统计未补跑。

### E5：固定任务对照入口与服务预检（2026-10-18）

- [x] 用户决定暂缓 E4，开始 E5，并明确使用客户端当前模型。新增 `examples/e5_eval.rs` 和 `evals/e5/`：同一 Rust Agent / 同一模型比较 99% 延后整理与 60% 提前整理，统一 16384-token 压力窗口，保留其余模型请求参数；不是 Java/Rust 或 Codex CLI 整体对比，不修改产品默认值。
- [x] 六组固定任务涵盖只读查询、多项状态、早期事实、目标更正、日志伪指令及历史观测复用。运行生产桌面 Agent、MCP、审批、整理与回执链路；仅一个回环 inventory 模拟服务，不连接用户 SSH/MCP，不执行真实 Shell。当前模型配置以只读方式复制到临时库，不导入用户聊天/连接。
- [x] 报告包含事实/约束/目标断言、上游 usage、完成延迟、模型与整理请求数、额外调用、重复操作及 requestId 重放；A/B 顺序交替，配置指纹和任务 SHA256 保证复跑可核对，缺少 usage 显示 null。逐场景 JSONL 防止中断丢失既有证据，上游不可用时停止整批。
- 验证：计分回归 6 项通过，严格 Clippy 通过；12 个模拟场景和 12 次回执重放已验证，无重复 MCP 执行，B 的四个长历史场景实际触发整理。模拟答案不构成真实模型质量证据，A 的容量失败也不能外推为产品日常完成率。
- 真实预检：客户端唯一可用模型 `gpt-5-6-mini` / low，当前服务 `localhost:3000` 连续返回 HTTP 502，无有效真实输出。用户选择先修复服务后再跑，已停止真实请求；**E5 保持未完成**。入口/口径/命令与证据见 [E5 评测说明](../evals/e5/README.md)。仅新增评测代码与文档，未修改生产功能、未提交、推送或打包。

### 客户端交互：主导航文字、图标与悬停说明（2026-10-17）

- [x] 展开导航宽度增至 208px，标题与说明可换行，短窗口内部滚动；保留图标和文字的展开过渡。收起时增加深色圆角 Tooltip，显示名称与用途，支持悬停 / 键盘聚焦、Esc、滚动 / 调整窗口关闭，通过 Portal 避免被裁切。
- [x] 使用语义明确的终端窗口、工作区、模型芯片、扩展拼图和调色盘图标。主导航与独立设置页共用菜单配置，删除过时“关于试用版”入口、页签、正文与闲置 CSS；旧更新入口请求回到本地工作区，避免进入空白页。
- 仅客户端及文档改动，同步既有测试的导航断言；按用户此前要求未运行测试、类型检查、构建或实际界面验收。未提交、推送或打包。

### 客户端交互：参数档位与输入框推理选择（2026-10-17）

- [x] 安全预留、整理比例和 Temperature 复用上下文数字输入 / 档位菜单，Temperature 支持默认不发送和手动小数。请求参数及 Responses 远端整理开关直接展示，移除高级折叠与原推理设置及闲置 CSS。
- [x] 输入框模型选择旁新增推理强度入口，提供默认、none、minimal、low、medium、high、xhigh。通过既有模型配置接口保存当前模型的后续请求选项，合并最新配置、保留密钥并校验 revision；保存成功后回显、失败保留原档位并提示。保存期间用状态和同步 ref 阻止发送及模型切换，暂停队列消费；生成 / 停止 / 恢复任务期间禁用修改。载入失败支持重试，模型切换与设置页保存后重新读取，异步结果按生命周期隔离。
- 仅客户端及文档改动，同步既有测试适配；按用户此前要求未运行测试、类型检查、构建或实际界面验收。未提交、推送或打包。

### 客户端交互：统一操作提示（2026-10-17）

- [x] 复用 NotificationToast，增加全局通知入口与根级 Portal 容器，设置和配置弹窗共享顶部浮动提示；最多四条，相同内容合并，支持自动消失、悬停 / 聚焦暂停、手动关闭及读屏播报。
- [x] 模型、额度、MCP、Skills、本地工作区操作改用浮动结果提示，移除页面成功条和闲置样式；补齐 SSH 连接删除、项目创建 / 标题修改、记忆启停和壁纸 / 外观设置反馈，命令广播结果改为浮动提示，模型删除改为确认弹窗。既有对话 / 项目 / 记忆 / 文件删除确认和成功提示保留。
- 仅客户端及说明文档改动，同步既有测试适配。沿用用户要求，未运行测试、类型检查、构建或实际界面验收；未提交、推送或打包。

### E3 补充：MCP 编辑器鼠标定位（2026-10-17）

- [x] 修复弹窗全局输入字体覆盖透明 textarea，导致输入命中位置与高亮文字不一致。编辑器统一输入层、高亮层和行号的字体、字重、字号、行高及字符间距；根据输入层实际 clientWidth/clientHeight 对齐高亮层和行号，编辑、滚动及 ResizeObserver 尺寸变化时同步，排除横纵滚动条占位差异。
- 仅修改客户端编辑器 TSX / CSS 和说明文档；遵循用户要求，未运行测试、类型检查或构建。未提交、推送或打包。

### E3 补充：enabled 回显与分区保存（2026-10-17）

- [x] MCP JSON 回显始终保留 enabled 的 true / false，修复启用时字段被格式化逻辑省略的问题。
- [x] 客户端三个入口显式传递 execution / mcp / skills 保存范围。原生 IPC 与 Rust 配置事务只合并对应分区，其余字段取数据库当前值，继续保留 revision 校验；外部保存只更新执行额度与子任务开关，非 MCP 保存不重写 MCP 凭据。弹窗保存保留页面未提交的额度和子任务草稿，取消丢弃弹窗草稿。
- 按用户要求仅改代码，未运行测试、类型检查或构建；包含原生修改，需重新编译客户端加载。未提交、推送或打包。

### E3 补充：Skills 目录配置弹窗（2026-10-17）

- [x] 客户端 Skills 主页面只保留目录数量和“配置 Skills”入口；弹窗中每个目录一行，行号/文件夹图标、交替淡色背景、聚焦高亮及错误行标记区分。支持新增、移除和粘贴多行，最多 16 个；校验 Windows 盘符 / UNC 绝对路径和 UTF-8 字节上限。
- [x] 独立保存目录，保留尚未保存的额度和子任务开关，MCP 后续保存沿用最新目录及 CAS revision。取消还原，失败保留草稿，生成中禁用，保存中不关闭；复用 Modal 焦点和 Esc 行为，支持 Ctrl+Enter 保存。
- 验证：前端配置/目录/高亮编辑器/执行/历史/IPC **87 项**、TypeScript、Vite 构建通过。日志 `.codex-tmp/e3-skills-dialog-*.txt`。仅前端改动，未重跑 Rust 或 MCP 传输套件，实际浏览器视觉未验收。未提交、推送或打包。

### E3 补充：MCP 配置弹窗与 JSON 高亮（2026-10-17）

- [x] 客户端设置页只保留 MCP 服务摘要和“配置 MCP”入口，JSON 移至现有 Modal 弹窗。新增 JSON 高亮编辑器，区分键/字符串/数字/字面量，提供行号和滚动同步，兼容中文输入、明暗主题及高对比模式；复用原格式化、示例及校验逻辑，移除原页面编辑框的闲置样式。
- [x] 弹窗独立保存 MCP，保留页面其他额度、Skills、子任务草稿；取消还原 JSON，失败在弹窗保留内容，保存中阻止关闭，生成中禁用编辑。支持 Ctrl+Enter 保存与 Ctrl+Shift+F 格式化；复用 Modal 焦点约束、Esc 和关闭后焦点恢复。
- 验证：前端配置/编辑器/执行/历史/IPC **81 项**、TypeScript、Vite 构建通过。本次未修改 Rust 逻辑，不重跑传输测试。Tabbit 诊断在沙箱内外均无输出退出，未做实际浏览器视觉验收。日志 `.codex-tmp/e3-mcp-dialog-*.txt`；未提交、推送或打包。

### E3 补充：HTTP / SSE 联合回归（2026-10-17）

- [x] 按用户后续要求恢复 HTTP / SSE 测试。新增 Streamable HTTP 的 JSON / 碎片多行 SSE 回包、鉴权和会话头、401 / 重定向 / 畸形 JSON / 错误 ID / 断线 / 超时 / 超大响应及派发前后取消验证，确认不自动重放。
- [x] SSE 验证 JSON 中的 Authorization 与自定义头同时用于 GET 和每次 POST，完整 URL 中查询 Key 及 `+` 保持。既有 BOM / UTF-8 / CRLF / CR / 多行、异源 endpoint 拒绝、连接释放、取消 / 超时 / 断线和不重连测试通过。Agent 集成验证 HTTP、SSE 均从加密配置读取凭据，审批后单次执行，未知结果不重放，模型消息不带 Token。
- 验证：HTTP/SSE 协议 **10 项通过**（同批另含 stdio **1 项**），扩展集成 **28 项通过**，原生扩展配置 IPC **1 项通过**。日志 `.codex-tmp/e3-http-sse-protocol.txt`、`e3-http-sse-runtime.txt`、`e3-http-sse-native.txt`。使用回环模拟服务和测试凭据；未连接真实百度 MCP，未将真实第三方互操作标为通过。本次只补充测试和文档，未改动生产传输逻辑。

### E3 补充：统一 MCP JSON 配置（2026-10-17）

- [x] 客户端 MCP 表单改为固定高度的单一 JSON 编辑器，支持一个 mcpServers 对象批量管理 Streamable HTTP、旧版 SSE 和 stdio。提供格式化、停用示例、类型推断与错误提示；移除原服务卡片及闲置 CSS。兼容 type / transport、url / baseUrl、headers、description、command / args / env，旧环境变量名称列表仍可读取。
- [x] schema 21 增加独立加密 headers/env 表，值通过 Windows DPAPI 绑定用户、服务和连接目标，读取仅返回键名。__SHELLPILOT_SAVED__ 保留原值，修改替换、删除清理；目标更改须重新填写。旧 Bearer Token 可迁移，配置版本 CAS 和审批失效语义保持。stdio 实际传入 env 值，HTTP 使用自定义头；不把凭据放入模型上下文。
- 验证：按用户要求跳过 SSE 测试（名称过滤 sse），Rust **371 通过、1 既有忽略**，前端配置/执行/历史/IPC **75 通过**，原生 JSON 配置 IPC **1 通过**，TypeScript、Vite 构建和生产库严格 Clippy 通过。使用临时库与回环服务，未连接真实百度 MCP。日志 `.codex-tmp/e3-mcp-json-*.txt`；未完成实际 WebView 视觉验收。未提交、推送或打包。

### E3 补充：旧版 SSE MCP（2026-10-17）

- [x] MCP 配置新增 `sse` 传输。前端选择“旧版 SSE”，直接填写含 Key 的完整 `sse-endpoint`；不增加独立 Key 输入或变量替换。服务名支持连字符，界面请求超时默认 300 秒、可设 1–300 秒。完整 URL 在用户本地保存，通过原生 IPC 往返保持查询参数及 `+`。
- [x] GET 事件流 + endpoint 指定的同源 POST，兼容远端 HTTP、碎片 UTF-8、多行 data 和通知。禁止重定向和异源 endpoint；取消、超时、断线和协议错误关闭连接，不重连、不重复执行。沿用统一工具 schema、精确审批、日志及 unknown 结果语义。
- 验证：Rust 全量 **381 通过、1 忽略**，含新增 SSE 协议 5 项及扩展集成 3 项；前端设置/执行/历史/IPC **65 项**、原生 E3 IPC **1 项**、TypeScript 和 Vite 构建通过。日志 `.codex-tmp/e3-sse-*.txt`。使用回环模拟服务，无真实百度 Key，尚未验证真实百度互操作和实际 WebView 界面；未提交、推送或打包。

### E3 本地实现与验证（2026-10-16）

- [x] 通用工具轮次接入单层子 Agent、工作流计划、MCP stdio/Streamable HTTP 与 Skills 按需读取。普通对话不注册 SSH；子任务复用父任务目标、执行器和实时对话审批策略。
- [x] 设置 → 工具与 Skills、MCP 一次审批卡片、子任务实时事件和历史分组、schema 20 持久配置/审批/工作流记录已接通。关闭子 Agent 后保留历史签名，仍拒绝新委派。
- [x] 按工具能力并行相邻独立调用（每批最多 4 个）：子任务、Skills/目录发现、输出回查；已发现的只读 MCP 可跨服务重叠。同服务传输、SSH、未标记只读的 MCP 和计划更新保留串行约束。调用实际获得资源后才显示，运行中历史过滤尚未开始的调用；子任务 ID 隔离，共享 Shell 的精确审批不被兄弟子任务写日志作废。
- [x] 排查实际失败记录，子任务回查传入 `limit=1000/4096`（父任务还出现 1500），超出单页 512 上限；引用均为同任务已完成输出。对过大的正整数做有界归一化，工具说明补充 `outputs[].id` / `nextOffset` 分页规则，不放宽任务隔离或其他非法参数；耗尽预算时明确说明。最新额度规则见下一项。
- [x] 后续按用户要求开放执行额度配置：主对话推荐 32 轮/128 次，每个子任务 24 轮/64 次，一次处理最多 6 个子任务；支持前端编辑、恢复推荐值、原生 IPC 持久化与旧配置补默认值。有效输出回查免计工具次数，仍计模型请求轮数并执行归属校验。模型轮数、工具次数分别提示超限原因；其他资源限制保留独立保护。工具与 Skills 页修复全局 flex 样式冲突，改为纵向分区、并排额度及底部保存栏，窄面板改为单列。
- [x] 验证并修复畸形 MCP schema 被当成省略字段、畸形 MCP 成功响应被接受、关闭子 Agent 后旧历史导致协议错误。保留既有 SSH 超时归一化；旧测试使用真正无效的空命令验证拒绝。压缩回归按测量的新增工具目录成本调整固定预算，仍验证真实触发与检查点提交。
- 测试：Rust 完整回归 **373 通过、1 忽略**（既有 POSIX 环境专用测试），含 E3 扩展集成 23 项、工具循环 11 项、工具会话 24 项及 stdio/HTTP/Skills/schema 单元覆盖。额度新增回归覆盖旧配置补默认值、校验/隔离、主子任务独立额度、派发数、超限原因以及超过旧默认轮数；输出回查在主任务一次/子任务两次工具额度内仍可分页，模型轮数继续限制。客户端 TypeScript、Vite 构建、额度设置/审批 UI/历史/IPC **65 项**、原生 E3 IPC **1 项**均通过。最新日志 `.codex-tmp/e3-limits-*.txt`。Tabbit 启动器内外均无输出退出，未完成本次浏览器视觉验收。
- 使用临时库及回环模拟模型、MCP、SSH。真实模型、第三方服务互操作与完整 GUI 验收仍归 E6；未提交、推送或打包。范围与限制见 [E3_EXTENSIONS.md](E3_EXTENSIONS.md)。下一项 E4。


### 历史标题滑动延迟改为 0.5 秒（2026-10-16）

- [x] 按最新要求，将客户端 `src/components/ProjectChatHistory.css` 的长标题动画延迟从 1 秒改为 0.5 秒，保留移除开头额外静止段的调整；同步客户端 docs/agent-model-settings.md。
- 延续用户要求，不运行测试、类型检查、构建或视觉验收；未提交、推送或打包。

### 历史标题滑动延迟调整（2026-10-16）

- [x] 客户端 `src/components/ProjectChatHistory.css`：长标题动画延迟由 0.5 秒改为持续悬停 1 秒，并移除关键帧开头额外 12% 静止段，确保延迟后即开始左移；原移开取消、往返及减少动态效果行为保留。同步客户端 docs/agent-model-settings.md。
- 按用户此前要求不运行验证；未执行测试、类型检查、资源构建或视觉验收，未提交、推送或打包。

### 历史对话标题编辑与悬停操作（2026-10-16）

- [x] 客户端 `src/components/ProjectChatHistory.tsx` / .css：历史行右侧新增编辑图标，打开标题编辑框并保存；编辑、删除并排放置，默认透明且不拦截鼠标，仅悬停该行或键盘 focus-visible 时显示，选中状态不单独触发显示。取消原 hover:none 下始终显示删除按钮的样式；两个按钮按下不缩放或位移。编辑支持 1–80 字符、空值/控制字符校验、保存中防重复及失败保留输入；成功只更新已加载列表中的该条标题，不切换对话或重置分页。
- [x] Rust `migrations/019_agent_chat_titles.sql`、`src/store.rs`、`src/agent/titles.rs`、projects.rs、delete.rs；客户端 `src/api/agent.ts`、`src-tauri/src/backend_ipc.rs`：新增原生 POST agent/rename_chat，核对用户/原模型/会话归属；独立标题表持久保存，历史列表优先读取，不改写原始消息/任务/模型上下文/创建时间。删除聊天时一并清理标题。schema 19，已有记录使用原先自动标题；迁移版本断言同步至 19。客户端 docs/agent-model-settings.md / agent-ipc.md 已同步。
- 验证：用户明确要求“改就好了，不用验证”。已停止先前启动的 Rust 库/项目回归进程，无本轮通过结论；未继续运行测试、类型检查、资源构建或视觉验收。已补写标题存储/归属/重启/删除及原生派发测试用例，但尚未运行，不记作通过。
- 保留已有未提交改动，未提交、推送或打包；含原生/Rust 修改，开发应用需要加载重新编译后的原生代码才能使用新接口。

### E2 补充：记忆编辑删除按钮高亮（2026-10-16）

- [x] 客户端 `src/components/AgentMemory.tsx` / .css：为列表编辑、删除增加紧凑圆角点击区域及悬停/键盘聚焦/按压高亮，编辑使用主题色、删除使用红色浅底；禁用时不高亮，按压不缩放或位移，减少动态效果设置下关闭颜色过渡。继续在标题右侧同排，不增加操作行。同步 docs/agent-memory.md。
- 验证：已有记忆组件 **10/10**，TypeScript、Vite 资源构建及差异检查通过。纯前端样式调整，未新增重复测试或重跑无关原生套件；实际 WebView 视觉未记作通过。保留已有未提交改动，未提交、推送或打包。

### 对话侧栏：允许删除当前空闲会话并修正按压位移（2026-10-16）

- [x] 客户端 `src/components/ProjectChatHistory.tsx`、`src/App.tsx`：历史行状态与删除入口/确认按钮统一读取当前会话实时运行状态（含停止中），不再被列表旧 running 标记误锁；非当前会话及已打开的确认窗口使用最新历史记录检查运行状态。当前空闲会话允许确认删除，继续沿用删除成功后 clearChat 回到普通新对话的清理路径，不断开 SSH；真实运行中/切换中仍禁用，原生后端的准入锁及运行/输出写入检查保持。同步客户端 docs/agent-model-settings.md。
- [x] `src/components/ProjectChatHistory.css`：删除图标定位依赖 translateY(-50%)，显式保留按压态居中变换，避免被全局 button:active 的 translateY(1px)/scale 覆盖造成下跳；仅保留 120ms 透明度与颜色反馈，删除确认按钮取消缩放位移。
- 验证：侧栏/项目草稿/历史预览 **30/30**，新增当前已空闲但列表旧 running 为 true 仍可删除、实时运行优先于旧空闲状态、确认期间新任务开始及后台记录刷新后禁止删除的回归；TypeScript、Vite 资源构建及差异检查通过。未改 Rust 生产逻辑，未重跑无关原生套件；实际 WebView 视觉未记作验收通过。保留已有未提交改动，未提交、推送或打包。

### E2 补充：记忆卡片操作合并到标题行（2026-10-16）

- [x] 客户端 `src/components/AgentMemory.tsx` / .css：将编辑、删除移到卡片右上角启用开关后，移除底部独立操作行及上边距；标题与右侧控制同排，保留既有保存中/禁用/服务器范围校验。其他服务器的编辑提示并入来源/更新时间信息区，普通记录不增加额外一行。同步客户端 docs/agent-memory.md。
- 验证：已有记忆组件 **10/10**、TypeScript、Vite 资源构建及差异检查通过。仅前端排版调整，未新增重复测试或重跑无关原生套件；实际 WebView 视觉未记作验收通过。保留已有未提交改动，未提交、推送或打包。

### E2 补充：记忆列表快捷启停（2026-10-16）

- [x] 客户端 `src/components/AgentMemory.tsx` / .css：每条记忆右上角复用 Ui.Toggle，保留类型和状态文字，点击直接调用现有 saveMemory 保存启用状态，无需进入编辑。仅发送可编辑字段及原始版本；成功后以返回记录更新状态/版本/时间，失败或版本冲突保留已确认状态并提示刷新。保存中显示“保存中…”并禁用重复操作，使用同步防重入保护连续点击；其他服务器记录沿用当前管理范围校验。编辑表单说明继续保留，同步 docs/agent-memory.md。
- 验证：记忆组件 **10/10**，含新增列表启停/版本递增/重复点击/失败保留状态及错误服务器范围回归；TypeScript、Vite 资源构建及差异检查通过。复用既有原生接口，未重跑无关 Rust 套件；实际 WebView 视觉未记作通过。保留已有未提交改动，未提交、推送或打包。

### E2 补充：记忆启用选项说明（2026-10-16）

- [x] 客户端 `src/components/AgentMemory.tsx` / .css：将复选框文案明确为“启用这条记忆”，下方直接说明勾选后按适用范围/上下文容量供后续消息参考，取消勾选仅保留记录并暂停使用、可随时重新启用；明确保存后生效，不改写已发出的请求或已有聊天内容。说明使用主题弱文字和缩进，通过 useId/aria-describedby 与复选框关联；未改动启停逻辑或原生接口。同步客户端 docs/agent-memory.md。
- 验证：已有记忆组件回归 **8/8**，TypeScript、Vite 资源构建和差异检查通过。仅文案/样式调整，未新增重复测试、未重跑原生/Rust 无关套件；实际 WebView 视觉未记作验收通过。保留已有未提交改动，未提交、推送或打包。

### E2 补充：空白对话常驻记忆入口与跨项目管理（2026-10-16）

- [x] 客户端 `src/App.tsx`、`src/components/AgentPanel.tsx`、AgentMemory.tsx、`src/api/agent.ts`：Rust 对话页顶部始终提供“记忆”，空白页和未选择模型时也可管理全局记忆；记忆窗口新增“管理范围”，普通或服务器对话内都能直接选择任意已有服务器项目，使用其已确认身份管理服务器记忆，不切换当前聊天、不创建空聊天、不发送模型请求。未确认身份的项目说明先连接 SSH，同时保留全局操作；新对话/项目/模型切换关闭原窗口，异步读取隔离过期响应。窗口独立加载用户项目列表并支持失败重试；编辑/保存期间固定管理目标，避免草稿转写到其他项目；重复选择同一项目不会卡在加载中。
- [x] Rust `src/agent/memory.rs`、projects.rs 与客户端 `src-tauri/src/backend_ipc.rs`：管理范围支持已有会话或无会话的全局/项目；从原生持久项目读取用户归属与完整服务器身份，拒绝同时指定项目和会话、其他用户/已移除项目及无身份的服务器写入，编辑/删除继续校验版本。无会话来源显示“记忆管理”或“项目记忆管理”，不伪造来源会话；既有 JSON 兼容，schema 仍为 18。同步客户端 docs/agent-memory.md / agent-ipc.md。
- 验证：前端记忆/IPC/输入队列 **53/53**，覆盖空白页无模型操作、普通对话中给其他项目保存记忆、目标固定、迟到读取隔离及相同项目重复选择；Rust `cargo test --offline -j 1 --lib agent::memory::tests -- --test-threads=1` **6/6**，包含无模型/无会话管理、零新增聊天、项目身份/归属/移除校验及既有生命周期回归；原生 `cargo test --offline -j 1 --lib memory_dispatch -- --test-threads=1` **2/2**，核对真实派发、空白页无模型管理、来源与版本冲突。TypeScript、Vite 资源构建、生产库严格 Clippy、目标 Rust 格式及差异检查通过。
- 保留已有其他改动，未提交、推送或打包；实际 WebView 交互未记作通过，前台需重新编译并重启开发应用后验证。

### E2 长期记忆与短期任务上下文分离（2026-10-16，完成本地联验）

- [x] E2.1 持久记录与管理接口：`migrations/018_agent_memories.sql`、`src/agent/memory.rs`、store.rs/chat.rs；schema 18 独立保存用户确认的偏好/经验、原始来源会话/模型、创建/更新时间、启用状态及版本。全局记录跨会话/模型复用；服务器记录绑定 connectionId 与完整 ServerIdentity。所有读写核对用户和当前模型/会话；创建/更新/删除使用事务和版本校验，不从模型或摘要自动写入。来源聊天删除后记忆仍保留，最多每用户 128 条。
- [x] E2.2 模型上下文：`src/agent/compact/auto.rs`、auto/responses.rs、`src/agent/tool_session/context.rs`。按启用/适用范围和更新顺序选取完整记录，当前服务器优先；JSON 数据限额为可用输入额度四分之一且至多 8192 字节，整体提示仍由正常预算器计数。独立宿主提示明确历史背景、当前更正优先、不能推断实时状态或产生权限，不写入原始事件/语义检查点；Responses 的历史整理请求排除记忆片段，生成时重新读取。
- [x] E2.3 客户端：`src/components/AgentMemory.tsx` / .css、AgentPanel.tsx、App.tsx、`src/api/agent.ts`、`src-tauri/src/backend_ipc.rs`。对话顶部常驻“记忆”打开管理窗口（含空白对话，详见上方补充），支持当前适用/全部、新建/编辑、启停/删除、来源/目标/更新时间及失败提示；版本冲突保留编辑文字，其他服务器记录需回到对应对话编辑。原生 GET/POST/DELETE `agent/memories` 不接收客户端伪造目标或来源。使用说明见客户端 docs/agent-memory.md / agent-ipc.md，CLI 参考依据补入 CODEX_AGENT_REFERENCE.md。
- 验证：Rust 库 **56 通过/1 项既有 POSIX 环境测试忽略**，附件 **12/12**、自动整理 **25/25**、文字对话 **19/19**、上下文 **19/19**、模型 **22/22**、Responses **21/21**、工具会话 **20/20**，合计 **194 通过/1 忽略**；新增记忆内核 4 项及文字、Responses、工具各 1 项包含在上述结果中。前端 `tests/agent-memory-ui.test.mjs tests/agent-ipc.test.ts tests/agent-composer-queue.test.mjs` **48/48**；原生 `memory_dispatch_has_native_owner_revision_and_provenance_checks` **1/1**，验证真实派发、归属、来源、防伪造与版本冲突。TypeScript、Vite 资源构建、生产库严格 Clippy、改动 Rust 格式及差异检查通过。
- 首次组合回归在文字套件发现 3 个旧夹具问题：停止/中断用例把 C5 的 Context 事件当成文本已开始，改为等待真实 Text；无效附件仍预期 E1 前的 Unsupported，改为 Attachment 错误。修正后文字套件 19/19 复测通过；组合命令未执行的其余套件使用其已编译程序单独运行并通过，不将首轮组合命令记作全绿。
- E2 主项已勾选，下一项 E3。使用临时库、回环模拟模型及本地模拟 SSH，未调用真实模型或用户服务器；实际模型质量和 GUI/用户远端验收仍归 E6。含 Rust/原生 IPC 修改，前台需重新编译并重启开发应用后验证；未提交、推送或打包。


### 文档预览阅读布局调整（2026-10-16）

- [x] 只读核对本机 Codex 桌面端 `26.1007.2314.0` 的 `app.asar`，定位 `webview/assets/text-document-preview-174237b8d637.js` 与 `file-preview-1f88dac8f350.js`：标题/操作与独立滚动正文分离，使用大预览区域和阅读留白；本次参考桌面端静态资源，未复制其实现代码。证据与适用范围记录于客户端 `docs/chat-attachments.md`。
- [x] 客户端 `src/components/ChatAttachments.tsx` / .css：PDF/文本预览改为最大 1040px 的阅读弹窗，使用 Portal 避免聊天区域限制，文件名和工具栏固定、正文独立滚动，显示类型/大小/行数。文本默认保留原始长行并允许横向滚动，PDF 提取文本默认换行；支持自动换行切换、复制原始全文、键盘滚动及读取失败后重新加载。普通文本移除不相关的图片/PDF 说明，PDF 仅在底部保留提取范围提示。图片放大保留原实现；会话隔离、纯文本安全渲染及 Modal 原有关闭/焦点恢复继续沿用。
- 验证：既有附件/输入队列回归 **22/22**，附件用例同步覆盖原文复制、换行切换和失败重试，TypeScript、Vite 资源构建及差异检查通过。实际 WebView 视觉尚未验收，未记作通过；未提交、推送或打包。


### 文件卡片直接点击查看（2026-10-16）

- [x] 客户端 `src/components/ChatAttachments.tsx` / .css：移除已发送 PDF/文本文件卡片中单独的“查看发送内容”文字，将图标、文件名及大小区域整体作为预览按钮，支持鼠标点击与键盘 Enter/空格；下载入口独立保留，避免与预览嵌套或误触。沿用原会话范围校验、过期响应隔离及读取失败提示。同步 docs/chat-attachments.md 和已有附件预览测试定位。
- 验证：`node --import tsx --test tests/agent-attachment-ui.test.mjs tests/agent-composer-queue.test.mjs` **22/22**，TypeScript、Vite 资源构建及差异检查通过。实际 WebView 视觉尚未验收；未打包或提交。


### 深色提示统一增加悬停延迟（2026-10-16）

- [x] 客户端 `src/components/AgentPanel.tsx`：添加附件、编辑/恢复、用户/回复复制及完整时间的共用深色提示均在鼠标持续悬停 400 ms 后显示。鼠标提前移开、操作激活、Esc、滚动/调整窗口/失焦、禁用状态或草稿范围切换时取消等待；即使浮层尚未显示也响应取消，卸载清理计时器，触发时复核节点仍连接且未禁用。键盘 focus 保留立即提示，原样式/文案/日期格式不变。同步 docs/chat-attachments.md。
- 验证：`node --import tsx --test tests/agent-composer-queue.test.mjs tests/message-time.test.ts` **19/19**，其中新增模拟时钟回归覆盖 399 ms 不显示、快速离开取消、400 ms 显示、等待期间 Esc/范围切换取消以及 focus 立即显示；其余沿用队列及日期边界回归。TypeScript、Vite 资源构建和差异检查通过。实际 WebView 视觉仍待 E6，未记作通过；保留已有未提交代码，未提交、推送或打包。


### 时间悬停使用统一深色提示（2026-10-16）

- [x] 客户端 `src/components/AgentPanel.tsx`、`src/components/AgentComposer.css`：用户消息和回复的 time 节点移除原生 title，复用已有深色圆角 Portal 浮层显示完整日期与时分秒；提示锚点由按钮推广至 HTMLElement，保留定位与关闭规则。时间支持 Tab 聚焦查看，增加清晰焦点轮廓，原日期格式、午夜刷新及两类消息操作顺序不变。同步 docs/chat-attachments.md。
- 验证：日期边界与既有输入队列回归 **17/17**，TypeScript、Vite 资源构建及差异检查通过。低影响前端提示调整，未新增重复样式断言、未重跑无关原生套件；实际 WebView 视觉验收仍待 E6，未记作通过。保留已有未提交改动，未提交、推送或打包。


### 回复时间后置与历史日期显示（2026-10-16）

- [x] 客户端 `src/components/AgentPanel.tsx`：更正回复操作行为“复制回复 → 时间”，时间在最后；用户消息继续按“时间 → 编辑/恢复 → 复制消息”排列，深色复制提示保留。
- [x] `src/state/messageTime.ts`、AgentPanel.tsx：用户与回复统一使用本机自然日判断，当天 HH:mm，同年往日 MM-DD HH:mm，跨年 YYYY-MM-DD HH:mm；保留原始时间戳、ISO 语义及完整时间悬停提示。使用单个午夜定时器并在窗口 focus 时刷新，关闭/卸载时清理，不为每条消息创建定时器。同步 docs/chat-attachments.md；两个已有组件夹具补充模拟浏览器定时器。
- 验证：`node --import tsx --test tests/message-time.test.ts tests/agent-composer-queue.test.mjs` **17/17**（日期边界 4 项、既有队列/项目/附件 13 项），覆盖同一天、相隔不足 24 小时但跨日、跨年、午夜切换及无效时间戳；TypeScript、Vite 资源构建和差异检查通过。未修改历史记录或执行无关原生套件；实际 WebView 视觉仍待 E6，未记作通过。保留已有未提交改动，未提交、推送或打包。


### 消息操作顺序与复制悬停提示（2026-10-16）

- [x] 客户端 `src/components/AgentPanel.tsx`：用户消息操作顺序调整为时间、编辑/恢复、复制；回复操作行也将时间放在复制前。用户与回复复制按钮分别显示“复制消息”“复制回复”，使用既有深色圆角 Portal 提示，保留鼠标/聚焦、关闭与边界定位行为。
- [x] `src/components/CopyMarkdownButton.tsx`、CopyTextButton.tsx：增加可选提示事件透传到实际按钮，使用自定义提示时关闭浏览器原生 title，点击复制时关闭浮层；复制内容生成、成功反馈及剪贴板行为保持。同步 docs/chat-attachments.md。
- 验证：既有附件/输入队列回归 **20/20**，TypeScript、Vite 资源构建及差异检查通过。本次为前端排序与提示调整，未新增重复实现断言、未重跑无关原生套件；实际 WebView 悬停视觉仍待 E6，未记作通过。保留已有未提交变更，未提交、推送或打包。


### 输入框附件与消息恢复按钮的悬停提示（2026-10-16）

- [x] 客户端 `src/components/AgentPanel.tsx`、`src/components/AgentComposer.css`：将“＋”及消息恢复按钮的浏览器 title 提示替换为统一深色圆角、浅色文字、轻阴影浮层，分别显示“添加文件等内容”与“恢复到输入框”。使用 Portal 避免聊天滚动区域裁剪，优先位于按钮上方并限制在视口内；支持鼠标悬停、键盘聚焦和 aria-describedby，点击/失焦/Esc/滚动/窗口失焦或会话切换后关闭，不添加未实现的快捷键提示。附件选择、恢复文字及附件的原有操作继续保留，docs/chat-attachments.md 已同步。
- 验证：既有输入队列/项目/附件按钮接线回归 `tests/agent-composer-queue.test.mjs` **13/13**，TypeScript、Vite 资源构建及差异检查通过。低影响前端交互调整，未新增重复样式断言或重跑原生套件；实际 WebView 悬停视觉仍待 E6 验收，未记作通过。保留已有未提交代码，未提交、推送或打包。


### E1 补充：移除重复的再次附加入口（2026-10-16）

- [x] 客户端 `src/components/ChatAttachments.tsx` / .css、`src/components/AgentPanel.tsx`：移除图片悬停及 PDF/文本卡片上的“再次附加”，清理相关参数、回调和样式。继续使用消息悬停/聚焦时的“恢复到输入框”恢复原文字与附件；图片放大、文档查看和草稿内移除保留。同步 docs/chat-attachments.md 当前说明。
- 验证：附件与输入队列既有组件回归 **20/20**，TypeScript、Vite 资源构建及差异检查通过；源码搜索确认重复入口及回调无残留。仅前端入口精简，未新增重复实现断言，未重跑无关原生套件；实际 WebView 视觉验收仍待 E6。保留现有未提交改动，未提交、推送或打包。


### E1 补充：草稿图片使用紧凑缩略图（2026-10-16）

- [x] 客户端 `src/components/ChatAttachments.tsx` / .css：将待发送图片从铺满输入区的文件行改为 120×120 独立缩略图，附件容器使用横向排列并自动换行；准备中的图片使用相同尺寸占位，保留圆形加载状态。图片文件名/大小仍供无障碍读取，名称可悬停查看，移除按钮移至图片右上角；点击放大、错误重试及普通文件信息继续保留。同步 docs/chat-attachments.md。
- 验证：`node --import tsx --test tests/agent-attachment-ui.test.mjs tests/agent-composer-queue.test.mjs` **20/20**，TypeScript、Vite 资源构建与差异检查通过。本次是前端样式及卡片分类调整，未新增重复样式断言，未重跑无关 Rust 套件；实际 WebView 视觉验收仍归 E6，未记作通过。保留已有未提交改动，未提交、推送或打包。


### E1 补充：所有附件使用卡片内圆形加载指示（2026-10-16）

- [x] 客户端 `src/components/ChatAttachments.tsx` / .css：图片在缩略图区域显示圆形加载指示，PDF/文本等其他支持文件在文件图标区域显示；覆盖队列等待、上传/原生准备及图片预览加载。完成后逐项消失，错误仍在卡片中提示；使用可访问的非确定进度状态，没有可验证的字节进度时不显示百分比。
- [x] `src-tauri/src/attachment_input.rs`、backend_ipc.rs、`src/api/files.ts`：增加文件级 preparing 事件，携带临时 id/名称/大小，在整批类型/数量/字节预检通过后、原生存储前显示卡片；uploaded 同 id 关联完成。聊天系统选择器与复制文件共用预检/导入流程，文件正文与本机路径仍由原生持有。
- [x] `src/components/AgentPanel.tsx`：剪贴板读取期间显示临时准备卡片，随后替换成各文件卡片；完成、失败或普通文字粘贴结束时清理。旧草稿的迟到进度不能进入新会话，准备期间仍锁定发送。去掉底部“附件正在准备中，请等待上传完成”整条提示，只保留具体错误；原生选择器错误保留可读原因。客户端 docs/chat-attachments.md 与 ipc-transport.md 已同步。
- 验证：原生 `cargo test --offline -j 1 --manifest-path src-tauri/Cargo.toml --lib attachment_input -- --test-threads=1` **3/3**，补充逐文件准备/完成事件、id 匹配及整批预检先于事件的断言；前端 `node --import tsx --test tests/agent-attachment-ui.test.mjs tests/agent-composer-queue.test.mjs tests/agent-ipc.test.ts` **48/48**，覆盖图片/PDF/文本指示、单文件完成、旧进度丢弃、无底部等待横条及真实 IPC 接线；最后补齐选择器错误透传断言后 IPC 单独 **28/28**。TypeScript、Vite 资源构建、目标原生文件 rustfmt 和差异检查通过。实际 WebView 视觉验收仍归 E6，未记作通过；未提交、推送或打包，保留已有未提交变更。


### 输入框项目选择菜单（2026-10-16）

- [x] 客户端 `src/components/ComposerProjectPicker.tsx` / .css、`src/components/AgentPanel.tsx`：未选择项目的新文字对话在输入框上方显示“选择项目”文件夹按钮；点击打开独立 Portal 浮层，顶部搜索、可滚动已有项目列表和底部“新建项目”。名称/主机搜索、键盘上下键与 Enter、Esc/外部点击关闭、焦点恢复和随视口调整位置已接通，保留主题与三栏布局。
- [x] `src/App.tsx`、`src/components/ProjectChatHistory.tsx`：底部新建入口打开已有项目创建窗口；创建成功等待原项目连接流程结束，取消/管理服务器返回时释放输入区；选择和新建期间保留文字草稿，已有对话、运行/排队中的会话继续固定归属。沿用现有服务器项目数据和连接流程，未增加不绑定服务器的文字项目类型。`docs/agent-model-settings.md` 已同步使用说明。
- 验证：`node --import tsx --test tests/composer-project-picker.test.mjs tests/agent-composer-queue.test.mjs tests/project-history-ui.test.mjs` **37/37**，覆盖搜索、无结果不误选、键盘/关闭/运行锁、新建窗口及取消回执、草稿跨连接范围恢复与历史交互。TypeScript、Vite 资源构建、差异检查通过。本次仅改客户端 UI 和接线，不重新运行无关 Rust 套件；实际 WebView 截图/完整交互仍待 E6 验收，未记为通过。保留已有未提交代码，未提交、推送或打包；下一主项仍为 E2。


### E1 补充：图片预览、输入框粘贴与发送排版（2026-10-16）

- [x] 图片预览：客户端 `src/components/ChatAttachments.tsx` / .css、`src/api/files.ts`，Rust `src/agent/attachments.rs::image_preview`。原生选择器给图片附带临时缩略图，草稿可直接点击放大；历史按会话/模型/文件 ID 自动读取发送快照，原上传清理后仍可显示，迟到响应不能替换其他会话。原生草稿预览只接受当前实例文件 ID，沿用本地用户文件归属限制；共享现有受限图片解码、1024 缩放、JPEG 输出逻辑，预览并发为 2。失败可重试，不把文件图标当成已显示图片。
- [x] 输入框粘贴：客户端 `src/components/AgentPanel.tsx`、`src/api/files.ts`，`src-tauri/src/attachment_input.rs`、backend_ipc.rs、lib.rs / Cargo.toml。仅响应 Ctrl+V/Cmd+V 或菜单粘贴，原生读取截图/图片与 Windows CF_HDROP 复制文件，文件路径及原始字节不由 WebView 上传。普通文字按选区插入，用户继续编辑时保留新草稿；导入期间（包括 React 更新前）禁止漏发，完成回调绑定原草稿范围。复制文件整批预检剩余数量/20 MB、类型、空文件/目录，仍不支持 Office 文档或拖入上传。
- [x] 发送排版：`src/components/AgentPanel.tsx`、ChatAttachments.tsx/.css。图片缩略图独立放在文字气泡上方并右对齐，多图换行，点击放大，悬停/聚焦支持再次附加；PDF/文本继续显示文件卡片。图片与文字不再嵌在同一个大文件卡片中，保持三栏及主题。前端 docs/chat-attachments.md、agent-model-settings.md、ipc-transport.md 和 Rust AGENT_CHAT.md 已同步。
- 验证：原生 `cargo test --offline -j 1 --manifest-path src-tauri/Cargo.toml --lib attachment_input -- --test-threads=1` **3/3**，覆盖截图编码、原始文件存储及缩略图、清理后拒绝读取、复制文件整批预检、数量/字节限制和无效像素缓冲。首次编译仅遇到测试访问跨仓库私有 Store 字段，已改为公开文件服务接口后通过，未放宽封装。Rust `cargo test --offline -j 1 --test agent_attachments -- --test-threads=1` **12/12**，验证共享图片处理不影响两种模型协议、预算、归属、快照和自动整理。
- 前端 `node --import tsx --test tests/agent-attachment-ui.test.mjs tests/agent-composer-queue.test.mjs tests/chat-attachments.test.ts tests/agent-ipc.test.ts` **60/60**；补齐同步发送互斥后队列套件单独 **11/11**，TypeScript 与最终 Vite 资源构建、目标文件 rustfmt 和差异检查通过。没有调用真实模型、连接用户远端或读取用户系统剪贴板做测试；实际系统粘贴、WebView 截图/交互及真实模型验收仍归 E6，未记作通过。保留已有未提交变更，未提交、推送或打包；下一主项仍为 E2。


### E1 补充：图片理解默认开启（2026-10-16）

- [x] `src/agent/provider/config.rs`：图片理解与工具一样默认开启；缺省 vision 字段按 true 读取，旧配置保存的 false 在读取/保存时归一为 true，无需用户重新保存模型。保留字段用于存量配置兼容，不再作为用户启停开关。
- [x] `src/agent/attachments.rs`、`src/agent/provider/protocol.rs` / responses.rs：移除图片能力开关拦截，图片直接走已有附件准备和模型协议链路；文件归属、格式、大小及输入预算校验继续保留。供应商实际协议和识图支持仍以真实响应为准。
- [x] 客户端 `src/api/models.ts`、`src/components/ModelSettings.tsx`：新建及载入旧配置统一开启图片理解，删除高级参数中的图片理解开关；同步 README、模型设置及 Rust 模型/附件说明。
- 验证：`cargo test --offline -j 1 --test agent_attachments --test agent_models -- --test-threads=1` 附件 **12/12**、模型 **22/22**；包含旧 false 配置在 Chat Completions / Responses 仍发送图片、缺字段默认值和预算不足准入拒绝。`node --import tsx --test tests/model-compaction-ui.test.mjs tests/agent-attachment-ui.test.mjs` **13/13**，覆盖无开关、旧配置保存归一化和预览；TypeScript、Vite 资源构建及差异检查通过。未执行实际 WebView/真实模型验收，未提交、推送或打包。


### E1 附件链路（2026-10-16，完成本地联验）

- 基于 Rust main/c14ea5b 与客户端 test-rust-ui/61600b7 开发，保留此前仓库同步记录。实际参考固定 Codex CLI 的 core/src/image_preparation.rs、protocol/src/models.rs、core/src/context_manager/history.rs 与 compact_remote_v2_images.rs；按“原始引用、准备后的媒体、模型预算”分离接入，并核对官方图片输入格式，未执行参考项目代码。
- [x] E1.1 转换与归属：`src/agent/attachments.rs`、`migrations/017_agent_attachments.sql`、`src/store.rs` / services.rs。沿用同一个文件服务句柄和 Store，核对 file_assets.owner_id、会话/模型/用户及 SHA256；只接受文件 ID，不接受客户端路径、下载地址或转换结果。原始附件最多 4 个/20 MB，PNG/JPG/WEBP 经限制解码后等比缩至 1024 并编码 JPEG；UTF-8 每份至多 256 KiB；PDF 至多 8 MiB/100 页，限制单个加载流和逐页解压，提取文字带页码并计入 256 KiB 上限。两个解析任务，按会话 24 MiB/用户 128 MiB 保存准备快照；配额按 UTF-8 实际字节计。
- [x] E1.2 模型、预算与持久记录：`src/agent/provider/protocol.rs` / responses.rs、chat.rs、chat/execution.rs、turns.rs、compact.rs、compact/auto.rs、compact/auto/responses.rs、tool_session/context.rs。Chat Completions 使用 text/image_url，Responses 使用 input_text/input_image 并对真实输入调用计数。图片每张预留 8192 估算 tokens，不把 Base64 当正文；仍明确为启发式预留。文字/工具历史与摘要接收附件，整理后最新附件仍保留。轮次准入事务关联事件、幂等比较有序附件 ID；快照在原上传清理后仍可回查。`src/agent/delete.rs` 清理对话内关联与快照，不删除其他上传；移除项目继续保留聊天。附件准备期间响应原生停止/关闭和调用方取消，后台受限解析不产生执行权限。
- [x] E1.3 前台：客户端 `src/components/AgentPanel.tsx`、ChatAttachments.tsx/.css、ModelSettings.tsx、`src/state/agentHistory.ts`、`src/api/agent.ts` / files.ts、`src-tauri/src/backend_ipc.rs`。聊天“＋”打开系统选择器，数量/类型/大小预检；图片理解默认开启，不再提供高级参数开关，旧关闭配置自动兼容。历史卡片恢复并支持“查看发送内容”，经 agent/attachment 按原会话/模型/用户只读查看缩放图片或文档文字，迟到响应不替换别的会话。保持三栏布局、主题、现有队列与审批入口。
- [x] E1.4 收尾兼容回归：专项 `cargo test --offline -j 1 --test agent_attachments -- --test-threads=1` **11/11**，包括三类附件、无文字输入、错误内容/能力/预算、归属与不同 Store 拒绝、迁移、删除、重开历史/清理原上传、重复请求与摘要原始证据、Responses 计数/生成、编码长度不冒充文本 token，以及自动整理旧附件后最新图片完整保留、旧文件仍可回查的连续场景。native IPC/Channel + 附件 + 人工审批 + 本地模拟 SSH 的 D5 完整链路定向单项 **1/1**；前端附件预览、队列、模型、历史、IPC 和附件策略 **69/69**，TypeScript/Vite 资源构建、核心改动 rustfmt/diff 检查通过。生产库严格 Clippy 已通过；全目标 `cargo clippy --offline -j 1 --all-targets -- -D warnings -A clippy::items_after_test_module` 通过，仅豁免 HEAD 已有测试模块排序项。
- 兼容回归：`cargo test --offline -j 1 --lib --test agent_chat --test agent_auto_compact --test agent_responses --test agent_tool_session --test agent_context --test agent_models --test agent_projects -- --test-threads=1` 经夹具修正后，lib **52 通过/1 个原有 POSIX 用例按配置忽略**、auto_compact **25/25**、chat **19/19**、context **19/19**、models **21/21**、projects **6/6**。Responses 初轮 **19/20**，仅修正并复测 `endpoint_model_protocol_and_key_changes_never_reuse_an_opaque_window` **1/1**；工具会话随后单独 **19/19**。这些是分轮结果，不冒充一次全项目全量无失败运行；未跑 SFTP 等无关套件，也未再次重复已通过的全部套件。
- 验证修正：初轮 Responses 模拟服务漏返回 response.input_tokens 的 object 标识，修正测试服务后 10 项全过；旧审批测试仍按 command 字段及旧 SSH 专用审查提示断言，与 HEAD 的通用 tool-action 实现不一致，已仅修正夹具及 schema 16→17 断言，正式审批不变。Responses 旧密钥测试原先在认证关闭时更换保存密钥，此时有效运行凭据仍为 no-key；夹具现先启用认证并设置虚拟旧密钥，再验证虚拟新密钥拒绝旧窗口，生产凭据规则不变。工具整理 5 项原先沿用 35% 水位，实测完整输入约 3759 tokens，必需提示加检查点超过原低水位 2078；仅将夹具水位改为 45%，保持整批仍触发、压缩后必需输入可装入，最终 19 项通过，生产触发/回退规则不变。Rust 全目标默认严格 Clippy 命中 HEAD 已有 chat/execution.rs 的 items_after_test_module；未移动无关测试模块，另跑仅豁免该项的全目标检查。
- 扫描 PDF、OCR、Office 文档及拖入上传不在当前支持范围，明确提示而不伪造已读；剪贴板图片/复制文件和缩略图排版已在后续 E1 补充中接通；带附件队列按下一轮发送，同轮引导仍仅纯文本。实际 WebView/真实模型/用户远端验收仍归 E6，不把组件或协议验证当成实际 GUI 通过。只使用临时库、回环模型和本地模拟 SSH，不提交、推送或打包；E1 主清单已勾选，下一项 E2；实际模型质量和视觉验收仍须另做。

### 仓库同步核对（2026-10-16）

- 已执行远端拉取：Rust `main` 从 `6c93fff` 快进至 `c14ea5b`；客户端 `test-rust-ui` 已与远端一致，HEAD 为 `61600b7`。拉取前后两个仓库均无未提交代码；本次仅补充此进度记录，不提交、推送或打包。
- 核对最新源码：`src/store.rs` 已加载 schema 16；同轮引导见 `src/agent/steering.rs`，三种审批模式及隔离审查见 `src/agent/approvals.rs` / `approvals/reviewer.rs`，无损移除项目与删除对话见 `src/agent/projects.rs` / `delete.rs`。
- 当前桌面工具使用绑定终端的当前 Shell，继承已完成的身份/目录/环境变更，按调用隔离捕获输出并在终端展示；入口见 `src/terminal/agent_exec.rs` / `shared_shell.rs`。工具默认可用仍须符合绑定、协议及审批策略；此前“须手动启用工具、右侧不展示输出”的说明属于旧阶段。
- 工具正文已接真实 Chat Completions 增量流，见 `src/agent/provider/tool_stream.rs` / `tool_loop.rs`；前端已包含输入队列/同轮引导、审批模式、延迟建会话、项目移除/对话删除、历史悬停预览与快捷定位轨道。已按实际源码核对，未将文档旧的“未提交”表述解释为当前工作区仍有改动。
- 阶段状态仍为 A/B/C 与 D1–D5 完成本地联验，下一项 E1（图片/PDF/文本附件转换、模型能力、预算和所有权检查）。E2–E6 保持未完成；真实模型、用户远端及完整 WebView 验收仍待完成。此次同步未重新执行新版本测试或启动应用，各功能验证沿用对应章节的既有证据。

### 最新工作区：对话快捷定位轨道与审批行收简（2026-10-16）

- 客户端移除对话顶部“工具权限 · … / 审批与记录”整行，审批卡仍按原有待确认流程进入输入区，权限选择仍保留在输入框底部。根据桌面端 `floating-navigation-rail-layout` 的 Marker 样式重做快捷定位：节点只取用户消息/对话轮次，至少 4 个节点才显示；每个节点为固定 10px 行高中的 26px 独立短线，节点从轨道中间自然堆叠，超出最大高度后轨道自身滚动；普通消息使用统一 6px 基础宽度，悬停/聚焦/拖动选中时才按 20/14/9px 邻近宽度递减。悬停/聚焦/点击节点显示对应回复摘要、标题和标记图标；摘要浮层改为 Portal 挂载，脱离轨道滚动裁剪，并保留鼠标移入浮层的交互窗口；点击使用目标元素原生平滑滚动，修正嵌套滚动容器 offset 计算；补齐轨道移入/移出、焦点离开、指针拖动即时定位、拖动点击抑制、窗口尺寸重算和定位后短暂高亮。
- 验证：TypeScript、输入队列/审批组件 20 项、Vite 构建及差异检查通过；未完成实际 WebView 截图验收，未提交或推送。

### 最新工作区：默认使用可变字重中文字体（2026-10-16）

- 客户端默认界面字体改为 `Noto Sans SC`，并将 `Noto Sans SC` 标记为可变字重选项；新增 `Noto Serif SC` 与 `Segoe UI Variable` 选项。系统未安装时沿用原有回退字体栈，代码、终端和公式字体不变。字体设置保存/恢复逻辑保持兼容，旧的空 family 配置会自然使用新的默认值。
- 验证：待运行前端字体设置与构建检查；未提交或推送。

### 最新工作区：日常对话字体加粗（2026-10-16）

- 项目归属栏和输入文字采用 600 半粗字重；正文和用户消息保持 500 常规字重并关闭粗体合成，不使用描边，避免静态中文字体将 550 直接跳成 700。强调文字/标题/表头保持 600；字号、间距和布局不变。
- 验证：最终样式构建和差异检查通过；未完成实际 WebView 截图验收，未提交或推送。

### 最新工作区：工具对话恢复真实流式正文（2026-10-16）

- 原因：启用 SSH 后，ToolLoop 原先调用 ModelClient.complete，模型完整回复到齐才进入 Channel；前端 Markdown 本身仍支持流式更新。新增 Chat Completions 工具流适配，桌面 ToolSession 按真实 content/refusal 增量发 Text，复用现有顺序 Channel/ACK；独立非桌面循环和审查请求保持原调用路径，不增加前端模拟打字。
- 按 index 拼接工具 ID、函数名和 JSON 参数，收到 finish_reason 与 [DONE] 后复用完整响应/工具白名单验证，再进入参数校验、审批及执行。保持响应/帧/字段大小限制、取消与模型超时，不重试；兼容忽略 stream=true 而返回 application/json 的服务，后者只能完整返回后显示。
- 部分正文复用 Running.partial，心跳落库，取消/失败保留已接收内容；工具前说明、同轮引导与最终回复统一到可见结果，并从模型历史最终正文中扣除已持久化的步骤前缀，避免双重展示/上下文重复。正文及工具事件沿原顺序传输，审批和执行权限不变，无迁移。
- 验证：新流式解析 4 项、同轮引导 4 项、工具循环 9 项及桌面真实 HTTP/模拟 SSH/Channel/SQLite 定向回归 1 项通过；桌面测试通过延迟释放模型后半段，验证前半段已先收到、最终内容不重复、工具证据可回查、明确停止后 partialText 保留、历史与缓存重读不再次执行 SSH。客户端 IPC/首次发送 27 项、cargo check 和 clippy --lib -D warnings 通过。仅访问本地虚拟服务，未调用真实模型或 SSH；后端修改需重新编译启动，未提交或推送。

### 最新工作区：历史记录悬停预览（2026-10-16）

- 替换历史行原生 title 提示，新增独立 Portal 浮层：三行标题、右上角创建时间、底部项目图标/名称；统一界面字体、600 标题字重、浅边框和柔和阴影，支持明暗主题。时间来自原有 createdAt，不伪装为最后活跃时间；不额外查询聊天或服务器。
- 首次悬停延迟 400ms，已展开时切换相邻行立即更新；允许移入浮层继续查看，离开延迟关闭。支持键盘聚焦和 Esc，外部点击、滚动、窗口调整/失焦以及对话/弹窗状态切换会清除预览和计时器。忽略触摸悬停，卸载或原行移除后不再弹出。运行中禁用行也能悬停查看。优先侧栏右侧定位，空间不足时翻转或移至上下方并限制在视口内；字体在测量前继承来源行，避免自定义字体改变高度后越界。
- 验证：历史组件 21 项与定位/时间 3 项，共 24 项通过；TypeScript、Vite 构建及差异检查通过。未完成实际 WebView 截图验收，本次没有后端逻辑/迁移变更，未提交或推送。

### 最新工作区：历史列表展开/收起入口（2026-10-16）

- “显示更多 / 收起”改为文字配向下/向上小箭头，保持与历史标题左对齐；局部使用至少 600 字重并允许粗体合成，去掉悬停下划线，改用柔和圆角背景及按下反馈。统一按钮间距和点击区域，支持键盘焦点与减少动态效果偏好；原有每次显示 5 条、后端游标分页和收起逻辑不变。
- 验证：历史组件既有 16 项、TypeScript、Vite 构建及差异检查通过，仅前端样式/图标调整，未做实际 WebView 截图验收，未提交或推送。

### 最新工作区：审批文字字重（2026-10-16）

- 后续交互修复：批准按钮下拉触发器的通用 hover 浅色背景覆盖了深色分段按钮，白色箭头因而失去对比。明确左右两段默认、hover、展开和按下状态，保持统一深色系；鼠标聚焦不显示外圈，键盘焦点使用内侧描边。仅调整 CSS，审批组件 13 项与 Vite 构建、差异检查通过，未做实际 WebView 截图验收。

- 按截图加粗审批提问标题、取消/拒绝/允许按钮及“请求批准”触发器，局部字重至少 600；明确界面字体并允许 weight 合成，避免全局 font-synthesis:none 使缺少对应粗细的中文字体仍显得很细。操作按钮规则覆盖全局按钮默认字重，兼容输入区 Portal；审核模式菜单标题同步加粗。保持现有字号、命令字体和审批逻辑。
- 验证：既有审批组件 13 项及 Vite 构建通过，差异检查通过；仅前端 CSS 修改，未做实际 WebView 截图验收，未提交或推送。

### 最新工作区：新建项目弹窗样式（2026-10-16）

- 新建服务器项目弹窗统一标题、字段和帮助文字的字号/间距，底部独立浅色操作区：“管理服务器”改为带图标的次要入口，“创建并开始对话”改为实底主按钮。输入控件、聚焦、悬停、禁用及窄窗口换行样式补齐，颜色沿用主题变量；创建与连接逻辑不变。去掉 reference.css 中对应旧样式，集中至 ProjectChatHistory.css。
- 验证：既有项目历史/创建/审批状态 16 项、TypeScript 与 Vite 构建通过；未完成实际 WebView 截图验收。本次仅前端展示修改，未提交或推送。

### 最新工作区：命令用途占位文字修复（2026-10-16）

- 原因：兼容旧工具参数时后端补入“未提供命令用途说明”，前端误认为有效 explanation，覆盖了 df -h / whoami / pwd 的准确默认说明。后端现在为这些完整简单命令补上用途后进入参数校验、审批和持久化；前端识别旧占位文字，使已有记录也能显示用途。模型实际提供的说明优先保留；不根据命令前缀猜测复合脚本，错误参数类型仍由原校验拒绝；命令文本、超时和授权匹配规则不变。
- 验证：审批组件 13 项、Rust 用途归一化 2 项及 TypeScript 检查通过，覆盖旧占位、缺失/空白、模型说明优先、复合命令及无效类型。未连接实际 SSH/模型，未做实际 WebView 截图验收；后端修改需要重新编译启动，未提交或推送。

### 最新工作区：等待审批标签与无损移除项目（2026-10-16）

- 后续侧栏状态补齐：普通生成/工具执行时在标题右侧显示灰色旋转圆圈，当前会话采用实时 chatBusy/recoveredRunning 覆盖历史快照，其他记录沿用原生 running。审批标签与普通圆圈互斥，结束/停止即清除。选中记录保留已有浅灰圆角底色，进行中不降低整行透明度；减少动态效果偏好静态显示。历史组件 16 项及 TypeScript 检查通过，仅前端改动，未做实际 WebView 截图验收。

- 历史标题旁新增绿色“等待审批”标签和小型旋转图标。当前对话复用审批组件的实际状态，原生历史查询返回 awaitingApproval；按准确 sessionId 更新。逐条审批和自动审查要求人工确认时显示，批准/拒绝/取消/过期后清除；自动审查进行中、允许和完全访问模式不显示。支持减少动态效果偏好。
- 覆盖之前的项目删除行为：项目编辑图标左侧“⋯ / 移除项目”现在只移除分组，确认弹窗明确保留聊天与工具记录。新增幂等 migration 016 保存移除标记及原归属，避免启动时历史导入重建项目；记录可在“未分组对话”分页查看，原服务器身份和绑定不变。当前对话与草稿不清空。原单条 delete_chat 仍按确认执行彻底删除；运行状态及原生排他准入检查保留。
- 验证：前端列表/审批/队列/首次发送/IPC 61 项、Rust 审批历史状态 1 项、删除与运行互斥 5 项、项目历史/重启/迁移/分页 6 项、桌面项目移除 IPC 1 项通过；TypeScript 与 Vite 资源构建通过。使用临时数据库及虚拟服务配置，未访问真实 SSH/模型；未完成实际 WebView 截图验收，未提交或推送。有后端和迁移修改，需要重新编译启动桌面应用。

### 最新工作区：延迟建会话、项目菜单、关联标记及历史删除（2026-10-16）

- 最新侧栏反馈：历史标题显式使用界面字体与至少 500 字重；长标题默认省略，悬停/键盘聚焦时按实际溢出宽度缓慢向左展示，末尾停留后往返，离开复位。短标题不滚动，减少动态效果偏好保留静态省略和全文提示。历史组件既有 12 项及 TypeScript 检查通过；未做实际 WebView 截图验收，未提交。
- 最新视觉反馈：终端 Agent 关联标记缩为居中的 18px 无底色图标，去掉文字徽标与纵向拉伸；页签和地址栏保留悬停说明与可访问名称。仅展示调整，终端绑定不变。既有终端回归 13 项及 TypeScript 检查通过，未做实际 WebView 截图验收，未提交。
- 选择项目/点击项目“＋”时仅连接并原生核验 SSH，不创建持久会话。客户端首次发送才调用 project_chat，随后使用该会话和准确终端绑定工具；历史列表过滤尚未实际对话的空准备记录，保留已有原文及旧任务。项目下拉改为自适应宽度的轻量触发器，项目名与地址上下两行；Portal 浮层显式指定界面字体与字重，修复继承默认字体。
- 右侧页签和工具栏通过准确 terminalSessionId 显示 Agent 标记，不按 connectionId 批量标记；当前目标存活时自动绑定逻辑不因同配置页签顺序而换目标。
- 新增原生 delete_chat 与桌面 IPC 路由及前端确认入口。按用户、模型、会话校验，复用生成/工具的排他准入槽并在 Immediate 事务中核查持久 running、admitted 和写入状态。删除该会话的原文、轮次、引导、工具结果/字节、检查点/Responses 窗口、审批/授权和项目关联；保留项目、模型、SSH 配置与其他会话。删除旧 general 根时，将独立 server 会话保留为自有根，不连带删除其聊天。生成期间前台禁用删除，服务端拒绝竞争请求；删除当前空闲记录回到文字草稿，不断开 SSH。
- 验证：前端输入/项目准备、列表、绑定、终端和 IPC 65 项通过；Rust 删除完整性/运行互斥 3 项、项目历史/迁移/分页 5 项、桌面原生删除路由 1 项通过；TypeScript、Vite 资源构建和差异检查通过。删除完整性在启用外键检查的临时库中核对，模型与 SSH 均未访问真实服务。未做实际 WebView 截图验收，未提交、推送或打包；有 Rust 修改，开发应用需重新启动编译。

### 最新工作区：项目新建入口与输入区连接选择（2026-10-16）

- 项目右侧“＋”改为常驻显示。输入框上方增加与截图一致的归属栏：新对话默认文字，可选择已在历史中创建的 SSH 项目；项目候选共享侧栏成功加载的列表，不展示其他已保存连接，也不增加重复查询。
- 选择项目复用原有 `newProjectChat / ensureProjectTerminal` 的连接、原生身份核验与项目会话创建流程；已有消息后只显示固定归属，生成、排队或切换时不能改目标。连接失败保留草稿，允许重选与重试；正常历史导航仍清空旧草稿。修复立即失败时 React 合并状态更新可能导致选择锁未释放的问题，并增加回归。
- 验证：输入/队列、历史列表、项目连接与恢复共 21 项通过，TypeScript 与 Vite 资源构建通过。仅本地组件/逻辑验证，未连接用户 SSH 或模型，未完成实际 WebView 视觉验收。无后端生产代码或迁移变更，未提交、推送或打包。

### 最新工作区：会话侧栏视觉重构（2026-10-16）

- 参考用户的 Codex 桌面截图，并补读固定提交 f82e36592278a0a782fb0d0fc97e5fa10e832ac2 的 `tui/src/resume_picker.rs`：参考标题截断、行选中背景、游标分页与跨页去重；CLI 源码不包含桌面侧栏 CSS。参考副本补入工作区 `.codex-tmp/c5-codex-reference`，不作为产品代码提交。
- 客户端会话列表去掉逐条历史图标、蓝色文字、边框与阴影，改为中性文字、左对齐标题、浅色选中行及统一缩进。项目文件夹与名称左对齐，悬停/键盘聚焦展示新建入口；顶部集中普通新对话、创建项目和刷新操作。分组间增加留白，列表保持单一纵向滚动，主题与用户字体配置沿用现有变量。
- 每组默认显示 5 条，显示更多先展开已加载数据，再使用原游标分页并按 sessionId 去重；支持收起，当前选中的已加载旧会话保留在预览中。项目新建、服务器绑定、会话原模型/历史恢复和生成期间切换锁定逻辑保留；无新增后端接口或数据库迁移。
- 验证：会话/项目恢复 13 项、侧栏尺寸/收起回归 11 项、TypeScript 检查、Vite 资源构建和差异检查通过。Tabbit 稳定启动器在沙箱外仍退出码 1 且无诊断输出，未做浏览器截图或真实 WebView 验收。未连接真实 SSH 或模型，未打包、提交或推送；原有 icon.icns 变更未修改。
- 前一轮提交已完成：客户端 test-rust-ui/3a9b22e，Rust main/52816c1；本节为其后的未提交 UI 与说明修改。下一项仍为 E1。

### 最新工作区：模型设置输入与认证布局（2026-10-16，保存与认证定向验证通过）

- 客户端将 TokenBudgetInput 泛化为 ModelNumberInput，保留原 token 档位并为请求超时、最大响应提供同样的“精确数值输入 + 预设选择”。超时仍为 1–600 秒；响应仍为 1024–8388608 字节，KiB/MiB 档位保存精确字节数。安全预留、自动整理比例和 Temperature 在常规宽度下同排，窄容器换行。
- 身份认证简化为单个密码输入框与右侧“使用 API Key”开关；配置存在时提示“已配置，留空表示不修改”，没有密钥时提示未配置。不回显保存的密钥，不再提供密钥操作下拉。保存新输入发送 set，留空发送 keep；关闭开关不清空密钥，更改基础地址保留原密钥的安全校验仍有效。
- Rust ModelRepository::runtime 在 requireApiKey=false 时不解密、不返回运行密钥，确保关闭认证后 HTTP 请求不携带已保存的 Bearer token；加密凭据保留，重新启用时复用。未修改数据库结构。
- 用户随后要求简单测试保存与认证：前端模型设置 9 项、Rust agent_models 21 项及 TypeScript 检查通过。使用虚拟密钥、临时数据库和本机 HTTP 服务确认留空保留、输入替换、关闭认证不发送 Authorization、重新启用恢复密钥、替换后重启仍使用新密钥；同时验证公开配置不回显密钥和变更地址必须显式处理原密钥。调整旧测试以匹配工具默认启用及禁用认证不解密的现行行为。未做构建或 GUI 验证；未提交。

当前状态已更新至文首 2026-10-20 总览（v0.2.2 / schema 24）；下述阶段状态与测试数字保留为历史证据。E4 已按用户要求取消，不再实施。E5 首次 A 17/18、B 18/18 的原始统计保留；E6 的回归、真实服务及压力复测结果和用户 GUI 确认见顶部验收记录。后续按用户需求优化产品，用户自行打包。
最新桌面执行模式：工具使用绑定终端的当前 POSIX Shell（继承已完成的 sudo 切换），工具输出走每次调用专属的描述符/SSH 收集通道；终端展示真实命令、结果和真实提示符，隐藏内部协议。下文早期“独立 exec 不继承终端”记录属于历史阶段，以末尾最新章节为准。
用户要求：参考 Codex CLI 优化上下文和工具决策；先列清单，每完成一项更新标记，供换窗口后续接。当前不打包。前端按用户最新明确要求在 master 同步修改，覆盖早期 test-rust-ui 约定。
源码参考与设计依据：[CODEX_AGENT_REFERENCE.md](CODEX_AGENT_REFERENCE.md)。本文件是进度唯一入口，完成标记必须有代码与验证证据，不以“已设计”替代“已实现”。

### 最新工作区：工具默认启用与统一授权入口（2026-10-15，未验证）

- 移除模型“函数工具能力”和聊天“启用 SSH 工具 / 切换文字对话”开关。模型配置归一化将旧 toolCalls=false 适配为 true；无需重新保存。工具协议目前仍仅支持 Chat Completions，Responses 保持文字对话。
- 已连接服务器对话自动使用原 scope 的终端，发送前准备并核验绑定；不随可见终端页签改变服务器。离线可讨论历史，重连后工具自动可用。该默认不等于自动批准，默认审批仍为 confirm。
- 对照本地 Codex CLI 的 permissions_menu.rs / approval_presets.rs，并核对官方 core/src/tools/handlers/mcp.rs 与 core/src/session/mcp.rs：审批方式为任务级，涉及命令、文件、网络及需要授权的 MCP 操作；Skill 调用遵循底层工具权限，并非独立的权限绕过入口。
- ToolExecutor 新增统一 authorize 挂钩，所有工具在进入执行/未知结果标记前经过它；默认拒绝。ToolSession 统一读取当前任务策略，安全的已捕获输出读取直接放行；SSH 继续使用同一策略下原有持久审批、CAS、撤销及原子准入。任务策略设置不再要求 SSH target。
- 独立审查提取为工具无关的 ToolReviewAction / review_tool_action：输入工具名、准确参数、目标、任务约束和最近用户消息；保留无工具、低风险才自动允许及失败转人工。前台文案改为“工具审核边界”。
- 当前产品执行器仍只有 SSH 与输出回读，没有新增 MCP 或 Skill 执行器。后续工具必须提供原生授权、持久审批和准入适配；仅注册函数不能获得权限。通用审查函数及授权入口已可复用，不表示所有后续适配器已经实现。
- 按用户要求只修改代码和说明；同步调整受接口变化影响的既有测试源，但未运行测试、类型检查、构建或 GUI 检查。此段为未验证工作区状态，不覆盖此前已通过验证的历史记录；未提交。

### 最新补充：命令说明与生成中修改审批（2026-10-15，未验证）

- 审批卡片移除命令外框和单独的环境标签，无输出超时紧随命令；展开说明新增命令用途。ssh_execute 可携带最多 240 字的 explanation，持久化在原命令 JSON 中，无需数据库迁移或额外模型请求；该字段是展示数据，不影响精确命令短期授权的匹配键。旧命令保持兼容，简单 df -h / whoami / pwd 有明确说明，其余未附说明时不猜测脚本用途。
- 解除生成期间的前端审批模式锁；等待审批时模式入口移到卡片下方，避免隐藏输入框后无法切换，也避免菜单被卡片滚动区裁切。停止处理中、切换会话及保存请求期间仍锁定，防止错用任务。
- 切换策略后，未使用的自动/短期授权回到待审批；fullAccess 放行当前任务未过期的 pending，auto 重新审查 pending，confirm 等待人工；已准入命令不被模式切换中断。自动审查监控模式及 revision，切换时取消旧审查，不将旧结论用于新策略；准入复核保持不变。
- 参考本地 CLI 权限菜单，并核对官方 [CLI /permissions 说明](https://developers.openai.com/codex/cli/slash-commands)：会话中可更新权限，后续操作使用新设置。按用户要求本轮未运行测试、类型检查、构建或浏览器检查，未提交。

### 最新修复：工具定义失败导致权限读取失效（2026-10-15，未验证）

- 只读排查本机最近失败记录：两条为 AGENT_INVALID_REQUEST；当前任务已 cancelled，策略仍指向之前的任务。根因是上一轮新增 explanation 为可选属性，与现有闭合 Schema 校验器“properties 必须全部 required”的约束不一致，导致 SSH 工具在调用模型/SSH 前注册失败。
- 将 explanation 纳入必填定义；旧调用缺少该纯展示字段时，原生归一化补充“未提供命令用途说明，请核对完整命令”，不改命令、不授予权限。工具定义编译前置到 preflight，配置错误在创建执行轮次之前暴露。
- 权限配置读取与执行授权分离：审核模式现按当前用户/对话保存，普通对话与服务器对话共用 conversation_policy / set_conversation_policy；没有 SSH 或活动任务也可设置。同一对话重绑或更换任务保留审核模式，精确命令授权和一次性批准仍按原任务与目标隔离。SSH 的 policy 与实际准入仍验证活动任务和绑定身份。
- 前台刷新保留最近成功读到的策略，仅任务/目标范围变化时清空；失败明确显示“权限读取失败”及重试，不再显示成一直读取。当前请求失败保留原始错误返回，只有重复读取旧请求才提示历史失败；前台增加请求/工具定义校验失败的说明。
- 本轮仅只读诊断状态及修改代码，未运行测试、类型检查、构建或 GUI 验证；未修改用户数据库、未重发任何失败命令、未提交。

Rust 重构交接、已完成工作与运行方式见 [WORK_HANDOFF_2026-10-14.md](WORK_HANDOFF_2026-10-14.md)。

## 工作区与约束

- 历史 E2 基线为客户端 test-rust-ui/098df3b、Rust main/8097e04（schema 19）；不是当前工作区状态。当前分支、schema 和已提交基线见文首总览。
- 项目、参考源码和工具位置由本机环境决定；跨电脑续接时按实际位置核对 Cargo 依赖，不沿用旧电脑的绝对路径。
- 本轮不打包；已有安装包不代表本轮新增 Google 源码，v0.2.2 发布准备与实际包级验收分别记录。
- 保留各仓库已有未提交变更；不连接用户远端、不执行实际变更命令来做测试。
- 里程碑、测试结果和续接状态统一维护本文件，不再额外创建逐阶段验证文档；commit note 直接发给用户，不保存到文档。
- SSH 任务绑定目标，切换 UI 页签不能改变在途任务目标。权限来自可信应用状态，模型摘要不产生权限。
- 当前主要验证是临时数据库/回环协议测试；真实模型、完整 GUI 和用户远端验证单独记录。
- 后续前端变更需要同时做好功能和样式，优先复用客户端 master 分支的设计系统、Select/Ui 控件与主题参数；相关界面需检查明暗主题、窄窗口和实际交互。
- 最新 UI 要求覆盖此前撤销布局时的约束：参考用户新截图，左侧上下分区放置会话历史和 SSH 连接，中央 Agent 对话，右侧终端；本地工作区、模型设置、外观和关于与“终端”同级放在左侧主导航，取消设置页内重复分类栏。只调整页面，沿用现有业务逻辑。

## 实施清单

### A. 可续接基础与上下文内核
- [x] A0 建立持久进度文件、源码参考和新窗口续接入口。
- [x] A1 Rust Agent 会话/任务模型：目标绑定、版本、约束，跨会话隔离。
- [x] A2 SQLite 原始事件日志：顺序追加、幂等请求 ID、工具调用/结果关联、取消与未知结果、重启恢复。
- [x] A3 请求预算器：模型窗口、输出/系统/工具/媒体/安全预留、UTF-8 估算、完整工具组选择和硬上限。
- [x] A4 检查点持久化：结构化目标/约束/证据/待办，版本快照与比较提交，压缩中用户变更/取消不能被覆盖。
- [x] A5 工具完整输出存储与按范围回查；只将有界结果投入模型窗口。
- [x] A6 内核测试：多会话、错误主机、任务继续、重复副作用、超限、过期检查点、重启与保留原始证据。内核组合回归已通过，用户已手动确认勾选；D3/D5 已完成本地模拟 SSH 的真实协议副作用去重；用户远端验收仍归 E6。

### B. 模型配置与基本对话
- [x] B1 模型适配器与配置：供应商能力、模型窗口、API 地址/密钥安全保存、基础参数校验。OpenAI 兼容 Chat Completions 非流式适配与回环测试完成；前端配置入口已由 B4 接入。
- [x] B2 对接 agent 配置列表、create_session、chat_stream 和 stop_chat；保留现有前端事件协议。基础文本流式对话与本地 IPC/Channel 测试完成，UI 入口已由 B4 接入。
- [x] B3 流式取消/超时/错误、同会话并发隔离、历史与恢复，不将 EOF 当成功。持久轮次、请求幂等、停止/完成事务排序及租约恢复已实现并通过回环测试；可见历史面板已由 B4 接入。
- [x] B4 前端模型设置和 Rust AI 入口；已在 B1–B3 后端测试基础上启用文本对话、模型切换及原生历史，配置桥和前端回归通过。
- [x] B5 真实回环模型协议联调，前端解析与停止/恢复回归。Rust 55、原生 IPC 15、前端 48 项及构建/静态检查通过；用户本轮免除浏览器连接与截图前置要求，真实 WebView 视觉验收留待 E6，详见本文件 B5 完成记录。

### C. 检查点压缩
- [x] C1 普通模型语义摘要适配器与摘要结构校验，不开放副作用工具。本阶段返回经校验的摘要草稿；13 项专门测试、相关 Agent 97 项及原生 IPC 15 项通过，自动触发/低水位/提交已由 C2 接入。
- [x] C2 自动压缩状态机：当前文字模型请求前自动检查、较低水位、有限重试和检查点提交；工具批次预算边界已提供并验证，真实工具循环已由 D4/D5 接入。失败保留日志/旧检查点；Agent 112、桌面 IPC 17、前端 51 项通过。
- [x] C3 支持的 Responses 供应商远端压缩能力分支；完整保存/复用返回窗口，用供应商输入计数验证预算，绑定原模型/接口/凭据，前端明确配置能力。专测 17、相关 Agent 130、桌面 IPC 19、前端 58 项通过。
- [x] C4 压缩后重新注入实时目标、环境、可信授权与最新用户请求。宿主共享真实终端身份/生命周期，当前文字权限不从摘要产生；Agent 136、终端 22、桌面 IPC 20、前端 63 项及构建/静态检查通过，详细记录见本文件 C4。
- [x] C4R 持久项目/作用域与重启恢复：schema 9/10 保留原日志/检查点/窗口，同身份重连接续、离线文字讨论、不同项目隔离。Rust 173、桌面 IPC 22、前端 74 项及构建/静态检查通过；随后按新参考图调整为左侧会话/SSH、中央对话、右侧终端及统一主导航，见末尾 UI 记录。
- [x] C5 前端上下文占用与真实压缩事件：最近输入快照、计数来源、实际检查点次数及进行中/成功/失败/取消/中断状态，schema 11 持久恢复；Rust 定向 8、原生 Channel 1、前端 21 项及 TypeScript/Vite/严格 Clippy 通过。
- [x] C6 当前文字阶段的跨整理约束、新消息插入、模型缩窗、超大输入/附件拒绝边界与中断恢复。补齐 4 个连续场景，修复未回复用户补充遗漏与中断误标失败；自动整理 25、状态 4 项及严格 Clippy 通过。实际图片/PDF 输入与媒体计数尚未接入，验证归 E1，不能把拒绝测试当作媒体能力已完成。

### D. 工具决策与执行
- [x] D1 主模型工具循环与严格参数校验：Chat Completions 非流式原生内核、宿主工具白名单、闭合 schema 子集、本地参数校验、有界循环与取消/未知结果保护；9 项定向测试及严格 Clippy 通过。原生持久执行准入/日志及批次整理由 D3/D4 接入；桌面工具已由 D5 接通；Responses 工具协议仍未开放。辅助意图标签不作为授权或强制路由。
- [x] D2 SSH 工具执行绑定 connectionId/terminalSessionId/taskId；实际目标状态复核。新增可注册的 ssh_execute 原生处理器、同认证传输的独立 exec 通道、任务/归属/主机身份/连接配置复核及未知结果保护；默认授权拒绝。新增 8、D1 9、既有终端 22 项及严格 Clippy 通过；前端入口已由 D5 接通。
- [x] D3 命令策略和明确执行确认；持久授权范围，取消/结果未知/恢复时不盲目重执行。新增 schema 12 的原生审批请求、任务/命令精确授权、策略 CAS、原子 admitted 准入、完成回执与 unknown 永久阻断；前端审批入口和结果展示已由 D5 接通。
- [x] D4 工具批次、输出回查与错误分类；执行失败不自动判为意图偏差。原生 ToolSession 接通整批日志、完整 SSH 输出、read_tool_output、批次整理与分类结果；新增 12 项端到端回归通过，桌面产品入口已由 D5 接通，详见末尾完成证据。
- [x] D5 前端执行确认/绑定服务器/工具结果 UI 和真实本地 SSH 协议回归。显式启用、审批/拒绝/撤销、固定目标、结果/原始输出和历史恢复已接通；工具 18、native IPC 24、前端 54 项通过。真实 GUI/模型/用户服务器仍待 E6。

### E. 附件、记忆与高级 Agent
- [x] E1 图片/PDF/文本附件到模型的转换、能力/预算/所有权检查。schema 17 的不可变会话快照、原生模型媒体输入、历史/整理/回查与前端系统选择器已接通；专项 11、前端 69、native 完整链路单项及相关兼容回归通过。PDF 仅提取文字，OCR/Office/拖入尚未支持；剪贴板图片和 Windows 复制文件已在 E1 补充接通。实际 WebView/模型/远端仍待 E6。
- [x] E2 长期记忆与短期任务上下文分离，来源/更新时间/适用目标。schema 18 独立持久保存用户确认的偏好/经验，全局/精确服务器范围、有界模型注入、编辑/停用/删除及前端原生入口已接通；Rust 194、前端 48、原生派发 1 项通过，既有 POSIX 1 项忽略，详见顶部 E2 记录。
- [x] E3 子 Agent 与工作流、MCP、Skills，继承目标和授权边界。单层有界子任务、MCP 两种传输、Skills 渐进读取及桌面入口已接通并完成本地验证，详见顶部记录及 E3_EXTENSIONS.md。
- ~~E4 旧数据导入设计与迁移验证（连接、聊天、附件、记忆）~~。**用户明确取消，不再实施（2026-10-19），并非已完成迁移。**
- [x] E5 同模型固定任务 A/B 评测：完成全新 36 场景及完成率、约束/事实/目标、token、延迟、额外调用、重复执行统计。首次 A 17/18、B 18/18；唯一失败项独立补测通过，原始统计和风险保留。完整报告见 evals/e5/results/full-ab-2026-10-18/REPORT.md。
- [x] E6 全部前后端回归、真实模型/GUI/远端验收；自动结果与用户确认边界见顶部记录及 evals/e6/results/2026-10-18/REPORT.md。用户要求时再生成安装包。

## 当前进度与下一项

以文首“当前状态总览”及最新功能记录为准：A/B/C、D1–D5、E1–E3、E5/E6 已完成，E4 取消；当前 v0.2.2 / schema 24，前端 master / Rust main。Google Gemini 原生协议、模型目录查询和 DeepSeek 策略已完成本地定向验收；后续按用户需求处理产品优化及真实供应商/中转、桌面视觉和包级升级验收，不重复实施已完成的接入或 E1/E2。

已知产品边界：扫描 PDF/OCR/Office、拖入上传、本机通用命令、后台/递归子任务仍未开放；子任务/MCP/Skills 受现有授权与目标约束。Responses 工具续聊已经接入，不再按早期阶段视为只支持文字。用户自行打包；新供应商真实接口与包级升级验收单独记录。

## 验证与完成记录

- A0：2026-10-13，创建本文件；已有源码研究报告保留明确提交和实现边界。
- 既有基线：后端 60 项测试、前端 69 项＋1 项真实附件联调、原生 15 项通过。页签修复另有 25 项相关回归和前端生产构建通过。

## 新窗口如何继续

先阅读本文件，再阅读 CODEX_AGENT_REFERENCE.md 与 MIGRATION.md；核对实际源码、git status 和最近验证记录。
只推进尚未勾选的第一组必要工作，不重复已通过的验证、不将旧进度当作新功能已经启用。
对一项实现并通过适当测试后，更新标记、代码位置、命令/结果与下一项。
若有用户新指令，以新指令更新本文件的范围与约束。

### A1–A4：2026-10-13 完成

- 代码：src/agent/model.rs、repository.rs、budget.rs、mod.rs；migrations/003_agent_context.sql，Store 初始化加载 schema 3。
- A1：持久会话、不可变任务目标快照、约束、乐观版本。目标字段形状已校验，真实终端状态复核仍待 D2。
- A2：事务追加事件、请求 ID 幂等回执（inserted=false 不得再次执行）、调用/结果唯一关联；迟到结果保留原 taskId。单事件限 64 KiB、同时未返回的工具最多 64 个。超大输出文件化仍待 A5。原始事件不被检查点替换。
- A3：上下文/输出/安全预留、固定提示/工具/必需请求/媒体成本计入预算；UTF-8 启发式估算包含工具参数；完整工具组选择，超限需压缩，不直接拿裁剪结果冒充完整模型上下文。精确媒体成本和模型 usage 校准待 B1。
- A4：检查点必须保存相同目标、用户约束、未返回调用与显式 unknown/cancelled 结果；证据引用限定本会话；新消息、取消、目标变更或另一检查点提交后，旧版本提交被拒绝。这里只保存/校验检查点，模型摘要生成和自动压缩仍待 C1–C4。
- 验证：cargo test --test agent_context，17/17 通过；cargo clippy --all-targets -- -D warnings 通过。包括真实 SQLite 关闭重开与两个线程同时写入。
- 前端：本批没有新 HTTP/聊天能力，继续禁用 Rust AI 入口；不把内核完成显示为 AI 已可用。
- A6 尚未勾选：已有 17 项内核回归，但完整输出回查、恢复时实际工具副作用去重仍需后续集成测试。

- 首批全量回归：cargo test，77/77 通过；cargo fmt --check 通过。既有 SSH、SFTP、附件与基础 API 全部通过；未连接用户实际远端。

- 桌面集成：cargo test --manifest-path src-tauri/Cargo.toml --lib rust_preview_tests -- --test-threads=1，3/3 通过（其余 12 项未在本批重复运行）。
- 历史阶段记录：首批 A0–A4 完成时的游标为 A5；当前 A5 也已完成，最新游标见本文件末尾。

### A5：2026-10-13 完成

采用 SQLite 64 KiB 分块存储原始字节，避免新增磁盘目录和文件锁恢复机制。事件保存有界预览及输出 ID/长度/SHA256 引用，输出和既有会话库一起备份。模型层与 HTTP 尚未接通，不开放用户可见 AI 入口。
验证完成：大输出分块、跨块/Unicode 字节回查、会话/调用归属、幂等捕获、取消/失败/过期恢复、配额、事件引用与检查点后回查。
- A5 当前证据：src/agent/output.rs、output_writer.rs、output_model.rs 和 migrations/004_agent_outputs.sql 已实现；tests/agent_outputs.rs 17 项及原有 agent_context 17 项通过，Clippy -D warnings 通过。最终后端全量 94 项、桌面桥 3 项回归均通过，A5 已完成。
- A5 边界与接口说明：AGENT_OUTPUTS.md。保留原始二进制，范围读取校验块摘要；模型只使用预览/引用。未新增 HTTP 或用户界面入口，实际模型/执行器未接通。
### A5 最终验证与续接

- 实现：OutputWriter 有界流式捕获；SQLite 固定数据块及逐块/整路 SHA256；每次回查最多 64 KiB；约 4 KiB 的头尾预览及原始字节引用。空输出和无效 UTF-8 均可保留原始字节。
- 一致性：输出引用与结果事件事务关联；核对 session/user/callId/元数据；迟到结果保留原任务；引用后的输出不可丢弃，检查点不删除证据。旧 ToolResult 默认 outputs=[]。
- 资源边界：单路 16 MiB、会话 128 MiB、全库 512 MiB、最多 10000 项/4 路捕获、120 秒无写入过期；完整边界见 AGENT_OUTPUTS.md。
- 测试：cargo test，94/94 通过（新增 agent_outputs 17，原有 agent_context 17，以及 SSH/SFTP/附件/API 等 60 项）。
- 静态检查：cargo clippy --all-targets -- -D warnings、cargo fmt --check 通过。
- 桌面集成：cargo test --manifest-path src-tauri/Cargo.toml --lib rust_preview_tests -- --test-threads=1，3/3 通过；其余 12 项未重复运行。
- 数据库：新增 004_agent_outputs.sql，schema 4；既有事件和连接/附件数据保持可读。配额为逻辑字节，SQLite 文件不保证即时缩小。
- 本批没有模型/HTTP/GUI 回查入口，前端保持 Rust AI 禁用；仍未打包、未调用用户远端或真实模型。
- A6 尚未完成：需要内核综合场景验证；实际 SSH 副作用去重和恢复核实仍依赖 D3 执行器，不能仅凭回执测试宣称已完成。
- 最新续接游标：A6，然后 B1。A0–A5 已完成；没有正在后台执行的测试或重构。继续维护本清单，每个完成项附代码与验证证据。

### 2026-10-13：内置 IPC 传输改造

用户要求内置业务使用 invoke/Channel，消除 WebView HTTP/1.1 长轮询连接槽瓶颈。本项是传输适配，不推进 Agent A6/B1，不启用 Rust AI，不生成安装包。

- 后端：src/services.rs 提供共享 Store/TerminalManager/SFTP/Files 生命周期，HTTP 与 IPC 使用相同实例；终端新增最多 64 KiB 的 UTF-8 输出读取，附件新增原生 AsyncRead 流式入口并共用存储校验/提交。
- 客户端：src-tauri/src/backend_ipc.rs、src/api/builtin.ts；SSH 配置和操作、健康与附件元数据走 invoke，终端输出走带逐帧确认的 Channel。Rust 文件桥直接使用内置 SFTP 服务；附件原生选择/保存，内容不经过前端。
- 远程兼容：HTTP 协议保留；src/api/nativeFetch.ts 转发原生 HTTP 及有界流式响应，WebView 不再承载远程终端长轮询/Agent 流。浏览器和远程 multipart 保留 fetch。
- 验证：客户端原生 21/21，包括 8 个真实回环 SSH 窗口、输出确认/Unicode、无 HTTP 的 SFTP 直连；前端新增传输测试 7/7；附件原生测试 2/2。最终后端全量 96/96、fmt 检查通过；后端 Clippy -D warnings 在新增流式入口后已通过。
- 已知已有问题：前端 mjs 回归 177/178，正式版 bundle 名称断言与 Rust Preview 配置不一致。HEAD 中已有相同配置和断言，本批未修改名称或此测试。
- 文档：兄弟客户端 docs/ipc-transport.md。旧电脑工具/安装包路径只作为历史记录；本机项目路径已更新。
- 实际 GUI 对话框和用户真实远端未验证。续接仍从 Agent A6，然后 B1。

- 最终前端 TypeScript/业务回归 83/83，生产构建通过；原生参数整理后 Clippy 无新增警告，仍有原项目的 3 项警告（旧 TLS refresh、连接测试嵌套 if、聊天历史排序）。未更改这些无关代码。


### 2026-10-13：第 10 个 SSH 窗口附近失败的修复

用户试用报告第 10 个左右的新窗口返回 SSH_TRANSPORT_FAILED。原实现每 connectionId 仅一条已认证传输，与 OpenSSH 默认每连接 10 个会话的限额吻合；实际用户服务器配置尚未核实。

- src/terminal/mod.rs 改为有界连接池；默认每传输 8 个窗口、每配置 32 条缓存传输，保留全局 256 个窗口上限。通道名额涵盖未确认和迟到通道，关闭最后一个窗口后回收传输。
- 对已有传输的新通道被 AdministrativelyProhibited/ResourceShortage 拒绝，记下已观察容量并在新传输上最多重试一次。重试发生在 PTY/shell/startupCommand 之前；新传输仍拒绝时返回 SSH_CHANNEL_OPEN_REJECTED，不无限重连。
- 更新配置保留旧窗口；关闭/取消/传输失效只影响对应传输上的窗口。服务退出清理缓存传输与窗口资源。
- 新增 24 窗口/服务器限额 10、限额 2 与启动命令只执行一次、连接池容量/故障隔离、限额 0 无无限重试回归。
- 用户要求本次只改代码、自行测试：不再运行后续测试或启动程序。该指令到达前终端 22 项已通过；收尾后的版本未重新运行全量测试。GUI/用户服务器仍由用户验证；未提交/打包。


### 2026-10-13：桌面整体运行，不再监听本地 HTTP 端口

- embedded_backend.rs 不再绑定 TcpListener 或调用 serve_services，只初始化同进程 Services::native，并在退出/重新初始化时清理运行时与业务资源。
- 健康检查、SSH/SFTP、附件操作全部通过 IPC；每次初始化生成独立身份，旧实例请求不能误路由到新实例。附件为原生文件引用，HTTP 签名链接仅用于独立 server。
- 本地设置移除端口和下载链接有效期，继续沿用数据目录与旧配置；远程 HTTP/HTTPS 档案不变。前端打包为静态资源，仅开发时 Vite 使用 1420。
- 按用户要求仅修改代码、现有测试适配和文档，未编译/构建/运行测试/启动 GUI/生成安装包/提交 Git。此前通过记录不能用于证明本轮改动已通过。
- 不推进 A6/B1，不启用尚未迁移的 Rust AI 功能。


### 2026-10-13：移除桌面远程后端模式

用户明确要求桌面是一个整体，不提供外部 ShellPilot 后端。客户端已移除远程地址/证书/连接档案及原生 HTTP 转发，SSH 终端输出统一 Channel，SFTP 桥直接调用应用内 Manager，附件只使用原生文件句柄。旧远程活动档案不影响启动，旧 HTTP URL 不能路由应用请求。

与目标服务器的 SSH/SFTP 网络通信保留；Agent 的模型请求继续属于后续本地 Rust Agent 迁移。独立 HTTP 测试适配不是桌面部署方式。不推进 A6/B1，不启用 AI。源码、测试适配和文档已更新；遵照用户要求未编译/测试/启动/打包/提交。


### 2026-10-13：移除旧独立 HTTP 启动入口

用户要求应用作为整体，无独立后端部署。删除 src/main.rs，移除 Cargo [[bin]] 并设置 autobins=false；本项目只作为 shellpilot_server 业务库供 Tauri 引用。cargo build --lib/cargo test 可独立编译或测试库，无需运行前端。库内历史 HTTP 协议测试适配保留，但不提供独立可执行程序。README 和客户端说明已更新；按要求未编译/测试/启动/打包/提交。

### A6 内核组合验证与任务绑定补强：2026-10-13

用户最新要求：回到 Agent 主线，本轮做简单测试。此前“只改代码、不运行测试”的限制不再适用于本轮；仍不打包、不提交、不连接用户服务器或真实模型。

- 代码：src/agent/model.rs 新增 ToolBinding，绑定由应用可信快照构造，不从模型参数推导执行权限。src/agent/repository.rs 新增 append_tool_call 与 tool_call_binding。
- 准入：在同一 SQLite 事务内核对当前活动 taskId/connectionId/terminalSessionId。模型参数中出现路由字段时也必须匹配绑定；错误任务、主机或终端均原子拒绝，不追加事件或改变版本。
- 恢复：通过原始 TaskStarted 事件读取旧调用目标，不使用当前 UI 页签或新任务目标。重放请求返回 inserted=false 的原始回执，目标绑定不匹配则拒绝。该回执和历史绑定均不是执行授权；D2/D3 还需检查实时 SSH 状态与可信用户授权。
- tests/agent_kernel.rs 新增 6 个组合场景：错误任务/主机/终端与模型路由参数、并发重复请求和重启回执、切换目标后的迟到完整输出与检查点恢复、取消/新约束/过期检查点、跨会话与用户输出隔离、小模型窗口完整工具组与必需输入超限。
- 验证命令：cargo test --manifest-path E:/project/AIShellTerminal/ai-ssh-terminal-server-rust/Cargo.toml --offline --test agent_kernel --test agent_context --test agent_outputs。结果：6+17+17，共 40/40 通过。
- 前端：在客户端目录执行 node node_modules/typescript/bin/tsc --noEmit，退出码 0。未修改前端业务或开放 AI 功能。
- 格式化：rustfmt --edition 2021 --config skip_children=true src/agent/mod.rs src/agent/model.rs src/agent/repository.rs tests/agent_kernel.rs 成功。
- 环境与修正：首次沙箱运行的两项临时数据库重开测试因 Windows 路径权限失败，正常用户权限重跑通过。新组合测试中的跨会话输出拒绝断言修正为既有 NotFound 语义，未改变输出存储的错误约定。
- 边界：重复执行计数是模拟派发，未执行真实 SSH 命令，不能宣称 D3 已完成。未跑全量 Rust/Tauri 测试、完整 GUI、真实模型或打包。随后用户已手动确认 A6 内核阶段勾选；D3 接通后回补真实执行器联验。

### B1：2026-10-13 完成

- 用户要求：继续 B1，做简单测试，完成后像 A1–A5 一样回填勾选；保留用户手动勾选的 A6。仍不打包、不提交、不使用真实密钥/模型或用户 SSH 服务器。
- 代码：src/agent/provider/{config,repository,protocol,transport,mod}.rs；migrations/005_agent_models.sql；Store 初始化加载 schema 5；tests/agent_models.rs。客户端 src-tauri/Cargo.lock 仅同步内嵌库新增的 reqwest 依赖。
- 配置：供应商协议、显式能力、模型 ID/上下文窗口、输出与安全预留、参数支持、URL/代理前缀、超时与响应大小校验。按用户隔离、每用户最多 32 项、乐观版本保存/删除。不根据模型名称推断能力和窗口。
- 密钥：Windows DPAPI 加密，绑定 Windows 账户与用户/模型配置 ID；公开数据只有是否配置/就绪标志。keep/set/clear 分开，携带旧密钥更改 API 基础 URL 必须明确替换或清除；运行配置不序列化，Debug/错误不泄露密钥。非 Windows 不降级为明文存储。
- 协议：OpenAI 兼容 Chat Completions 文本与 function 工具的非流式请求；完整历史工具组与 A3 预算校验；超限拒绝，返回压缩提示和响应 usage。关闭重定向与自动重试、响应逐块限额、整体超时与取消；HTTP 错误正文不透传，截断/过滤/无效响应不判完整成功。没有工具执行或模型授权。
- 测试命令（Rust 项目目录）：cargo test --offline --test agent_models --test agent_context --test agent_kernel --test agent_outputs。首轮 17+17+6+17 共 57/57 通过；补充无 Content-Length 分块限额/读取超时并调整 IPv6 回环代理判断后，cargo test --offline --test agent_models 最终 18/18 通过；既有 40 项无后续业务改动。
- 静态检查：cargo clippy --offline --lib --test agent_models -- -D warnings 通过；对新增/修改模块和测试执行 rustfmt，并做针对性格式检查。
- 桌面兼容（客户端目录）：cargo test --manifest-path src-tauri/Cargo.toml --offline --lib rust_preview_tests -- --test-threads=1，5/5 通过。验证同进程库和既有本地 SFTP 桥，不连接用户服务器；未运行其余 9 项。
- 环境：首次离线编译缺少新增依赖 want，联网下载依赖后测试均离线执行；测试在正常 Windows 用户权限下运行，避免已有沙箱临时路径 canonicalize 权限问题。测试服务仅回环监听，产品库不新增监听端口。
- 说明：AGENT_MODELS.md 记录配置字段、密钥动作、调用入口、协议边界与测试命令。模型能力与窗口尚未通过真实供应商探测；token 估算仍为启发式，usage 持续校准/媒体预算留给后续阶段。
- 续接：B1 已勾选，下一项 B2；B3 流式取消/结束与恢复、B4 模型设置和 AI 入口继续保持待办。未做真实模型/完整 GUI/全量 Rust 回归或安装包。较早的 A6 未勾选文字为当时记录，以顶部清单及当前进度为准。

### B2：2026-10-14 完成

- 用户要求：接通 B2，做简单测试并给 commit note；继续回填完成标记，不替用户提交 Git。A6/B1 已有勾选保持不变。
- Rust：src/agent/chat.rs 提供按用户/配置创建会话、就绪模型列表、活动生成和停止；src/agent/provider/stream.rs 提供字节级 SSE 文本适配；Services 管理同进程 Agent 生命周期，不新增产品监听端口。
- 桌面：backend_ipc.rs 对接 GET query_ai_agent_config_list、POST create_session/stop_chat，以及 builtin_agent_subscribe/builtin_agent_ack。目标来自存活 SSH 终端与 Store 所属用户，不能由模型或前端额外 routing 参数指定。
- 前端：src/api/agent.ts 改用 invoke/Channel，沿用 text/done/error 事件与 parseStreamEvent，逐帧确认、取消只针对对应 streamId；保留 App.tsx 的 Rust AI 面板禁用逻辑，模型设置仍待 B4。
- 持久化：复用 schema 5 的 A2 原始日志与模型配置。用户消息先入库，完整 assistant 入库后才发 done；异常写取消记录，不把部分文本当完整成功。同会话只允许一个活动生成；旧目标与用户约束重新注入模型请求。
- 边界：文本与回复 16000 UTF-8 字节、回复 JSON 编码 60000 字节、SSE 行/帧 64 KiB，网络响应上限与整体超时沿用 B1。历史达到 200 条或已有检查点明确拒绝，不静默截断。附件/工具对话未接入，不执行 SSH 命令或产生模型授权。
- 流式验证：finish_reason=stop 和 [DONE] 缺一不可；截断/过滤/错误 JSON/意外工具调用失败。停止、调用方取消、工作区关闭清理当前生成；信号回执不保证供应商计算已停止。
- 简单 Rust 测试：cargo test --offline --test agent_chat --test agent_models --test agent_context，8+18+17，共 43/43 通过。包含 Unicode/CRLF 网络分片、历史续聊、EOF、停止、同会话互斥、用户/目标隔离、附件拒绝、预算/JSON 大小和非成功响应。最终补强取消后的任务约束保留后，agent_chat 8 项再次通过；另外 35 项代码未变。
- 原生测试（客户端目录）：cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1，7/7 通过，其中新增模型控制、Channel 输出确认与 stop_chat 回环测试 3 项；没有运行其余 10 项。
- 前端测试（客户端目录）：node --import tsx --test tests/agent-ipc.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts，27/27 通过；node node_modules/typescript/bin/tsc --noEmit 通过。新增 Agent IPC 6 项覆盖控制形状、Unicode/空白、done/error、取消和接收器错误。
- 静态检查：cargo clippy --offline --lib --test agent_chat -- -D warnings 通过；修改 Rust 文件 rustfmt/针对性格式检查通过。桌面 cargo clippy --manifest-path src-tauri/Cargo.toml --offline --lib --tests 通过，既有 chat_history 排序及旧 backend_process 测试进程警告保留，无新 Agent 警告。
- 环境与修正：一次静态检查的自动授权超时，普通沙箱重试因父目录配置访问失败；正常权限重试成功。Clippy 指出测试锁跨 await，已改为明确作用域后通过。没有改用户凭据或连接真实供应商/远端。
- 文档：AGENT_CHAT.md 记录原生接口、事件、参数、日志和边界；客户端 docs/agent-ipc.md 指向该文档。下一项 B3，保持 B3/B4/B5 未勾选；未做完整 GUI、真实模型、全量测试或安装包。

### B3：2026-10-14 完成

- 用户要求：接通 B3；网络中断后继续同一任务，保留已有修改。未运行被自动授权超时拒绝的首次检查；恢复后重试编译成功。客户端原有 icon.icns 修改保留，未参与本轮操作。
- 数据：006_agent_turns.sql、src/agent/turns.rs；Store 加载 schema 6，保留旧事件/输出/模型表。请求准入与用户事件同事务；终态、完整回复、usage 与原始事件同事务提交，单会话 running 唯一索引保护跨 Manager 并发。
- 会话：src/agent/chat.rs 增加 requestId、持久收据、原生历史/会话/事件分页；完成重放不需要原模型配置或密钥，不调用模型。失败/取消/中断重放返回记录，不自动重新生成。新请求可明确继续，同目标和用户约束保持绑定。
- 停止：requestId 精确匹配，准备阶段先复核用户/Agent 再信号取消；已准入轮次的停止标记与完成提交由 SQLite 排序。停止先到不提交成功，完成先到不反写取消；旧停止请求不影响下一轮。跨 Manager 通过心跳观察取消标记。
- 恢复：首个/有界增量保存部分文本；future Drop 安排 interrupted 写入，心跳每秒续租、30 秒过期。下一次 receipt/history/admission 回收过期轮次，不抢占新鲜租约、不盲目重试、不接受旧 owner 的迟到成功。上下文新版本不被旧回复覆盖。
- 流：保留明确 stop+[DONE] 才成功，新增状态化首帧前后超时、HTTP/协议/usage/容量错误。失败部分文本可回查，模型历史只纳入完整轮次；旧 B2 原始记录保留，可通过 events 读取。检查点重建和自动压缩仍待 C。
- 客户端：Agent API 新增 sessions/history/events；App 为每次生成固定 requestId，停止携带同一 ID。Channel 检查请求关联/序号，去重、缺帧拒绝并屏蔽结束后的回调；原生订阅 Drop 清理自己的状态。AI 面板未开放，B4 再接模型设置和可见原生历史。
- 测试：cargo test --offline --lib agent::，6/6；cargo test --offline --test agent_chat 最终 15/15；原有 agent_models 18、agent_context 17 通过（schema 断言更新为 6），共 56 项。核心包括停止/完成顺序、幂等重放、错误主机/用户/Agent、跨会话/Manager、租约过期及真实 SQLite 关闭重开、future Drop、错误/超时和成功后的接收失败。
- 客户端验证：cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1，9/9；node --import tsx --test tests/agent-ipc.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts，最终 32/32；TypeScript --noEmit 通过。补充结构化 native done 的 JSON 原文重读回归，避免旧包装解析改变模型文本。
- 静态检查：Rust clippy --offline --lib --test agent_chat -- -D warnings 通过；桌面 clippy --manifest-path src-tauri/Cargo.toml --offline --lib --tests 通过，仅报告既有聊天排序和旧进程测试警告；修改的 Rust 文件格式检查通过。
- 边界：租约测试人为设为过期以模拟崩溃时间，再关闭重开数据库；硬崩溃可能丢失尚未持久的少量增量。没有真实模型/GUI/用户远端/打包或全量回归。原有 A6/B1/B2 勾选保留，B3 勾选，下一项 B4。
- 文档：AGENT_TURNS.md 记录请求身份、终态排序、历史接口、恢复宽限和验证命令；AGENT_CHAT.md / 客户端 agent-ipc.md 更新续接指向。

### B5：2026-10-14 完成

- 按本机 ShellPilot 两个仓库读取交接并核对分支/基线/工作区；库的 .idea/misc.xml 保留，Cargo path 无需修改。
- 新增 Rust 协议测试 3 项、原生 IPC 组合测试 3 项、前端 IPC 3 项、界面组件 3 项；总验证 Rust 55、原生 15、前端 48 项全部通过。
- 前端 App.tsx 固定恢复中的轮次 ID 并防止重复停止，5 秒确认超时后重读权威历史；AgentPanel.tsx/ModelSettings.css 同步补等待、禁用及现有图标，保留 B4 模型设置布局。
- TypeScript/Vite 构建、Rust Clippy -D warnings、桌面 Clippy 与修改 Rust 文件格式检查通过；桌面只有两项既有告警，大 chunk 提示保留。
- B5 按自动回归勾选。没有真实桌面截图、明暗/窄屏视觉验收、真实模型矩阵或用户服务器操作，验证边界与后续实际检查保留在本文件 B5 / E6 记录中。
- B1–B4 没有重写；schema 7、稳定配置 ID、名称、DPAPI 密钥和历史关联未改变。未提交、未推送、未打包。下一项 C1。

#### B5 验证命令与结果

| 范围 | 命令（对应项目目录执行） | 结果 |
| --- | --- | --- |
| Rust 模型/对话/上下文 | cargo test --test agent_chat --test agent_models --test agent_context | 18 + 20 + 17 = 55 通过 |
| 原生桌面 IPC | cargo test --manifest-path src-tauri/Cargo.toml --lib backend_ipc::tests -- --test-threads=1 | 15 通过，其他 10 项未运行 |
| 前端解析/历史/传输/附件兼容/组件操作 | node --import tsx --test tests/agent-ipc.test.ts tests/agent-history.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts tests/chat-text-ui.test.mjs | 15 + 4 + 5 + 16 + 8 = 48 通过 |
| TypeScript | node node_modules/typescript/bin/tsc --noEmit | 通过 |
| 前端生产资源构建 | node node_modules/vite/bin/vite.js build | 通过，既有大 chunk 提示保留 |
| Rust 静态检查 | cargo clippy --offline --lib --test agent_chat --test agent_models -- -D warnings | 通过 |
| 桌面静态检查 | cargo clippy --manifest-path src-tauri/Cargo.toml --offline --lib --tests | 通过；仅既有 chat_history 排序及旧 backend_process zombie_processes 警告 |
| 修改 Rust 文件 | rustfmt --edition 2021 --check 对应测试文件 | 通过 |
| 改动检查 | git diff --check（允许 CRLF） | 通过 |

Node 使用 C:/Users/kingdee/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/bin/node.exe；Cargo 使用 C:/Users/kingdee/.cargo/bin/cargo.exe。后续按本机 PATH 或实际安装路径执行。没有 npm prebuild 图标生成、Git 提交/推送或安装包构建。

#### B5 覆盖与验收边界

- Rust tests/agent_chat.rs：多行 SSE/注释/CRLF/UTF-8 分片、JSON/代码原文与 usage；10 种 choice/index/role、工具/旧 function、拒绝、截断/过滤、非法 JSON、过早 DONE 异常；行/帧/响应 3 种超限。保留已交付文本，不误报成功或自动重试。
- 客户端 src-tauri/src/backend_ipc_tests.rs：回环模拟模型供应商经过产品 agent_stream/dispatch/ACK 和真实 SQLite，覆盖未 ACK 时停止、旧请求/ACK 隔离、显式续聊、future Drop、中断/失败记录、关闭重开及缓存重读。
- tests/agent-ipc.test.ts / tests/chat-text-ui.test.mjs：取消隔离、完成/停止竞态、迟到回调、恢复原请求不自动发送或覆盖草稿、旧会话回执不清空新草稿。修正元素夹具遍历 undefined 时重入根节点的问题。
- 原生发送端为测试接收器，前端使用 IPC/hooks/元素夹具；没有完整真实 WebView 截图。测试端口仅模拟模型供应商，不是产品 HTTP 后端。
- 用户要求本轮不处理浏览器连接，因此 B5 以自动回归完成；E6 补验：途中停止及原请求恢复、历史生成中的等待/禁用、停止与完成竞态、切换会话不跳回旧会话、关闭重开后原文/状态、明暗主题和 320px AI 面板。
- 未验证真实供应商兼容矩阵或真实 SSH 副作用；没有实现 C/D/E。schema 7 与 DPAPI 不变，用户 .idea/misc.xml 保留。

### C1：2026-10-14 完成

- 核对基线：客户端 test-rust-ui/b71ed34，Rust main/01ad303；两者包含交接基线且开始工作区干净。src-tauri 的 ../../ai-ssh-terminal-server-rust 正确解析本机路径，不需要修改。
- 代码：src/agent/compact.rs，mod.rs 公开 compact 模块；tests/agent_compact.rs。没有新增逐阶段文档、数据库迁移、依赖或前端接口。
- 入口：compact::Summarizer::new(store).summarize(sessionId, userId, cancel)。按会话所属用户读取其模型配置，使用现有普通 Chat Completions 非流式适配器。一次调用只发一个请求、不自动重试、没有可执行工具定义；模型返回工具、拒绝、截断或协议错误均不能变成有效摘要。
- 来源：版本快照、可信任务与目标、旧检查点、之后的原始事件和失败/取消/中断文字轮次的部分文本。所有模型输入中的日志/旧摘要/命令文本均标作不可信证据，不能产生执行授权。生成中的文字轮次返回 Busy，不争抢 B3 租约或恢复任务。
- 范围：最多 200 条新增事件/失败轮次、序列化来源最多 512 KiB。完整请求连同提示、输出与安全预留经过 B1/A3 预算检查，超限明确拒绝，不默默丢消息。没有新增证据时返回 EmptySource；更大历史的分段和自动重试由 C2 后续处理。
- 输出：只接受完整 JSON 对象，且必须仅有 summary、nextSteps、evidenceEventIds。拒绝缺字段、未知/重复字段、代码围栏或嵌入 JSON 修复；summary 为非空、最多 16000 UTF-8 字节，nextSteps 最多 32 项且每项非空/最多 4000 字节；证据 ID 必须非空、唯一、在本次提供的来源范围内，最多 200 项。
- 应用字段：CheckpointContent 的 task（含目标/约束/状态）、pendingToolCallIds、uncertainToolCallIds 完全从快照构造，模型不能编辑。旧检查点证据和本次未决工具/失败轮次的关键引用由程序合并保留，累计超过 200 项或整个检查点超过 64 KiB 时拒绝，不截掉回查线索。
- 返回：Draft 包含 snapshot、content、usage 和实际使用的模型 ID。生成、无效输出、取消、超时和失败不写事件或检查点。输入采集前后及返回前核对快照；新消息、停止或目标变更使旧草稿失效。调用方以后提交仍必须使用 Repository::save_checkpoint 的 A4 CAS，不能用生成时的校验替代最终事务检查。
- 测试：真实随机回环模型服务验证语义输出协议与 usage、17 种坏摘要、越权字段、跨用户/会话/未来证据、拒绝/工具/截断、取消/超时/HTTP 错误、输入容量/模型窗口、活动轮次互斥、失败部分文本、旧检查点引用、未知工具保留、生成后版本冲突，以及 SQLite 关闭重开后的检查点/原事件/原始输出范围回查。13 项专门测试通过；测试显式保存检查点仅验证 A4 兼容，不是 C1 生产自动保存。
- 凭据：使用临时测试 DPAPI Key 验证认证仅进入原生 Authorization 请求头，未加入摘要输入、Draft 或原日志；没有使用用户密钥或实际供应商/SSH 服务器。用户原文和日志自行携带的敏感信息并未实现通用自动脱敏。
- 边界：结构/出处与可信字段可以校验，模型自由文本的全部语义正确性不能由这些规则保证；真实模型摘要质量与收益比较留到后续 C/E5。C1 不保证摘要更短、不执行自动压缩、不提交检查点、不替换模型聊天窗口；当前文字对话行为和模型设置保持原状。尚未提供 UI/IPC 摘要入口，因此本轮不改前端业务和样式，也不重复前端 JS/视觉验收。
- README 顶部同步当前文字对话能力，并明确其早期迁移/安装包段落属于历史记录，当前能力以本清单为准。

#### C1 验证命令与结果

在 Rust 库目录：
- cargo test --offline --lib agent::：6/6，通过 B3 轮次/恢复内核。
- cargo test --offline --test agent_compact --test agent_context --test agent_kernel --test agent_outputs --test agent_models --test agent_chat：13+17+6+17+20+18 = 91/91；连同上述 6 项，相关 Agent 共 97/97。
- cargo clippy --offline --lib --test agent_compact -- -D warnings：通过，无告警。
- rustfmt --edition 2021 --config skip_children=true --check src/agent/mod.rs src/agent/compact.rs tests/agent_compact.rs：通过。

在客户端目录：
- cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1：15/15，通过同进程库嵌入与模型/停止/历史/Channel 的既有协议；其余 10 项未重复运行。
- 前端工作区未改，Cargo 依赖锁未改。未启动真实 GUI、用户服务器或安装包。

完成标记 C1 已回填，下一项 C2。本轮未提交、推送或打包，commit note 只在聊天中提供。

### C2：2026-10-14 完成

- 基线：客户端 test-rust-ui/b71ed34、Rust main/a142a46；本轮开始工作区干净，Cargo 的 ../../ai-ssh-terminal-server-rust 依赖按实际目录正确解析。B5 与 C1 是用户已提交版本，C2 本轮不提交。
- 代码：Rust src/agent/compact/auto.rs、compact.rs、chat.rs、provider/protocol.rs、provider/repository.rs、repository.rs；tests/agent_auto_compact.rs。前端 ModelSettings.tsx、RustBackendSettingsDialog.tsx、api/chatErrors.ts、tests/model-compaction-ui.test.mjs；原生 src-tauri/src/backend_ipc_tests.rs。
- 触发：每次基础文本请求入轮次前估算完整序列化消息（含实时任务、旧检查点、近期历史、最新输入），达到 compactPercent × 可用输入容量，或原始增量达到 200 条等消息/请求体压力时尝试整理。可用输入 = 窗口 − 输出 − 安全预留；默认触发 75%。估算仍是 UTF-8 启发式，没有冒充精确 tokenizer。
- 低水位：候选摘要替换较早的完整对话前缀，目标降至触发水位的 80%（默认 75% → 60%，设置 60% → 48%）。必须同时满足完整普通请求的窗口、256 消息、2 MiB 请求体硬上限，并确实减少估算成本或被强制整理的消息数，才提交。最新用户输入不被裁剪；实时目标/约束从可信状态加入 system，摘要作为不可信历史数据。
- 范围与重试：增量读取最多 256 条，单次摘要来源最多 200 条/512 KiB；完整对话/工具组边界不被拆分。有界只读预检寻找能进入当前模型窗口的前缀，尽可能保留最近两轮完整对话。最多两次摘要调用，第二次要求更短摘要（4096 → 2048 UTF-8 字节）；只对无效摘要或缩减不足进行这次重试，网络/认证/超时错误不自动重试。旧检查点可再次整理，原始日志不删除。
- 失败：只在完整原始上下文本身可发送且快照未变时回退原上下文，不声称整理成功；否则明确报错，不发普通请求、不创建文字轮次。无效/未够低水位摘要、来源超限、取消、目标或新消息变化均不覆盖旧检查点。最新必需输入自身超窗时，在调用摘要之前拒绝。
- 提交：Repository::save_checkpoint_guarded 支持较早前缀，但以完整最新 Snapshot 做 A4 CAS，前缀必须为同会话事件、不得后退或拆开工具调用/结果组，引用不得超过前缀。事务写入前和最终提交前检查取消，已观察到取消则回滚 checkpoint/revision；若有效提交先于停止胜出，保留有效检查点但不继续普通模型请求。成功后重读并按新 revision 入轮次。
- 配置与停止：在途请求冻结用户所属模型配置和原生密钥，摘要期间设置修改/删除不能改路由；后续请求使用最新配置。准备阶段仍注册准确 requestId，可在首个文字帧/轮次入库前停止；停止、丢弃订阅、重读旧完成回执均不重新摘要或重发模型。
- 工具边界：check_after_tool_batch 校验完整真实调用/结果组并返回预算/触发信息，显式 unknown 仍为未知；不制造成功结果、不执行工具。当前文本请求继续拒绝工具历史，真实工具循环在 D1/D4 接入，不能把本项当成 SSH 执行已完成。
- 前端：高级参数增加 1–99% 自动整理比例，复用 ModelField、主题参数、标签与蓝色高亮；保存保留配置 ID、密钥及历史关联，锁定时仍禁用。新增容量/整理失败指导，工作区能力说明同步。C5 的占用/真实整理状态标记尚未实现；此轮没有新增 CSS 或截图，真实明暗主题/窄屏视觉验收仍留待 E6。

#### C2 验证命令与结果

在 Rust 库目录：
- cargo test --offline --lib agent::：8/8（原轮次内核 6 + C2 取消事务/前缀和并行工具组 2）。
- cargo test --offline --test agent_auto_compact --test agent_context --test agent_kernel --test agent_outputs --test agent_models --test agent_chat --test agent_compact：13+17+6+17+20+18+13 = 104/104；连同单元 8 项，共 112/112。
- C2 集成 13 项覆盖：普通低预算无摘要、触发和硬超窗缩减、保持近期轮次/最新原文/可信约束/全量历史、无效摘要两次上限、HTTP 错误无重试且仅完整可用原文回退、缩减不足不提交、准备停止/调用方取消、新消息/目标竞态、最新必需输入超窗、SQLite 重开/请求回执重放、工具批次完整性、在途模型配置冻结、失败保留已有检查点及其后的全部证据。
- cargo clippy --offline --lib --test agent_auto_compact --test agent_compact -- -D warnings：通过。
- 修改的 Rust 文件 rustfmt --edition 2021 --config skip_children=true --check：通过。

在客户端目录：
- cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1：17/17。新增 2 项通过真实产品 agent_stream/dispatch/ACK 和 SQLite 验证自动压缩后 Channel 与可见历史兼容、首帧前停止摘要；其余 10 项未重复运行。
- node --import tsx --test tests/agent-history.test.ts tests/agent-ipc.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts tests/chat-text-ui.test.mjs tests/model-compaction-ui.test.mjs：51/51。新增 3 项验证百分比读取/保存/身份与密钥保持、锁定交互及错误提示。
- TypeScript noEmit 和 Vite 生产构建：通过。保留既有大 chunk 提示。
- cargo clippy --manifest-path src-tauri/Cargo.toml --offline --lib --tests：通过，无新增告警，既有 sort_by_key/zombie_processes 告警保留；修改原生文件 rustfmt 检查通过。
- 协议测试使用随机回环供应商与临时数据，不使用用户 Key、不访问真实供应商或 SSH 服务器。原生测试接收器/前端元素夹具不等于真实 WebView 视觉验收。未启动用户现有安装包验证、未打包。
- schema 7、Cargo/npm 依赖锁均未改。完成标记 C2 已回填；最新续接游标 C3。commit note 只在聊天中提供。

### 2026-10-14：修复 Node 18 开发启动故障

- 用户运行 npm run tauri dev 时，系统 Node 18.12.0 无法导入 Rolldown 使用的 node:util.styleText，BeforeDevCommand 退出。已安装 Vite 8.3.0 / Rolldown 1.2.8 的 package.json 要求 Node ^20.19.0 或 >=22.12.0，本机已有 Node 24.19.0。
- 客户端 scripts/node-runtime.mjs 在导入工具前选择兼容 Node，优先显式 SHELLPILOT_NODE，否则检查当前 Node、npm runtime、PATH 和已有 Codex runtime。只调整该工具及子进程的 PATH；不用下载、不修改系统 Node、不硬编码用户绝对路径。缺失/错误的显式路径给出明确提示。
- package.json 的 dev、tauri、build、preview 使用该启动入口，原来的 npm run tauri dev 命令保持可用；工具参数和 npm 调用身份继续传递。README 同步运行版本与可选路径配置。没有新增 npm 依赖，不改用户已有锁文件/Cargo 配置/图标变更。
- 验证：tests/node-runtime.test.mjs 5/5，覆盖版本边界、从旧 Node/PATH 选取新版本、显式路径错误、子进程环境隔离和缺失运行时错误。额外使用系统 Node 18.12.0 且移除 PATH 中 Node 24 的环境，Vite --version / Tauri --version 均成功自动切换至 24.19.0；TypeScript noEmit、Vite 生产构建通过。
- 实际启动：从旧 Node 18 的 npm-cli 执行 npm run dev，自动切换 Node 24，Vite 在 1420 就绪且 GET / 返回有效前端页面；不再出现 styleText 错误。只终止本轮测试创建的进程树，未保留开发端口。未做完整桌面 GUI/供应商调用，此次不重复 Agent 业务测试。
- 当前 C2 已完成、下一项仍为 C3。本次未提交、推送或打包，commit note 只在聊天中给用户。

### C3：2026-10-14 完成

- 基线：客户端 test-rust-ui/3585c82、Rust main/0b0b47a，C2 已由用户提交；Node 18 启动修复和用户锁文件/Cargo 配置/图标/暂存状态保留。本轮不提交、不推送、不打包。
- 代码：src/agent/provider/responses.rs、config.rs、protocol.rs、transport.rs、stream.rs、mod.rs；src/agent/compact/auto/responses.rs、auto.rs、responses_store.rs、repository.rs、turns.rs、chat.rs、store.rs；migrations/008_agent_responses.sql、tests/agent_responses.rs。既有模型/上下文/对话/压缩夹具仅补默认字段和 schema 8 断言。
- 协议：新增 openAiResponses 与默认 false 的 remoteCompaction，拒绝 Chat Completions 启用远端能力。普通 Responses 请求使用完整 input、可信 instructions、store=false、truncation=disabled、max_output_tokens 和需要时的 reasoning.effort；不用 previous_response_id 或隐式 context_management。当前仅文字/原生 reasoning 项，不启用工具执行或附件。
- 流与输出：语义 SSE 只把 output_text.delta 送到前端；确认 response ID、递增 sequence_number、output/item/content 索引、完整 output_item.done 与最终 response.completed/output/已交付文本一致。EOF、缺失终态/项目、拒绝、tool、failed/incomplete、错序或文本不一致不能成为成功。保留完整 message（含 phase/元数据）与 reasoning 原生输出，下一轮原样进入 input；失败只有文字部分证据，无完整窗口。
- 精确预算：Responses 使用同地址和路径前缀下的 input_tokens 计数完整输入及 instructions；密文字节长度与 compact 的本次 usage 不当作下次窗口 token 数。普通请求和最终发送点都检查可用容量，限制 2 MiB 请求/窗口、512 输入项目；不允许供应商默默截断。计数接口缺失明确报 MODEL_RESPONSES_UNSUPPORTED，不偷偷切协议。
- 远端整理：触发条件仍为 compactPercent 或 200 条增量压力；先确认最新必需输入可用，历史来源仍能进入模型窗口，再发一个 standalone compact 请求，不自动重试。完整返回数组整体保存/重放，包括保留消息、reasoning、encrypted compaction 和附加元数据；不取单个加密项或剪掉返回消息。追加最新用户原文、重新加入可信 instructions，再计数，必须降到触发水位的 80% 且成本下降才提交。Responses 开关关闭时仅按硬容量文字对话；C2 普通语义摘要路径保持。
- 保存与失败：schema 8 新增原生检查点窗口和 assistant 事件窗口表，分别在 A4 CAS 检查点事务、B3 已完成文字轮次事务中写入。取消在最终提交前被观察到则连同窗口/检查点回滚；旧检查点、原始事件、失败部分文本和回查线索保留。网络/无效输出/不足低水位时，仅完整且仍可发送、快照未变的原窗口可回退；不能发送则拒绝，不创建轮次或伪造成功。来源超窗不先剪历史来调用 compact。
- 隔离：窗口绑定规范化的供应商协议、完整模型接口地址、模型 ID 与原生凭据指纹，按会话/用户读取；跨接口/模型/密钥复用在网络前拒绝，需原配置或新建会话。改名/预算不改变绑定，在途请求冻结配置；活动期间删配置仍按原地址完成，不改路由。窗口及凭据绑定不通过 IPC、可见历史、Completion 序列化或 Debug 回显。ChatMessage 的原生窗口变体不能由 IPC JSON 构造。
- 资源：单窗口最多 2 MiB/512 项；累计原生读最多 8 MiB/256 项；越界整体拒绝。HTTP 使用既有取消、总时限、正文大小边界和禁止重定向/自动重试的客户端；供应商错误正文不返回用户。schema 7 旧配置/名称/DPAPI/文字历史保持可读，旧配置省略 remoteCompaction 时默认关闭。
- 前端：ModelSettings.tsx / api/models.ts 新增协议选择、Responses 固定输出参数、默认关闭的远端能力开关与状态一致的百分比说明；chatErrors.ts 给出能力缺失和窗口绑定变化的指导。复用 Select、Toggle、字段、标签/主题参数；少量 CSS 保持蓝色选择高亮、段落换行和窄栏开关排列，没有新增冗余标题。原生 backend_ipc_tests.rs 补同进程 Responses → Channel/ACK → SQLite 的成功与首帧前停止场景。
- 边界：此项是供应商支持时的文字远端整理，不开放工具/附件、不把不透明窗口作为授权。完整实时环境/授权注入仍是 C4，真实整理/占用 UI 是 C5，跨阶段综合及真实模型质量验证仍待 C6/E5/E6。未连接浏览器做截图，真实明暗主题/窄屏 WebView 视觉验收仍留待 E6。
- 官方依据与详细协议在已有 AGENT_MODELS.md；用户操作在客户端 docs/agent-model-settings.md。里程碑和测试结果只记录本清单，无新增逐阶段验证文档；commit note 只在聊天中提供。

#### C3 验证命令与结果

Rust 库：
- cargo test --offline --lib --test agent_responses --test agent_auto_compact --test agent_compact --test agent_chat --test agent_context --test agent_models --test agent_kernel --test agent_outputs：14 单元 + 17+13+13+18+17+20+6+17 集成 = 135/135。Agent 部分为 9 单元 + 121 集成 = 130/130。
- C3 17 项集成：Responses 流/非流与原生输出重放、完整 canonical 窗口/最新输入/可信约束/原始历史、密文长度与实际预算区别、能力关闭、7 种坏 compact/缩减不足、完整可用原窗口回退、两个取消入口、新消息竞态、接口/模型/协议/密钥隔离、SQLite 重开/删配置缓存回执、10 种错误 SSE 保留失败证据、计数接口缺失、旧配置默认值/IPC 无法伪造窗口、失败保留旧检查点及原始尾部、来源/必需输入超窗、在途配置冻结、schema 7 升级。
- 原子窗口事务单元 1 项验证取消发生在窗口插入后时全部回滚，及完整窗口/用户隔离；原有 C1/C2/模型/内核/输出/对话回归保持通过。
- 最终流分段校验后 cargo test --offline --lib --test agent_responses --test agent_auto_compact：14+17+13 = 44/44；最终发送点硬预算检查另复验 agent_responses。
- cargo clippy --offline --lib --test agent_responses --test agent_auto_compact --test agent_models --test agent_chat --test agent_compact -- -D warnings：通过；最终修改另做 --lib --test agent_responses 严格检查通过。修改 Rust 文件 rustfmt --check 通过。

客户端：
- cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1：19/19；最终分段校验后 --lib backend_ipc::tests::c3_ 的 2/2 再次通过，不额外累计为新测试。其余 10 项未重复运行。
- node --import tsx --test tests/agent-history.test.ts tests/agent-ipc.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts tests/chat-text-ui.test.mjs tests/model-compaction-ui.test.mjs tests/node-runtime.test.mjs：58/58；最终提示修订后模型 UI 的 5/5 再次通过。覆盖保存协议/能力/百分比、稳定配置/名称/密钥、切回协议清除远端能力及锁定交互。
- TypeScript noEmit 与最终 Vite 生产构建通过，保留既有大 chunk 提示。桌面 cargo clippy --offline --lib --tests 通过，无新增告警，sort_by_key/zombie_processes 既有告警保留；修改原生文件格式检查通过。
- Windows 多个 Rust 链接与用户开发构建同时发生，首次综合构建较慢，出现一次增量缓存 finalize 的访问提示但测试最终全部通过；后续编译并行度限制为 2。没有停止用户开发程序。
- 回归模型都是随机回环模拟供应商和临时 SQLite，未使用用户 Key 或真实模型服务、不执行 SSH 变更；前端 hooks/元素夹具和原生测试接收器不等于完整 WebView 视觉验收。

完成标记 C3 已回填，最新续接游标 C4。本轮未提交、推送、打包。

### C4：2026-10-14 完成

- 基线与范围：Rust main/0c27b34、客户端 test-rust-ui/980ecb6，均包含既定基线，工作开始时干净。实现 C4，不重写 B1–B4、不跳至 C5/D；未提交、推送或打包。
- 参考与适配：实际读取本机固定 Codex 提交 f82e36592278a0a782fb0d0fc97e5fa10e832ac2 的 compact.rs/build_compaction_replacement_history、session/mod.rs/build_initial_context_with_world_state、context/world_state/compact_permissions.rs。采用宿主重建当前状态、权限和最新用户请求的原则；不搬 CLI 工作目录、沙箱允许前缀、插件世界状态或自动 SSH 探测。
- Rust 代码：src/agent/live.rs、chat.rs、compact/auto.rs、compact/auto/responses.rs、agent/mod.rs、services.rs；src/terminal/mod.rs、output.rs。Services 的 Agent 与终端共享 TerminalManager，模型准备从阻塞线程读取 SQLite/宿主；不会新增产品端口或独立 server。
- 身份与有效性：PTY 打开时只捕获实际连接的 connectionId/owner/host/port/username；连接配置修改不改变已有终端身份。准备、整理完成、检查点提交前、入轮次和发普通模型前复核原 terminalSessionId/connectionId/当前用户。共享生命周期 token 中止整理/计数/文字等待，检查点事务在提交前直接检查原终端取消；关闭原终端后不发后续普通请求、不生成成功回执，原日志/旧检查点保留。已经有效提交的检查点保留，关闭发生在完成提交之后的回执仍可只读重放。
- 上下文：两条整理路径都重新注入当前目标、约束与活动 taskId。停止后新任务目标使用最新提问，保留约束但不将旧 taskId 当成活动任务；最新用户原文仍为最后一条 user，不被摘要或远端保留消息替代。任务/消息更改仍由 A4 完整快照 CAS 拒绝旧结果；Responses 提交后重新读取/构造，核对完整 body 与已计数请求一致，不剪规范窗口。
- 环境与权限：区分 desktopOs 和远端 os/cwd（未探测为 null/未知）；不解析终端输出来确认事实，不用本机 Windows 或 startupCommand 冒充远端环境。不向模型发送 SSH 密码、私钥、启动脚本或 known_hosts。原生权限为 textOnly、sshExecute/localExecute=false、tools/approvals 为空；来源不是 IPC、历史摘要、root 身份或模型的授权描述。当前没有命令审批数据库/执行器，D3 才接入真实原生授权来源。无终端宿主的库级调用明确 verified=false，产品始终装配真实终端宿主。
- 前端：src/state/chatTarget.ts、App.tsx、AgentPanel.tsx、api/chatErrors.ts、src-tauri/src/backend_ipc.rs。聊天标题名称与状态灯一起跟随保存的 connectionId+terminalSessionId，不随当前页签切换；纯文本历史标记未绑定，关闭/重连新 ID 显示原终端已断开。AGENT_TARGET_UNAVAILABLE/AGENT_TARGET_MISMATCH 提示连接目标后新建会话。复用既有标题、状态灯、主题与宽度规则，完整名称通过 title 提示；没有新增表单/CSS/冗余标题。组件元素及状态回归通过，实际明暗主题/窄面板 WebView 视觉验收仍待用户/E6。
- 文档：已有 AGENT_MODELS.md、客户端 docs/agent-model-settings.md、docs/agent-ipc.md 同步当前边界和可测试步骤；交接文档只更新续接摘要。里程碑和验证只维护本文件，不新增阶段 MD、commit note 文件。
- C4 六项新业务回归：普通/远端整理后正确注入实际主机、任务和约束；修改连接配置不冒充新主机；两种历史均保留伪造授权文本但不能改变当前权限；最新 Unicode/换行/两侧空白原样；两种整理期间断开不改完整 Snapshot、不发文字请求；重连 ID 不能改绑旧聊天；停止后旧检查点保留、新目标/约束/无授权重建；错误主机或用户在模型调用前拒绝。

#### C4 验证命令与结果

Rust 库：
- cargo test --offline --lib --test agent_auto_compact --test agent_responses --test agent_chat --test agent_context --test agent_compact --test agent_models --test agent_outputs --test agent_kernel --test terminal：14 + 17+19+18+17+13+20+17+6 + 22 = 163/163。Agent 部分 136/136；C4 新增 6 项，其余回归保持通过。
- cargo clippy --offline --lib --test agent_auto_compact --test agent_responses --test agent_chat --test terminal -- -D warnings：通过。测试锁改为词法作用域明确释放，避免跨 await 告警；最终对 C4 六项复验。
- 所有修改 Rust 文件 rustfmt --edition 2021 --config skip_children=true --check、Git diff --check（Windows cr-at-eol）：通过。

客户端：
- cargo test --manifest-path src-tauri/Cargo.toml --offline --lib backend_ipc::tests -- --test-threads=1：20/20；新增 1 项在两种协议上通过真实产品 agent_stream/Channel/ACK 验证整理中断开、历史/检查点保留、重连拒绝和已完成回执重放。其余 10 个库测试未重复运行。
- node --import tsx --test tests/agent-history.test.ts tests/agent-ipc.test.ts tests/ipc-transport.test.ts tests/chat-attachments.test.ts tests/chat-text-ui.test.mjs tests/model-compaction-ui.test.mjs tests/node-runtime.test.mjs tests/chat-target.test.ts：63/63。新增 4 项绑定目标状态和 1 项标题元素/状态灯/完整名称提示回归。
- TypeScript noEmit 和 Vite 生产构建：通过，保留既有大 chunk 提示。桌面 cargo clippy --offline --lib --tests 通过，无新增告警；原生修改文件 rustfmt 与补丁空白检查通过。
- 初次编译较慢，桌面改为 1 个编译作业，Rust 2 个作业；不终止用户开发程序。原生新测试最初误用 B5 短文本回执预期，修正为本轮模拟模型原文后全部通过，不影响生产代码。
- 验证只使用随机回环模拟模型/SSH 和临时 SQLite；不使用真实供应商 Key，不执行用户服务器变更。没有实际供应商语义质量验证、完整 WebView 截图或安装包构建。

完成标记 C4 已回填，最新续接游标 C5。本轮未提交、推送、打包。
## C4R 完成记录：持久项目与重启恢复（2026-10-14）

用户发现早期作用域入口仍依赖当次普通根会话/终端：重启后点击“针对当前服务器”可能进入新聊天，侧边栏不能直接找到原服务器历史。最终方案改为默认普通 Agent 对话、服务器项目及项目内持久聊天，替换临时范围选择 UX。

- schema 9 增加普通/server scope：服务器身份从真实 SSH 握手获取 host/port/username/hostKey；普通对话不随页签绑定。文字生成/整理与 SSH 执行 lease 分离，关闭终端可离线解释已有证据。原始日志、检查点与完整 Responses 窗口按原 sessionId 隔离；整理后状态变化重新投影/计数，不能复用失效预算。
- schema 10 增加 agent_projects/agent_project_chats。项目检查连接所有权、保存创建时地址/端口/用户快照、首次实际 SSH 密钥；之后重新绑定需身份一致。创建聊天与项目归属同事务，失败不留下半个会话。既有已验证 scope 按服务器身份补项目归属，未验证旧 task 归档，不改原任务/消息/检查点，不从修改后的配置推断旧密钥。
- GET projects/project_chats 支持跨模型侧边栏与每组 50 条分页；POST create_project/project_terminal/project_chat 分别负责保存项目、核对当前终端和显式创建聊天。历史点击从 SQLite 读原 sessionId/agentId，连接失败保留原文；绑定操作不创建聊天或调用模型。项目内新聊天/换模型创建独立上下文，旧聊天留在项目。
- AgentWorkspace 使用现有 Icon/Select/Modal 与主题变量，项目与普通历史在左，主区“对话 / 终端”切换；终端仍挂载，文件/SSH 页签与手动命令入口保留。管理服务器和模型设置可从侧边栏打开。复用现有连接，重开时重连原逻辑终端页签，避免反复新增终端；自动重连后原生重新核对身份。终端选区仅可加入来源项目已验证终端的聊天，普通/其他项目拒绝自动混入。
- Rust 项目 4 项专测覆盖重开不新增聊天、原日志保留、不同项目/模型隔离、分页/所有权、地址/账号/密钥变化拒绝、实际 schema 9→10 及重复打开不重复迁移；基础库/Agent/终端合计 173 项通过。
- 桌面 IPC 22 项通过，新增用随机 SSH/模型夹具完成：原项目聊天生成 → 关闭工作区 → 重开 → 同 ID 读原文 → 新 PTY 核对 → 显式下一轮继续。恢复与绑定阶段模型请求计数不增长；续聊请求含原约束和新 PTY，原会话已有两轮，未生成替代会话。
- 前端相关 74 项通过，新流程测试原会话/原模型先恢复、断线保留、普通不连接、复用连接、修改资料先阻止连接、身份失败不绑定；UI 测试项目/普通分组、历史点击、新聊天、保存服务器选择、视图切换与错误反馈。TypeScript/Vite 通过，Rust 严格 Clippy 通过。桌面 Clippy 沿用已有告警；真实供应商、真实服务器与 WebView 视觉需用户验收，未提交/推送/打包。

续接请从 C5 开始，保持本项目与 scope 层；不要重新恢复“针对当前服务器”入口，不把终端页签选择当成聊天归属。真正 SSH 工具执行仍要在 D 阶段绑定当次 PTY/taskId 与原生授权；已验证项目/文字环境不等于执行许可。下一窗口先核对实际目录、分支、工作区与本文件，保留用户修改。


### C4R 界面追加：最左侧导航（2026-10-14，随后已撤销）

- [x] 按用户 Codex 截图将模型配置、外观、SSH 连接、本地设置/关于移动到 56px 最左侧图标栏；项目与普通历史位于第二列，主内容位于第三列。入口有名称提示、键盘焦点和当前项高亮，使用现有 Icon/主题变量，窄窗口项目列缩为 210px。
- [x] 模型与外观入口直接选择设置页的受控分类，移除桌面设置页内的重复分类菜单；SSH 连接管理展示在主内容区，点击终端入口返回终端。普通/项目历史保持挂载，切换配置页不新建聊天、不修改项目或服务器绑定。独立启动设置页保留原导航。
- [x] 前端项目工作区交互 5/5，相关前端整体 75/75；TypeScript/Vite 与 diff 空白检查通过。新增一项验证模型/外观/SSH/设置切换保持项目/会话 ID，不触发新建/恢复聊天。真实 WebView 明暗主题/窄窗口视觉仍由用户确认，本次沿用先前免除浏览器前置的约定。Rust 业务/IPC/数据库未修改，不重复编译 Rust；C5 仍是下一项。不提交、推送或打包。

代码位置：客户端 src/components/AgentWorkspace.tsx、AgentWorkspace.css、RustBackendSettingsDialog.tsx、src/App.tsx；复用 tests/project-workspace-ui.test.mjs。


### 用户要求撤销最近两次 UI（2026-10-14）

- [x] 撤销 Agent 主区/项目侧栏布局和最左图标导航，恢复原左侧服务器列表、中央终端、右侧 Agent、PanelDivider/命令区和设置分类。RustBackendSettingsDialog 的仅本次 UI 改动恢复到原版本。
- [x] 删除本轮自行创建的 AgentWorkspace.tsx/AgentWorkspace.css 与已撤销布局测试，项目/普通聊天归组入口保留在 Agent 原“历史”弹层（ProjectChatHistory.tsx）；新建项目、原 ID/原模型历史恢复、自动连接/同身份重连仍使用已验证的底层逻辑。没有回退 Rust 库、IPC、schema 或数据库，不覆盖用户 icon.icns 修改。
- [x] 项目历史组件 4/4，前端相关整体 74/74；TypeScript/Vite 与补丁空白检查通过。仅前端 UI 与进度/使用说明调整，不重复 Rust 编译/协议回归。不提交、推送、打包。
- 续接保持此原窗口布局，不重新启用已撤销的项目侧栏/图标栏；C5 仍为下一项。历史 C4R/最左导航章节中的截图布局属于已撤销版本，不能作为当前 UI 的要求。

代码位置：客户端 src/App.tsx、src/components/AgentPanel.tsx、ProjectChatHistory.tsx、RustBackendSettingsDialog.tsx、src/reference.css；测试改为 tests/project-history-ui.test.mjs，projectConversation.ts 与相关功能回归保留。

### 按新参考图调整桌面布局（2026-10-14）

- [x] 用户明确要求只改 UI；该新要求覆盖此前“保持原窗口布局”的限制。左侧工作区同时显示 Agent 项目/普通会话和 SSH 连接，中央为对话，右侧为终端。保留原主导航，没有增加新的业务或项目系统。
- [x] 将已有 ProjectChatHistory 移到常驻侧栏，沿用原新建/恢复回调、sessionId/agentId、锁定和分页；SSH 组件沿用原连接/复用/编辑/删除逻辑。设置分类改为右侧图标与名称栏，原设置内容与保存行为不变。
- [x] 对话、终端及连接组件保持挂载；支持侧栏、SSH 分区、终端面板折叠以及对话/终端宽度调整。选择连接时展开终端。常用命令入口保留，在终端下方展开；窄窗口改为对话在上、终端在下。
- [x] 111 项相关前端回归通过，TypeScript 与 Vite 构建通过，保留已有 chunk 大小提示。新增外置历史在生成期间可折叠且保留草稿的回归；更新布局断言。广播测试原有旧断言在 HEAD 也不匹配 C4R 的作用域目标，已改为同时验证现有 Rust 固定作用域目标与旧单终端分支，未修改广播或对话发送业务代码。
- [x] 使用现有真实 React 组件、隔离模拟项目/模型/终端数据完成浏览器浅色、深色、1760px/900px 布局、设置右栏、分区折叠、草稿保留和设置返回检查；900px 页面无横向溢出。模拟预览不调用真实 SSH/模型，不等同于原生 WebView 全链路验收。
- 未修改 Rust 业务、原生 IPC、API、数据库 schema 或依赖。本轮不重复 Rust 测试、不提交、不推送、不打包。C5 仍是下一项，未因 UI 调整勾选。

#### 用户澄清：分类与“终端”放在同一主导航

- 用户按截图箭头明确要求将设置页内的分类合并到“终端”所在的左侧主导航。新增本地工作区、模型设置、外观和关于的同级入口，移除嵌入设置页的重复分类栏；高亮直接跟随当前分类，AI 面板的模型设置按钮仍打开对应页面。
- 设置页面沿用既有内容和操作，仅增加分类的受控 UI 参数；独立启动设置页仍保留自身导航。修正“关于”区域受旧通用 flex 样式影响的横向排列。
- 用户明确“UI 修改不用测试”：此次导航修正不运行测试、构建或浏览器验收。上文 111 项及浏览器结果属于此前三栏布局版本，不宣称覆盖本次修正。不提交、不推送、不打包，C5 保持待办。

### C5：2026-10-14 完成

本轮用户要求继续参考文档与 Codex CLI，实现后做少量必要测试，并说明能否在前端测试。基线为客户端 test-rust-ui/99fde44、Rust main/b02037e，开始时均干净；保持最新三栏布局与统一主导航，不推进 C6/D/E，不提交、推送或打包。

#### 参考与实现

- 沿用 CODEX_AGENT_REFERENCE.md 固定的 Codex 提交 [f82e36592278a0a782fb0d0fc97e5fa10e832ac2](https://github.com/openai/codex/tree/f82e36592278a0a782fb0d0fc97e5fa10e832ac2)。实际核对 `codex-rs/core/src/session/context_window.rs` 的当前窗口/可用输入语义、`codex-rs/protocol/src/protocol.rs` 的 TokenCount/ContextCompacted 事件，以及 `codex-rs/core/src/compact.rs` 的替换并重算后完成、失败/中断分支。另核对 [OpenAI compaction 文档](https://developers.openai.com/api/docs/guides/compaction) 的完整窗口处理；没有将 Codex 内部代码或不透明项直接套用为应用授权。
- Rust：新增 `src/agent/context_status.rs`，普通/Responses Controller 在测量、实际开始、失败/回退和 guarded checkpoint 提交处报告真实状态；`chat.rs` 把状态接到现有 Channel，准备后重测最终完整输入。Responses 复用已有供应商计数，不新增计数请求，不用密文字节估算占用。
- `migrations/011_agent_context_status.sql` 与 Store 升至 schema 11。独立表保存最后一次状态、输入预算快照及请求所有者，不修改原事件/轮次/检查点/窗口，不增加会话 revision。读取只核对用户、模型和会话，不解密凭据、不调用模型或新建聊天。
- `phase` 为 idle/running/succeeded/failed/cancelled/interrupted。普通摘要和远端整理分别使用 summary/remote；只保存固定错误码，不保存供应商错误正文、提示、API Key 或完整供应商窗口。整理次数每次从同会话真实检查点数量读取，成功必须验证对应 checkpointId 已存在。
- 提交 worker 在实际检查点成功后、等待 UI ACK 前持久记录成功，避免订阅断开/停止发生在提交后时将已成功结果降为取消；迟到的 Started/Failed/Finished 不覆盖已提交成功。每次请求拥有独立观察者令牌，旧请求不能覆盖新请求状态。准备中定期续 30 秒租约，异常退出遗留的 running 在过期读取时投影为 interrupted；不自动重试模型或执行操作。
- 客户端原生 `backend_ipc.rs` 增加 GET `agent/context_status`；流增加 `{event:"context",status}`，沿用原 sequence/ACK、requestId 和取消校验。前端 `api/agentContext.ts` 校验状态结构/容量关系，`api/agent.ts` 检查关联后交付；非法帧按协议失败处理，不误判为回复完成。
- `App.tsx` 按当前模型/会话读取快照，异步切换使用 AbortSignal/会话版本隔离，流式更新仍受当前请求限制。历史 running 仅在必要时短间隔刷新到终态；新发送清除旧请求快照，读取失败可手动重试，不重新发送用户消息。
- `AgentContextStatus.tsx/.css` 在 AI 面板模型说明下增加紧凑状态条，复用 Icon 与主题变量，允许换行；点击展开容量、预留、触发比例和时间。显示最近输入百分比、估算/供应商计数、已整理次数及真实阶段。未测量不冒充 0%，不增加两层标题、手动整理按钮或执行权限。

#### 语义与边界

- “最近输入”是最近一次准备发送的完整输入快照，分母为模型窗口减输出/安全预留；不是累计计费 usage，不包含未发送草稿或回复后的新增内容。输入确实超过容量时明确提示超出。Chat Completions 仍为启发式估算，Responses 为供应商计数。
- 新请求开始重置本次 phase，但总次数按真实检查点保留；旧历史没有状态表记录时显示未知占用和实际次数。配置更改后不会伪造重算旧快照，新请求才按新配置测量。
- 失败保留旧日志/检查点；原窗口确实可发送才显示回退。未提交的取消、中断、过期观察者或摘要缩减不足不增加次数。有效检查点先于停止提交时保留该真实成功，即使普通回复未继续。
- 状态不是新的上下文来源或授权；当前仍只进行文字对话，不执行 SSH/本地工具。没有调用用户服务器、真实模型或生成安装包；完整 WebView 视觉及真实模型摘要质量仍需用户验收。

#### 必要验证（未跑全量）

| 范围 | 命令或过滤项 | 结果 |
| --- | --- | --- |
| Rust 状态存储 | `cargo test --offline --lib agent::context_status::tests` | 4/4；schema 10 升级/重开、无凭据只读、租约中断、旧请求隔离、未提交不报成功、提交先于 ACK/取消 |
| 普通摘要成功 | `cargo test --offline --test agent_auto_compact threshold_compacts_only_complete_prefix` | 1/1；真实 running/succeeded、检查点次数及估算来源 |
| 普通摘要失败回退 | `cargo test --offline --test agent_auto_compact network_summary_error` | 1/1；失败不报成功、无额外摘要重试、次数不增加 |
| 准备阶段取消 | `cargo test --offline --test agent_auto_compact cancellation_during_summary` | 1/1；调用方/原生停止均不提交或继续普通请求 |
| Responses | `cargo test --offline --test agent_responses remote_compaction_preserves_the_entire` | 1/1；真实保存、供应商计数及完整窗口保持 |
| 原生真实 Channel | 客户端 `cargo test --manifest-path src-tauri/Cargo.toml --offline --lib c5_context_status_uses_real_channel_events_and_owner_scoped_persisted_measurements -- --test-threads=1` | 1/1；逐帧 ACK、成功持久读取、越权拒绝、模型摘要仅一次；最终提交/取消保护修改后再次通过 |
| 前端 | `node --import tsx --test tests/agent-context-status.test.mjs tests/agent-ipc.test.ts` | 21/21；快照校验、真实状态渲染、关联与 ACK、异常隔离 |
| 静态检查 | Rust `cargo clippy --offline --lib -- -D warnings`；客户端 TypeScript `--noEmit`、Vite `build`；修改文件 rustfmt 与两仓库 `git diff --check` | 通过；Vite 保留既有大 chunk 提示 |

以上 Rust 共 8 项，不把历史全量结果计入本轮。原生测试接收器适配新增的 context 帧，继续逐帧确认；未将未知帧忽略行为加到生产解析。测试只使用临时数据库和回环模型。

#### 可以在前端测试

1. 在客户端开发工作区重新运行 `npm run tauri dev`，等待 Rust 重编译；仅刷新前端或启动旧安装包不能验证本轮。
2. 选择现有模型，新建普通或项目聊天，发送短消息。模型区下方应从“未测量”变为“最近输入 …%”，点击可见详细预算与来源。不要把未发送草稿算入已测量值。
3. 可暂将高级参数触发比例设为 60%；若真实模型窗口至少 8192，可用 8192 窗口、1024 输出、512 安全预留。以短任务/约束开始，再多轮加入较长文本，达到触发条件时观察整理中/保存成功与次数增加，原始历史仍完整。触发取决于完整请求容量，不保证固定第几条发生；单条输入最多 16000 UTF-8 字节。
4. 整理等待时停止、切换历史/重启后重新查看。未提交不应增加次数，提交先胜出则保留真实成功；异常退出 running 租约过期后显示中断。不同聊天不串状态。真实失败时显示失败或回退，不能显示成功；无需特意破坏现有密钥来测试。
5. Responses 需要供应商支持 input_tokens，远端整理还需明确开启并支持 compact。应显示“供应商计数”，不展示密文。测试后可把比例恢复默认 75%。

客户端教程和交接文档已同步。C5 勾选完成；下一项 C6，再进入 D 阶段工具决策与审批。

### C6 用户实测追加：重复摘要丢失业务事实（2026-10-14）

- 用户在同一聊天补充公开报告编号，早期可回答，经过多次整理后回答“不确定”。只读检查测试数据库确认：编号原文仍在，第三、第四次检查点摘要含编号，第五次摘要遗漏编号；当前投影已越过原文。这是重复有损摘要的事实遗漏，不能把检查点提交成功当作语义保留成功。未修改用户数据库、替换摘要或重新调用用户模型。
- 原测试的“验收口令”可能与摘要不复述凭据的规则冲突，因此改用公开业务编号复验；公开编号的遗漏已排除仅由口令命名产生的解释。
- 新增 `RetainedUserFact { eventId, quote }`，检查点保存 `retainedUserFacts`。旧检查点 JSON 缺字段时默认空数组，schema 仍为 11，无新表/迁移。新的普通摘要 JSON 必须返回该字段，可为 []；提示优先提取用户明确要求保留的公开业务标识、日期、路径和决策，不能提取认证凭据或授予执行权限。
- 程序在新草稿中合并上次已收录事实，即使模型后续在 prose 或新事实中完全省略，也不会丢失。保留不同事件的更正，回答时按事件出处与明确更正理解，不把旧记录当实时环境。事实属于不可信历史数据，与 task/target/permissions 分离。
- 新 `src/agent/facts.rs` 校验引用必须为本会话、检查点范围内的真实 user_message，quote 必须是该原文的非空逐字片段；拒绝助手猜测、其他会话、未来事件、伪造或越权字段。草稿在阻塞工作线程校验，最后仍做快照/CAS/取消检查。Repository 事务再次验证，不能通过绕过摘要器删除既有事实或遗漏其证据引用。
- 总数最多 32 条，每条最多 512 UTF-8 字节；保留事实与 prose 一起计入完整输入容量、低水位和请求限制。超出上限拒绝候选/沿用原安全回退，不截掉旧事实。仍由模型选择初次收录内容，不保证任意细节都被自动挑中，也不是无限长期记忆；认证秘密不外泄仍依赖既有原生密钥隔离与模型提示，不声称已有通用敏感数据分类器。
- `compact/auto.rs` 把事实显式放进真正的下一轮模型请求，较低水位预检也计入已有事实；Responses 分支保留已有字段但不解析不透明窗口。没有新增普通对话端口、工具执行、前端界面或外部请求。原生测试夹具同步新摘要格式，C4 停止测试先确认 C5 状态帧，再在正文阶段停止，保持“已整理后停止”的原场景。

定向验证（不跑全项目）：

- `cargo test --offline --test agent_compact`：16/16，新增 3 项覆盖首次有出处事实、五次模型主动省略后的持久保留、SQLite 重开、拒绝事务删除、伪造/跨会话/助手/未来引用、缺字段、旧 JSON 读取与 32 条配额。
- `cargo test --offline --test agent_auto_compact`：21/21，新增 1 项跨两次真实自动整理，确认编号原文已不在 user 消息中，但事实仍随 checkpoint 实际送给模型；既有预算、取消、版本竞态、宿主身份和权限测试保持。
- 客户端 `cargo test --manifest-path src-tauri/Cargo.toml --offline --lib c5_context_status_uses_real_channel_events_and_owner_scoped_persisted_measurements -- --test-threads=1`：1/1，修订摘要格式后真实 Channel/ACK 与持久状态仍兼容。
- `cargo check --offline --tests`、`cargo clippy --offline --lib --test agent_compact --test agent_auto_compact -- -D warnings`、修改文件 rustfmt 和两仓库 `git diff --check` 通过。未改前端业务/UI，不重复 TypeScript、Vite 或浏览器测试。普通摘要 16 + 自动整理 21 + 原生 1 共 38 项；真实供应商修复后语义质量仍待用户复验。

前端复验：重新启动 `npm run tauri dev` 让 Rust 编译。在原聊天重新补充一次公开编号（或新聊天中在第二轮加入新的公开业务事实），再经过多轮其他话题和至少两次整理后询问。既有已遗漏的旧摘要不会被此升级自动回填，原始历史仍保留，不需要清空数据库。程序保证已收录事实后续不被摘要器静默删除，真实模型是否初次选择/正确使用这些事实需继续验收。

本项是用户反馈修复及 C6 部分证据，不把完整 C6 勾选完成；其他跨整理/模型缩窗/附件边界与中断综合回归仍待办。未提交、推送或打包。

### C6：2026-10-14 当前文字阶段完成

本轮以 Rust `30a2733`、客户端 `f8e73b7` 为干净基线，按用户要求继续文档主线并做少量必要测试。上面的“C6 部分证据”属于当时记录；本节完成后续连续场景，顶部清单和下一项已更新。

#### 参考与修复

- 实际重新阅读固定 Codex 提交 `f82e36592278a0a782fb0d0fc97e5fa10e832ac2` 的 `codex-rs/core/src/session/context_window.rs` 与 `codex-rs/core/src/compact.rs`：按当前模型窗口判断硬上限、从真实用户消息重建替换历史、重新注入宿主上下文，替换并重算后才报告成功；取消/中断不能等同完成。本轮使用已保存的源码副本，没有运行 Codex 或复制其远端压缩协议。
- `src/agent/compact/auto.rs`：原投影把用户事件暂存至下一条完整 assistant 才加入请求，循环结束时遗漏仍待回复的用户补充。整理中插入消息会使旧草稿失效，但后续继续请求可能漏掉该补充。现在把有效尾部 user 事件放在本次最新请求之前；仍跳过失败/中断轮次的输入，并由明确取消/任务切换边界清理暂存，不把未完成回复伪造成完整回复。不新增前端插队或并发发送入口。
- `src/agent/context_status.rs`：`AGENT_INTERRUPTED` 原来被通用错误分支标为 failed，现在明确映射 interrupted。用户停止仍是 cancelled，真实失败仍是 failed，实际提交成功不被晚到的中断降级；租约过期逻辑保持。
- 没有新 schema、端口、接口、依赖、前端布局或工具权限变更。模型窗口由用户明确配置，缩窗不能保证任何输入都可压入；不能安全压缩时继续明确拒绝并保留原记录。

#### 新增的 4 个连续场景

`tests/agent_auto_compact.rs` 中以 `c6_` 开头：

1. **重复整理后缩窗**：带可信任务约束和用户原文出处事实，从 12288 窗口提交首个检查点；增加其他话题后切到 8192 再整理。检查新请求的实际输入预算、最新用户消息、约束、事实、真实次数及原始日志；测试模型后续摘要刻意遗漏事实。
2. **已有检查点时插入新约束**：让第二次摘要等待，追加未回复的用户补充；旧草稿因 revision 改变被拒绝，不追加失败轮次或普通模型请求。显式继续后，新补充仍在未被压缩的尾部，送入模型且早于本次最新请求；旧的未准入请求不被偷偷重放。
3. **中断、完全重开与继续**：第二次摘要等待时 Drop/abort future，确认 interrupted、次数未增加、无普通请求、旧检查点/原文不变；释放原 Store/Manager，独立打开同一临时 SQLite 验证状态。旧完成请求在原回环端点关闭后仍能本地重放；更换临时测试端点后，使用原 sessionId 和新 requestId 继续，成功提交下一检查点且只发送一次普通请求。
4. **附件/超大文本拒绝边界**：已有检查点时单个/256 个未支持附件引用、超过 16000 UTF-8 字节的文本、合法但超出模型预算的输入都不能修改原日志/检查点或触发无用模型请求；随后恢复容量仍可正常对话。这里只测试附件拒绝，未读媒体文件，实际图片/PDF 转换和精确媒体计数仍归 E1。

#### 验证结果与边界

- `cargo test --offline --test agent_auto_compact -- --test-threads=1`：25/25，通过新增 4 个场景与受投影变更影响的既有自动整理回归。
- `cargo test --offline --lib agent::context_status::tests -- --test-threads=1`：4/4，验证已提交成功、旧请求隔离、状态恢复与未提交不报成功。本次共 29 项，不把早期事实回归或 C5 结果累加成本轮运行数。
- `cargo clippy --offline --lib --test agent_auto_compact -- -D warnings`、3 个修改 Rust 文件 rustfmt/格式检查及两个仓库 `git diff --check` 通过。
- 首次沙箱运行被 Windows DPAPI/本机回环限制，改用正常用户权限；正常权限首次运行复现中断误标，并暴露测试夹具误把新任务当原任务补充及过紧低水位的预期，已按真实语义修正夹具。修复后目标回归全通过，不降低预算/版本/取消保护以迎合测试。
- 未运行全项目、前端、Tauri 或真实 GUI 回归，没有调用真实模型或用户服务器，没有打包、提交、推送。用户上一轮截图显示三次整理后正确回答公开编号/时间/天数，属于用户手动反馈，不能替代本轮真实供应商验收。

#### 前端可以验证的部分

重新运行 `npm run tauri dev` 等待 Rust 重编译。在真实模型支持的范围内，用 16384 窗口、2048 输出、512 安全预留、60% 触发做一次长对话整理，再保存 8192 窗口并返回原聊天继续；检查新请求详情采用 8192、可以整理则真实增加次数，不能安全降到容量则明确报错并保留原历史。用公开报告编号、备份时间和天数测试事实，不使用凭据。

整理等待时点停止，再发送含新约束的“继续”；退出并重开同一历史，检查原文/已提交次数仍在且可继续。停止前检查点已经提交时仍保留真实成功；尚未提交不应增加次数。异常中断才显示中断，不要求正常用户停止也显示中断。UI 生成期间锁定发送，所以并发插入竞态由自动化测试覆盖，不需要用户绕过界面。

C6 在当前文字能力范围内勾选完成；下一项 **D1 主模型工具循环与严格参数校验**。D2/D3 再接实际目标和执行审批，E1 再验证真实媒体，不提前宣称 SSH 命令执行或附件对话已启用。

### D1：2026-10-15 主模型工具循环内核完成

以 Rust `08960fd`、客户端 `9585898` 为干净基线，本轮仅推进第一项未完成里程碑，不改变桌面业务入口。此前 C6 末尾的“下一项 D1”是历史记录，当前游标为 D2。

#### 参考与实现

- 重新读取固定 Codex 提交 `f82e36592278a0a782fb0d0fc97e5fa10e832ac2` 的 [ToolRouter](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/tools/router.rs#L257) 及 [turn loop](https://github.com/openai/codex/blob/f82e36592278a0a782fb0d0fc97e5fa10e832ac2/codex-rs/core/src/session/turn.rs)：主模型产生调用，宿主路由执行，结果反馈后继续模型回合；完整批次后复核上下文边界。没有添加每轮必经的业务意图分类器。
- 核对 [OpenAI function calling](https://developers.openai.com/api/docs/guides/function-calling) 的多步工具流程和严格对象约束。本轮对 OpenAI 兼容服务使用本地确定性校验，不依赖供应商是否支持 `strict:true`，也没有给所有兼容服务强加这个供应商参数。
- `src/agent/tool_loop.rs`：新增 `ToolRegistry`、`ToolExecutor`、`ValidatedCall`、`ToolLoop`。注册表在回合内不可变；调用既有 `ModelClient.complete` 完成“主模型 → 工具批次 → 结果 → 主模型”，收到无工具的完整 assistant 回复才结束。处理器只由原生宿主提供，模型传入的字符串不能选择任意执行器或产生权限。
- `src/agent/tool_loop/schema.rs`：schema 和参数使用同一闭合子集校验。支持 object/array/string/integer/number/boolean/null、enum、数值/长度/数量边界及 description；所有对象字段必填且禁止未知字段，嵌套规则相同；拒绝未知关键字、引用、联合类型和重复 JSON 字段。不做字符串转数字，integer 限有符号 64 位。注册时限制 schema 大小/深度/节点数，调用参数上限 64 KiB；不宣称实现完整 JSON Schema。
- 批次先整体校验协议、调用 ID、参数和预算，再派发合法调用。错误参数只返回 `invalid_arguments`，供模型修正，不执行工具或把参数值拼入错误诊断。模型不认识的工具、跨回合重用 ID 或不完整协议明确失败。
- 默认最多 8 次模型请求、32 个请求调用，每项工具 30 秒、结果 8 KiB；配置有硬范围。结果超限仅回传 `output_too_large`，不把超大原文塞入模型；当前尚未接完整输出存储，不宣称可回查。
- 工具定义、调用、结果和协议开销都计入每次请求及批次边界；需要整理时返回 `NeedsCompaction` 并保留完整调用/结果，不裁剪、不继续发送，也不在本轮内自动压缩。D4 再接 C2 的持久上下文整理和工具输出链路。
- 调用结果在等待执行前先标为 `result_unknown`，尚未开始的调用明确 `not_executed`。取消、超时或宿主返回未知结果即停止剩余批次，不自动重试。成功/失败/拒绝分别反馈；同一 loop 对象一旦开始就不能再次 run，包括被丢弃的 future，避免同对象重放。子取消令牌在 future 丢弃时也会取消。
- `ModelClient.config()` 只读返回该客户端实际采用的归一化配置，避免循环预算和请求配置不一致。无新增依赖、schema、端口、IPC 或前端改动。

#### 验证与范围

- `cargo test --test agent_tool_loop`：9/9。使用真实回环 HTTP 模型响应、临时 SQLite 和模拟工具，验证多回合/多工具、严格参数与模型修正、非法 schema、跨轮重复 ID、回合/调用额度、工具定义/结果预算、取消/超时/未知、失败/超大输出、关闭工具能力和 Responses 拒绝。
- `cargo clippy --test agent_tool_loop --lib -- -D warnings` 和修改 Rust 文件的 rustfmt 检查、`git diff --check` 通过。未运行全量、前端/Tauri、真实模型或用户 SSH 测试，不提交、推送或打包。
- **本轮前端没有新增可测试入口**：桌面仍使用既有文本流。D1 是 Chat Completions 非流式工具循环内核，尚未连接持久轮次/执行日志、C2 自动整理、SSH 执行器或审批；Responses 工具调用与工具流增量未开放。不能从此勾选推断 Agent 已能操作服务器。
- **下一项 D2**：实现绑定任务与目标的 SSH 处理器，执行前核对真实连接/终端状态；D3 完成可信审批与持久执行恢复后再开放副作用能力，D4/D5 对接完整输出、模型/上下文链路及前端展示。

### D2：2026-10-15 SSH 工具绑定与原生执行器完成

本轮基线 Rust `1862335`、客户端 `9585898`，开始前均干净。D1 已由用户提交；本节完成后当前游标为 D3，前文 D1 的“下一项 D2”属于历史记录。

#### 目标、身份与执行路径

- 参考固定 Codex 提交 `f82e36592278a0a782fb0d0fc97e5fa10e832ac2` 的 `ToolRouter`/`ToolInvocation`：宿主 session、step context、取消令牌与模型 payload 分开传递。本轮使用已保存并重新读取的固定源码，不把 Codex 本地执行权限映射成 SSH 授权。
- `src/agent/ssh_tools.rs`：新增 `ssh_execute` 定义及 `SshExecutor`，实现 D1 的 `ToolExecutor`，可通过 `register` 加入原生工具注册表。参数只有 `command` 和 `timeoutSeconds`，禁止额外字段；命令再检查 UTF-8 字节数、空内容/NUL，超时限制 1–120 秒。模型不能指定 taskId、connectionId、terminalSessionId 或用户名。
- `bind` 从同一 Store 的真实活动任务读取 `ToolBinding`，核对宿主给定 taskId，固定 sessionId/userId、连接/终端 ID 与完整任务快照。必须已有显式 `Task.target`；普通/离线文字会话不会自动转成执行任务。服务器作用域还必须匹配持久的服务器身份和主机密钥；普通作用域拒绝执行，即使错误地附上目标也不能获得权限。
- 原生终端租约固定原 Session 对象与其认证传输；检查用户归属、当前任务/约束/状态、作用域、真实 host/port/username/hostKey 和连接配置。授权等待之后、异步开通道之后再次复核，断线、换任务、连接归属或地址/主机信任配置变化均拒绝。新终端 ID 不接替旧租约，切换 UI 页签不能改变在途目标；要使用重连终端，宿主必须明确建立新目标任务/绑定。
- `src/terminal/agent_exec.rs`：在原终端的认证传输上创建独立 exec 通道，不向交互 PTY 注入输入，不消费或混入其输出。该通道没有 PTY，cwd/env 不继承可见终端，不注入 profile startupCommand。满额、被拒绝或断线时不重连、不换传输、不重发命令；原终端和其他终端仍使用原通道。
- `src/terminal/mod.rs`/`output.rs`：将真实身份与主机信任配置在认证传输创建时保存，Session 从传输取得身份，不再从后续读到的配置重建。连接池复用时核对真实目标/归属/主机信任配置，即使原生直接保存配置而绕过普通缓存失效路径，也不能把旧传输标成新身份。Session 仅持有传输弱引用，避免已关闭终端的历史保留期延长网络连接寿命。
- exec 结果保留原 task/connection/terminal ID、真实 exitCode、stdout/stderr 及 truncated；非零退出码保留诊断，不冒充零。只在收到退出状态且通道结束后报告完成；缺少退出状态、发送后的断线/取消/超时均为结果未知，不自动重试。关闭 exec 通道不保证远端进程已终止，未知结果仍需后续恢复/核查。
- 当前输出是合计最多 1024 原始字节的预览，跨 SSH 消息拼接 UTF-8 后解码，超限继续读取并明确 truncated。D4 再接 A5 的完整输出存储/回查，不要求模型重复命令获取余下内容。

#### 授权与产品边界

- `Authorizer` 仅供原生宿主注入，收到固定身份、任务、调用 ID、原命令及超时；默认实现始终拒绝。测试中的 Permit/Gate 只存在于测试代码，没有新增生产“全部允许”开关。
- **D3 尚未完成**：还没有审批 UI、权限模式、持久授权范围、执行事务准入或跨重启副作用去重。D2 的多次状态复核和默认拒绝不能替代这些工作；模型参数/摘要/历史不能创建授权。原生调用者不得把同一 callId 手工再次调用当作安全恢复。
- **前端仍没有新增可测入口**：没有修改客户端、IPC 或桌面聊天文本流，也没有把 ssh_execute 自动暴露给现有 AI 面板。回环联调已证明工具可以注册给 D1 主模型循环并在测试专用授权后走实际 SSH 协议，但不等于产品已经开放服务器自动执行。
- 无新增依赖、数据库迁移或端口；schema 11 保持。未提交、推送或打包。

#### 适量验证

- 新增 `tests/agent_ssh_tools.rs` 8 项：同认证传输/独立通道、UTF-8/非零退出/有界预览；默认拒绝/归属/非法参数；等待授权时取消任务、修改地址或连接归属、断线重连；开通道中换任务；无退出状态/超时/取消/终端断开；容量/离线/错误连接；注册到真实回环模型循环并拒绝模型注入目标；作用域主机密钥及已认证主机信任配置不能被后续保存覆盖。
- `tests/support/mod.rs` 增加模拟 SSH exec 响应与命令计数，服务端仅返回固定测试数据，不调用操作系统 shell。修正服务端主动关闭 exec 时测试夹具的通道计数，断开是异步流程，测试等待实际关闭，不依赖即时计数。
- `cargo test --offline --test agent_ssh_tools --test agent_tool_loop --test terminal`：8 + 9 + 22，共 **39/39 通过**。共享认证/连接池有修改，因此运行既有终端目标，未扩大到全部业务测试。
- `cargo clippy --offline --lib --test agent_ssh_tools --test agent_tool_loop --test terminal -- -D warnings`、修改 Rust 文件 rustfmt 检查与 `git diff --check` 通过。测试全部使用临时 SQLite、随机回环模型与本地模拟 SSH；未访问真实模型或用户远端，没有运行前端/Tauri/GUI。
- 下一项 **D3：命令策略、明确审批、持久执行准入与结果未知恢复**。D4/D5 随后接完整输出、工具批次整理、产品链路和 UI；Responses 工具协议/工具流仍未开放。


### 工作进度同步（2026-10-15）

- 实际代码：客户端 test-rust-ui/9585898，Rust main/8565529（包含 D2/05da511、D1/1862335）；同步开始前 git status 均为空。此前“D2 未提交”是上次交接时状态，本次已修正顶部当前状态和交接续接提示，保留历史阶段记录。
- 核对 src/agent/tool_loop.rs、ssh_tools.rs、terminal/agent_exec.rs、context_status.rs 与 schema 11 初始化；D1/D2 模块存在，SSH Authorizer 的生产默认仍拒绝；客户端/Tauri 未注册 ssh_execute 或接入主模型工具循环。桌面仍为文本 Agent，手动 SSH 终端可用。
- 当前 UI 是后续新要求确定的左侧会话/SSH、中央 Agent、右侧终端及统一主导航；不以早期撤销布局说明作为当前界面约束。项目历史恢复与同身份重连逻辑保留，普通聊天不随终端页签自动改绑。
- 已完成 A0–A6、B1–B5、当前文字阶段 C1–C6/C4R，以及原生内核 D1/D2；下一项 D3（命令策略、明确审批、持久执行准入/去重和结果未知恢复）。D4/D5 产品执行链路、完整输出与 UI，E 附件/记忆/高级能力仍待办；Responses 工具协议未开放。
- 本次是源码/提交/文档核对，不重复已有通过的测试。D2 的最近验证仍为 SSH 工具 8、工具循环 9、终端 22，共 39/39 和严格 Clippy；不能将此记录描述为本次全项目重测。未连接真实服务器、未调用真实模型、未构建安装包。

### D3：2026-10-15 原生审批、执行准入与未知结果恢复完成

本次接续中断会话留下的未提交代码，实际已经在 D2 SSH 执行器上推进了 D3。保留这些改动并完成验证，没有接入桌面命令入口，也没有把原生内核描述为用户界面可用。

- `src/agent/approvals.rs` 新增 native-only 审批存储：请求与 `sessionId/userId/taskId/callId` 绑定，保存完整任务/目标/命令快照；`Policy` 通过 revision CAS 设置为 deny/confirm，明确 `ApproveOnce` 或精确 task-command grant，策略变更撤销旧 grant 并取消待处理请求。
- `migrations/012_agent_execution_approvals.sql` 持久化 pending/approved/denied/cancelled/expired/admitted/completed/notSent/unknown 状态、审批范围、结果回执和时间边界；`Store` 启动时加载 schema 12。已有会话、事件和检查点保持可读。
- `src/agent/ssh_tools.rs` 通过 `with_approvals` 仅由可信原生宿主开启 D3 流程；审批等待、任务/连接/终端/主机信任复核、独立 exec 通道和 admitted 事务按顺序执行。已完成回执只读重放； admitted 后取消、超时、断线、进程退出缺少可信结果均标记 unknown，新 callId 或重启后也拒绝自动重发。旧 `Authorizer` 兼容路径仍逐调用决策并写入同一回执，默认构造仍拒绝。
- `tests/agent_approvals.rs` 新增 11 项：拒绝/归属、一次性回执重放、并发去重、精确任务命令授权与撤销、开通道期间撤销、策略 CAS、取消/任务变化/断线、过期、unknown 重启恢复、新任务/新终端隔离和原生撤回。

验证结果：`cargo test --offline --test agent_approvals --test agent_ssh_tools --test agent_tool_loop --test terminal -- --test-threads=1` 为 50/50；`cargo clippy --offline --all-targets -- -D warnings` 通过；修改 Rust 文件 rustfmt 和 `git diff --check` 通过。全目标回归的既有 `agent_chat` 中两个依赖首个状态帧时序的测试仍失败：`stop_interrupts_stream_and_same_session_cannot_overlap` 在首帧尚未触发模型请求时检查请求数，`aborting_a_future_recovers_partial_as_interrupted_instead_of_completing_or_replaying` 在历史首项尚未写入时读取；单独复测仍重现，与本轮审批/SSH代码无调用路径。首次全量运行的 `response_and_sse_frame_limits_keep_the_delivered_prefix_without_retry` 也曾时序失败，但单独复测通过。其余全目标测试在失败前均通过。

D3 完成边界：审批 API 只有可信原生宿主可调用，桌面审批入口、服务器选择/绑定、工具结果面板和真实本地 SSH 协议回归归 D5；完整输出存储/回查、工具批次整理与错误分类归 D4。没有运行真实模型、用户远端或打包。
### 2026-10-15：聊天停止/中断测试同步条件修复完成

- [x] 修复 `tests/agent_chat.rs` 的 `stop_interrupts_stream_and_same_session_cannot_overlap` 和 `aborting_a_future_recovers_partial_as_interrupted_instead_of_completing_or_replaying`：上下文 Context 状态可在轮次准入、模型请求和正文前送出，原测试只等任意首帧便停止/abort，导致请求计数竞态或空历史越界。
- 两项测试现在在原有 2 秒上限内消费 Context，明确收到并断言 Text 正文为 `partial` 后再停止/abort；异常结束或错误帧明确失败。首个正文在 `src/agent/chat.rs` 中先写入部分回复再送出，满足这些场景要求的已准入且已有部分正文前提。保留原有 Interrupted、partial_text、Busy、错误归属、请求计数及不重复请求断言，没有延长等待时间或改变生产逻辑。
- 验证：`cargo test --offline --test agent_chat -- stop_interrupts_stream_and_same_session_cannot_overlap aborting_a_future_recovers_partial_as_interrupted_instead_of_completing_or_replaying --exact --test-threads=1`：2/2 通过，16 项被过滤。`rustfmt --edition 2021 --check tests/agent_chat.rs`、`git diff --check` 通过。
- 本次仅修复这两个测试并同步进度；未重跑其余测试、前端、Tauri 或全量回归，未连接真实模型或用户服务器，未提交、推送或打包。前文 D3 的失败描述保留为当时记录，当前这两项已通过；下一项仍为 D4。

### D4：2026-10-15 原生工具批次、完整输出回查与错误分类完成

本轮以 Rust `8706399`（已包含 D3/`6c5111f` 和聊天测试修复）、客户端 test-rust-ui/`9585898` 为基线，开始前两边均干净。本项接通原生 Rust 工具回合，桌面 Agent 的文字 IPC/Channel 仍未挂载工具；用户可见入口与审批/结果 UI 归 D5。无新增依赖、迁移或端口，schema 12 保持。本轮改动未提交、推送或打包。

#### Codex 源码参考与实现

重新读取本机固定 Codex 提交 `f82e36592278a0a782fb0d0fc97e5fa10e832ac2` 的 `tools/router.rs`、`session/turn.rs`、`utils/output-truncation/src/lib.rs` 和 `compact_retained_tool_outputs.rs`：主模型决定调用，宿主持有上下文与取消；完整批次后检查预算；有界头尾预览保留错误属性；压缩后需要有来源的工具证据。没有复制其 Responses 专用输出格式，没有增加每轮必经的意图分类请求，也不将其本地沙箱当作 SSH 授权。

- [x] **D4.1 原生入口与原始批次日志**：`src/agent/tool_session.rs` 提供只可由可信 Rust 宿主构造的 `ToolSession::new(SshExecutor)` / `run`。`tool_loop.rs` 增加可选 `LoopContext`；未配置时保留 D1 行为。`tool_journal.rs` 在同一事务中追加完整 assistant 前文与所有调用、检查任务/版本与 callId 唯一性，失败整批回滚。结果写回原调用并关联原任务；取消/drop 时已进入处理器的调用保守记 unknown，未开始的剩余项记 not_executed。已用过的 requestId 不启动新的模型/SSH 请求；即使旧调用已被压缩，从数据库仍会拒绝 callId 复用。进程异常退出留下的原始 pending 调用是未知证据，不能作为自动执行许可。
- [x] **D4.2 SSH 原始输出捕获**：`tool_output.rs` 复用 A5 的预留配额、64 KiB 分块、逐块/整体 SHA256 与字节范围读取；`terminal/agent_exec.rs` 在原认证传输的独立 exec 通道中逐帧送入有界捕获，SQLite 写入经顺序 blocking worker，未引入无界队列。stdout/stderr 在 exec 发送前预留，配额失败不执行。默认每路最多保存 16 MiB，达到捕获上限后仍排空 SSH 并明确标记，不能声称余下数据已保存。取消/未知结果可以保留已捕获前缀，结果状态仍为 unknown。
- [x] **D4.3 预览与只读回查**：`ssh_tools.rs` 将输出引用、`outputTruncated` 与 `outputError` 写入 D3 回执，旧 JSON 缺字段仍可读。D4 模型预览从 A5 头尾预览再限至每路 384 UTF-8 字节加省略标记，长 stdout 不挤掉 stderr；`truncated` 表示预览/捕获不完整，`outputTruncated` 特指完整捕获达到上限，`outputError` 表示存储失败。`read_tool_output` 只读当前绑定 session/user/task 内已关联原始结果的输出，每次最多 512 字节，返回 offset/nextOffset/eof/size、精确 bytesHex 和可能有损的 UTF-8 text；原生 A5 范围接口仍最多 64 KiB。回查不发送 SSH，不需要命令审批；不存在、跨任务或越界统一返回 output_unavailable，避免泄露其他作用域。
- [x] **D4.4 批次预算与检查点**：`tool_session/context.rs` 使用 C2 的完整批次预算闸门、C1 的冻结模型摘要与 A4 的受保护检查点提交；投影真实工具调用/结果并计入工具定义、JSON 转义与协议开销。摘要前做低水位预检，每个边界最多两个完整前缀尝试，不对网络失败自动重试；成功才提交，失败仅在未变更且满足硬预算时保留完整原投影继续。最新用户请求、可信任务/约束/原目标与最近最多 64 个带 callId 的输出引用由宿主重新提供；终端断开后只解释历史证据，不声称当前目标在线。取消/新用户补充使旧草稿失效，未知调用不会被摘要伪装完成。必需输入在写入日志前做硬预算检查，拒绝不污染下一次正常请求；投影/摘要源码仍有 200 事件等硬边界，越界明确暂停，不静默裁剪或拆开调用结果组。
- [x] **D4.5 分类结果与准入竞态**：非零 exitCode 返回 execution_failed 并保留原退出码、stdout/stderr 与引用；权限、目标不可用、忙、参数、输出存储、冲突和 unknown 分别反馈，不更改用户目标或引入“执行失败=意图偏差”。`approvals.rs::admit_revision` 在最终 admitted 事务中额外核对 D4 计划的事件版本：等待审批期间即便 Task 对象没变，新的用户补充也能阻止旧命令发送；原 D3 原生 API 继续使用原有精确任务授权规则。

#### 验证结果

- 新增 `tests/agent_tool_session.rs` **12 项，最终稳定源码 12/12 通过**：回环 HTTP 模型协议 + 本地模拟 SSH 的多工具批次；131,073 字节二进制跨块完整捕获/回查与独立 Store 重开；非零退出码；跨任务读取拒绝；非法参数修正；取消/drop 的 unknown 与剩余未执行项；完整批次整理及摘要后回查；取消/新消息使草稿失效；失败摘要安全降级；配额失败不执行与捕获截断；压缩后 callId 复用整批回滚；断线降级刷新在线状态；审批期间补充消息禁止旧计划发送；超限拒绝后的正常继续（部分测试包含多个子场景，不另计项数）。
- 定向兼容回归命令：`cargo test --offline --lib --test agent_tool_session --test agent_tool_loop --test agent_ssh_tools --test agent_approvals --test agent_outputs --test agent_compact --test agent_auto_compact --test terminal -- --test-threads=1`，当时版本 **141/141**（23 + 11 + 25 + 16 + 17 + 8 + 9 + 10 + 22）通过。此后补充头尾预览、断线刷新、版本准入和必需输入预检；收尾的 lib + agent_approvals + agent_ssh_tools + agent_tool_session 为 **54/54**，并再次单独验证最终 D4 **12/12**。这些运行包含重复项，不累加冒充新的测试总数。
- 最终 `cargo clippy --offline --all-targets -- -D warnings`、修改/新增 Rust 文件的 rustfmt 检查及 `git diff --check` 通过。没有重复运行全项目、前端、Tauri 或真实 GUI，没有使用真实模型 Key、用户服务器或操作系统 shell 来执行模拟命令。

#### 产品边界与下一项

D4 完成的是原生宿主执行/上下文链路，不是桌面已开放服务器自动执行。生产默认授权仍拒绝；模型参数、摘要和读取到的内容均不能启用工具或扩大权限。`ToolSession` 是新原生入口，尚未与现有桌面文字聊天的请求生命周期、原生 Channel/C5 状态、审批 UI 及历史恢复面板合并；不能将其重复 requestId 拒绝行为描述为桌面已具备工具回合回放/恢复功能。Responses 工具协议和工具流仍未开放。

下一项 **D5：接入前后端产品调用链、显式服务器任务绑定、执行审批/拒绝/撤销、工具结果与输出回查 UI，以及实际 IPC/Channel + 本地 SSH 联验**；需要同步修改 test-rust-ui 的功能和样式。真实模型、用户远端及完整 GUI 验收继续归 E6；当前不打包。


### D5：2026-10-15 完成前后端产品接入与本地联验

#### 实际范围与代码位置

- [x] D5.1 **显式启用、固定目标及桌面生命周期**：`src/agent/chat/execution.rs` 的 `bind_execution` / `chat_desktop` / `chat_tools`；启用前核对持久 scope 身份与真实终端。运行中禁止重绑；工具轮次复用既有 requestId 回执、会话互斥、停止、租约和历史。`src/agent/turns.rs` 与 `migrations/013_agent_tool_turns.sql` 原子标记工具轮次，模型批次由 D4 记原始日志，最终回复只由宿主版本校验提交，避免双写。恢复时未完成调用保守闭合为 unknown，admitted 不回收重试。
- [x] D5.2 **可信审批和执行准入**：`src/agent/chat/execution.rs` 的 execution_requests / execution_decision / execution_policy / execution_output，及客户端 `src-tauri/src/backend_ipc.rs` 原生路由。仅开放 approveOnce、精确任务/命令 5 分钟授权、deny、cancel、revoke 和 confirm/deny 策略 CAS；没有 allow-all。`src/agent/approvals.rs` 显示持久 grant 过期/撤销状态，并在 admitted 同一事务内检查桌面停止标记、轮次状态和租约，关闭停止提交/令牌送达竞态。
- [x] D5.3 **工具 Channel 与上下文整理**：`src/agent/tool_session.rs` 复用宿主已准入用户事件，采用容量 64 的有界工具事件通道；`src/agent/tool_session/context.rs` 与 compact.rs 仅允许当前有主人的工具轮次在完整批次边界整理。`src/agent/context_status.rs` 报告真实测量、提交和多次整理状态；迟到的 Finished 不重写已结束状态的 updatedAt。`src/agent/compact/auto.rs` / live.rs 保留工具后离线文字讨论，历史工具证据不产生执行权限。
- [x] D5.4 **前端入口、确认与结果/回查**：客户端 `src/api/agent.ts`、`src/App.tsx`、`src/components/AgentPanel.tsx`、新增 `src/components/AgentExecution.tsx/.css`，及 `src/state/agentHistory.ts`。服务器聊天中显式启用，固定终端 ID；卡片显示准确服务器、完整命令和超时，支持批准/拒绝/取消/撤销/禁止后续执行。结果显示退出码、错误、截断和原始 stdout/stderr 引用，按 4 KiB 字节分页回读，非 UTF-8 可查十六进制。历史保留失败/未知及部分输出引用，不用重执行取输出。复用主题变量、flex 换行与窄容器样式；没有改动三栏布局。
- [x] D5.5 **模型能力显式设置**：`src/agent/chat.rs` 公布 toolsAvailable；客户端 ModelSettings.tsx 高级参数增加「函数工具能力」，默认关闭，仅支持 Chat Completions。`executionEnabled` 只作为宿主订阅参数，缺省 false，不进入模型参数或摘要。

固定参考仍为 `E:/project/ai-ssh/artifacts/codex-reference` 的 f82e36592278a0a782fb0d0fc97e5fa10e832ac2；核对 tools/router.rs 的宿主派发，不新增每轮意图分类器，不将本机沙箱视为 SSH 授权。

#### 验证与修正记录

- `cargo test --offline --test agent_tool_session -- --test-threads=1`：**18/18**，含 D4 12 项与 D5 6 项。D5 子场景包括：批准前零 exec、准确绑定、错误归属拒绝、批准后一次发送、跨 64 KiB 块读取原始字节、重开历史/只读输出、重复 requestId 不再执行、拒绝/停止/deny 不发送、在途重绑拒绝、drop 后 unknown、已写入停止标记而尚未取消 token 时仍不发送、完整批次压缩和 grant 撤销、过预算不建立轮次/不请求模型。
- `cargo test --offline --lib --test agent_auto_compact --test agent_chat --test agent_context --test agent_tool_session -- --test-threads=1` 的最终生产源码：lib **25/25**、auto_compact **25/25**；聊天先 **17/18**，响应上限用例意外得到 MODEL_TIMEOUT（原期待 MODEL_RESPONSE_LIMIT），仅用原命令过滤该单项复测 **1/1**，没有放宽生产超时或断言。其余未执行套件随后单独运行。
- 上下文初轮 **18/19**，失败为 schema 仍断言 12，统一修正 context/models/responses 的版本断言为 13；仅复测 `restart_restores_checkpoint_raw_events_and_uncertain_actions` **1/1**；`cargo test --offline --test agent_models --test agent_responses schema_ -- --test-threads=1` **2/2**。审批相关 `--test agent_approvals` 在收尾前 **11/11**，之后未修改审批生产代码。
- C6 曾复现已完成状态的后台 Finished 重写时间戳，使「拒绝输入不改状态」断言失败；已修复真实竞态，最终 auto_compact **25/25**。新增两项单元回归分别验证 tool-turn 过期恢复 unknown，以及同轮再次整理/失败/取消保留先前真实检查点、迟到收尾不改时间戳，包含于 lib 25。
- 客户端 `cargo test --offline --manifest-path src-tauri/Cargo.toml --lib backend_ipc::tests -- --test-threads=1`：**24/24**。新增 D5 回环 HTTP 模型 + 本地 SSH + 真实原生路由/逐帧 Channel ACK 联验，核对等待审批零发送、所有权、准确目标、一次 exec、结果/历史、跨块输出、断线后文字分析、缓存回放及离线读取。没有实际 WebView 自动化。
- 前端使用项目已有兼容 Node（本机系统 Node 过旧），`--import tsx --test` 运行 agent-execution-ui、agent-ipc、agent-history、model-compaction-ui、chat-target、project-conversation、agent-context-status：**54/54**。包括审批真实点击、过期/unknown 不显示可批准、持久 grant 撤销、原始字节、固定会话路由、无效工具帧取消和历史恢复。修复旧测试夹具遗漏 TokenBudgetInput/React hooks，并按当前占用圆环实际聚焦行为验证，没有改动 C5 展示组件。新测试树的空格归一化断言也已修正。
- `cargo clippy --offline --all-targets -- -D warnings`（Rust 库）、前端 TypeScript 和 `vite build` 资源构建通过；没有生成安装包。修改文件 rustfmt/diff 检查通过。桌面默认严格 Clippy 命中 HEAD 原有 `src-tauri/src/chat_history.rs` unnecessary_sort_by 和 `src-tauri/src/backend_process.rs` 测试夹具 zombie_processes 两项；未改动这些无关文件，已通过 `git show HEAD` 确认属于原有代码，未修改无关文件；`cargo clippy --offline --manifest-path src-tauri/Cargo.toml --lib --tests -- -D warnings -A clippy::unnecessary_sort_by -A clippy::zombie_processes` 通过。此结果不冒充未豁免的桌面严格检查通过。

#### 前台验证入口与边界

在当前源码启动的开发版中：模型设置 → 高级参数 → 开启「函数工具能力」（供应商须支持 Chat Completions function tools）；选择项目/服务器聊天并连接对应服务器 →「启用 SSH 工具」→ 发送任务 →「审批与记录」核对命令并批准/拒绝 → 查看结果和原始输出。工具 exec 不继承可见终端当前目录；停止生成不保证终止远端进程，unknown 必须先核查，不自动重发。

- [ ] E6 仍待：真实模型、用户服务器、明暗主题/窄窗口/实际 WebView 完整验收。本轮没有截图证据。
- 所有已跑协议测试使用临时 SQLite、回环 HTTP 模型和本地模拟 SSH，不调用真实 Key、用户服务器或 OS shell 来执行模拟命令。
- 本轮未提交、未推送、未打包。下一项 E1；D5 的本地产品链路完成不代表附件、Responses 工具、本地 shell 或高级 Agent 已开放。


### D5 试用反馈修复：批准后立即 unknown（2026-10-15，本地定向验证完成）

- 用户截图反馈：点击允许后立即失败，未显示工具输出。仅以只读方式核对本机相关执行回执和输出长度，没有读取凭据、连接或重跑用户服务器命令。
- 已定位并复现：`src/terminal/agent_exec.rs` 将 SSH 的 WindowAdjusted / XonXoff 正常控制帧落入未知错误分支，提早关闭独立 exec，后续 stdout/stderr 和退出码未读到。`tests/support/mod.rs` 在 __exec_chunked__ 响应前发送真实 XON/XOFF 控制帧；新增 `tests/agent_ssh_tools.rs::informational_ssh_flow_control_before_exec_reply_keeps_result_and_terminal_alive` 在修复前约 0.1 秒稳定失败为 OutcomeUnknown。
- [x] 修复代码：接收循环容忍窗口调整与流控，继续读取输出和真实 ExitStatus；未放宽审批、目标核对、超时、缺少退出码或断线仍 unknown 的规则。
- [x] 验证结果补记：该次修复源码的 `cargo test --offline --lib --test agent_ssh_tools --test agent_tool_session -- --test-threads=1` 通过（27 + 9 + 18 项）；桌面 `backend_ipc::tests::d5_native_channel_waits_for_human_approval_executes_bound_ssh_and_restores_outputs` 单项通过。WindowAdjusted 前/中/后、非零退出码、缺少退出码、真实 SSH XON/XOFF、审批后输出/历史均有覆盖。Rust 全目标严格 Clippy、修改文件 rustfmt/diff 检查、前端审批组件 6 项及 TypeScript 检查通过。这是此前运行的收尾结果，不是本次拉取后对新版本的重新回归。
- 前端审批卡片补充「输出显示在对话中」；右侧 PTY 不回显独立 exec 是现有设计，不能用它推断执行成功。保留原有 D5 改动和用户自行生成的 icon.icns 变更；不提交、不打包。

### D5 UI 优化：命令审批与执行输出（2026-10-15）

- [x] 参考用户提供的 Codex 桌面截图，待审批命令采用简洁终端卡片：显示准确服务器、完整可滚动命令、超时与可展开的独立环境说明；主操作为拒绝和允许一次，5 分钟本任务精确命令授权收进分体按钮菜单，保留取消与撤销入口。没有扩大授权范围，也没有将精确命令授权改为类似命令授权。
- [x] 已处理的审批记录默认折叠为单行；聊天工具记录采用终端图标、状态、单行省略命令和展开箭头。展开显示 Shell、完整命令、复制入口和限定高度的滚动输出；stdout/stderr、退出码、错误、截断及原始字节分页回查保留。
- [x] 沿用现有主题变量和窄容器样式，补充键盘焦点、禁用状态及减少动态效果偏好；不修改后端执行、审批 API、固定目标或三栏布局。
- 验证：前端 TypeScript 检查、审批组件定向回归 **6/6**、diff 检查通过；未进行实际 WebView 视觉验收，未重新打包、提交或推送。此前同步状态时的 Rust 测试在 SSH/凭据夹具初始化阶段失败（包含 Windows 拒绝访问），不能作为本轮 UI 验证或流控修复已验证的依据。
- 下一项仍为 E1；用户已经完成 D5 提交，本段记录的是后续 UI 优化。

### D5 后续：队列、同轮引导和输入区审批（2026-10-15）

- [x] 读取固定参考提交 f82e36592278a0a782fb0d0fc97e5fa10e832ac2 的 `tui/src/chatwidget/input_queue.rs`、`user_messages.rs` 和 `core/src/session/turn.rs`，核对 [turn/steer 官方接口](https://learn.chatgpt.com/docs/app-server#steer-an-active-turn)。采用“普通队列 / 已提交待回执引导”两条路径，不以停止后重新发送、下一轮优先发送来冒充同轮引导。
- [x] 发送后立即清空草稿，生成期间保持输入可编辑。普通消息先排队，当前轮正常结束后依次发送；支持移回草稿编辑、删除和引导。用户停止或发送失败暂停自动续发；消息提交状态不确定时保留原消息 ID，不自动重发或改成新轮次。未提交队列受当前界面会话范围隔离；已提交引导持久保存在原轮次。
- [x] 新增 schema 14 原生轮次输入收件箱与收尾门。`agent/steer` 核对用户、模型、会话和 expectedRequestId，重复 ID 只重放同内容回执，每轮至多 32 条、每条 16000 UTF-8 字节。文字聊天和工具循环在后续模型请求前接收输入，保持同一轮次，不取消在途请求；最终收尾与新输入准入通过 SQLite 事务排序，迟到输入明确拒绝。
- [x] 引导会取消尚未准入的旧审批、撤销旧任务的短期命令授权，并阻止有待接收输入时新 SSH 执行准入；已准入命令保持原有执行结果和 unknown 规则。完整工具调用/结果闭合后再处理引导，不拆开工具组或扩大授权。
- [x] 原生 Channel 新增有序 `input` 回执；历史保留引导 ID、原文、时间与是否接收，重开不会把已接收输入重新执行。中途模型回复和最终回复分别记入原始上下文，避免把界面汇总文字重复投入模型。
- [x] 待审批卡片通过 portal 显示在输入框原位置，期间收起表单但保留草稿；处理完恢复输入框。下拉箭头和菜单改为可逆的旋转、透明度与位移动画，支持焦点、Escape、禁用状态与减少动态效果偏好。默认合并展示命令结果，去掉 stdout/stderr 分栏和空通道；完整输出字节回查收进折叠区域。
- 验证：Rust 收件箱单元 **4/4**、文字聊天 **19/19**、工具轮次 **19/19**；前端队列/审批/历史/IPC **41/41**。TypeScript、桌面原生 IPC 编译、Rust 全目标严格 Clippy 和差异检查通过。均使用本地临时数据和模拟协议，没有访问真实模型或用户服务器；未做实际 WebView 视觉验收，未打包、提交或推送。
- 前台验证：重启 `npm run tauri dev` 以重新编译 Rust。发送一个较长任务，确认草稿立即清空；生成中再输入两条，检查排队/编辑/删除；选一条点击“引导”，确认当前轮继续并最终回复补充内容。服务器任务待审批时应在输入框处出现卡片，允许/拒绝后恢复输入；结果默认只展示文本，完整回查可展开。
- 下一项仍为 E1。本轮包含必要的原生引导能力，不能描述成纯样式修改；已有 0.4.0 安装包不包含本次改动。

### D5 UI 后续：自然正文与活动记录折叠（2026-10-15）

- [x] 补读同一 Codex CLI 固定版本的 `tui/src/history_cell/mod.rs`、`history_cell/activity_group.rs` 和 `exec_cell/mod.rs`。CLI 有历史单元与相邻活动分组，桌面 CSS 不在这些终端源码中；界面按用户提供的桌面截图调整。
- [x] 去掉 AI 回复正文外层的边框、底色、阴影和大块留白，短回复随内容自然排版；移除通用 Markdown 的“查看原文 / 返回排版”，保留悬停复制 Markdown、时间、代码块复制和 Markdown 排版。
- [x] 相邻工具/智能体活动合并为默认折叠的摘要行；中间穿插的正文保持原顺序。摘要根据真实记录统计命令、输出回查或其他工具，标明进行中、失败和结果未知；展开区域限定高度并滚动，单条命令仍可展开结果。稳定的首条记录 ID 保留生成期间的展开选择，不自动开合。
- 本次仅调整前端展示，不修改原始消息、事件、执行或审批逻辑。TypeScript 检查通过，未执行 UI 自动化或实际 WebView 视觉验收；保留前序未提交改动，不提交或打包。下一项仍为 E1。

### D5 反馈：历史重连后的工具目标与聊天排版（2026-10-15）

- [x] 修复前端启用 SSH 工具后继续缓存旧 terminalSessionId 的问题：用户断开再从历史重连同一服务器后，下一轮先选择该对话范围内的新连接，通过既有原生 bindExecution 核验真实主机密钥、账号与服务器身份，再更新任务目标并发送。保留同一聊天历史；运行中的轮次不改目标，原目标仍存活时继续固定它，不因切换页签换到另一条连接。
- [x] 离线、身份不符、原生轮次繁忙或重绑未确认时停止发送并保留错误，不回退其他服务器，不重跑过去失败的命令。新任务沿用原生的审批和授权隔离；前端项目连接状态同步也避开正在生成/停止的轮次。
- [x] 聊天正文与用户消息改为 16px，覆盖遗留的 11px 段落样式；使用明暗主题中性色，统一段落、列表、标题、代码字号与消息间距。保留用户自定义字体、字重与正文颜色，终端字号不受影响，悬停复制与时间布局保持。
- 截图中的 Docker socket permission denied 是独立的远端权限问题：工具 exec 使用 SSH 认证账号，不继承交互 PTY 中 sudo -i 的 root 身份。本轮没有提升工具权限或修改远端账号配置。
- 验证：前端 TypeScript、重连/目标/项目历史恢复定向检查 **19/19**、差异检查通过；未调用真实模型或用户服务器，未做实际 WebView 视觉验收。本轮没有新增 Rust 功能或迁移；保留前序未提交内容，不提交、不推送、不打包。下一项仍为 E1。

### D5 UI 反馈：输入框底栏排列（2026-10-15）

- [x] 修复加入队列按钮与旧版发送按钮 margin-left:auto 冲突，导致模型/停止按钮偏到中间的问题。附件入口留在最左侧，模型、设置、上下文圆环和操作按钮保持右侧紧邻排列。
- [x] 生成中草稿为空时只显示停止按钮，去掉多余的禁用“＋”；输入补充内容后显示向上箭头加入队列，保留停止入口。普通发送同样使用向上箭头，队列和审批行为不变。
- 验证：TypeScript、既有队列定向回归 **4/4** 和差异检查通过；未做实际 WebView 视觉验收，未提交或打包。

### D5 UI 反馈：审批卡片字体（2026-10-15）

- [x] 修复命令与服务器地址引用不存在的 --font-mono 变量，导致回退到浏览器默认等宽字体的问题，统一使用项目现有的 --mono 字体栈。审批提示使用界面字体与 16px 字号，操作按钮 14px，命令 13px，辅助信息 12–13px；使用中性正文色并兼容明暗主题和用户字体偏好。仅修改样式，不改审批或执行行为。
- 差异检查通过；按用户要求未新增 UI 测试，未做实际 WebView 视觉验收，未提交或打包。

### D5 反馈：工具结果同步展示到终端（2026-10-15）

- [x] 当时按用户澄清采用独立 SSH exec 执行、结果单向展示；此方案已被后续“继承当前 Shell 身份且结果隔离”的明确要求替代，当前行为见下一节。
- [x] 独立 exec 发送后，命令、stdout/stderr 输出及真实退出码写入原终端的本地显示缓冲区，经既有读取/IPC 订阅显示；取消、缺少退出状态等仍标记结果未知，不重试。展示不经过 SSH stdin，不重复执行，也不会把终端手工输入转成 AI 工具。断线后不将晚到的结果转移至新连接或其他页签。
- [x] 输出展示沿用终端缓冲上限，原始工具结果和字节回查保持。独立解码展示流，避免工具字节和手工 PTY 尚未完整的 UTF-8 字节混在一起；换行适配终端，命令中的控制字符转义展示。原审批、目标校验及 at-most-once 规则不变。
- [x] 审批卡片说明同步改为“独立执行、终端仅展示”；SSH 登录账号继续为工具执行身份，右侧 sudo -i、目录和环境变量不被工具继承。聊天正文和用户消息由 16px 收小至 15px。
- 验证：新增本地模拟 SSH 集成 **2/2**（命令/双路结果/退出码显示、零 PTY 输入、手工操作不走工具、另一个页签不接收结果、取消不重试），终端单元/协议检查 **7/7**、前端 TypeScript 和差异检查通过。模拟 SSH 在沙箱内因权限失败，沙箱外复测通过；没有访问真实模型或用户服务器，未做实际 WebView 视觉验收。含 Rust 修改，前台需重启开发应用重新编译；未提交、推送或打包。下一项仍为 E1。

### D5 最新反馈：当前 Shell 身份、结果隔离与原生终端展示（2026-10-15）

- [x] 用户明确要求：右侧终端 sudo -i 后，AI 的 whoami 也应返回 root；手工命令/后台程序输出不得被模型当成工具结果；终端仅显示实际命令、返回内容和真实提示符，不显示 AI 标识或内部包装脚本。

- [x] 桌面宿主显式选择当前终端执行模式，仍固定原终端、真实 SSH 传输与主机身份。业务命令通过当前 Shell 的 eval/命令组运行，继承有效用户、目录和环境变量；不自动 sudo、不收集密码，也不降级为另一个身份或服务器。低层旧独立 exec 接口仍用于既有原生兼容测试，不是桌面故障回退。
- [x] 每次调用的 stdout/stderr 分别重定向到随机私有目录中的命名管道，由同一认证传输上的专属 SSH 收集通道读取；真实退出码通过第三个管道回传。工具结果从不读取 PTY 文本，先前手工/后台进程没有这些输出描述符。临时管道由收集器的退出清理处理；目标需有 POSIX Shell、mktemp、mkfifo 和 cat。
- [x] 运行期间原生输入锁拒绝手工按键和并行工具进入同一 Shell，不自动排队重发；审批期间手工输入变化会使旧计划失效，输入 revision 同时进入精确命令授权上下文。未提交输入、未返回 Shell 提示符、未知的前次执行会阻止发送；超时/停止仍为 unknown，不盲目注入下一条命令。
- [x] 展示协议按随机标识识别开始/结束，隐藏探测、编码包装和管道路径；实际命令与捕获结果按普通终端排版。内部运行期间的真实 PTY 后台输出及实际提示符在工具结果显示后恢复，不伪造提示符，不把这些显示字节交给模型。正常桌面显示不含 AI 标签、重复的 $ 提示或退出码装饰；退出码仍保存在聊天和审批记录。
- 验证：本地模拟 SSH 当前 Shell/隔离检查 **3/3**（root、手工/后台结果不混入、输入互斥、未知不重试、审批中切换用户作废），协议/展示/本机真实 POSIX 管道检查 **4/4**（含逐字节分块和真实目录/环境继承），桌面审批-输出-历史-防重放链路 **1/1**、前端审批组件 **8/8**、TypeScript、Rust 全目标严格 Clippy 和差异检查通过。本机 POSIX 测试仅操作临时管道，不通过 SSH 访问用户服务器。未做实际 WebView 视觉验收，需重启开发应用并重连，再手工 sudo -i 后验证；未提交、推送或打包。下一项仍为 E1。

### D5 审批模式与工具详情滚动（2026-10-15，覆盖前述审批默认说明）

- 对照本地补充下载的 Codex CLI permissions_menu.rs、approval_presets.rs、guardian_review.rs：请求批准对应用户审查；帮我批准对应隔离自动审查；完全访问权限对应不请求批准。ShellPilot 的默认仍为请求批准，不把自动审查简化为命令前缀或只读白名单。
- schema 15 支持 confirm / auto / fullAccess，保留 deny。策略绑定当前用户、任务和原 SSH 目标；CAS revision 防止覆盖并发设置。自动批准保存来源策略 revision，降低或切换权限会取消未准入的自动批准，准入时仍复核来源、任务、身份和未知结果。
- 自动审查使用当前配置模型的独立、无工具能力请求，携带任务、绑定目标、最近用户消息和精确命令。仅明确 low + allow 自动批准；风险、不确定、解析/连接错误或最多 20 秒的审查请求超时回人工。没有调用 OpenAI 内置 Guardian，也没有为远程 SSH 增加本地文件沙箱。审查结论与原因持久化并可在审批记录查看。
- 输入框附件按钮右侧可选三种审核边界；保存成功才更新勾选，运行期间禁止切换。自动审查过程中保持输入框，只有需要人工时才替换成审批卡片。原逐命令允许一次、精确命令短期授权、撤销和禁止执行入口保留。
- 工具详情与结果 pre 不再独立纵向滚动，仅执行列表保留高度上限；列表允许原生 scroll chaining。真实浏览器模拟页确认内部纵向滚动容器为 0，列表到底后外层聊天 scrollTop 从约 34 增至 928；菜单切换、方向键和 Esc 行为已检查。模拟页不连接真实 SSH，也不请求真实模型。
- 测试：审批相关 Rust 10/10（包括无工具模型 HTTP 回环、低/高风险、失败回退、策略撤销、CAS 和升级）；既有完整 Rust 单元测试 39 通过、1 个 POSIX 用例按配置跳过；SSH 审批/工具循环集成 20/20；前端审批/队列 16/16、IPC 23/23，TypeScript 与生产构建通过。含后端与数据库变更，需重启开发应用。未提交、推送或打包。

### 对话级公共工具审批（2026-10-16，覆盖前述任务级模式说明）

- 审核模式归属于当前用户/对话，普通与服务器对话共用同一设置接口，不依赖 SSH 连接或活动任务。新对话默认 confirm，首条普通消息可将预选模式与会话原子保存。同一对话更换任务保留模式；精确命令授权、单次批准及服务器身份仍按任务/目标隔离。
- `ToolLoop` 增加公共 `ApprovalBoundary`，持久化工具轮次在每次调用前读取对话策略，再调用工具适配器的 `authorize_with_policy`。缺失公共边界或策略读取失败时拒绝分派；新适配器未实现 confirm / auto 授权流程时默认拒绝。fullAccess 仍须通过工具自身的访问检查。
- 客户端不再以 SSH 是否连接作为整个工具能力的开关。普通对话不注册 SSH；当前已接入 SSH 与输出回查，后续 MCP/通用工具必须使用相同边界和适配器授权入口，MCP 客户端本身仍未实现。
- 按用户要求，本次未运行测试、类型检查、构建或 GUI 验证；以上旧记录的测试结果不涵盖本次修改。未提交、推送或打包。


## 2026-10-19：供应商推理续接策略

按已配置协议和供应商实际响应选择续接策略，不按模型名称硬编码。Chat Completions 保存 reasoning_content / Gemini 工具 thought_signature；Messages 保存完整带签名思考块；Responses 保存完整推理与函数调用输出项，并开放原生工具轮次。工具定义进入 Responses 供应商计数，远端整理继续拒绝引入工具调用。

schema 22 增加独立原生状态表，工具批次、最终回复与追加输入的中间回复同事务保存。历史恢复保持同轮正文、签名和工具调用完整；按协议/端点/模型/凭据绑定，状态不进入可见历史、IPC 或诊断正文。检查点候选与数据库提交均禁止截断一个原生回复，宿主工具摘要作为数据传入。删除对话同步清理状态。旧版已丢失的推理无法补回，需新对话或整理旧历史；Gemini 仅支持其 OpenAI 兼容入口，Claude 的思考模式沿用供应商默认。

验证：后端 `cargo test --tests` 443 项通过、6 项既有忽略；新增四类状态各含 JSON / 分块 SSE 的真实回环工具续聊契约，覆盖原样回传与日志隔离。补充数据库重开、凭据隔离、检查点截断拒绝和删除清理测试通过。`cargo clippy --lib -- -D warnings` 通过。客户端原生库 54 项通过（含 1 MiB 栈、诊断日志与 IPC），前端协议目录 3 项和 TypeScript 检查通过。适配两条旧测试契约：显式用户正文应进入日志，明确编辑模型地址时可保留加密 Key；未改变这些既有产品行为。未使用真实模型 Key / SSH，不打包、发布或提交 Git。


## 2026-10-20：失败信息保留与会话内切换模型

- 已进入会话的失败消息不再重复回到输入框上方队列；尚未发送的草稿继续保留。失败原因、保留说明、Trace ID 复制和手动恢复入口固定在对应回复下方，部分正文不再整体染红或混入错误文字。恢复保留附件，不覆盖现有草稿、不自动重发或执行命令。
- schema 23 同事务保存 errorMessage 与回合失败状态，恢复历史仍能查看。准入前无回合的错误明确显示仅保留在当前页面；旧版本未保存的原因不能补回。删除对话清理失败信息和模型切换边界。
- 切换模型复用原 session、项目、任务、SSH 身份和审批边界，执行锁及 CAS 防止并发覆盖。旧原生思考/签名不跨模型传递；旧文本与工具证据作为标注的历史数据，后续轮次使用新协议。远端加密检查点解除当前引用并恢复原始事件前缀，仍遵守新模型容量限制。
- 验证：后端全量 447 项通过、6 项既有忽略；客户端原生库 55 项、前端 472 项通过，TypeScript 与 Vite 资源构建通过，生产库严格 Clippy 通过。包括三种协议六个切换方向的文字/工具共 12 个回环场景、数据库重开、权限/CAS/忙碌拒绝、SSH 目标、文本及远端检查点、失败原因重载与删除。同步几处既有前端测试的 Toast 契约，未改变对应产品行为。
- 使用临时数据库、回环模型及测试 SSH；未调用真实模型供应商，未完成实际桌面视觉验收。需退出托盘旧进程并重启开发应用载入 Rust 变更。不提交、推送或生成安装包。
