# CometixCode tools 调研：清单差异与实现优雅度

调研对象：`examples/CometixCode/src/tools/`（39 个工具目录）+ `src/tool.rs` + `src/types/tools.rs` + `src/services/tools/`。
对比对象：Hikari 的 `Tool`/`ToolRegistry` 与全部注册工具（`crates/hikari-agent/src/tool.rs`、`file_tools.rs`、`execution.rs`、`delegate.rs`、`hikari-runtime/src/{mcp,skills,web,image_gen,history}.rs`、`hikari-memory/src/tools.rs`）。

结论先写三句：

1. **Hikari 没有而 CometixCode 有的工具，共 15 组**（含按开关注入的），其中值得考虑的是 `LSP`、`NotebookEdit`、`EnterPlanMode`/`ExitPlanMode`、`TaskCreate`/`Get`/`Update`/`List`（任务图）、`EnterWorktree`/`ExitWorktree`；明显不需要的是 `SendMessage`/`TeamCreate`/`TeamDelete`（团队/蜂群）、`CronCreate`/`Delete`/`List`、`RemoteTrigger`、`Brief`、`Config`、`StructuredOutput`、`Sleep`。
2. **两边都有的工具，CometixCode 的实现深度普遍高一个到两个数量级**（`Bash` 12253 行 vs Hikari 的 `shell` 约 175 行；`Read` 4599 行 vs 约 55 行；`Grep` 2341 行 vs 约 75 行）。但**深度不等于优雅**——其中很大一部分行数是在复刻 JavaScript/TypeScript 语义（`parseInt` 前缀解析、`Number` 溢出、`localeCompare` 排序、zod 投影），这部分不值得抄。
3. **真正"更优雅"的是抽象层，不是工具体**：`Tool`（wire 元数据）与 `ToolCall`（行为）二分、30 个带默认实现的钩子、类型化的进度枚举、每工具 `max_result_size_chars`、每工具 `check_permissions`、以及用 `include_str!` 做的输入 schema 一致性测试。这几条是 Hikari 可以直接借鉴的。

---

## 1. 规模对照

| | Hikari | CometixCode | 倍数 |
|---|---:|---:|---:|
| 工具层总行数 | 约 6 200 | 约 90 100（`src/tools` 178 个 .rs / 87 072 行，含 `tools/mod.rs` 1 056 与一致性测试 535；另 `src/tool.rs` 3 050） | 14.5× |
| 工具目录数 | 0（工具集中在 3 个文件里） | 39 个工具目录 + `shared` | — |
| 模型可见工具名 | 默认约 20（开记忆、有一个 Skill 时）；不含按需发现的 MCP 工具 | 常驻 21 + 条件 16 = 37；另按会话注入 `StructuredOutput`、按需注册 MCP 工具 | — |
| 最大单工具 | `shell` 约 175 行 | `Agent` 20 245 行 | 116× |
| 工具测试 | 约 15 个集成测试文件 | 133 个 `#[cfg(test)]` 模块 / 约 839 个测试函数 | — |

CometixCode 的 `Agent` 一个工具（20 245 行）就比 Hikari 整个工具层（约 6 200 行）大 3 倍。这个对比要先摆出来，因为它决定了后面每一条该怎么读：**CometixCode 是在复刻一个成熟商业产品的完整行为面，Hikari 是在做一个个人助手需要的最小够用集。**

---

## 2. 工具清单对比

### 2.1 两边都有（16 组）

| Hikari | CometixCode | CometixCode 目录 | 文件 | 行数 | 深度差 |
|---|---|---|---:|---:|---|
| `shell` | `Bash` | `bash_tool` | 18 | 12 253 | 极大 |
| `shell`（Windows） | `PowerShell` | `powershell_tool` | 13 | 8 212 | Hikari 无此工具 |
| `read` / `read_file` | `Read` | `file_read_tool` | 4 | 4 599 | 极大 |
| `write` | `Write` | `file_write_tool` | 3 | 2 284 | 大 |
| `edit` | `Edit` | `file_edit_tool` | 6 | 3 146 | 极大 |
| `grep` | `Grep` | `grep_tool` | 3 | 2 341 | 极大 |
| `find` | `Glob` | `glob_tool` | 3 | 1 282 | 大 |
| `ls` | 无（靠 `Bash ls` / `Glob`） | — | — | — | — |
| `web_fetch` | `WebFetch` | `web_fetch_tool` | 5 | 1 773 | 大 |
| `web_search` | `WebSearch` | `web_search_tool` | 3 | 1 119 | 大 |
| `load_skill` | `Skill` | `skill_tool` | 4 | 1 927 | 大 |
| `tool_search` | `ToolSearch` | `tool_search_tool` | 2 | 829 | 中 |
| `todo_list` | `TodoWrite` | `todo_write_tool` | 3 | 551 | 相当 |
| `clarify` | `AskUserQuestion` | `ask_user_question_tool` | 3 | 934 | 相当 |
| `process_manage` | `TaskOutput` + `TaskStop` | 两个目录 | 3 + 3 | 1 136 + 438 | 相当 |
| `delegate_task` | `Agent`（别名 `Task`） | `agent_tool` | 21 | 20 245 | 极大 |

### 2.2 只有 CometixCode 有

| 工具 | 目录 | 行数 | 开关 | 对 Hikari 的价值 |
|---|---|---:|---|---|
| `LSP` | `lsp_tool` | 2 497 | `is_lsp_tool_enabled` | **值得考虑**。诊断/hover/定义，是"改完代码知道对不对"的关键反馈源。Hikari 完全没有 |
| `NotebookEdit` | `notebook_edit_tool` | 2 493 | 常驻 | **低优先**。Jupyter cell 编辑；Hikari 的定位是个人助手，不是数据科学 IDE |
| `EnterPlanMode` / `ExitPlanMode` | `enter_plan_mode_tool` / `exit_plan_mode_tool` | 499 / 818 | 常驻 | **值得考虑**。把"先规划再动手"做成权限模式切换，而不只是一条提示词 |
| `TaskCreate` / `TaskGet` / `TaskUpdate` / `TaskList` | 四个目录 | 约 2 300 | `is_todo_v2_enabled` | **值得考虑**。任务依赖图（`blocks`/`blockedBy`），比 Hikari 的平铺 `todo_list` 强 |
| `EnterWorktree` / `ExitWorktree` | 两个目录 | 319 / 528 | 常驻 | **值得考虑**。git worktree 隔离，配合 `delegate_task` 用可以避免子 agent 污染工作区 |
| `SendMessage` | `send_message_tool` | 1 834 | 常驻 | 低。多 agent 互发消息；Hikari 的 `delegate_task steer` 已覆盖主要用法 |
| `TeamCreate` / `TeamDelete` | 两个目录 | 641 / 384 | `is_agent_swarms_enabled` | 低。蜂群团队管理 |
| `CronCreate` / `CronDelete` / `CronList` | `schedule_cron_tool` | 1 026 | `is_kairos_cron_enabled` | 与 Hikari 路线图的 `cronjob_manage` 同构，但 Hikari 已明确后置 |
| `RemoteTrigger` | `remote_trigger_tool` | 411 | `is_remote_trigger_tool_enabled` | 低。远程触发 |
| `Brief` | `brief_tool` | 1 098 | 常驻 | 低。简洁输出模式 |
| `Config` | `config_tool` | 2 189 | `is_config_tool_enabled` | 低。让模型直接改 CLI 配置；Hikari 的设置是 GUI，模型不该改 |
| `ListMcpResources` / `ReadMcpResource` | 两个目录 | 590 / 507 | 在 `special_tools` 被剔除出默认池 | **值得考虑**。MCP 不只暴露 tools，还暴露 resources；Hikari 的 `mcp.rs` 只处理 tools |
| `mcp__<server>__authenticate` | `mcp_auth_tool` | 338 | 动态 | 中。MCP OAuth 交互式鉴权 |
| `StructuredOutput` | `synthetic_output_tool` | 341 | 按会话注入 | 低。Hikari 已在 `delegate_task` 的 `output_schema` 里覆盖了主要场景 |
| `Sleep` | `sleep_tool` | 50 | **未注册**（无 schema、无 `ToolCall`） | 无 |

### 2.3 只有 Hikari 有

这一栏是 Hikari 不可替换的部分，调研 CometixCode 时不要顺手删掉：

| 工具 | 位置 | 为什么 CometixCode 没有 |
|---|---|---|
| `recall` / `search_observations` / `expand` / `forget` | `hikari-memory/src/tools.rs:4-43` | CC 的 memory 是文件式的（CLAUDE.md 注入），没有召回式长期记忆 |
| `session_search` / `session_read` | `hikari-runtime/src/history.rs:13-28` | CC 有 `/resume` 但不对模型暴露会话检索工具 |
| `image_gen` | `hikari-runtime/src/image_gen.rs:104-134` | CC 不生成图片 |
| `skill_manage` | `hikari-runtime/src/skills.rs:530-562` | CC 的 `Skill` 只能加载，不能安装/制作/回滚 |
| `artifact_read` | `tool.rs:260-268`、`322-348` | CometixCode 走的是另一套：`maybe_persist_large_tool_result_text`（`services/tools/tool_execution.rs:4056-4069`）把超长结果落文件并给引用，没有分页读回工具 |
| `read_file` | `tool.rs:620-660` | CometixCode 只有 `Read`；Hikari 这个是 `read` 的重复实现，见 4.2 |

**顺带一条清理建议**：`read_file` 和 `read` 功能重叠（前者整文件读、无分页、无截断提示；后者分页），两个都暴露给模型只会增加选择噪声。CometixCode 只有 `Read` 一个。

---

## 3. 抽象层对比：真正更优雅的部分

### 3.1 `Tool`（元数据）与 `ToolCall`（行为）二分

CometixCode 把 CC 里"一个对象同时是元数据和行为"拆成两半：

- **wire 侧** `pub struct Tool`（`src/types/tools.rs:184-226`）：`name`、`aliases`、`description`、`input_schema: serde_json::Value`、`input_zod_schema: Option<InputSchema>`、`is_mcp`、`mcp_info`、`mcp_annotations`、`strict`。这是**可序列化的、可 clone 的、进 API 请求的**那一半。
- **行为侧** `pub trait ToolCall: Sync`（`src/tool.rs:2121-2431`）：3 个必需方法（`name`、`prompt`、`call`）+ **30 个带默认实现**的钩子。

好处很具体：进程级单例行为 + 每实例元数据，使得 MCP 工具（运行时才知道名字和 schema）和内置工具可以走同一条 dispatch 路径，而不需要为 MCP 造一套影子类型。`Tool::prompt()`（`types/tools.rs:260-268`）就是这个 dispatch 的唯一入口，注释解释了"CC 的 method dispatch，在这个 port 里必须是一个函数"。

**Hikari 的对应物**：`pub trait Tool`（`tool.rs:22-61`）10 个方法，5 个必需 5 个默认；没有"元数据/行为"分离，`name()`/`description()`/`parameters()` 既是元数据又是行为接口。对内置工具够用；但 `defs` 和 `tools` 是两个平行数组（`tool.rs:64-65`），且 `defs` 里允许存在没有 `tools` 对应的合成条目（`tool_search`、`artifact_read` 靠 `call_output` 里的名字硬编码分发，`tool.rs:388-394`）。这是"优雅度"差距的根源。

### 3.2 30 个钩子里真正有用的那些

| 钩子 | 位置 | 作用 | Hikari 有对应物吗 |
|---|---|---|---|
| `validate_input(args, ctx) -> ValidationResult` | `tool.rs:2310-2316` | 调 `call` 之前校验 | **没有**。Hikari 完全不做入参 schema 校验，只有各工具内部手写检查 |
| `check_permissions(args, ctx) -> PermissionResult` | `tool.rs:2320-2333` | 工具自己声明权限结论（`Allow`/`Ask`/`Deny` + `updated_input`） | **没有**。Hikari 的权限完全靠工具名在 `permission.rs:109-122` 硬编码 |
| `is_read_only` / `is_concurrency_safe` / `is_destructive` / `is_open_world` / `requires_user_interaction` | `tool.rs:2183-2221` | 语义标记 | **没有** |
| `max_result_size_chars()` | `tool.rs:2299-2301` | 每工具输出上限 | 部分有：`output_limit()`（`tool.rs:48-50`），但只有记忆工具和 `load_skill` 覆写 |
| `map_tool_result_to_tool_result_block_param(data, id)` | `tool.rs:2381-2399` | 工具自己决定给模型的 tool_result 长什么样 | **没有**。Hikari 统一走 `tool_result_text`（`protocols.rs:421-452`） |
| `get_tool_use_summary` / `get_activity_description` / `user_facing_name` | `tool.rs:2231-2256` | UI 摘要 | **没有** |
| `inputs_equivalent` / `normalize_input` / `backfill_observable_input` | `tool.rs:2134-2173` | 交互式修改输入时的语义比较与归一化 | **没有** |
| `interrupt_behavior() -> Block | Cancel` | `tool.rs:2205-2207` | 中断时阻塞还是取消 | **没有** |
| `should_defer()` | `tool.rs:2304-2306` | 是否延迟暴露 | 有等价物，但在 registry 层（`register_deferred`，`tool.rs:246-258`），不在工具自己身上 |

这张表就是"更优雅"的核心证据：**CometixCode 让每个工具自己声明它的语义，Hikari 让中央代码猜。**

### 3.3 结果类型：统一信封 vs 类型化枚举

- **Hikari**：`ToolOutput { text, structured, attachments, failure, truncated, retryable }`（`attachment.rs:124-136`）。**一个统一信封**，三个协议共享同一套投影（`protocols.rs:421-452`）。对一个支持三家协议、UI 不做 per-tool 渲染的桌面应用，这是**更优雅**的设计——加一个工具不用动结果类型。
- **CometixCode**：`ToolOutput` 是**按工具分变体的枚举**（`Composed` / `FileReadCall` / `Write` / `WriteError` / `Edit` / `EditError` / `Glob` / `Grep` / `Bash` / `PowerShell` / `TodoWrite` / `AskUserQuestion` / `Config` / `Lsp` / `WebFetch` / `WebSearch` / `Skill` / `Agent` / ... 30 个变体，见 `tool.rs:2433-2460` 的 `output_variant_name`）。每个工具自己实现 `map_tool_result_to_tool_result_block_param` 和 `tool_use_result`（`tool.rs:2381-2411`）。

CometixCode 这套必须这样，因为它的 UI 要为每种工具渲染不同的折叠视图（`ui.rs` 文件占了工具层相当比例：`file_read_tool/ui.rs` 966 行、`bash_tool/ui.rs` 792 行、`agent_tool/ui.rs` 数千行）。**Hikari 不应该抄这个**——GPUI 的工具输出展示是统一的，引入 30 个变体只会增加 `protocols.rs` 的 match 负担。

### 3.4 输入 schema 的一致性保证

CometixCode 有一件事 Hikari 完全没有，而且成本不高：

- `src/tools/cc_input_schemas.json`（41 225 字节）：从官方 CC 提取的 38 个工具的输入 schema 快照。
- `src/tools/input_schema_parity_test.rs`（535 行）：把 Rust 侧 `*_tool_schema()` 生成的 schema 与快照逐字段比对。

也就是说，**"工具的输入 schema 和我们声称对齐的那个版本一致"是被测试钉住的**。Hikari 没有 schema 快照，`parameters()` 改了没有任何测试会红（除了 `tests/discovery.rs` 里对工具可见性的间接覆盖）。

### 3.5 输出预算：每工具上限 + 统一持久化阈值

- CometixCode：`get_persistence_threshold()`（`utils/tool_result_storage.rs:212-229`）+ `maybe_persist_large_tool_result_text`（`services/tools/tool_execution.rs:4056-4069`）；每工具 `max_result_size_chars()` 默认 100 000，`Bash`/`PowerShell` 30 000，`Grep` 20 000，`Read` 用 `usize::MAX`（无上限，因为它自己有 token 校验）。
- Hikari：全局 `OUTPUT_CAP = 16_000`（`tool.rs:17`），溢出落 artifact（sha256 文件名）+ `artifact_read` 分页读回（`tool.rs:270-309`、`322-348`），`structured`/`failure` 与 `text` 共享同一配额（`tool.rs:564-571`）。

**Hikari 这套其实更优雅**：一个上限、一个溢出通道、一个读回工具，模型学一次就够；`tests/discovery.rs:203-250` 还钉住了多字节 emoji 的字符偏移正确性。CometixCode 的"每工具上限 + 落文件 + 引用"在终端 TUI 里合适（用户可以自己 `cat` 那个文件），在 Hikari 里反而需要一个读回工具——而 Hikari 已经有了。**结论：保留 Hikari 的 artifact 设计，但补上每工具上限的覆写能力和"截断"的显式信号。**

### 3.6 进度反馈

CometixCode：`pub enum ToolProgress`（`types/tools.rs:6-95`，7 个变体，含 `BashProgress`/`AgentProgress`/`WebSearch*`），通过 `ToolCallProgressFn<'a>`（`tool.rs:2019`）和 context 级 `ToolProgressSink`（`tool.rs:366-369`）传递。`call` 的签名里就带 `on_progress`（`tool.rs:2374`）。

Hikari：没有工具级进度通道。长工具（`shell`、`delegate_task`）只能在结束时一次性返回；UI 的进度靠 `AgentEvent::ToolExecutionStart`/`End` 和 `Execution` 的 revision 轮询。`shell` 的 stdout 在 8 MiB 上限内被两个线程捕获后**在进程结束时**才返回（`tool.rs:699-700`）。

**这是 Hikari 的一个真实短板**：跑一个 10 分钟的命令，界面 10 分钟没有新信息。CometixCode 的 `ToolProgress` 是解法，但 Hikari 不需要 7 个变体——一个 `enum ToolProgress { Text(String), Percent(f32) }` 加一个 `on_progress: &mut dyn FnMut(ToolProgress)` 参数就够。

### 3.7 注册、延迟暴露与 MCP

两边都有延迟暴露，但形状不同：

- CometixCode 是**请求时叠加**：`is_deferred_tool`（`tool_search_tool/prompt.rs:33-75`）判定，`utils/api.rs:233` 在构造请求时给这些工具加 `defer_loading`，`utils/tool_search.rs:15-46` 扫已发现的名字、`:56-116` 算增量。工具池是 `assemble_tool_pool(permission_context, mcp_tools)`（`tools/mod.rs:162-179`），MCP 工具与内置合并时**内置保持连续前缀**（缓存断点，见 `tools/mod.rs:150-161`）。
- Hikari 是**会话状态**：`deferred: BTreeMap<String, String>` + `loaded: Mutex<Vec<String>>`（`tool.rs:66-67`），加载状态随会话保存、压缩后由 `Message::ToolCatalog` 恢复（`agent.rs:189-211`、`943-965`），BM25 + CJK bigram 检索（`tool_search.rs:123-151`）。

**Hikari 这套不差**：把"加载了什么"变成会话数据而不是每请求重算，更符合"会话可恢复"的要求，而且 `tests/discovery.rs:586-647` 钉住了"压缩后目录仍在、工具仍可调"。CometixCode 的优势在协议侧（原生 `additional_tools` / `tool_addition` / `defer_loading`），Hikari 的 `ToolDisclosure`（`protocols.rs:63-71`）已经覆盖了同样的三种模式。

### 3.8 权限

CometixCode 三层：

1. **池子层**：`filter_tools_by_deny_rules`（`tools/mod.rs:121-136`）——被 deny 规则命中的工具**根本不进请求**，模型看不见。
2. **调用层**：`has_permissions_to_use_tool_inner`（`utils/permissions/permissions.rs:861+`），其中 `:812`/`:1218`/`:1946` 调工具自己的 `check_permissions`。
3. **审批层**：`can_use_tool`（`hooks/use_can_use_tool.rs:420`）→ `handle_interactive_permission`（`hooks/tool_permission/handlers/interactive_handler.rs:20`）推 `ToolUseConfirm`。

Hikari 两层：`assess`（`permission.rs:85-134`）+ `execution.approve`（`tool.rs:480-487`）。没有池子层（未加载的 MCP 工具是被隐藏而不是被 deny），`assess` 靠工具名分支（`permission.rs:92-131`）。

**Hikari 真正做得好的地方**（CometixCode 没有的）：审批后**重新核对已解析目标**——`shell` 命令的资源集合在等待审批前后各算一次，不一致就报 `"命令目标路径已改变，需要重新审批"`（`tool.rs:488-496`），`tests/execution.rs:635-668` 用 junction 钉住了这条。CometixCode 的 `check_permissions` 没有这个 TOCTOU 兜底。**这条要保留。**

**该抄的**：`filter_tools_by_deny_rules` 的"不可用即不可见"是一条干净的规则；Hikari 可以让 `assess` 的 deny 结论同时影响 `visible_defs()`，而不是等到调用时才报错。

---

## 4. 两边都有的工具：实现深度逐项

### 4.1 `shell` vs `Bash` + `PowerShell`

**CometixCode `Bash`（18 文件 / 12 253 行）** 的模块切分本身就是设计：

| 模块 | 行数 | 职责 |
|---|---:|---|
| `bash_security.rs` | 1 123 | 命令注入、危险模式识别 |
| `bash_permissions.rs` | 1 827 | 命令 → 权限判定 |
| `read_only_validation.rs` | 1 881 | 判断命令是否只读（喂给 `is_read_only`） |
| `path_validation.rs` | 1 397 | 命令里出现的路径校验 |
| `sed_validation.rs` + `sed_edit_parser.rs` | 620 + 339 | `sed -i` 被当作编辑操作解析 |
| `should_use_sandbox.rs` | 175 | 是否需要沙箱 |
| `destructive_command_warning.rs` | 121 | 破坏性命令警告 |
| `mode_validation.rs` | 135 | 模式校验（如 `$()` 嵌套） |
| `bash_command_helpers.rs` / `command_semantics.rs` | 414 / 122 | 语义辅助 |
| `mod.rs` | 2 342 | 执行体 |

`PowerShell` 另有 13 文件 / 8 212 行，含独立的 `mode_validation.rs` 和 `path_validation.rs`（受限语言模式、`-EncodedCommand`、`iex` 递归展开）。

**Hikari `shell`（`tool.rs:582-757`，约 175 行）** 也做对了最难的那部分工程：

- 进程**挂起创建**（`creation_flags(0x08000000 | 0x00000004)`，`execution.rs:1042-1051`），先挂到 Win32 Job Object（`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`）再恢复主线程 —— 遍历线程快照找到主线程 `ResumeThread`（`process_job.rs:10-72`）。这样保证子进程不可能逃出 job。**这是 CometixCode 在 Windows 上也要面对的问题，Hikari 的解法是干净的。**
- stdout/stderr 双线程并发排空，各 8 MiB 硬上限（`tool.rs:699-700`、`read_capped` 759-787）。
- 40 ms 轮询，abort/超时就 `job.terminate()` + `taskkill /T /F` + `child.kill()`（`tool.rs:702-726`）。
- 非零退出把输出**和**退出码一起作为 `Err` 返回（`tool.rs:751-755`），空输出区分"完成，没有输出。"和"命令失败了，没有输出。"（`tool.rs:744-750`）。
- 崩溃安全：重启时未完成的执行标记为中断，**不重跑副作用、不操作旧 PID**（`execution.rs:99-116`）。

**Hikari 明显粗糙的地方**：

- 30 秒超时是硬编码常量，没有 per-call 覆写；
- 只设 `PYTHONIOENCODING`（`tool.rs:684`），只用 `cmd.exe`（`execution.rs:1046`），输出 `from_utf8_lossy`（`tool.rs:782`）→ **GBK 控制台中文会乱码**；
- 没有沙箱决策（`should_use_sandbox` 的对应物）；
- `read_capped` 里 `Err(_) => break`（`tool.rs:768`）静默吞掉读取错误。

**判定**：CometixCode 的 `Bash` 更完备，但它的完备性是靠 18 个文件堆出来的。**Hikari 应该补的是三件小事**：per-call 超时、Windows 代码页处理（`chcp`/`encoding_rs`）、以及 PowerShell 作为独立工具（Hikari 的 `shell` 在 Windows 上跑 PowerShell 命令必须写 `powershell -c "..."`，会踩 `inspect_command` 的 wrapper 展开路径，`permission.rs:412-440`）。**不该抄的是把 shell 安全拆成 5 个千行模块**——Hikari 的 `inspect_command`（`permission.rs:376-551`，约 175 行）已经把灾难性清除、编码命令递归、复合语法、wrapper 展开都覆盖了，且有 `tests/execution.rs:87-175` 的回归测试。

### 4.2 `read` / `read_file` vs `Read`

**CometixCode `Read`（4 文件 / 4 599 行）**：

- 文本：`offset` + `limit`，token 上限校验（`validate_content_tokens`，`limits.rs` 的 `DEFAULT_MAX_OUTPUT_TOKENS = 25_000`），超限报 `"File content ({} tokens) exceeds maximum allowed tokens ({}). Use offset and limit parameters to read specific portions of the file, or search for specific content instead of reading the whole file."`；
- **图片**：`IMAGE_EXTENSIONS` 白名单 + `compress_image_buffer_with_token_limit` / `maybe_resize_and_downsample_image_buffer` / `create_image_metadata_text`（`utils/image_resizer`）——按 token 预算压缩和降采样后作为图片结果返回；
- **PDF**：`pages` 参数（`"1-5"`/`"3"`/`"10-20"`），`PDF_MAX_PAGES_PER_READ` 上限，`is_pdf_supported()` 能力探测；
- 边界：`is_blocked_device_path`、`is_unc_path`、alternate screenshot path；
- 环境覆写 `CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS`，且**复刻 JS `parseInt(...,10)` 的十进制前缀行为和 `Number` 溢出**（`limits.rs:26-60`）。

**Hikari `read`（`file_tools.rs:74-86`，约 13 行）**：`offset.max(1)`、`limit.clamp(1,1000)` 默认 200、`format!("{}: {s}", i+1)`。**没有任何截断提示**——offset 越过 EOF 返回空串，模型无法区分"文件读完了"和"文件是空的"。`read_file`（`tool.rs:620-660` + `793-807`）是无分页版本，1 MB 上限，只看前 800 字节有没有 NUL 来判断二进制，`from_utf8_lossy`。

**判定**：CometixCode 的 `Read` 是**明显更优雅也更完备**的。Hikari 必须补的最低限度：

1. `read` 结尾加"文件共 N 行，已读到第 M 行"的显式信号——这是 3 行代码，收益很大；
2. 合并 `read_file` 进 `read`（去掉一个工具）；
3. 图片读取可以复用 Hikari 已有的 `AttachmentStore`（`attachment.rs`）——`read` 到一个 `.png` 时走附件通道，CometixCode 的 `compress_image_buffer_with_token_limit` 思路可以直接借；
4. PDF 分页、UNC、设备路径**不需要**。

**不该抄的**：`FileReadingLimits` 里那套 JS `parseInt` 复刻（`limits.rs:26-60` 用了近 40 行处理正负号、Unicode 空白、ASCII 数字前缀）。这是 Rust 端口为了和 TS 行为逐字节一致付出的代价，Hikari 没有这个约束。

### 4.3 `write` vs `Write`

CometixCode `Write`（3 文件 / 2 284 行）；Hikari `write`（`file_tools.rs:87-98`，约 12 行）：`create_dir_all(parent)` + `fs::write`，报 `content.len()` 标为 bytes。

Hikari 缺：**原子写（临时文件 + rename）**、大小上限、换行/BOM 处理（`edit` 有，`write` 没有——不一致）。CometixCode 的 `Write` 有 2 284 行说明它做了 diff 输出、read-state 更新、结构化 patch 等 UI/上下文侧工作，这些 Hikari 不需要。**但原子写该补，这是 5 行的事。**

### 4.4 `edit` vs `Edit`

**CometixCode `Edit`（6 文件 / 3 146 行）**：

- `replace_all: bool`（`types.rs` 的 `FileEditInput`）；
- `find_actual_string` + `preserve_quote_style`（`utils.rs` 613 行）——**引号风格归一化匹配**：模型写 `"` 而文件里是 `"` 时也能匹配，替换后保留原风格；
- 结构化 patch：`structured_patch: Vec<StructuredDiffHunk>` + `git_diff: Option<ToolUseDiff>`（`types.rs` 的 `FileEditOutput`）；
- `simulated_settings_edit`（`mod.rs:137`）——对配置文件模拟应用；
- `is_non_regular_existing_path`（`mod.rs:71`）——拒绝非普通文件；
- `EditValidationError` 带 `error_code`（`mod.rs:130`）。

**Hikari `edit`（`file_tools.rs:99-123`）**：

- 复用 Pi 的 `strip_bom` / `detect_line_ending` / `normalize_to_lf`（`file_tools.rs:4`），写回时恢复 BOM 和原换行；
- 要求**恰好一次**匹配（`"old_text 匹配 {count} 次，需要精确匹配一次"`）；
- 拒绝空 `old_text`；
- 写前重读文件比对，检测并发修改（`"文件已被其他程序修改，请重新读取"`）。

**判定**：Hikari 的 `edit` **在核心正确性上不输**——BOM/换行保持、精确匹配、并发检测都是对的（`tests/tools.rs` 钉住了 BOM+CRLF 和歧义匹配不写入）。缺的是 `replace_all`（这个模型经常需要）和引号风格容错（这个能显著减少"匹配失败"的往返）。

**Elegance 判定**：CometixCode 用 613 行 `utils.rs` 做字符串匹配，Hikari 用 `content.matches(old).count()`。**Hikari 更优雅；CometixCode 更宽容。** 建议补 `replace_all`，引号容错可以只做最基础的一层（把 `"` `"` `'` `'` 视为等价后重试一次）。

### 4.5 `grep` / `find` vs `Grep` / `Glob`

**CometixCode `Grep`（3 文件 / 2 341 行）**：外壳调 ripgrep（`README.md` 说明了三级 `rg` 解析顺序），支持完整正则、`glob` / `type` 过滤、三种输出模式（`content` / `files_with_matches` / `count`）、`multiline`、上下文行、`head_limit`（默认 250，配 `format_limit_info` 明确报告被截断）、VCS 目录排除表（`.git .svn .hg .bzr .jj .sl`）、deny globs、`to_relative_path` 重写路径、以及 `javascript_locale_compare` 排序。

**Hikari `grep` / `find`（`file_tools.rs:143-217`，两个共用约 75 行）**：

- 手写显式栈 DFS；
- **只支持字面量子串匹配**（`line.contains(pattern)`，`file_tools.rs:197`）——没有正则、没有大小写选项、没有 glob、没有文件类型过滤；
- 硬编码忽略 `.git` / `target` / `node_modules` / `.hikari`（`file_tools.rs:175-180`）；
- 只读 `< 1_000_000` 字节且 `fs::read_to_string` 成功的文件（`file_tools.rs:194-195`）——**二进制和编码错误的文件被静默跳过，从不报告**；
- 行截断 300 字符（`cap_chars`，`file_tools.rs:201`）；
- 200 命中上限、20 000 访问上限（`file_tools.rs:167-169`），**没有"结果被截断"信号**——上限到了就 `break`，模型看到的是"好像只有 200 条"。

**判定**：这是**差距最大的一组，也是收益最大的一组**。原因是具体的：

- Hikari 的 `grep` 无法做 `fn \w+` 这类搜索，模型只能退回 `shell` + 系统 grep，而 Windows 上没有 `grep`；
- 静默跳过不可解码文件 + 静默截断到 200 条，会让模型得出错误结论（"代码里没有用到 X"）；
- `find` 是路径子串匹配，而 CometixCode 的 `Glob` 支持 `**/*.tsx` 这种模式——模型写 Glob 模式是本能。

应该做的：把 `grep` 改成调 `rg`（CometixCode 的 `README.md` 解释了为什么 ripgrep 是外部资产而不是编译进去的：`rg.exe` 放 `vendor/ripgrep/<arch>-<os>/`，找不到就用 PATH 上的），或者至少加正则支持和显式截断提示。这是 Hikari 路线图里没列但值得单独立项的一条。

### 4.6 `web_fetch` / `web_search`

CometixCode `WebFetch`（5 文件 / 1 773 行）含 `preapproved.rs`（186 行）预批准域名和 `utils.rs`（721 行）HTML→文本与安全处理；`WebSearch` 3 文件 / 1 119 行。

Hikari `web.rs`（325 行）适配 Tavily / Exa / Serper 三家 + HTML 提取 + >2 MB 拒绝 + abort（`tests/web_history.rs:146-246` 钉住了 per-provider 认证字段、body 映射、结果上限、凭据出现在 URL 时拒绝、401 响应体不回显）。

**判定**：**Hikari 在这组上更好**——它**不自建搜索后端**，而是适配三家（这是 Hikari 路线图里明确的设计决策，`roadmap.md:43`），CometixCode 则是绑死官方搜索 API。Hikari 的 `>2 MB 拒绝` + `凭据不回显` 是明确的安全考量。

### 4.7 `load_skill` vs `Skill`

CometixCode `Skill`（4 文件 / 1 927 行，`prompt.rs` 178 行）。Hikari `load_skill`（`skills.rs:563-637`）带 `resource` 参数（默认 `SKILL.md`）、`output_limit()` 覆写 128 KiB（`skills.rs:602-604`）、每资源 128 KiB 检查。

加上 Hikari 特有的 `skill_manage`（安装/制作/修改/删除/回滚 + 历史），**Hikari 的 Skill 能力面更宽**（CometixCode 的 Skill 只能加载）。差距在加载路径的细节（CometixCode 的 `prompt.rs` 178 行在描述技能如何被发现和列出）。

**判定**：Hikari 不落后。

### 4.8 `tool_search` vs `ToolSearch`

CometixCode 829 行，做的是**请求时叠加 `defer_loading`**；Hikari 159 行，做的是**会话内 BM25 检索 + 加载状态持久化**（`tool_search.rs`）。Hikari 还多一层：`call_output` 对未加载的 deferred 工具直接拒绝（`tool.rs:395-399`），`tests/discovery.rs:36-131` 钉住了"第一轮隐藏、第二轮可见、新 registry 能恢复"。

**判定**：**两者相当，各有侧重**。Hikari 的 BM25 实现（含 CJK bigram，`tool_search.rs:11-49`）比 CometixCode 的"列出 deferred 工具让模型自己挑"更主动。**Hikari 不落后。**

### 4.9 `todo_list` vs `TodoWrite`（+ `TaskCreate`/`Get`/`Update`/`List`）

`TodoWrite` 551 行 vs Hikari `todo_list`（`execution.rs:1129-1191`，约 62 行）。**规模相当**，功能也相当：都是"LLM 维护的会话内待办，`list`/`replace`"。

差别在 CometixCode 另有一套**任务依赖图**（`TaskCreate`/`TaskGet`/`TaskUpdate`/`TaskList`，约 2 300 行，开关 `is_todo_v2_enabled`），支持 `blocks` / `blockedBy` 关系。

**判定**：平手；Hikari 若要支持多步长任务，`TaskUpdate` 的依赖图是参考。

### 4.10 `clarify` vs `AskUserQuestion`

`AskUserQuestion` 934 行 vs Hikari `clarify`（`execution.rs:1192-1239`，约 47 行 + `execution.rs:613-712` 的等待机制）。

Hikari 的 `clarify` 覆盖了回答问题、拒绝、取消、超时（`tests/execution.rs:325-374` 逐个钉住），且**只阻塞所属会话**（`tests/execution.rs:283-323`）——这比 CometixCode 强，因为 Hikari 是多会话桌面应用，`clarify` 不能阻塞 UI 或其它会话。

**判定**：**Hikari 更优雅**。CometixCode 的 934 行主要在多选 UI 和 channel 门控上。

### 4.11 `process_manage` vs `TaskOutput` + `TaskStop`

`TaskOutput` 1 136 行 + `TaskStop` 438 行 vs Hikari `process_manage`（`execution.rs:1240-1281`，约 41 行 + `Execution::output`/`stop`）。

Hikari 做得好的：后台输出**按 Unicode 字符增量读取**，且 `tests/execution.rs:451-485` 钉住了"不完整 UTF-8 不会导致更早的字符偏移错位"——这是个真问题，CometixCode 的字节偏移实现没有这个保证。跨会话访问被拒绝（`tests/execution.rs:376-447`）。

**判定**：**Hikari 在正确性细节上更好**。

### 4.12 `delegate_task` vs `Agent`

`Agent` 20 245 行 / 21 文件 vs Hikari `delegate.rs` 257 行 + `execution.rs:388-466` 的 `spawn_agent`。

CometixCode 的 `Agent` 覆盖：子 agent 定义解析（`.claude/agents/*.md`）、fork subagent、resume、teams、worktree 隔离、inline/slow-inline 执行模式、UI 渲染（`ui.rs` 数千行）。

Hikari 的 `delegate_task`：`spawn`/`list`/`steer`/`stop`、异步结果投递、隔离工具可见性（`fork_for_execution` 排除 `delegate_task`/`todo_list`/`clarify`/`process_manage`/`session_*`/记忆工具，`tool.rs:152-162`）、共享预算与权限、**子任务不能扩大父任务授权范围**（`tests/delegate.rs:355-432` 钉住 frozen parent permissions）、cancel race、重启报告。

**判定**：CometixCode 的 `Agent` 能力面大得多（团队、worktree、resume），但对 Hikari 的定位（个人助手 + 隔离子任务）来说，**Hikari 的 257 行已经覆盖了 80% 的有用部分，而且权限继承和预算共享的测试比 CometixCode 更细**。值得从 `Agent` 借的是：**resume**（子任务中断后能续）和 **worktree 隔离**（避免多个子任务同时改同一份文件，这正是 `roadmap.md:113` 担心的场景）。

---

## 5. "CometixCode 会不会更优雅"——分三类回答

### 第一类：真优雅，Hikari 该抄

| 项 | 位置 | 为什么该抄 | 成本 |
|---|---|---|---|
| 每工具 `validate_input` | `tool.rs:2310-2316` | 入参校验从"每个工具自己手写"变成"框架调一次" | 低 |
| 每工具 `max_result_size_chars` | `tool.rs:2299-2301` | Hikari 已有 `output_limit()`，只需让更多工具覆写 | 极低 |
| 语义标记 `is_read_only` / `is_concurrency_safe` / `is_destructive` | `tool.rs:2183-2200` | 让权限判定和未来的并发执行有结构化依据，而不是靠 `permission.rs:109-122` 的名字白名单 | 低 |
| 每工具 `check_permissions` | `tool.rs:2320-2333` | 同上；Hikari 现在的名字分支在加工具时必然要改中央代码 | 中 |
| 输入 schema 快照 + 一致性测试 | `input_schema_parity_test.rs` + `cc_input_schemas.json` | Hikari 的 `parameters()` 改动目前无人看守 | 低 |
| 显式截断信号 | 全部工具 | Hikari 的 `ls`（500）、`grep`/`find`（200）是**静默**截断，模型会误判 | 极低 |
| 工具级进度通道 | `ToolProgress`（`types/tools.rs:6-95`） | Hikari 的长工具执行期间界面无反馈 | 中 |
| 池子层 deny 过滤 | `tools/mod.rs:121-136` | 不可用即不可见，比"调用时才报错"干净 | 低 |

### 第二类：细致但不优雅（是 1:1 复刻的代价，不该抄）

- `file_read_tool/limits.rs:26-60` 复刻 JS `parseInt(...,10)` 的十进制前缀 + `Number` 溢出；
- `grep_tool` 里自实现 `is_javascript_whitespace` 和 `javascript_parse_int_base10`；
- `tools/mod.rs:183-185` 用 `javascript_locale_compare` 排序工具名；
- 统一的 `javascript_to_string` / `javascript_to_number` / `javascript_truthy` / `javascript_strict_equal` 出现在 `file_read_tool/mod.rs:225-397`；
- `ToolOutput` 的 30 个按工具分变体 + 每工具 `map_tool_result_to_tool_result_block_param`（因为要 per-tool 终端渲染）。

这些行数占了 CometixCode 工具层相当比例，但它们服务的约束（与官方 TS 逐字节一致、终端 TUI 的 per-tool 折叠视图）Hikari 一个都没有。

### 第三类：Hikari 反而更优雅

| 项 | Hikari 位置 | CometixCode 对应物 |
|---|---|---|
| 统一 `ToolOutput` 信封（text/structured/attachments/failure/truncated/retryable） | `attachment.rs:124-136` | 30 个变体 + per-tool 映射函数 |
| artifact 溢出 + `artifact_read` 分页读回 | `tool.rs:270-348` | 落文件 + 外部引用，无读回工具 |
| 审批后重新核对已解析目标（TOCTOU） | `tool.rs:488-496` + `tests/execution.rs:635-668` | `check_permissions` 无此兜底 |
| 后台输出的 Unicode 字符偏移稳定性 | `tests/execution.rs:451-485` | 字节偏移 |
| `clarify` 只阻塞所属会话 | `tests/execution.rs:283-323` | 全局等待 |
| 会话级工具加载状态随压缩恢复 | `agent.rs:189-211`、`943-965` | 每请求重算 |
| 自带搜索后端适配（Tavily/Exa/Serper）而非绑死官方 | `web.rs` | 绑死官方 WebSearch |
| `skill_manage`（安装/制作/回滚/历史） | `skills.rs:530-562` | `Skill` 只能加载 |
| Windows Job Object 挂起创建 + 进程树托管 | `process_job.rs:10-72`、`execution.rs:1042-1051` | 需自身实现 |

---

## 6. 建议

**立刻做（低成本、纯收益）**

1. `read` 增加"文件共 N 行 / 已读到第 M 行"的截断信号；删除重复的 `read_file`。
2. `ls`（`take(500)`）、`grep`/`find`（200 命中、20 000 访问）加上显式截断提示 —— 现在这三处是静默的，会让模型得出错误结论。
3. `write` 改成原子写（临时文件 + rename）。
4. `edit` 增加 `replace_all` 参数。
5. `Tool` trait 增加 `is_read_only()` / `max_result_size_chars()` 默认方法，先给只读工具和 memory/load_skill 之外的少数工具覆写。

**中期做（中成本、明确收益）**

6. **`grep` 支持正则 —— 这是差距最大的一条。** 要么接 `rg`（外部资产 + PATH 回退），要么至少加正则与大小写选项。当前的字面量子串匹配在 Windows 上没有替代方案，是功能缺口，不只是优雅度问题。
7. `Tool` trait 增加 `validate_input()` / `check_permissions()`，把 `permission.rs:92-131` 的工具名分支逐步下沉到工具自己身上；同时把 deny 结论反映到 `visible_defs()`。
8. 长工具（`shell`、`delegate_task`）增加进度回调，先只做 `Text(String)` 一种。
9. `shell` 增加 per-call 超时参数；`shell` 在 Windows 上的输出编码处理（当前 `from_utf8_lossy` 会乱码 GBK）。
10. 输入 schema 快照测试：把每个工具 `parameters()` 的输出固化，改动即报错。

**评估后再做（需求驱动）**

11. **`LSP` 工具**：如果 Hikari 要认真做代码任务，这是"改完知道对不对"的反馈闭环。CometixCode 的 `lsp_tool`（2 497 行）+ `services/lsp/` 是参考。但它是唯一一个需要常驻外部进程的工具，成本不低。
12. **`EnterPlanMode`/`ExitPlanMode`**：把"先规划"做成权限模式，而不是只靠提示词。对 Hikari 的"审慎/自主"双模式是个自然的第三态。
13. **`Agent` 的 resume + worktree 隔离**：直接解决 `roadmap.md:113` 提出的"多个子任务同时修改同一资源"问题。
14. **`TaskCreate`/`Get`/`Update`/`List` 的依赖图**：只有在 `todo_list` 的平铺列表真的不够用时才做。

**明确不做**

- `SendMessage` / `TeamCreate` / `TeamDelete`（团队蜂群）：Hikari 的 `delegate_task steer` 已够用。
- `NotebookEdit`（Jupyter）、`RemoteTrigger`、`Brief`、`Config`、`Sleep`、`StructuredOutput`：与定位不符或已有替代。
- `CronCreate`/`Delete`/`List`：路线图已后置，且 Hikari 有 GUI 侧的计划。
- JS 语义复刻的全部代码（第二类）。
- 每工具 `ToolOutput` 变体与 per-tool 渲染映射：GPUI 侧统一渲染，引入变体只会增加 `protocols.rs` 的 match 负担。
- `PowerShell` 作为 13 文件 / 8 212 行的独立工具：Hikari 只需要保证 `shell` 在 Windows 上能正确跑 PowerShell 命令并正确解码输出。

### 一句话

CometixCode 的**工具体**更完备（因为它要 1:1 复刻一个成熟产品），**抽象层**更优雅（因为它必须容纳 MCP 动态工具 + 38 个内置工具 + 终端 per-tool 渲染三个约束）；Hikari 的**结果信封、artifact 分页、审批 TOCTOU、会话级延迟加载**这四处反而更优雅。该抄的是抽象层的那几条钩子和"显式截断"这个习惯，该补的是 `grep` 的正则能力和长工具的进度反馈。
