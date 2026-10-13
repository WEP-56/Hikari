# Hikari 开发交接

这份文档用于快速恢复项目上下文，并交代下一阶段的开发顺序。Hikari 是一个带长期记忆的个人陪伴型助手，核心技术栈是 Rust、GPUI Kit 和 Pi agent loop。项目的方向是：保留 Pi 的工具使用和模型协议能力，让 agent 原生拥有 Hindsight 风格的长期记忆。

## 当前状态

更新于 2026-10-13。第一阶段已完成基础 agent、设置和桌面会话交互，本轮已完成 Hindsight 风格 Memory Core、全局角色卡和桌面设置接入，进入真实模型与 GPUI 手工验收阶段。

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
- `hikari-memory` 已提供 SQLite/WAL durable documents、事实/观察、中文轻量索引、后台抽取/沉淀、任务恢复、四个 memory tools、自动召回、expand、forget 与预算截断；运行时已按会话注册 scope。
- Hindsight 的研究和 Pi 侧接缝已经记录在 [hindsight-research.md](hindsight-research.md) 和 [pi-memory-plan.md](pi-memory-plan.md)。

当前限制：辅助抽取/沉淀需要用户配置可用的模型；默认不使用 embedding。记忆库浏览页和角色卡编辑器已接入，实际付费模型调用与桌面手工流程尚未在 CI 中执行。

本轮验证已通过：

```powershell
cargo check -p hikari
cargo test -p hikari-agent -p hikari-runtime -p hikari-memory -p hikari
cargo test -p hikari chat_ui_interactions -- --ignored --nocapture
git diff --check
```

本轮共通过 72 项自动测试，另单独通过 1 项真实 GPUI 交互测试。GPUI 测试使用隔离的 `target/chat-test-data`，覆盖深浅色主题、Markdown、折叠、输入框扩展/滚动、工作区隔离、终端切换、草稿、会话管理、密钥保存恢复，以及记忆设置/管理、角色卡编辑与预览。Windows GPUI 后端不支持 `render_to_image`，未生成像素截图。测试使用本地 HTTP / stdio fixture，不调用付费模型；真实模型网关和实际 MCP 服务仍需验收。聊天展示细节见 [chat-ui.md](chat-ui.md)。

## 目录速览

| 目录 | 责任 |
| --- | --- |
| `crates/hikari` | GPUI 桌面应用、设置页、会话 UI、运行时编排；资源在 `resources/branding` |
| `crates/hikari-agent` | Pi 改造版 loop、消息模型、Provider、协议适配、工具注册和 Memory trait |
| `crates/hikari-runtime` | JSON 配置、会话与工作区历史存储、MCP 管理、Skill 发现和 Skill 工具注册 |
| `crates/hikari-memory` | SQLite Memory Core、轻量检索、辅助抽取/沉淀、持久任务和记忆工具 |
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
.hikari/memory.db
.hikari/AGENTS.md
.hikari/chat/
.hikari/skills/
```

API key 按提供商明文写入本地配置；空值使用环境变量，设置恢复操作也恢复保存的密钥。窗口图标和头像属于前端资源，不放在根目录或通用 `assets` 中。

配置格式升级至 v2，加载 v1 时自动迁移为默认提供商；保存前验证配置，写入临时文件并原子替换，保存前保留 `config.json.bak`。损坏配置再次保存时先保留 `config.corrupt-<timestamp>.json`。提供商草稿与已保存配置分离；MCP/Skill 管理单独保存，不会顺带提交提供商草稿。认证请求头使用 `${ENV_NAME}` 引用，例如 `Bearer ${MY_API_KEY}`。图片支持目前仅为模型能力元数据，尚无聊天附件输入；MCP 的旧式 SSE/OAuth 仍未接入。Memory Core 使用 `.hikari/memory.db`，角色卡使用 `.hikari/AGENTS.md`，会话保存 checkpoint 与来源时间/ID。

## 重要边界

修改 agent 时优先复用 `crates/hikari-agent/src/pi` 和现有 loop，不要把 `examples/pi_agent_rust` 整体重新搬入。Provider 只负责协议和流，不负责记忆；Memory 只在 agent loop 的三个边界工作：

1. 用户话轮开始前 recall，只附加到本轮请求的 system prompt；工具循环缓存，steering/follow-up 刷新。
2. 一轮结束后或 compaction 前 retain 新增原文与抽取任务，落库失败阻止压缩。
3. retain 成功后由持久队列后台抽取事实，再执行 consolidation。

Compaction 不能代替 retain。压缩前必须先提交尚未 retain 的消息；`ContextSummary` 是上下文派生物，不应再次写入长期记忆。

## 当前实现的记忆边界

`crates/hikari-memory` 已是可运行的本地 Hindsight 风格核心：单用户、单 bank、SQLite/WAL、跨会话共享，不依赖 embedding、Hindsight HTTP 服务或 reranker。它保存 document、world/experience fact 和 observation，使用中文字符/双字与英文词项的 FTS5 轻量索引；这不是中文搜索引擎，表达完全不重合时可能漏召回。

每轮在 provider 请求前最多自动注入 5 条、1000 字节上限的相关 observation/未沉淀事实；工具预算固定为 `search_observations` 5/2000、`recall` 5/3000、`expand`/来源展开 5000，并保证 JSON 截断。工具只在记忆启用时注册：`search_observations`、`recall`、`expand` 和用户明确要求时使用的 `forget`。宿主负责参数、来源和预算校验，agent 不拥有 retain/consolidation 工具。

新会话默认开启记忆，会话可关闭且运行中的轮次不可切换。全局关闭立即停止自动召回、工具注册和新 document；已经 running 的抽取/沉淀任务继续完成，queued 任务暂停，重新打开后继续。关闭期间产生的消息不会在之后被自动回填。retain 在轮末以及 compaction 前可靠落库，辅助抽取与 consolidation 在持久队列中后台运行，可重启恢复、失败重试；召回结果、ContextSummary、思考和记忆工具结果不会成为新的记忆证据。

遗忘按用户明确指令由 `forget` 执行，至少删除目标事实及其来源消息相关的事实和观察，遮蔽来源并留下 suppression tombstone，聊天 transcript 保留。详细的语义匹配和更细的范围规则暂不扩展。角色卡与记忆分离：`.hikari/AGENTS.md` 是全局 Markdown 角色卡，类似 SillyTavern 的角色设定，编辑器直接修改它并在下一轮注入；角色卡内容和示例不作为记忆证据。

推荐的数据布局如下：

```text
.hikari/
  config.json
  sessions.json
  workspaces.json
  chat/
  memory.db
  AGENTS.md
  skills/
```

## 下一阶段：模型与产品验收

代码闭环和本地 fixture 已完成，后续工作集中在真实模型网关质量：验证明确偏好/纠正的抽取、experience 的成功与失败边界、跨会话召回、工具补查、遗忘后的不回填，以及中文表达换词时的可接受漏召回率。辅助模型配置缺失时应在设置页显示失败状态，不静默退化成 NullMemory。

继续改动时，保持 provider、agent loop、runtime 和 GPUI 的边界；不要把角色卡写入 Memory Core，也不要为了提高召回率悄然引入 embedding。新增行为需补 durable job、全局开关、session switch、checkpoint/recovery 或来源过滤测试。

图片附件、独立 artifact 消息、MCP OAuth / 旧式 SSE 和 Skill 在线市场仍未接入，作为后续增量，不阻塞本阶段记忆与人格闭环。

每次改动后至少运行：

```powershell
cargo fmt --all -- --check
cargo check
cargo test -p hikari-agent -p hikari-runtime -p hikari-memory -p hikari
```

涉及桌面交互时再运行 `cargo test -p hikari chat_ui_interactions -- --ignored --nocapture`。README、`docs/pi-port.md`、记忆方案文档和本交接文档中的“当前状态”需随实现同步更新。
