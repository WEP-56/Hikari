# CometixCode loop 调研：是否比 Hikari 更稳健

调研对象：`examples/CometixCode`（Rust 1:1 复刻 Claude Code，AGPL-3.0）。
对比对象：Hikari 当前的 agent loop，`crates/hikari-agent/src/agent.rs` + `pi/*` + `execution.rs`。

结论先写：**就 loop 本身而言，CometixCode 更稳健；但两个「稳健」不是同一种东西。**
CometixCode 的稳健来自对官方 `query.ts` 的逐行复刻（失败恢复路径、上下文分级压缩、显式终态、架构约束测试），代价是 `src/query.rs` 单文件 14750 行、`query_loop` 单函数约 3100 行，并与 CC 的传输层语义深度耦合。Hikari 的 loop 主体约 430 行（`agent.rs:255-684`），在三个方向上有 CC 没有的东西：记忆集成、启发式未完成续写、以及同步 provider 下可审计的简单执行模型。

下面逐维度给证据。

---

## 1. 两份 loop 的形态

| | Hikari | CometixCode |
|---|---|---|
| 入口 | `run_turn_with_control`，`agent.rs:183` | `spawn_query`，`query.rs:435` |
| 主体 | `loop {}`，`agent.rs:255` → `agent.rs:684` | `'query_loop: loop`，`query.rs:892` → `query.rs:3828` |
| 主体规模 | 约 430 行 | 约 3100 行（文件 14750 行） |
| 执行模型 | 同步阻塞函数，调用方自己开线程（`main.rs:887` 的 `std::thread` + `catch_unwind`） | 每个 query 一个独立线程 + 独立 current-thread tokio runtime（`query.rs:488-544`） |
| 事件出口 | `on_event: impl FnMut(AgentEvent)` 回调，`agent.rs:80-153`（16 个变体） | `async_channel` 上的 `QueryEvent`，`query.rs:269-409`（约 27 个变体）+ 命令通道 `QueryCommand`（`query.rs:414-427`） |
| 事件语义 | push | 默认 push；`spawn_query_generator` 走 pull（`query/event_channel.rs:23-79`），模拟 JS async generator 的 `next()`/`return()` |
| 终态 | `error: Option<String>`（中文句子）+ `usage` | `transitions::Terminal { reason, exception }`，`reason` 是 17 个白名单字面量（`query/transitions.rs:60-128`） |
| I/O 依赖 | `TurnHost { provider, tools, memory, abort }`，`agent.rs:14-19` | `QueryDeps` trait（`query/deps.rs:40-90`）：`call_model` / `microcompact` / `autocompact` / `uuid` |
| 第二条 loop | 无 | `src/query_engine.rs`（2239 行）对应 CC `QueryEngine.ts`，headless/SDK 路径独立 |

形态差异直接决定后面每一项的性质：CometixCode 有地方放状态（结构体字段 + 显式 continue），Hikari 只有局部变量和 `continue`。

---

## 2. 逐维度对比

### 2.1 终态与可观测性 —— CometixCode 胜

Hikari 的终止原因是拼出来的中文串，散落在 `agent.rs` 里：

- `"已停下。"`（`agent.rs:278`、`484`、`537`、`604`）
- `"任务 token 预算已用完"`（`agent.rs:367`、`875`）
- `"完整请求超过上下文预算，请缩短输入、角色卡或工具定义，或提高上下文限制"`（`agent.rs:386`）
- `"工具轮数已达上限，调用未执行"`（`agent.rs:589`）
- `"模型暂停的续接次数已达上限"`（`agent.rs:547`）
- `"压缩前没能记下原文。…"`（`agent.rs:811`）

UI 要区分「用户取消」和「预算用尽」，只能做字符串匹配。

CometixCode 有受测试约束的枚举式终态（`query/transitions.rs:60-128`）：

```
completed, model_error, aborted_streaming, aborted_tools, blocking_limit,
collapse_drain_retry, hook_stopped, image_error, max_output_tokens_escalate,
max_output_tokens_recovery, max_turns, next_turn, prompt_too_long,
reactive_compact_retry, stop_hook_blocking, stop_hook_prevented,
token_budget_continuation
```

而且这个白名单不是文档，是**会被执行的断言**：测试用 `include_str!("../query.rs")` 扫源码，找出所有 `Terminal::new("…")` 字面量，任何一个不在名单里就 fail（`query/transitions.rs:82-96`）。`Continue::with_reason` 同样被扫（`:98-113`），`"aborted_*"` 只允许两个值（`:115-127`）。

这一条是纯收益、改造成本极低，值得 Hikari 直接抄：把 `error: Option<String>` 换成 `error: Option<TurnError>`，人话由 UI 渲染。

### 2.2 上下文预算与压缩 —— CometixCode 明显胜

**Hikari**：单点压缩。

- 触发：`input >= available * 4 / 5`（`agent.rs:374`），其中 `input = provider.input_tokens(&context)`。
- 计数：`provider.rs:42-49` 默认实现 = `system + json(messages) + json(tools)` 的字节数 `/ 3`；`ProtocolProvider` 覆写为 `estimate_request`（`protocols.rs:296-298`），实际是把请求体建出来量长度再除 3（`provider.rs:75-91`）。**每轮重新估算全量，不使用上一轮 API 返回的真实 usage。**
- 策略：`maybe_compact`（`agent.rs:761-968`）。先 retain 到记忆（如果开了记忆且 `retained_until < end`），再 `compaction_cut` 保留最后 `COMPACT_KEEP = 8` 条、从最近的 `Message::User` 边界切开（`agent.rs:1064-1077`），旧窗口交给模型做一次结构化摘要，替换成一条 `Message::ContextSummary`。
- 失败处理（**这一处做得对**）：retain 失败就返回错误、不压缩、不丢原文（`agent.rs:810-812`）；摘要请求本身超预算就报错保留原文（`agent.rs:858-870`）；摘要为空或被 abort 就报错保留原文（`agent.rs:936-942`）。注释写得很清楚：`// 提交失败就停。先压缩会把还没记住的原文丢掉。`

**CometixCode**：五级流水线，每级独立模块 + 独立开关。

| 级 | 位置 | 作用 |
|---|---|---|
| snip | `services/compact/snip_compact.rs` | 当前是官方同位置 no-op 桩，只透传 `tokens_freed` |
| microcompact | `services/compact/micro_compact.rs`（1055 行） | 按时间/工具批量清理旧 tool_result，支持 cache-editing（`CachedMcEditsBlock`） |
| context collapse | `services/context_collapse/` | 粒度更细的折叠；开启时接管主动压缩 |
| autocompact | `services/compact/auto_compact.rs`（534 行） | 阈值触发，四档阈值模型 |
| reactive compact | `services/compact/reactive_compact.rs` | 被 400 prompt-too-long 打回来之后的补救压缩 |

关键差别有三个：

1. **token 计数锚定真实 usage。**`utils/tokens.rs:107-125`：从后往前找最后一条带 `usage` 的 assistant 消息，`get_token_count_from_usage(usage) + rough_token_count_estimation_for_messages(tail)`。只有尾部增量是估算的。Hikari 是全量估算，长会话下误差会累积且双向偏。
2. **阈值不是一个比例，是四档状态。**`auto_compact.rs:125-165` 算出 `is_above_warning_threshold` / `is_above_error_threshold` / `is_above_auto_compact_threshold` / `is_at_blocking_limit`，`blocking_limit` 还能用 `CLAUDE_CODE_BLOCKING_LIMIT_OVERRIDE` 覆盖。`get_effective_context_window_size` 会先扣掉「给摘要留的输出预算」（`auto_compact.rs:73-88`）。
3. **失败有断路器。**连续 `MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES` 次失败后不再尝试压缩（`auto_compact.rs:215-224`），`consecutive_failures` 作为 `AutoCompactTrackingState` 在 loop 迭代间传递（`query.rs:1168-1182`）。Hikari 没有这个状态——压缩失败一次就结束本轮。

反向的一条：**Hikari 的「压缩前先 retain 到长期记忆」是 CometixCode 完全没有的能力。**压缩会永久丢失原文，Hikari 至少在丢之前把它交给 Hindsight 落盘，并且 `retained_until` 游标保证不重复提交（`agent.rs:970-1044`）。这是 Hikari 的项目定位带来的结构性优势，不能因为压缩策略简陋就一起丢掉。

### 2.3 失败恢复路径 —— CometixCode 明显胜

先把 Hikari 的恢复路径列全：

| 情形 | Hikari 行为 | 位置 |
|---|---|---|
| 流开启前 429/500/502/503/504 | 重试最多 2 次，固定 500ms/1000ms 退避（无 jitter，无 Retry-After） | `protocols.rs:368-393` |
| 流中途断开/解析失败 | **不重试**，把已收到的部分文本保留成消息，追加「没有连上。{err}」，结束本轮 | `transport.rs:35-37`、`agent.rs:469-479`、`521-540` |
| 400 prompt-too-long | **没有 response-driven 处理**，只有请求前的 `input >= available` 预估拦截（`agent.rs:385-390`）；预估一旦偏低就是把 400 摊给用户 | — |
| `StopReason::Length` | 进 `turn_recovery` 分类为 `BudgetTruncated`，注入 nudge 继续，最多 2 次 | `pi/turn_recovery.rs:84-86`、`agent.rs:555-560` |
| 输出结构未闭合 / 承诺了没做 | `UnclosedStructure` / `SemanticPrematureStop` 启发式，注入 nudge 继续 | `pi/turn_recovery.rs:96-103` |
| `pause_turn` | 原样续接最多 3 次 | `agent.rs:542-549` |
| 模型切换 | **无 fallback** | — |
| token 预算 | 硬停，不是「继续到做完」 | `execution.rs:284-318` |
| 工具轮数 | 硬停，默认 50，上限 1000 | `pi/iterations.rs:3-10`、`agent.rs:584-600` |

CometixCode 的恢复路径（全部是 `continue 'query_loop`，不是结束）：

| 情形 | 行为 | 位置 |
|---|---|---|
| prompt-too-long 被 withheld | context collapse `recover_from_overflow` → 不行则 `try_reactive_compact` → 成功就压完重试 | `query.rs:2618-2702` |
| media size error | 只走 reactive compact（跳过 collapse） | `query.rs:2629`、`2646-2687` |
| `stop_reason == MaxTokens` 或被 withheld 的 max_output_tokens | 先 `max_output_tokens_escalate`：同一请求升到 64k 重试一次，不加恢复消息 | `query.rs:2715-2727` |
| 同上，escalate 用完 | `max_output_tokens_recovery`：把 assistant 输出留在 typed history，追加一条 `isMeta` 的「直接续写、不要道歉、把剩下的活拆小」消息，最多 3 次 | `query.rs:2728-2776`、`37`、`40` |
| 模型 fallback | `model_fallback_from_error` → 换 fallback 模型，剥离 thinking signature block（防签名不匹配），重跑整个请求，不泄漏部分工具结果 | `query.rs:1224`、`1289-1308`、`4179-4217` |
| stop hook 阻塞 | 把 hook 的 blocking message 加进 history 再继续，`stop_hook_active = true` | `query.rs:2830-2860` |
| stop hook 禁止继续 | `Terminal::new("stop_hook_prevented")` | `query.rs:2823-2825` |
| token budget 未达标 | `check_token_budget` 判定还有余量就注入 nudge 继续（0.9 阈值 + 收益递减检测） | `query/token_budget.rs:57-103`、`query.rs:2862-2906` |
| 工具执行中 abort | 为每个未执行的 tool_use 补齐 `Interrupted by user` 的 tool_result，再发中断标记 | `query.rs:2561-2610` |
| 流式→非流式回退产生的孤儿消息 | tombstone | `query.rs:331-333`、`tombstone_streamed_assistant_messages` |

**Hikari 唯一独有的恢复能力**是 `turn_recovery` 的启发式分类（`pi/turn_recovery.rs`）——代码围栏未闭合、悬空列表项、「I will now…」后停住，这三类会触发一次零成本续写。`has_unclosed_fence` 甚至处理了缩进代码块（`>3` 空格不算围栏，`turn_recovery.rs:113-118`）和「反引号围栏的 info string 里出现反引号使 opener 失效」（`:136-138`）。CC 没有这个，CometixCode 自然也没有。这个应该保留。

### 2.4 工具调用配对完整性 —— 平手

两边都做对了「任何一个 tool_use 必须有配对的 tool_result」，这对 Anthropic Messages API 是硬约束。

- Hikari：中断（`agent.rs:528-539`）、超轮数（`:584-599`）、逐个调用前检查 abort（`:603-611`）三处都补了错误 tool_result。
- CometixCode：`yield_missing_tool_result_blocks`（`services/tools/tool_execution.rs`，用于 `query.rs:635-646` 的模型错误路径和 `2572-2599` 的中断路径），配 `processed_tool_use_ids` 去重。

差别在**规模**：CometixCode 还要处理「流式过程中已经作为独立 Message 进入 history 的 assistant 块」，所以有 `streamed_assistant_messages` / `unstreamed_assistant_content` / tombstone 这一整套（`query.rs:1313-1331`、`4772-4938`）。Hikari 因为不是流式落盘、assistant 消息只在流结束后整条入 history（`agent.rs:495-514`），根本没有这个失效模式。**这是 Hikari 简单性换来的免费稳健。**

### 2.5 取消 —— CometixCode 更彻底，但 Hikari 的补丁值得注意

Hikari 用全局 `Arc<AtomicBool>`，检查点：loop 顶部（`agent.rs:277`）、每个工具调用前（`:603`）、流回调里（`:429`、`:443`）、`call_with_abort` 默认实现（`tool.rs:51-60`）。问题在于 provider 是同步阻塞的：`ureq` 阻塞在 `read_line` 时这个 flag 不会被看到。所以 `agent.rs:226-243` 专门开了一个看门狗线程，每 40ms 轮询一次：

```rust
while task.is_some() && execution.active() == task
    && execution.task().is_some_and(|t| !t.status.terminal())
    && !abort.load(Ordering::Relaxed)
{
    if execution.check_deadline().is_err() { abort.store(true, ...); break; }
    std::thread::sleep(std::time::Duration::from_millis(40));
}
```

注释说的是「deadline cancellation must also reach a provider blocked waiting for its next chunk」——即：这是一个**用轮询线程补同步 I/O 不可中断的补丁**。

CometixCode 的取消是贯穿的：`AbortController` 有父子链（`query.rs:429-433` 的 `child_of(seed.abort_controller)`），tokio `select!` 在命令通道上收 `Abort`（`query.rs:1377-1380`），HTTP 请求拿到同一个 signal（`query.rs:1244-1246`），工具执行循环轮询，pull 消费者 drop 也会 `close()`（`query/event_channel.rs:105-124`）。而且它显式记下了「为什么 Rust 需要这一层 CC 没有的东西」——CC 里 `query()` 是单事件循环上的 async generator，AbortSignal 直接生效。

**判断**：CometixCode 的方案在架构上更正确，但前提是 tokio 全异步栈。**不必因此重写 `Provider` trait**（`provider.rs:53-58` 现在是同步 `fn stream`）——同步栈下有一个几十行的等价改法：把流式读的超时从 `config.timeout_seconds`（默认 60 秒）降到 2 秒，并把 `read_json_stream` 的循环（`transport.rs:30-60`）改成"超时即检查 abort 再继续"，abort 的最坏延迟就从 60 秒降到 2 秒。具体方案见 [roadmap2.md 第 3 条](../roadmap2.md)。全异步重写（收益是可取消 + 工具并发，成本是整个 agent crate 的线程模型）可以推到以后。另外应补一条：`call_with_abort` 只在工具实现主动检查时才有效，而 `Tool::call` 的默认路径（`tool.rs:26`）压根不看 abort。

顺带记一条实测数据：取消延迟的实际上界是 `config.timeout_seconds`（默认 60 秒），因为 abort 无法打断已阻塞的 socket read；另外 SSE 读取期间的空闲超过该超时会被报成「流读取中断」并结束本轮（`transport.rs:37`），部分网关的心跳间隔会踩到这个。

### 2.6 并发 —— CometixCode 更激进，是 trade-off 不是纯优势

Hikari 严格串行：`for call in calls { host.tools.call_output(...) }`（`agent.rs:602-669`）。并行只发生在 `delegate_task` 的子 agent 和后台进程（`delegate.rs:103`、`execution.rs:388`）。

CometixCode 有两层并发：

1. `run_tools` 按 `is_concurrency_safe` 把 tool_use 分批，安全批用 `std::thread::spawn` 并发执行（`services/tools/tool_orchestration.rs:72-230`、`286-380`）。`is_concurrency_safe` 默认 **false**（`tool.rs:2183-2185`），只有只读工具覆写。
2. `StreamingToolExecutor`：模型还在流式输出时，tool_use 块一到就开始执行（`query.rs:1338-1361`、`2123-2129`），由 feature gate `tengu_streaming_tool_execution2` 控制（`query/config.rs:79-80`）。

**是否更稳健？** 对「延迟」和「吞吐」是，对「可预测性」不是。并发工具的副作用顺序不再是模型给的顺序，需要 `is_concurrency_safe` 判定正确、需要 permission 交互在并发下的暂停语义（`tool_orchestration.rs:472-560` 有专门测试 `run_tools_serially_pauses_at_first_interactive_permission_request`）。CometixCode 为此写了大量测试。**Hikari 工具集里只读工具（read/ls/find/grep/session_read/web_fetch）占比不低，理论上可以受益，但当前 50 轮上限 + 串行执行并不是瓶颈。建议先不做。**

另外 `is_read_only` / `is_destructive` / `is_open_world` / `requires_user_interaction` 这一组语义标记（`tool.rs:2192-2221`）Hikari 完全没有对应概念——权限判定（`permission.rs`）是靠工具名硬编码的（`permission.rs:109-122` 的 always-Allow 名单）。这属于 2.8 提到的「工具语义元数据」问题。

### 2.7 运行时隔离、panic 与卡死 —— 各有取舍

**Hikari 对 panic 更保守。**`main.rs:887-904` 用 `catch_unwind(AssertUnwindSafe(...))` 包住整个 `run_turn_with_control`，panic 变成一条 `AgentEnd { error: Some("内部出错了。") }`。用户不会看到界面卡在「运行中」。

**CometixCode 对 teardown 卡死更保守。**`query.rs:488-544`：actor 跑在一个 `new_current_thread` runtime 上，结束后用 `runtime.shutdown_timeout(Duration::from_secs(2))`。源码注释直接点名了失效模式：

> A plain `drop(runtime)` waits UNBOUNDED on in-flight `spawn_blocking` work — one wedged blocking task and `Terminal` below never sends, i.e. "UI stuck loading forever".

但同时：**actor 线程内的 panic 没有 `catch_unwind`。**`std::thread::spawn` 的闭包 unwind 之后 `event_tx` 被 drop，消费端只会看到 channel closed，不会收到 `QueryEvent::Terminal`。工具调用层有兜底（`tool_orchestration.rs:225`、`streaming_tool_executor.rs:167` 都有 `catch_unwind`），但 query loop 本身没有。

**两边可以互相抄**：Hikari 抄 `shutdown_timeout` 的思路（虽然它是同步的，对应的是「看门狗 + 有界 join」）；CometixCode 抄 `catch_unwind` 兜到 `Terminal`。

### 2.8 架构约束与可测试性 —— CometixCode 胜得最多

Hikari 用内嵌 `#[cfg(test)] mod tests`（`agent.rs:1145-1527`）和 `tests/loop.rs`（255 行）测 loop，provider 是手写 `Scripted`（`agent.rs:1155-1212`）。可以测行为，但没有结构约束。

CometixCode 把「架构规则」写成可执行断言（`query/transitions.rs`）：

- `production_query_core_keeps_permission_and_tool_result_ownership_outside_query_rs`（`:131-153`）：`query.rs` 的生产代码里**不允许出现** `PermissionRequest {`、`ToolResult {`、`query_model_with_streaming`。也就是说：query 层不许自己造 tool_result、不许自己发权限请求、不许绕过 `QueryDeps::call_model` 直接打模型。
- `query_layout_keeps_only_official_query_modules_and_no_legacy_tool_context_files`（`:156-196`）：`src/query/` 目录下**只允许** 6 个文件，且 `src/services/tools/tool_context.rs` / `tool_executor.rs` 不许存在。
- `query_terminal_literal_reasons_use_official_query_names`（`:60-128`）：上文的终态白名单。

这不是文档，是 CI 门禁。重构时谁想把 query 层和工具执行层揉在一起，测试会红。

另外 `QueryDeps` 依赖注入（`query/deps.rs:40-90`）+ `production_deps()`（`:93-113`）让 loop 可以在不碰网络的情况下被完整驱动；`CallModelStream` 是 `tokio::sync::mpsc::Receiver<QueryModelStreamItem>`（`deps.rs:17-19`），fixture 可以逐条投喂。

Hikari 的 `Provider` trait（`provider.rs:40-59`）其实已经有了同样的接缝（`Scripted` 就是这么用的），差距在**没有把架构规则落成断言**。

### 2.9 提示缓存稳定性 —— CometixCode 胜

Hikari 每轮重发全量上下文，没有缓存意识。唯一相关设计是 `Message::ToolCatalog` 包裹工具目录，压缩后插回原位（`agent.rs:189-211`、`943-965`）——这是为了压缩后工具加载状态不丢，不是为了缓存前缀。

CometixCode 有专门处理：

- `content_replacement_state` 在整个 query loop 内保持稳定，源码注明理由是「keeps decisions stable across this query loop so prompt cache prefixes remain byte-identical on later continuations」（`query.rs:854-858`）。
- `assemble_tool_pool` 明确**不是**扁平排序：内置工具和 MCP 工具各自排序、内置保持连续前缀，因为服务端的缓存断点落在「最后一个 prefix-matched 内置工具」之后（`tools/mod.rs:150-161`）。

### 2.10 记忆集成 —— Hikari 独有

这一层 CometixCode 完全没有，CC 的 memory 是文件式的（`memdir/`、CLAUDE.md 注入），不是召回式的。

Hikari 的设计里值得记录的几点：

- **召回不进 history。**`with_recall` 只改本轮 system prompt（`agent.rs:1050-1061`），`AgentEvent::MemoryRecall` 是主机事件而不是模型输出，测试 `recall_is_not_stored_as_a_message`（`agent.rs:1287-1349`）钉住了「召回文本不出现在 messages 里、也不进 retain」。这是对的：召回是资料，不是对话。
- **同一轮内复用召回。**`cached_recall` 按 `request.query` 做键（`agent.rs:298-339`），工具轮之间不重复打检索。
- **短查询补上下文。**query 少于 25 字且含「继续/刚才/那个/上次/接着/continue/that」时，拼上最近 3 条用户消息的 600 字（`agent.rs:311-323`）。
- **压缩前先 retain**（2.2 已述）。
- **forget 的额外闸门。**`forget` 只在本轮用户明确说了「忘/删除记忆/forget/erase memory/delete memory」时才执行（`agent.rs:625-631`），执行成功后清空 `cached_recall`（`:637-639`）。这是防模型自作主张删记忆，比 CC 那套「工具权限」更贴定位。

---

## 3. 结论

### 3.1 直接回答「是否更稳健」

**是。**在 loop 层面，CometixCode 在以下五个方面明确强于 Hikari：

1. **失败恢复**：有 response-driven 压缩、max_tokens 升档与恢复续写、模型 fallback；Hikari 只有请求前预估 + 启发式续写。
2. **上下文管理**：五级压缩 + 真实 usage 锚定的计数 + 四档阈值 + 失败断路器；Hikari 是单点压缩 + 全量字节估算。
3. **终态与可观测性**：17 个受测试约束的终态；Hikari 是中文错误串。
4. **架构约束**：用 `include_str!` 测试锁住分层；Hikari 没有。
5. **缓存前缀稳定性**：显式设计；Hikari 无意识。

但要把「更稳健」拆成两件事：

- **更不容易在长会话 / 异常网络 / 大上下文下失败**：CometixCode。这部分 Hikari 应该补。
- **更不容易因为复杂度本身出错**：Hikari。3100 行单函数、27 个事件变体、多 carrier（`QueryEvent::Message` vs `Row` vs `ModelMessage`，`query.rs:276-346` 的注释自己承认这是 "Batch-D3 audited residue"）意味着任何改动都要理解 CC 的原始形状。Hikari 430 行的 loop 一个下午能读完。

而且 Hikari 有三样 CometixCode 没有的：记忆召回/留存闭环、启发式未完成续写、`catch_unwind` 级别的整轮兜底。

### 3.2 建议补的（按投入产出排序）

**第一批（低成本，纯收益）**

1. **`TurnError` 枚举化。**`error: Option<String>` 换成 `Option<TurnError>`，人话交给 UI。顺手把 `AgentEnd` 的 `error` 字段类型改掉。借鉴 `query/transitions.rs:60-128` 写一个白名单断言测试。
2. **token 计数锚定真实 usage。**Hikari 已经在 `draft.usage` 里拿到了每轮真实 input/output（`agent.rs:411-413`、`491-494`），只是没用来做压缩决策。改成「最后一条带 usage 的 assistant 消息的 input + 尾部估算」，参照 `utils/tokens.rs:107-125`。
3. **模型 fallback。**`stream_result` 失败时，如果配置了 fallback 模型，剥掉 thinking 相关字段后换模型重试一次。Hikari 的 `Provider` 每次 `stream(&context, ...)` 都是独立调用，换模型需要在 `ProtocolProvider` 上加一个 override（`protocols.rs:302`）。

**第二批（中成本）**

4. **response-driven 压缩。**检测到错误文本里有 prompt-too-long 特征（或 HTTP 400 + 特定 code）时，强制压缩一次再重试，而不是直接把 400 摊给用户。这是 Hikari 当前最大的单点风险——`agent.rs:385-390` 的预估一旦偏低就必然失败。
5. **max_tokens 升档。**Hikari 已有 `turn_recovery` 处理 `StopReason::Length`（`pi/turn_recovery.rs:86`），但走的是「注入 nudge 继续」；CC 的做法是先无消息升档重试一次（`query.rs:2715-2727`）。两者可以合并：先升档，升档用尽再 nudge。
6. **压缩失败断路器。**`maybe_compact` 失败后本轮结束（`agent.rs:810-812`），但没有「连续失败 N 次就不再试」的状态。加在 `TurnRequest` 或 `Execution` 上。

**第三批（高成本，需要判断）**

7. **流中途失败重试。**需要 tombstone 机制：已流出的部分 assistant 文本要能从 history 里抹掉才能重试。Hikari 现在是「保留部分文本 + 结束本轮」（`agent.rs:469-479`），这是个诚实但不理想的行为。改造成本高，收益取决于 provider 稳定性。
8. **read-only 工具批并发。**需要先引入 `Tool::is_read_only` / `is_concurrency_safe`（Hikari 的 `Tool` trait 没有，`tool.rs:22-61`），再把 `agent.rs:602-669` 的串行循环改成分批。当前不是瓶颈，建议后置。

**不建议抄的**

- `StreamingToolExecutor`：收益是首 token 延迟，成本是整套流式工具状态机 + tombstone + 多 carrier。Hikari 的 UI 是 GPUI 桌面端，不是终端 TUI，延迟敏感度不同。
- content replacement / cache-editing microcompact：绑定 Anthropic 服务端缓存语义，Hikari 支持三家协议，收益不一致。
- stop hooks：CC 的 hooks 是一整个生态（用户 shell 脚本挂在 Stop/PreToolUse/PostToolUse 上），Hikari 没有这个产品面。
- 27 个事件变体级别的 UI 事件协议：Hikari 有 GPUI 的 `cx.notify()` 模型，不需要把渲染状态机搬到 agent 层。

### 3.3 一句话

CometixCode 的 loop 是「把 CC 的每一个失败模式都显式接住」，Hikari 的 loop 是「把一条主路径写清楚，剩下的交给用户」。前者更稳健，后者更可维护；Hikari 该补的是**失败恢复的密度**（第 1–6 项），不该补的是**状态机的规模**。
