# Hikari 开发交接

这份文档用于快速恢复项目上下文，并交代下一阶段的开发顺序。Hikari 是一个带长期记忆的个人陪伴型助手，核心技术栈是 Rust、GPUI Kit 和 Pi agent loop。项目的方向是：保留 Pi 的工具使用和模型协议能力，让 agent 原生拥有 Hindsight 风格的长期记忆。

## 当前状态

更新于 2026-10-13。第一阶段已完成基础 agent、设置和桌面会话交互，本轮界面与工作区改动已获用户验收。下一阶段开始 Hindsight Memory Core 接入，以及记忆、人格的真实配置。

已完成：

- GPUI 桌面应用已经从 `preview` 迁移到 `crates/hikari`。
- 根目录是 Cargo workspace，默认启动目标是 `crates/hikari`。
- 在项目根目录执行 `cargo run` 即可启动，编译产物统一在 `target`。
- agent loop 支持流式模型回复、工具调用、工具结果回传、取消、steering、follow-up、pause turn、恢复和上下文压缩。
- 已接入 OpenAI Chat Completions、OpenAI Responses、Anthropic Messages 三种协议。
- MCP 支持本地 stdio 和远程 Streamable HTTP，提供服务管理、握手、分页工具发现和工具调用。
- Skill 支持本地目录发现、YAML frontmatter、启停、按需加载指令和受目录限制的资源读取。
- 前端已经有会话列表、聊天、运行中任务控制、提供商设置、MCP 管理、Skill 管理和主题设置。
- 设置现已支持多个提供商及独立模型元数据、请求头、接口兼容项、思考挡位、token 限制、请求超时和工具轮数；聊天菜单使用已保存模型列表。
- 界面设置支持正文、代码与终端独立字体/字号、界面字号和正文行高，提供预览与恢复已保存设置。
- Skill 支持扫描 Codex、Claude、Hermes 和通用 Agents 的用户/项目资源目录，勾选复制完整资源到 `.hikari/skills`，也可直接挂载目录。
- MCP 支持从 Claude JSON、Codex TOML、Hermes YAML 扫描导入，显示来源与诊断，支持勾选和同名覆盖；导入服务默认停用。
- 关于页提供 GitHub 仓库、Issues、Releases 跳转及正式 Release tag 的版本比较。
- 会话保存完整 transcript，同时保存当前 provider context、usage 和 `retained_until`。
- 助手正文支持 GFM Markdown；每轮最终回答单独保留，执行过程完成后默认收起，思考和工具详情可独立展开。工具参数与输出可从历史恢复，失败状态清晰可见，长代码与输出内部滚动。
- 已压缩内容间距；输入框从 1 行向上扩展到 6 行，超过后内部滚动，清空后恢复紧凑高度。
- 新会话在欢迎文案下提供工作区下拉、历史路径和原生文件夹选择器，默认“不选择工作区”；普通对话固定使用 `.hikari/chat`，历史保存到 `.hikari/workspaces.json`。
- 工作区保存到各会话；Agent 工具 cwd、终端、Explorer / VS Code / Git Bash 外部打开均绑定该路径。未选择工作区时顶部终端和外部打开入口禁用，切换会话会关闭旧终端。
- MCP stdio 的工作目录纳入连接身份，发送时按会话目录重连；设置重连保持正在运行的会话目录，避免跟随另一个可见会话而串工作区。
- 侧栏会话右键支持重命名、置顶/取消置顶和删除；空白区域无菜单，运行中的会话不可删除。各会话的未发送草稿在当前进程内保留，切换时恢复。
- API key 按提供商明文保存到 `.hikari/config.json`，重启恢复，留空回退环境变量；切换提供商、恢复设置和清空密钥均保持独立状态。
- agent 侧已经定义 `Memory`、`MemoryRecall`、`MemoryRetain`、`MemoryConsolidation` 接口和事件，但当前运行时仍使用 `NullMemory`。
- Hindsight 的研究和 Pi 侧接缝已经记录在 [hindsight-research.md](hindsight-research.md) 和 [pi-memory-plan.md](pi-memory-plan.md)。

尚未接通的真实行为：记忆开关只是进程内 UI 状态，不持久化，也未决定 `TurnHost.memory`；人格仅名字 `persona_name` 已保存并传入 `companion_prompt`，其余陪伴语气仍写死在提示词里。下一阶段要替换这些占位行为，不能把已有设置页当作功能完成。

本轮验证已通过：

```powershell
cargo check -p hikari
cargo test -p hikari -p hikari-runtime
cargo test -p hikari chat_ui_interactions -- --ignored --nocapture
git diff --check
```

本轮共通过 8 项聊天逻辑测试、14 项 runtime 集成测试和 1 项真实 GPUI 交互测试；改动文件 rustfmt 检查通过。GPUI 测试使用隔离的 `target/chat-test-data`，覆盖深浅色主题、Markdown、折叠、输入框扩展/滚动、工作区隔离、终端切换、草稿、会话管理及密钥保存恢复。Windows GPUI 后端不支持 `render_to_image`，未生成像素截图。测试使用本地 HTTP / stdio fixture，不调用付费模型；真实模型网关和实际 MCP 服务仍需验收。聊天展示细节见 [chat-ui.md](chat-ui.md)。

## 目录速览

| 目录 | 责任 |
| --- | --- |
| `crates/hikari` | GPUI 桌面应用、设置页、会话 UI、运行时编排；资源在 `resources/branding` |
| `crates/hikari-agent` | Pi 改造版 loop、消息模型、Provider、协议适配、工具注册和 Memory trait |
| `crates/hikari-runtime` | JSON 配置、会话与工作区历史存储、MCP 管理、Skill 发现和 Skill 工具注册 |
| `docs` | Pi 接入、Memory 接缝、Hindsight 调研和本交接文档 |
| `examples/pi_agent_rust` | 本地忽略的 Pi Rust 参考源码，不作为依赖直接编译 |
| `examples/hindsight` | 本地忽略的 Hindsight 参考源码 |
| `.hikari` | 本机配置、会话和 Skill 数据，已加入 gitignore |
| `assets` | README 页面截图 |

程序数据当前使用：

```text
.hikari/config.json
.hikari/sessions.json
.hikari/workspaces.json
.hikari/chat/
.hikari/skills/
```

API key 按提供商明文写入本地配置；空值使用环境变量，设置恢复操作也恢复保存的密钥。窗口图标和头像属于前端资源，不放在根目录或通用 `assets` 中。

配置格式升级至 v2，加载 v1 时自动迁移为默认提供商；保存前验证配置，写入临时文件并原子替换，保存前保留 `config.json.bak`。损坏配置再次保存时先保留 `config.corrupt-<timestamp>.json`。提供商草稿与已保存配置分离；MCP/Skill 管理单独保存，不会顺带提交提供商草稿。认证请求头使用 `${ENV_NAME}` 引用，例如 `Bearer ${MY_API_KEY}`。图片支持目前仅为模型能力元数据，尚无聊天附件输入；MCP 的旧式 SSE/OAuth 仍未接入。会话 SQLite 和 Memory Core 尚未实现。

## 重要边界

修改 agent 时优先复用 `crates/hikari-agent/src/pi` 和现有 loop，不要把 `examples/pi_agent_rust` 整体重新搬入。Provider 只负责协议和流，不负责记忆；Memory 只在 agent loop 的三个边界工作：

1. 下一次 provider 请求前执行 recall，并只把结果附加到本次请求的 system prompt。
2. 一轮结束后 retain 新增原文。
3. retain 成功后后台执行 consolidation。

Compaction 不能代替 retain。压缩前必须先提交尚未 retain 的消息；`ContextSummary` 是上下文派生物，不应再次写入长期记忆。

## 下一阶段目标

下一阶段聚焦“Hindsight 接入、记忆与人格真实配置”。沿用现有 Rust 本地核心方案：单用户、单 bank、无网络服务，不把 Hindsight HTTP 服务作为默认依赖。方案语义以调研文档为准，实际接缝以当前 `Memory` trait 和 agent loop 为准。

### 1. 记忆与人格配置

配置模型放在 `hikari-runtime`，设置页沿用“草稿 → 校验 → 保存 → 下一轮使用”的流程；恢复设置恢复全部已保存值。新增字段带默认值和迁移测试，兼容已有 v1/v2 配置与 `persona_name`。

- 记忆：持久化启用状态、本地存储位置、召回预算，以及抽取/沉淀所用提供商和模型引用。避免复制一套 API key；是否需要独立 embedding 配置随首版检索方案确定。
- 明确启停语义：关闭后下一轮不自动 recall / retain，已有记忆保留；重新启用不应静默回填停用期间的历史。正在运行的任务使用启动时的配置快照，后台队列的暂停/续跑行为需定义并测试。
- 人格：将名字、人格描述/基础指令、说话风格与用户称呼接入持久配置，并参与下一轮 system prompt 构建。现有“光”和陪伴语气作为兼容默认值；模型 reasoning effort 继续由供应商配置控制。
- 区分用户明确设置的人格与长期记忆形成的用户偏好/observation；consolidation 不直接改写人格配置。
- 记忆页显示真实初始化、存储与后台任务状态；失败可见且可恢复，不把 UI 开关亮起当作 Memory Core 已可用。

验收：重启后记忆与人格设置恢复；保存后的下一轮实际采用配置；取消/恢复不泄漏草稿；关闭记忆停止自动读写但不删除数据；人格修改能从构造的请求中验证，而不是只改变页面文案。

### 2. 记忆存储与会话恢复边界

当前 JSON 存储适合验证，但正式使用需要明确数据模型和恢复策略：

- 配置继续保持单独文件，增加版本迁移和原子替换测试。
- 会话至少支持稳定 session id、标题、创建/更新时间、完整 transcript、当前 context、usage 和 retain 游标。
- 会话保存改为增量或可恢复写入，避免每次 UI 更新都重写全部会话。
- 保留已完成的新建、重命名、置顶、删除、工作区绑定和旧记录兼容；补损坏文件恢复及删除不影响其他会话的测试。
- 将临时运行状态、排队输入和 UI 展示状态与可恢复的对话记录分开。
- 为后续 Memory Core 预留独立存储边界。会话原文和长期记忆不是同一张表，也不能用 compaction 摘要替代长期记忆。
- Memory Core 采用独立版本化存储，评估 SQLite；会话 JSON → SQLite 可单独迭代，不作为首个记忆闭环的前置条件。先定义迁移版本、备份和失败恢复策略。
- 默认记忆库属于 Hikari 本地数据目录，不随会话工作区 cwd 改变。会话/工作区作为记忆来源 metadata；跨会话记忆的召回范围需明确，不因新建普通对话而生成一个新 bank。

推荐的本地布局：

```text
.hikari/
  config.json
  sessions.json     # 当前会话存储
  workspaces.json
  chat/             # 不选择工作区时的固定 cwd
  sessions.db       # 会话持久化，方案确定后落地
  memory.db         # Hindsight Memory Core，和 session 分开
  skills/
```

验收：强制终止程序后重启不会丢失已完成消息；单个损坏记录不会让整个应用无法启动；迁移旧版 `config.json` 和 `sessions.json` 有明确测试；session transcript 与 provider context 可以分别恢复。

### 3. Hindsight Memory Core

建议新增独立的 `crates/hikari-memory`，实现 `hikari_agent::Memory`，由 `hikari-runtime` 负责配置和生命周期。这样 Hindsight schema、检索和存储不会污染 Provider 或 GPUI 层。

第一版只实现单用户、单 bank、无网络服务的本地核心：

- `document`：保存一段原文、session 区间和 `content_hash`，重复提交必须幂等。
- `memory unit`：支持 `world`、`experience`、`observation` 三种 `fact_type`。
- `world` 保存用户、外部世界、偏好、约束、计划和纠正。
- `experience` 保存 Hikari 实际完成的编程、浏览器或桌面操作及结果，不保存逐行 stdout。
- `observation` 只能由 consolidation 生成，带 `source_memory_ids` 和 `proof_count`。
- 保存发生时间、提及时间、创建时间、更新时间、来源 document/chunk 和可扩展 metadata。
- 第一版可先使用关键词检索和简单向量接口，但保留 semantic、keyword、graph、temporal 四路融合的扩展边界。
- 召回结果按 `final` 排序并截断，不把向量分和图遍历细节直接写入模型提示词。

与 agent loop 的实现顺序：

1. 先实现本地 schema、迁移、`content_hash` 幂等和 document/memory unit CRUD。
2. 实现 `retain`：提交原文，再用辅助模型调用抽取 world/experience；辅助调用不能进入 session transcript。
3. 实现 `recall`：默认只召回 observation；需要继续一段流程时再展开 source facts。
4. 接入 `TurnEnd` 后的 retain 事件和后台 consolidation，保留 `queued/done/failed` 状态。
5. 在 compaction 前等待对应 document retain 成功；失败时保留原 context，不静默丢消息。
6. 增加只读 `recall` 工具，让模型在自动召回不足时主动展开 experience；不要提供 retain 工具。
7. 在前端显示“想起了什么”、retain 状态和 consolidation 状态；这些不是助手气泡，也不是普通工具卡片。

当前 `main.rs` 已将记忆事件转换成 `Line::Note`，但尚无真实任务生命周期与后台状态；接入时完善现有路径。后台任务事件需要携带会话/任务身份并允许任务结束后继续更新，避免仅沿用当前 turn generation 过滤而丢失慢到的 retain / consolidation 结果。

实现时必须遵守：

- 自动 recall 只作用于当前 provider 请求，不写回 session messages。
- 不要把本轮注入的 recall、ContextSummary 或辅助抽取结果再次 retain。
- retain 失败不能回滚已经展示给用户的回复，但在 compaction 前必须阻止原文被丢弃。
- consolidation 在后台运行，不阻塞下一轮回复。
- Hindsight 的 `world` / `experience` 划分遵循 [hindsight-research.md](hindsight-research.md)，不要改成泛化的“共同经历”。

## 建议的开发切片

每个切片都应能独立编译和验证：

1. 梳理 `memory.rs`、`agent.rs` 和 compaction 接缝，定义记忆/人格持久配置、默认值与启停语义；先接通人格请求构建和配置迁移测试。
2. 新增 `hikari-memory` 的 schema、迁移、本地存储与任务队列，验证 document 幂等及来源追溯，不接模型。
3. 接入 retain 辅助抽取、失败重试和后台 consolidation，使用 fixture 验证 world / experience / observation 分工。
4. 接入 recall、只读 recall 工具及 compaction 前保护，由 runtime 创建真实 Memory 实例并替换 `NullMemory`。
5. 接通记忆设置页、真实任务状态与跨会话事件，验证重启恢复、切换工作区及记忆启停。
6. 按需要补会话增量存储/SQLite 迁移，保留现有会话管理和工作区绑定行为。
7. 使用真实模型网关和长会话验收：记住明确偏好，跨会话召回，保留任务经验，支持用户纠正，不重复抽取召回或压缩摘要。

图片附件、独立 artifact 消息、MCP OAuth / 旧式 SSE 和 Skill 在线市场仍未接入，作为后续增量，不阻塞本阶段记忆与人格闭环。

每次改动后至少运行：

```powershell
cargo fmt --all -- --check
cargo check
cargo test -p hikari-agent -p hikari-runtime -p hikari
```

涉及桌面交互时再运行 `cargo test -p hikari chat_ui_interactions -- --ignored --nocapture`。如果新增 `hikari-memory`，再加入对应的 `cargo test -p hikari-memory`。完成本阶段后，README、`docs/pi-port.md`、记忆方案文档和本交接文档中的“当前状态”要同步更新。
