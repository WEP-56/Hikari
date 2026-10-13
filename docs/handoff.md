# Hikari 开发交接

这份文档用于快速恢复项目上下文，并交代下一阶段的开发顺序。Hikari 是一个带长期记忆的个人陪伴型助手，核心技术栈是 Rust、GPUI Kit 和 Pi agent loop。项目的方向是：保留 Pi 的工具使用和模型协议能力，让 agent 原生拥有 Hindsight 风格的长期记忆。

## 当前状态

第一阶段已经完成可行性验证和正式版基底：

- GPUI 桌面应用已经从 `preview` 迁移到 `crates/hikari`。
- 根目录是 Cargo workspace，默认启动目标是 `crates/hikari`。
- 在项目根目录执行 `cargo run` 即可启动，编译产物统一在 `target`。
- agent loop 支持流式模型回复、工具调用、工具结果回传、取消、steering、follow-up、pause turn、恢复和上下文压缩。
- 已接入 OpenAI Chat Completions、OpenAI Responses、Anthropic Messages 三种协议。
- MCP 支持本地 stdio 和远程 Streamable HTTP，提供服务管理、握手、分页工具发现和工具调用。
- Skill 支持本地目录发现、YAML frontmatter、启停、按需加载指令和受目录限制的资源读取。
- 前端已经有会话列表、聊天、运行中任务控制、提供商设置、MCP 管理、Skill 管理和主题设置。
- 会话保存完整 transcript，同时保存当前 provider context、usage 和 `retained_until`。
- agent 侧已经定义 `Memory`、`MemoryRecall`、`MemoryRetain`、`MemoryConsolidation` 接口和事件，但当前运行时仍使用 `NullMemory`。
- Hindsight 的研究和 Pi 侧接缝已经记录在 [hindsight-research.md](hindsight-research.md) 和 [pi-memory-plan.md](pi-memory-plan.md)。

验证命令：

```powershell
cargo check
cargo test -p hikari-agent -p hikari-runtime
cargo fmt --all -- --check
cargo build
```

当前测试使用本地 HTTP fixture 和 stdio fixture，不会调用付费模型。仍需要真实模型网关、实际 MCP 服务和 GPUI 手工交互验收。

## 目录速览

| 目录 | 责任 |
| --- | --- |
| `crates/hikari` | GPUI 桌面应用、设置页、会话 UI、运行时编排；资源在 `resources/branding` |
| `crates/hikari-agent` | Pi 改造版 loop、消息模型、Provider、协议适配、工具注册和 Memory trait |
| `crates/hikari-runtime` | JSON 配置、会话存储、MCP 管理、Skill 发现和 Skill 工具注册 |
| `docs` | Pi 接入、Memory 接缝、Hindsight 调研和本交接文档 |
| `examples/pi_agent_rust` | 本地忽略的 Pi Rust 参考源码，不作为依赖直接编译 |
| `examples/hindsight` | 本地忽略的 Hindsight 参考源码 |
| `.hikari` | 本机配置、会话和 Skill 数据，已加入 gitignore |
| `assets` | README 页面截图 |

程序数据当前使用：

```text
.hikari/config.json
.hikari/sessions.json
.hikari/skills/
```

API key 只通过运行时输入或环境变量读取，不写入配置文件。窗口图标和头像属于前端资源，不放在根目录或通用 `assets` 中。

## 重要边界

修改 agent 时优先复用 `crates/hikari-agent/src/pi` 和现有 loop，不要把 `examples/pi_agent_rust` 整体重新搬入。Provider 只负责协议和流，不负责记忆；Memory 只在 agent loop 的三个边界工作：

1. 下一次 provider 请求前执行 recall，并只把结果附加到本次请求的 system prompt。
2. 一轮结束后 retain 新增原文。
3. retain 成功后后台执行 consolidation。

Compaction 不能代替 retain。压缩前必须先提交尚未 retain 的消息；`ContextSummary` 是上下文派生物，不应再次写入长期记忆。

## 下一阶段目标

下一阶段是“设置优化与持久存储、Hindsight 集成”。建议按下面顺序推进。

### 1. 设置优化

- 重新梳理设置模型，将提供商、人格、运行参数、MCP、Skill、存储和记忆分成稳定的设置区块。
- 让 UI 中的 thinking level 真正映射到 `ProviderConfig.reasoning_effort`，不要只改变 companion prompt。
- 上下文用量显示使用 `ProviderConfig.context_tokens`，不要固定使用 `DEFAULT_CONTEXT_TOKENS`。
- 补齐 `max_output_tokens`、`context_tokens`、最大工具轮数和请求超时的编辑控件、范围校验和错误提示。
- 明确“草稿”和“已保存”状态。保存成功后显示结果；无效配置不能覆盖旧配置。
- 继续保持密钥不落盘。环境变量名称、协议、地址、模型等非敏感设置可以持久化。
- 对协议切换提供默认地址、默认环境变量和字段提示；切换协议后不应静默丢失用户输入。
- 配置版本增加迁移入口，为后续字段变更保留 `version` 处理，而不是直接拒绝旧配置。
- 设置页要覆盖空配置、错误 JSON、缺失环境变量、无效 MCP、重复 Skill 等可恢复错误。

验收：重启程序后设置仍准确恢复；切换协议和 thinking level 后下一轮请求使用对应字段；错误保存不会破坏上一次可用配置；密钥不会出现在 `.hikari/config.json`、日志或会话文件中。

### 2. 持久存储

当前 JSON 存储适合验证，但正式使用需要明确数据模型和恢复策略：

- 配置继续保持单独文件，增加版本迁移和原子替换测试。
- 会话至少支持稳定 session id、标题、创建/更新时间、完整 transcript、当前 context、usage 和 retain 游标。
- 会话保存改为增量或可恢复写入，避免每次 UI 更新都重写全部会话。
- 增加新建、重命名、删除、重新打开和损坏文件恢复流程；删除操作不能影响其他会话。
- 将临时运行状态、排队输入和 UI 展示状态与可恢复的对话记录分开。
- 为后续 Memory Core 预留独立存储边界。会话原文和长期记忆不是同一张表，也不能用 compaction 摘要替代长期记忆。
- 评估 SQLite 作为会话和记忆的正式存储；无论最终是否采用 SQLite，都先定义迁移版本、备份和失败恢复策略。

推荐的本地布局：

```text
.hikari/
  config.json
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

实现时必须遵守：

- 自动 recall 只作用于当前 provider 请求，不写回 session messages。
- 不要把本轮注入的 recall、ContextSummary 或辅助抽取结果再次 retain。
- retain 失败不能回滚已经展示给用户的回复，但在 compaction 前必须阻止原文被丢弃。
- consolidation 在后台运行，不阻塞下一轮回复。
- Hindsight 的 `world` / `experience` 划分遵循 [hindsight-research.md](hindsight-research.md)，不要改成泛化的“共同经历”。

## 建议的开发切片

每个切片都应能独立编译和验证：

1. 设置模型和 UI 字段接通，补配置迁移与验证测试。
2. 会话持久化重构，先保持现有 `Session` API，补崩溃恢复和旧文件迁移。
3. 新增 Memory Core 的 schema 与内存/SQLite 存储测试，不接模型。
4. 接入 retain 的抽取流程和幂等重试。
5. 接入 recall、compaction 前保护和后台 consolidation。
6. 接入记忆设置页、前端记忆事件和只读 recall 工具。
7. 使用真实模型网关、真实 MCP 服务和长会话做验收。

每次改动后至少运行：

```powershell
cargo fmt --all -- --check
cargo check
cargo test -p hikari-agent -p hikari-runtime
```

如果新增 `hikari-memory`，再加入对应的 `cargo test -p hikari-memory`。完成本阶段后，README、`docs/pi-port.md` 和本交接文档中的“当前状态”要同步更新。
