# Hikari 调优路线图 2

针对 [docs/cometixcode-loop.md](docs/cometixcode-loop.md) 与 [docs/cometixcode-tools.md](docs/cometixcode-tools.md) 的调研结论，按投入产出排序的可执行清单。每条给出：目标 / 现状证据 / 粗浅实现方案 / CometixCode 对照路径 / 验收标准。

排序原则：前 3 条互相耦合（取消与重试都落在 `transport.rs` 的同一个读循环里），必须一起看；4-5 条是一对（缓存改变 token 计数的含义）；6 条是当前最大的单点风险；7-10 条相对独立，可并行。

---

## 1. 终态枚举化

**目标**：把 loop 的终止原因从中文错误串变成机器可读枚举，UI 和测试按类型分流。

**现状证据**

- 终止原因全部是拼出来的中文串，散在 `agent.rs`：`"已停下。"`（`agent.rs:278`、`484`、`537`、`604`）、`"任务 token 预算已用完"`（`:367`、`875`）、`"完整请求超过上下文预算，…"`（`:386`）、`"工具轮数已达上限，调用未执行"`（`:589`）、`"模型暂停的续接次数已达上限"`（`:547`）、`"压缩前没能记下原文。…"`（`:811`）。
- 出口类型：`AgentEvent::AgentEnd { messages, error: Option<String>, usage, retained_until, context_base }`（`agent.rs:115-121`）。
- 唯一消费点：`main.rs:1141` → `finish_pending(idx, error.as_deref().unwrap_or(""))` → `fn finish_pending(&mut self, idx: usize, error: &str)`（`main.rs:1240-1252`），把字符串直接写进聊天气泡。
- 另有 `AgentEvent::AutoCompactionEnd { error: Option<String> }`（`agent.rs:125-127`）→ `main.rs:1119-1122` 走 `Line::note(error)`。

**粗浅实现方案**

在 `hikari-agent` 新增一个枚举，`error` 字段换成它，UI 侧再映射回文案：

```rust
// crates/hikari-agent/src/error.rs（或新增 turn.rs）
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TurnOutcome {
    Completed,
    Stopped,                       // 用户取消
    ToolRoundLimit { used: usize, cap: usize },
    PauseTurnLimit { used: usize },
    TokenBudgetExhausted,
    ContextOverBudget,             // 请求前预估就超了
    CompactionFailed { reason: String },
    Provider { kind: ProviderErrorKind, message: String },
    Panic,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ProviderErrorKind { Retryable, RateLimited, ContextOverflow, Auth, StreamBroken, IdleTimeout, Unknown }
```

改法：

1. `run_turn_with_control` 里的 `let mut error = None;`（`agent.rs:246`）改成 `let mut outcome = TurnOutcome::Completed;`，各 `error = Some("…".into())` 逐处替换成对应变体（10 处左右，机械改动）。
2. `AgentEvent::AgentEnd.error: Option<TurnOutcome>`；`AgentEvent::AutoCompactionEnd.error: Option<TurnOutcome>`。
3. `hikari` crate 加一个渲染函数 `fn outcome_text(o: &TurnOutcome) -> Cow<'_, str>`，`finish_pending` 的签名从 `&str` 改成 `Option<&TurnOutcome>`，或保留 `&str` 在调用点先渲染。**注意**：`finish_pending` 现在的语义是"如果气泡还是空的就填上这句"（`main.rs:1245-1251`），枚举化后这个逻辑不变。
4. 会话持久化：`error` 不进存档（只存 `messages`），所以没有迁移问题。

**CometixCode 对照路径**

- `src/query/transitions.rs:4-18` — `Terminal { reason: String, exception: Option<String> }`。注意它用的是**字符串** `reason`，不是 Rust 枚举；白名单靠测试守。
- 17 个合法值：`src/query/transitions.rs:62-80`。
- 守白名单的测试：`src/query/transitions.rs:82-96`（用 `include_str!("../query.rs")` 扫 `Terminal::new("…")` 字面量），`:98-113` 扫 `Continue::with_reason`，`:115-127` 限制 `"aborted_*"` 只有两个值。
- 产生点：`src/query.rs` 全文，例如 `:1210`（blocking_limit）、`:2701`（terminal_reason_for_system_api_error）、`:2824`（stop_hook_prevented）、`:3804`（max_turns）。

**建议取舍**：抄它的**思想**（显式终态 + 测试守白名单），但**用 Rust 枚举而不是字符串**——Hikari 是单 crate 编译期检查，没有 CC 那种"必须和官方字符串一致"的约束。同时抄它那条"扫源码字面量"的测试习惯，改成扫 `TurnOutcome::` 变体，防止以后有人偷偷加字符串错误。

**验收**

- `AgentEnd` 的 `error` 字段类型不再是 `String`。
- 新增测试，断言 `agent.rs` 生产代码里没有残留的 `error = Some("…".into())` 形态。
- 现有 `tests/loop.rs:222-257`（取消时每个未执行工具都要有结果）仍然通过。

---

## 2. 重试拆分 + `Retry-After`（只做流开启前那半）

**目标**：把"网络抖一下就断任务"变成"10 次指数退避重试"，并尊重服务端的退避指令。

**现状证据**

- 唯一的重试在 `protocols.rs:368-393`：

```rust
let mut attempts = 0;
let response = loop {
    match request.clone().send_json(body.clone()) {
        Ok(response) => break response,
        Err(error) if attempts < 2
            && matches!(&error, ureq::Error::Status(429 | 500 | 502 | 503 | 504, _)) =>
        { attempts += 1; for _ in 0..(attempts * 10) { /* 50ms x N */ } }
        Err(error) => return Err(crate::transport::http_error(error, &secrets)),
    }
};
```

- 事实：最多 **2 次重试**（3 次尝试），固定 500ms / 1000ms，**无 jitter**，**不读 `Retry-After`**（`pi/sse.rs:208-242` 解析了 SSE 的 `retry:` 字段但从未被消费）。
- `ureq::Error::Transport`（DNS、连接被拒、TLS、超时、连接重置）**完全不重试**（`protocols.rs:387-391`）。
- `OpenAiProvider` 无任何重试（`openai.rs:73-76`），且它是死代码。
- 流中途失败在 `read_json_stream` 之外（`protocols.rs:395`），**不在重试范围内**——这是本条的边界，不要顺手扩。
- 现有测试**完全没有覆盖重试路径**。

**粗浅实现方案**

1. 把重试策略抽成纯函数，便于测试：

```rust
// crates/hikari-agent/src/transport.rs（新增）
pub struct RetryPolicy { pub max_attempts: u32, pub base_ms: u64, pub max_ms: u64 }

impl Default for RetryPolicy {
    fn default() -> Self { Self { max_attempts: 11, base_ms: 500, max_ms: 32_000 } }
}

/// 纯函数：给定尝试序号与服务端 Retry-After，算出应等多久。
pub fn retry_delay_ms(policy: &RetryPolicy, attempt: u32, retry_after_s: Option<u64>) -> u64 {
    if let Some(s) = retry_after_s { return s.saturating_mul(1000); }   // 尊重服务端，不封顶
    let exp = policy.base_ms.saturating_mul(1u64 << (attempt.min(10)));
    let base = exp.min(policy.max_ms);
    base + pseudo_jitter(base)                                          // 0..25%
}

/// 哪些错误可重试。
pub fn is_retryable(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::Status(code, _) => matches!(*code, 408 | 409 | 429 | 500..=599),
        ureq::Error::Transport(_) => true,   // DNS/连接/TLS/超时/重置 都值得再试
    }
}

/// 从响应头取 Retry-After（整数秒；HTTP-date 形式先忽略）。
pub fn parse_retry_after(response: &ureq::Response) -> Option<u64> {
    response.header("retry-after").and_then(|v| v.trim().parse().ok())
}
```

2. `protocols.rs:368-393` 改成调这三个函数；退避循环里保留现有的 abort 检查（现在是 50ms 粒度，改成 100ms 粒度检查一次即可，因为总量变长了）。
3. jitter 不要引入 `rand` 依赖：`Cargo.toml` 已有 `getrandom 0.3.4`（CometixCode 的 `random_unit_interval` 也是 getrandom + 失败降级为 0）。
4. **注意**：`ureq::Error::Status(429)` 带的是 `Response`，所以能读 `Retry-After`；`Transport` 类错误没有 response。签名上要区分这两种情况。
5. 可配置：`ProviderConfig`（`protocols.rs:39-99`）加 `max_attempts: Option<u32>`，默认 11；`validate()`（`:102-144`）加范围校验。

**CometixCode 对照路径**

- 引擎：`src/services/api/with_retry.rs:787` `with_retry(...)`。
- 预算：`DEFAULT_MAX_RETRIES = 10`（`:150`）→ `let end = max_retries + 1; while attempt <= end`（`:818-820`），即 11 次尝试；环境变量 `CLAUDE_CODE_MAX_RETRIES`（`get_default_max_retries` `:702`）。
- 退避：`get_retry_delay`（`:569-582`）—— `base = 500 * 2^(attempt-1)`，`.min(max_delay_ms)`，加 `random_unit_interval() * 0.25 * base` 的 jitter；`BASE_DELAY_MS = 500`（`:147`）、`DEFAULT_MAX_DELAY_MS = 32_000`（`:180`）。
- `Retry-After`：`get_retry_after`（`:657`）/ `get_retry_after_ms`（`:664`）**只解析整数秒**，且命中时**直接返回、绕过 `max_delay_ms`**（`:570-574`）；429 在 persistent 模式下优先读 `anthropic-ratelimit-unified-reset`（`get_rate_limit_reset_delay_ms` `:674`），封顶 6 小时（`:688`）。
- 可重试集合：`should_retry`（`:481-552`）—— 408/409/429/401/`>=500`、body 含 `"type":"overloaded_error"`、可解析的 max_tokens 溢出、`x-should-retry: true`；另外**所有 `Connection` 类错误都可重试**（`:1086-1096`）。不可重试：400（除溢出）、403（除非 CCR 模式）、404、413。
- 流中途不重试的证明：`with_retry` 只包住 `create_stream_with_response_and_options`（`claude.rs:3640-3659`），SSE 排空在 `drain_sdk_message_stream_to_channel`（`claude.rs:3767`）**之外**。
- 重试心跳：`create_system_api_error_message(format_api_error(api_err), delay_ms, attempt, max_retries)`（`:1232-1243`），UI 渲染成 `"{error} · Retrying in {}s (attempt {}/{})"`（`components/messages/system_api_error_message.rs:22-25`）。**Hikari 应该抄这个**——Hikari 有 `AgentEvent` 通道，可以在退避期间发一条 note 让用户知道在重试。
- 测试：`with_retry.rs:1285-1340`（退避数值、retry-after）、`:1415-1427`（529 的 foreground/background 分流）、`:1619-1637`（jitter 范围）。Hikari 的 `retry_delay_ms` 是纯函数，可以照抄这套测试形状。

**验收**

- 新增单元测试：`retry_delay_ms` 的指数增长、上限封顶、`Retry-After` 覆盖、jitter 落在 `[base, base*1.25]`。
- 新增集成测试：用 `TcpListener` 起一个先返回 503 两次再返回正常 SSE 的假服务，断言 `stream()` 最终成功且总等待时间符合预期。`tests/protocols.rs:370` 已有 `TcpListener` 的用法可以复用。
- 手工验证：断网后发起一轮，观察重试次数与最终错误文案。

---
## 3. SSE 空闲独立化 + read timeout 降低（同时修取消）

**目标**：把"取消要等 60 秒"和"模型慢一点就被判定为断线"一起解决。**这是十条里最推荐先做的一条**——几十行换掉两个真问题。

**现状证据**

- provider 完全同步阻塞：`fn stream(&self, context: &Context<'_>, abort: &AtomicBool, on_event: &mut dyn FnMut(StreamEvent)) -> Result<()>`（`provider.rs:53-58`），`hikari-agent` 里没有任何 async。
- read timeout 就是整体超时：`protocols.rs:282-288` 用 `http.timeout_read(Duration::from_secs(timeout))`，`timeout = config.timeout_seconds`，默认 60（`:96`），校验范围 `1..=600`（`:117`）。
- 读循环（`transport.rs:28-60`）：

```rust
loop {
    if abort.load(Ordering::Relaxed) { return Ok(()); }     // 只在读之前检查
    line.clear();
    let n = reader.read_line(&mut line)
        .map_err(|e| AgentError::new(format!("流读取中断：{e}")))?;   // 超时 = 致命
    ...
}
```

- 后果一：**取消延迟上界 = `timeout_seconds`**（默认 60 秒），因为 abort 打不断已经阻塞在 socket 上的 `read_line`。
- 后果二：**任何 60 秒的 SSE 空闲都被报成 `"流读取中断：{e}"` 并结束本轮**（`transport.rs:37`）。部分网关心跳间隔大于 60 秒，或模型长时间 thinking，就会误杀。
- 后果三：无法区分"超时"和"连接断了"——两者都是同一句中文。
- `ureq` 的 read timeout 按单次 `read` 系统调用生效（`set_read_timeout`），所以超时后连接仍然可用，**可以继续读**。这是本条方案能成立的前提。

**粗浅实现方案**

不改 async，只把读循环改成"超时即检查 abort 再继续"：

```rust
// transport.rs
const IDLE_SLICE: Duration = Duration::from_secs(2);
const IDLE_LIMIT:  Duration = Duration::from_secs(90);   // 可配

// provider 构造时：read timeout 从 timeout_seconds 改成 IDLE_SLICE
http.timeout_read(IDLE_SLICE)

// 读循环
let mut last_data = Instant::now();
loop {
    if abort.load(Ordering::Relaxed) { return Ok(()); }
    line.clear();
    match reader.read_line(&mut line) {
        Ok(0) => { /* EOF 分支，原样保留 */ }
        Ok(_) => { last_data = Instant::now(); /* 原有 feed 逻辑 */ }
        Err(e) if is_timeout(&e) => {
            if abort.load(Ordering::Relaxed) { return Ok(()); }
            if last_data.elapsed() >= IDLE_LIMIT {
                return Err(AgentError::new("流空闲超时，未收到新数据。"));
            }
            continue;                       // 静默重试，不算错误
        }
        Err(e) => return Err(AgentError::new(format!("流读取中断：{e}"))),
    }
}
```

配套：

1. `is_timeout(&e)`：`ureq` 的超时错误是 `io::ErrorKind::WouldBlock` 或 `TimedOut`，用 `e.kind()` 判断；`ureq::Error::Transport` 里的 `io` 源也要看。实现时先打印实际 kind 确认一次。
2. 空闲上限走配置：`ProviderConfig` 加 `idle_timeout_seconds: Option<u64>`，默认 90；`validate()` 里约束 `>= 2 * IDLE_SLICE`。
3. `timeout_seconds` 的语义改变：现在它同时是"整体超时"和"读超时"。改造后只用于连接超时。**在设置界面加一句说明**，避免用户误解。
4. 新增两个错误变体，供第 1 条的 `ProviderErrorKind` 使用：`IdleTimeout` 与 `StreamBroken`。这样"模型慢"和"网络断"在 UI 上能分开。

**CometixCode 对照路径**

- 独立 idle watchdog：`CLAUDE_ENABLE_STREAM_WATCHDOG` 开关（`claude.rs:3702-3707`）、`CLAUDE_STREAM_IDLE_TIMEOUT_MS` 默认 `90_000`（`:3710-3715`）、半程警告、`STREAM_STALL_THRESHOLD_MS = 30_000`（`:3718`）。
- 超时行为：丢掉流并 `anyhow::bail!("Stream idle timeout - no chunks received")`（`claude.rs:3851-3871`）——**明确的错误类型**，不是笼统的读错误。
- 取消：在同一个 `tokio::select!` 里和 `signal.aborted()` 竞速（`claude.rs:3806-3814`、`:3832-3838`），所以 Esc 立即生效。
- HTTP 层超时：`client.rs:925` 只有一个整体 `timeout: Some(self.timeout_ms)`，`DEFAULT_API_TIMEOUT_MS = 600_000`（`client.rs:283-297`）；非流式回退用 `get_nonstreaming_fallback_timeout_ms()`（`claude.rs:4602`，远程 120s / 本地 300s，`:1777-1790`）。**注意：CometixCode 的 HTTP 栈没有 connect/read/idle 分级超时**（只有 Bedrock IMDS 链有 1 秒 `connect_timeout`，`client.rs:970-975`）——它是用应用层 watchdog 代替的。Hikari 因为同步阻塞，必须用"短 read timeout + 循环"来模拟，这是等价做法。
- 测试：`claude.rs:9329`、`:9343`、`:9353` 覆盖 watchdog/idle/stall；`:9358` 覆盖 abort signal 传入 request options。

**验收**

- 手工：故意用一个慢速 mock（每 30 秒发一个 SSE 帧），断言不会断开。
- 手工：点击停止，断言 **2 秒内**界面进入"已停下"。
- 集成测试：mock 服务发一帧后永久静默，断言约 90 秒后收到 `IdleTimeout`（测试里可以把 `idle_timeout_seconds` 设成 3 秒加速）。
- 回归：`tests/protocols.rs` 现有 11 个测试全过（尤其 `:370` 那个走真实 HTTP 的 usage 测试）。

---

## 4. Anthropic `cache_control` + Chat 侧 `ContextSummary` 改 user 角色

**目标**：让提示缓存真正命中（当前 0 命中），并修掉中间位置插 `role:"system"` 这个协议违规。

**现状证据**

- Hikari 全仓库 **`cache_control` 出现 0 次**（`crates/` 下所有 `.rs` 搜过）。CometixCode 是 99 次，其中 `services/api/claude.rs` 80 次。
- 但 Hikari 解码时**已经统计了缓存 token**：`protocols.rs:877-879`

```rust
"message_start" => {
    let u = &v["message"]["usage"];
    self.input = u["input_tokens"].as_u64().unwrap_or(0)
        + u["cache_read_input_tokens"].as_u64().unwrap_or(0)
        + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
}
```

  也就是说：它读得到缓存字段，但从不写缓存标记 → Anthropic 路径上 `cache_read` 永远是 0，每一轮全量重发都按未缓存计费。
- Anthropic 的 `system` 是**裸字符串**：`protocols.rs:642` `json!({"model":…,"system":context.system_prompt,"messages":…})`。要加 `cache_control` 必须改成 block 数组。
- Chat 侧的 `ContextSummary` 投影有问题：`openai.rs:141`

```rust
Message::ContextSummary { text } => out.push(json!({"role":"system","content":text})),
```

  **在消息数组中间插一条 `role:"system"`**。OpenAI 容忍，但大量兼容网关会拒绝或静默忽略，而且它直接破坏"system 在头部"的前缀缓存假设。对照：Responses 走 `role:"developer"`（`protocols.rs:539`），Anthropic 并进 `user`（`protocols.rs:590`）——这两个是对的。

**粗浅实现方案**

Anthropic 侧：

```rust
// protocols.rs，anthropic 分支
let system_blocks = json!([{
    "type": "text",
    "text": context.system_prompt,
    "cache_control": {"type": "ephemeral"}     // 前缀断点：system 之后
}]);
body["system"] = system_blocks;
```

具体落点：

1. `system` 改成 block 数组并带 `cache_control`。
2. 消息循环（`protocols.rs:539-640`）在"最后一个可作为断点的 block"上再插一个 `cache_control`。Anthropic 限制最多 4 个断点，先用 2 个（system + 尾部）。
3. **关键约束**：断点位置必须**每轮一致**，否则缓存前缀变化就全部失效。所以断点位置不能依赖"最后一条消息是什么"这种会变的东西——建议固定为"system 之后 + 最后一条 `Message::User` 的最后一个 block"。这条要在代码注释里写清楚，否则以后改动会静默破坏缓存。
4. Chat 侧：`ContextSummary` 改成并进 `user`，和 Anthropic 一致：

```rust
// openai.rs:141
Message::ContextSummary { text } =>
    out.push(json!({"role":"user","content":format!("<context-summary>{text}</context-summary>")})),
```

  Responses 的 `developer` 角色是官方支持的，保留即可。
5. Chat 侧如果目标网关支持 `prompt_cache_key`，可以加（OpenAI 官方路径），但**不要默认开**——兼容网关行为不一致。做成 `CompatibilityConfig` 的可选字段。

**CometixCode 对照路径**

- 形状：`get_cache_control(scope, query_source) -> CacheControl`（`claude.rs:235`），产出 `{cache_type:"ephemeral", ttl?: "1h", scope?}`。`ttl: "1h"` 由 `PromptCache1hConfig` feature gate + settings 白名单控制（`claude.rs:240-247`、`should_1h_cache_ttl` `:248-270`）。**Hikari 第一版只用 `{type:"ephemeral"}`，不要 ttl/scope。**
- 断点位置：system prompt 里"在最后一个 block 上加 cache_control"（`claude.rs:912-917`）、消息里"在最后一个合格 block 上加（排除 thinking）"（`claude.rs:1114-1127`）。
- 每个请求一个 message 级 cache marker，位置由 `CacheScope` 决定：`claude.rs:1472`（文档注释）、`:1564` `add_cache_references_before_last_cache_control(&mut result)`。
- 缓存 token 统计：`sdk_usage_to_token_usage`（`claude.rs:4206-4224`）含未文档化的 `cache_deleted_input_tokens`（`claude.rs:4257-4263`）。
- **配套的一条**：`assemble_tool_pool` 明确不是扁平排序——内置工具和 MCP 工具各自排序、内置保持连续前缀，因为服务端缓存断点落在"最后一个 prefix-matched 内置工具"之后（`tools/mod.rs:150-161`）。Hikari 的 `visible_defs()`（`tool.rs:218-233`）当前顺序是 `defs` 原序 + `loaded` 追加，**加载一个新 MCP 工具会把它追加到尾部**，这本身是对的（不破坏前缀），但 `loaded` 的**追加顺序**必须稳定。要在注释里固定这个不变量。

**验收**

- 用真实 Anthropic 端点（或能透传 `cache_control` 的网关）跑两轮，断言第二轮 `cache_read_input_tokens > 0`。**这是唯一能证明本条生效的方法**，Hikari 已经在 `protocols.rs:877-879` 读到这个字段，加个日志或断言即可。
- 用 `tests/protocols.rs:485` 那种 HTTP 级断言，验证 `body["system"]` 是数组且带 `cache_control`。
- 新增测试：连续两次构造同一个 `Context`，断言序列化出的 `body` **字节完全一致**（防止断点位置漂移）。
- 回归：`tests/protocols.rs:298`（Anthropic 签名 thinking 往返 + 缓存 token 计数 2+3+4=9）必须仍然通过。

---
## 5. token 计数锚定真实 usage

**目标**：压缩触发条件基于上一轮 API 返回的真实 usage，而不是每轮重新估算全量。

**现状证据**

- 压缩决策用 `provider.input_tokens(&context)`（`agent.rs:362`、`:374`）。
- `ProtocolProvider::input_tokens` 覆写成 `estimate_request`（`protocols.rs:296-298`），实际是把请求体建出来量 JSON 长度再 `/3`（`provider.rs:75-91`），再加每张图 32k 的固定预留（`provider.rs:64-73`）。
- **每轮重新估算全量**，不参考上一轮的真实 usage。
- 真实 usage 是拿到了的：`StreamEvent::Usage` → `draft.usage`（`agent.rs:411-413`），然后 `if draft.usage.input > usage.input { usage.input = draft.usage.input; }`（`agent.rs:491-493`）——但 `usage` 只用于最终上报，**不参与压缩决策**。
- **关键障碍**：`Message::Assistant` 变体**没有 usage 字段**（`model.rs:13-24`：`text` / `origin` / `thinking` / `tool_calls` / `provider_metadata`）。所以"找最后一条带 usage 的 assistant"目前做不到。
- 精度评估（修正 [docs/cometixcode-loop.md](docs/cometixcode-loop.md) 里的说法）：bytes/3 对 CJK 大致准（1 汉字 3 字节 ≈ 1 token），对 ASCII 偏高约 33%（4 字节/token）。所以它是**偏保守**的，不是偏危险。真正危险的是第 6 条（配置窗口 ≠ 实际窗口）。

**粗浅实现方案**

1. 给 assistant 消息加 usage：

```rust
// model.rs
Assistant {
    text: String,
    origin: Option<MessageOrigin>,
    thinking: String,
    tool_calls: Vec<ToolCall>,
    provider_metadata: Option<ProviderMetadata>,
    /// 本轮请求的真实用量。prompt_tokens 是全文，不是增量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage: Option<Usage>,
},
```

  所有构造点要跟着改：`Message::assistant()`（`model.rs:76-86`）、`AssistantDraft::message()`（`agent.rs:1119-1142`）、`push_assistant()`（`agent.rs:733-754`）。**注意 serde 兼容**：加 `#[serde(default)]` 后旧会话存档仍可读。

2. 计数改成"锚点 + 尾部估算"：

```rust
// provider.rs 或新的 tokens.rs
pub fn count_context_tokens(context: &Context<'_>) -> u32 {
    // 1) 从后往前找最后一条带 usage 的 assistant
    if let Some((idx, usage)) = context.messages.iter().enumerate().rev()
        .find_map(|(i, m)| match m {
            Message::Assistant { usage: Some(u), .. } => Some((i, u)),
            _ => None,
        })
    {
        let anchor = usage.input.max(usage.output);       // 保守：取大者
        let tail = rough_estimate(&context.messages[idx + 1..]);
        return anchor.saturating_add(tail);
    }
    // 2) 没有锚点（首轮）→ 退回全量估算
    rough_estimate(context.messages)
}
```

3. `rough_estimate` 保留现有的 bytes/3（`provider.rs:42-49`），它只用于尾部增量，误差影响小。
4. `Provider::input_tokens` 保留作为"首轮兜底"，新增的 `count_context_tokens` 放 `agent.rs:362` 调用点。**不要删 `input_tokens`**——`output_reserve` 的计算和摘要请求的预算检查（`agent.rs:858-870`）仍然需要它。
5. 顺序提醒：**本条必须在第 4 条之后做**。加上 `cache_control` 后 `cache_read` 从 0 变成实际值，如果先做本条再开缓存，压缩阈值会突变，两个变量混在一起调不出来。

**CometixCode 对照路径**

- `src/utils/tokens.rs:107-125` `token_count_with_estimation(messages)`：从后往前找最后一条带 `usage` 的 assistant，`get_token_count_from_usage(usage) + rough_token_count_estimation_for_messages(&messages[index+1..])`；没有锚点就全量粗估。
- `get_token_count_from_usage`（`:28`）。**本条最需要想清楚的一点**：压缩阈值该用"含缓存的总输入"还是"实际新增"？CC 在 `auto_compact.rs:1090-1093` 用 `final_context_tokens_from_last_response`（input + output，**不含 cache**）。对照 `message_token_count_from_last_api_response`（`:60`）的差别。
- 消费点：`should_auto_compact_with_config`（`auto_compact.rs:168-195`）用 `token_count_with_estimation(messages).saturating_sub(snip_tokens_freed)` 与阈值比较。
- `Usage` 类型与累加：`update_usage`（`claude.rs:1364-1423`，注释说明 input/cache 计数在 0 时不下降）、`accumulate_usage`（`:1433-1463`）。

**验收**

- 单元测试：构造 `[user, assistant(usage=input:1000), user, tool_result]`，断言 `count_context_tokens` 约等于 `1000 + rough(tail)`。
- 单元测试：没有带 usage 的消息时退回全量估算。
- 手工：长会话下观察压缩触发时机是否稳定（不再随尾部内容抖动而突然触发）。
- serde 回归：读一个加字段前保存的会话文件，不报错。

---

## 6. response-driven 压缩 + 压缩失败断路器

**目标**：被 400 prompt-too-long 打回来时压缩重试，而不是把 400 摊给用户；连续压缩失败后不再重试。

**现状证据**

- **没有任何 response-driven 压缩。** 压缩只由请求前预估触发（`agent.rs:374` 的 `input >= available*4/5`）。
- 触发用的窗口是**配置里的** `context_tokens`：`main.rs:866` 传 `provider_config.context_tokens`，默认 128_000（`protocols.rs:96`）。`available = context_limit_tokens - output_reserve`（`agent.rs:371-373`）。
- 所以：**用户配 128k，网关实际路由到 32k 模型（中转站常见）→ 预估说没问题 → 请求 400 → 本轮直接死。** 这是当前最大的单点风险。
- 失败路径：`stream_result` 出错 → `agent.rs:469-479` 把错误追加进可见文本并设 `error` → `agent.rs:521-540` 配齐未执行工具的 tool_result 后 `break`。
- 压缩失败也没有断路器：`maybe_compact` 出错就返回 Err → 本轮结束（`agent.rs:810-812`、`:930-935`），没有"连续失败 N 次就不再试"的状态。

**粗浅实现方案**

1. 识别可恢复的上下文溢出。在 `transport.rs:63-82` 的 `http_error` 之外，加一个分类函数：

```rust
// transport.rs
pub fn context_overflow(err: &AgentError) -> bool {
    let m = &err.message;
    m.contains("prompt is too long")
        || m.contains("context_length_exceeded")
        || m.contains("maximum context length")
        || m.contains("input length and `max_tokens` exceed context limit")
        || m.contains("reduce the length of the messages")
}
```

  更稳的做法是不要只看文本：在 `protocols.rs` 的 `http_error` 里把 HTTP status 和 `error.type` 一起带出来（见第 1 条的 `ProviderErrorKind`）。**推荐顺带做**——否则这个函数会随网关文案变化而失效。

2. `agent.rs` 的失败分支加一次压缩重试：

```rust
if let Err(err) = stream_result {
    if !reactive_compact_attempted && context_overflow(&err) {
        reactive_compact_attempted = true;
        match force_compact(host, &mut request, &mut budget_usage, &mut on_event) {
            Ok(()) => continue,          // 压完重发
            Err(e)  => { error = Some(...); break; }
        }
    }
    // 原有处理
}
```

  `force_compact` 就是 `maybe_compact`（`agent.rs:761-968`）加一个"跳过阈值判断"的参数；内部逻辑（先 retain 再摘要、失败保留原文）原样复用。

3. 断路器：

```rust
// Execution 的 State 上加（跨轮持久）
compact_failures: u32,

// maybe_compact 失败时
if compact_failures >= MAX_CONSECUTIVE_COMPACT_FAILURES { /* 不再尝试，直接报错 */ }
```

  放 `Execution` 上比放 `TurnRequest` 好——`State`（`execution.rs:146-153`）已经有 `revision` / `reservations` 之类的字段，加一个计数器很自然。

4. 压缩前的 retain 逻辑要特别小心：`maybe_compact` 现在"retain 失败就不压缩"（`agent.rs:810-812`）。强制压缩路径也必须保持这条——**不能为了救 400 就把没记住的原文丢掉**。

**CometixCode 对照路径**

- withheld 错误机制：`SystemError` 分支里三类错误被"扣住"不立即抛，先尝试恢复——`is_withheld_prompt_too_long` → `withheld_prompt_too_long_error`（`query.rs:2194-2202`）；`is_withheld_media_size_error` → `withheld_media_size_error`（`:2203-2208`）；`system_api_error_is_max_output_tokens` → `withheld_api_error`（`:2209-2214`）。
- 恢复顺序：先 context collapse `recover_from_overflow`（`query.rs:2629-2645`），`committed > 0` 就 `continue 'query_loop`；不行再 `try_reactive_compact`（`:2646-2687`），成功就压完重试。两者都失败才发 `QueryEvent::ApiError` 并返回（`:2688-2702`）。
- 一次性守卫：`has_attempted_reactive_compact`（`query.rs:846`、`:2679`），注释明确说是"prevents prompt-too-long/media recovery spirals after a reactive compact retry"。
- 断路器：`MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES`，`consecutive_failures` 存在 `AutoCompactTrackingState` 里跨迭代传递（`auto_compact.rs:31-41`、`:215-224`），query loop 在 `:1168-1182` 更新它。
- 判定函数：`is_prompt_too_long_message`（`errors.rs:202`）、`parse_prompt_too_long_token_counts` / `get_prompt_too_long_token_gap`（`errors.rs:217`/`:235`，正则 `(?i)prompt is too long[^0-9]*(\d+)\s*tokens?\s*>\s*(\d+)`）、`is_media_size_error`（`errors.rs:255`）。**它有 token gap 解析**——能算出"超了多少"，可以据此决定压缩幅度。Hikari 第一版不需要。
- 压缩实现：`services/compact/compact.rs:582` `compact_conversation`、`:765` `partial_compact_conversation`、`:199` `build_post_compact_messages`（边界消息在前）、`:113` `truncate_head_for_ptl_retry`（PTL 重试时砍头）、`:35` `strip_images_from_messages`（媒体超限时先剥图片）。
- 阈值与四档状态：`auto_compact.rs:125-165` `calculate_token_warning_state_with_config`。
- **一个反面参考**：CC 的 `classify_api_error` 和 `get_assistant_message_from_error`（`errors.rs:546`、`:733`）**在生产路径上没有调用者**——真实失败文案走的是 `CannotRetryError` 的 Display（`with_retry.rs:269-273` → `query.rs:5524-5539`）。也就是说 CC 这套精细分类有一部分是死代码。Hikari 抄的时候要确认每个分类函数都有真实调用点，别抄出一个没人用的分类器。

**验收**

- 集成测试：mock 服务第一次返回 400 + `"prompt is too long"`，第二次返回正常，断言本轮最终成功且发生了一次压缩（`AutoCompactionStart`/`End` 事件各一次）。
- 集成测试：mock 服务持续返回 400，断言压缩只尝试一次（`has_attempted` 守卫生效）且最终报 `ContextOverBudget`。
- 手工：把 `context_tokens` 故意配成 200000（超过模型实际窗口），断言任务仍然能完成而不是直接失败。

---
## 7. 子 agent 写隔离

**目标**：消掉"两个子 agent 同时改同一个文件"的竞态。

**现状证据**

- 子 agent **共享工作区**：`fork_for_execution` 直接克隆父的 `root`（`tool.rs:184`），`spawn_agent` 用 `owner.workspace`（`execution.rs:439`）。
- **没有任何写锁**：在 `file_tools.rs` 和 `delegate.rs` 里搜 `lock|flock|FileLock|mutex`，零命中。
- `edit` 的"写前重读比对"（`file_tools.rs:118-120`，报 `"文件已被其他程序修改，请重新读取"`）**只能检测，不能互斥**——检测到就是让子 agent 重试，谁赢是随机的。
- `roadmap.md:113` 已经把这个风险写出来了："隔离上下文不等于隔离文件系统；明确共享工作区及并发写入边界，避免多个子任务同时修改同一资源。"
- 后台进程也是同一问题：`shell background=true` 的进程直接跑在共享工作区里（`execution.rs:874-879`）。

**粗浅实现方案**

两个方向，建议**先做 A（约 30 行），需要更强隔离再做 B**。

**方案 A：进程内写锁（最小改动）**

Hikari 是单进程，一个 `Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>` 就够：

```rust
// crates/hikari-agent/src/fs_lock.rs（新增）
pub struct PathLocks { inner: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>> }

impl PathLocks {
    /// 取得某个已规范化绝对路径的排他锁。锁是进程内的，不做跨进程文件锁。
    pub fn acquire(&self, path: &Path) -> Arc<Mutex<()>> {
        let mut m = self.inner.lock().unwrap();
        m.entry(path.to_path_buf()).or_default().clone()
    }
}
```

落点：

1. `PathLocks` 挂到 `ToolRegistry` 上（`tool.rs:63-71` 的字段区），`fork_for_execution` 时**共享同一个 `Arc<PathLocks>`**（关键：子 agent 必须看到同一份锁表）。
2. `FileTool` 的 `write` / `edit` 分支（`file_tools.rs:87-123`）在读写前 `acquire(&resolved_path)` 并持有到写完成。
3. 加超时：等锁超过 N 秒就返回 `"该文件正被另一个任务修改，请稍后重试"`，避免死等。
4. **不要**给 `read` 加锁（读不需要互斥，加了会大幅降低并发）。
5. 锁的粒度是"整个文件"，不是范围。足够解决当前问题。

**方案 B：worktree 隔离（结构性）**

`delegate_task` 的每个 `spawn` 如果工作区是 git 仓库，就在 `.hikari/worktrees/<task_id>` 建一个 worktree，子 agent 的 `root` 指向它，完成后把 diff 交回父 agent 决定是否合并。

- 成本明显更高：要处理非 git 仓库、worktree 清理、结果合并。附件按会话内容哈希存（`AttachmentStore` 是会话级不是工作区级），跨 worktree 仍然可用。
- 收益：真正的文件系统级隔离，子 agent 可以随便改。

**CometixCode 对照路径**

- worktree 工具：`src/tools/enter_worktree_tool/mod.rs`（319 行）、`src/tools/exit_worktree_tool/mod.rs`（528 行）。这两个是**工具**，由模型显式调用，不是自动的。
- 子 agent 隔离：`src/tools/agent_tool/` 共 21 文件 / 20 245 行，含 `fork_subagent.rs`、`run_agent.rs`（`resume` 相关）。
- **重要事实**：CometixCode 也没有进程内文件写锁——它的解法是"用 worktree 把文件系统隔开"，而不是"给文件加锁"。所以方案 A 是 Hikari 特有的更轻的做法，方案 B 才是抄 CC。
- 别混淆：`src/services/tools/tool_execution.rs:1728` 提到"同一批次内相同权限请求被抑制"——这是权限去重，不是文件锁。

**验收**

- 单元测试：两个线程同时 `write` 同一个路径，断言输出内容是两个完整结果之一（不是交错写入的损坏内容）。
- 集成测试：`delegate_task` 起两个子任务同时 `edit` 同一个文件，断言至少一个成功、另一个得到明确的重试提示，且文件最终内容合法。
- 回归：`tests/delegate.rs` 现有 6 组测试全过（尤其 `:292-340` 的共享 token 预留）。

---

## 8. 去掉工具轮数上限 + 清理交班文案

**目标**：让 Hikari 有能力跑超长任务；同时删掉为多 agent 交接设计的噪音文案。

**现状证据**

- `max_tool_rounds` 默认 50、上限 1000（`pi/iterations.rs:3-10`），`agent.rs:212-213` 每轮 clamp。
- 超限行为：给所有未执行的 call 补 tool_result + 推一条"工具一轮接一轮，先停在这里。"然后 `break`（`agent.rs:584-600`）。
- 80% 时的交班提示（`agent.rs:670-683` → `pi/iterations.rs:111-128`）：

```
[runtime] Tool-iteration budget at >=80% (used {current} of {max}). Per the
iteration-aware-handoff protocol in your spec, begin graceful handoff now:
commit current work, post a one-line status note, and write an
incomplete-handoff envelope with what's done / what remains / next-agent
starting position. Do NOT compress remaining work into the last few iterations.
```

  这是从 Pi 的 coding-agent 场景搬来的（多 agent 交接协议）。对 Hikari 的个人助手定位是纯噪音，而且它**消耗一轮模型调用**去写一份没人读的交接信封。
- **旁证**：CometixCode 的 `max_turns` 对交互式 REPL 是 `None`（`query.rs:72-74`、`:3791`），即无上限；CC 也没有任何重复工具调用检测——搜过 `repeated_tool|duplicate_tool|loop_detect|identical.*tool_use`，只有 `constants/prompts.rs:245` 一句提示词"如果用户拒绝，不要重试完全相同的调用"。**CC 防失控靠的是 token 预算 + 用户按停止键。**
- Hikari 已经有预算（`ExecutionBudget`：每任务 30 分钟 / 5000 万累计 tokens，`execution.rs:50-63`）。所以放开轮数是安全的——**前提是第 3 条已完成**，否则停止键要等 60 秒，用户想中断一个失控循环会很难受。

**粗浅实现方案**

1. `max_tool_rounds` 的语义改成"可选安全阀"：默认 `None`（不限），保留配置项给需要限流的人。
2. `agent.rs:584-600` 的超限分支保留（`None` 时走不到），但文案改成枚举（第 1 条的 `TurnOutcome::ToolRoundLimit`）。
3. 删 `iteration_handoff_steering_text`（`pi/iterations.rs:119-128`）和它的调用点（`agent.rs:670-683`）；`should_warn_at_iteration_threshold`（`:111-114`）如果没有别的调用者就一起删。
4. `pi/iterations.rs` 里保留 `resolve_max_tool_iterations` / `clamp_max_tool_iterations`（`:37-99`），因为 CLI/SDK 仍然可能传值；但默认返回值要能表达 `None`。
5. **保留** `pause_turn` 的 3 次上限（`agent.rs:542-549`）——那是服务端长工具的续接，不是本地轮数。
6. **保留** `turn_recovery` 的 2 次自动续写上限（`pi/turn_recovery.rs:24`）——那是启发式，需要硬上限。

**CometixCode 对照路径**

- `max_turns: Option<u32>`（`query.rs:72-74`），注释："Interactive REPL leaves this `None`; SDK/headless callers may bound recursive tool continuations."
- 检查点：`query.rs:3790-3806` —— `next_turn_count > max_turns` 才 `emit_max_turns_reached` 并返回 `Terminal::new("max_turns")`；`None` 时整段跳过。
- `emit_max_turns_reached`（`query.rs:4066-4096`）。
- 无重复调用检测的旁证：搜 `repeated_tool|duplicate_tool|loop_detect|identical.*tool_use|consecutive.*same` 全仓，只有 `constants/prompts.rs:245` 的提示词、`services/api/errors.rs:444` 的 `DuplicateToolUseId`（协议层错误，指 tool_use_id 重复，不是重复调用）、`utils/messages.rs:1402-1425`（校验消息数组里的 tool_result 重复 id）。
- 预算侧：`ExecutionBudget`（`execution.rs:41-63`）对应 CC 的 `token_budget` + `check_token_budget`（`query/token_budget.rs:57-103`）。**CC 的 token budget 是"继续到做完"**（`Continue` 分支注入 nudge 继续，`query.rs:2869-2906`），而 Hikari 的预算是**硬停**（`execution.rs:284-318`）。这个差异是设计选择：Hikari 的预算是安全阀，CC 的是"别停太早"。**建议 Hikari 保持硬停**——个人助手跑 30 分钟没结果就该停，而不是继续烧钱。

**验收**

- 手工：跑一个需要 100+ 工具轮的任务（比如"遍历这个目录下所有 rs 文件并统计"），断言不被截断。
- 断言 `pi/iterations.rs` 里不再有交班文案；相关测试（如果有）删除或改写。
- 回归：`tests/loop.rs` 的 steering/follow-up 测试（`:143-220`）不受影响。

---
## 9. `grep` 接 ripgrep（含回退）

**目标**：让搜索支持正则，并在没有 rg 时优雅退化。

**现状证据**

- Hikari `grep` / `find` 共用约 75 行（`file_tools.rs:143-217`），问题清单：
  - **只支持字面量子串**：`line.contains(pattern)`（`file_tools.rs:197`）——没有正则、没有大小写选项、没有 glob、没有文件类型过滤。
  - 硬编码忽略 `.git` / `target` / `node_modules` / `.hikari`（`file_tools.rs:175-180`）。
  - 只读 `< 1_000_000` 字节且 `fs::read_to_string` 成功的文件（`:194-195`）——**二进制和编码错误的文件被静默跳过，从不报告**。
  - 行截断 300 字符（`cap_chars`，`:201`）。
  - 200 命中上限、20 000 访问上限（`:167-169`），**没有"结果被截断"信号**——上限到了就 `break`，模型看到的是"好像只有 200 条"。
- 后果：模型无法做 `fn \w+` 这类搜索，只能退回 `shell` + 系统 grep，而 **Windows 上没有 `grep`**。静默跳过 + 静默截断会让模型得出错误结论（"代码里没有用到 X"）。
- 你的判断是对的：LLM 会优先用 `grep` 工具，失败后再换。所以工具本身必须能干活。

**粗浅实现方案**

分两步，第一步是纯收益且不引入外部依赖。

**第一步：先修"静默"（不接 rg）**

1. 截断显式化：命中数达到上限时，在结果末尾追加 `"…结果被截断，共扫描 {visited} 个文件，仅显示前 200 条匹配"`。
2. 跳过显式化：统计被跳过的文件数（过大、二进制、解码失败），在结果里报告 `"另有 {n} 个文件因过大/二进制/编码问题被跳过"`。
3. `ls` 的 `take(500)`（`file_tools.rs:129`）同样加截断提示。

这三条加起来约 20 行，消掉最容易导致模型误判的问题。

**第二步：接 ripgrep**

```rust
// crates/hikari-agent/src/ripgrep.rs（新增）
pub struct RgCommand { pub program: PathBuf, pub args: Vec<String> }

/// 三级解析，和 CC 的顺序一致。
pub fn resolve_rg() -> Option<RgCommand> {
    // 1) 可执行文件旁边的 vendor/ripgrep/<arch>-<os>/rg(.exe)
    if let Some(p) = vendor_rg_path() { return Some(...); }
    // 2) 开发树里的 vendor/ripgrep/...
    if let Some(p) = in_tree_vendor_rg_path() { return Some(...); }
    // 3) PATH 上的 rg
    which_rg()
}
```

落点：

1. 平台目录命名照抄：`x86_64 -> x64`、`aarch64 -> arm64`；`windows -> win32`、`macos -> darwin`（对照 `utils/ripgrep.rs:78-90`）。
2. `grep` 分支（`file_tools.rs:194-209`）改成：解析到 rg → 拼参数调 `rg -n --no-heading`；解析不到 → **回退到现有的手写实现**，并在结果里说明"未找到 ripgrep，已使用内置字面量搜索（不支持正则）"。
3. 参数映射（Hikari 的工具 schema 要加字段）：
   - `pattern` → 位置参数（rg 默认就是正则）
   - 新增 `glob`（`-g`）、`type`（`-t`）、`case_sensitive`（`-s` / `-i`）、`multiline`（`-U`）、`context`（`-C`）
   - `--max-count` 用来实现 head limit
4. 输出处理：rg 的输出路径是绝对路径，要像 CometixCode 那样 `to_relative_path` 重写成相对工作区（`grep_tool/mod.rs:252`），否则模型看到一长串绝对路径。
5. 沙箱/权限：rg 是外部进程，**要走 `permission.rs` 的 `assess_command` 路径**，否则就是一个绕过权限的读文件后门。最简单做法是把它当作只读命令加入 allow-list（`permission.rs:502-512` 已有 `rg grep findstr`）。
6. 超时与 abort：rg 也要有超时和 abort 检查（复用 `shell` 的 `ProcessJob`，或 `Command::spawn` + 轮询）。
7. 二进制打包：Hikari 是 GPUI 桌面应用，**推荐把 rg 作为外部资产随安装包分发**（CometixCode 的 README 明确说明 rg 是 external asset、从不编译进可执行文件）。`installer/` 目录需要更新。

**CometixCode 对照路径**

- 模块：`src/utils/ripgrep.rs`（1152 行）。
- 解析顺序：`vendor_rg_path()`（`:101-120`）→ 可执行文件旁的 `vendor/ripgrep/<dir>/rg(.exe)`，再是 `CARGO_MANIFEST_DIR` 下的开发树；`get_ripgrep_config()`（`:123-`）→ `USE_BUILTIN_RIPGREP` 为假且系统有 rg 时用系统 `rg`；`system_rg_path()`（`:924`）、`system_rg_path_in()`（`:892`）。
- 平台目录：`vendor_ripgrep_platform_dir()`（`:78-90`）。
- 执行：`rip_grep`（`:650`）、`rip_grep_inner_with_config`（`:685`）、`rip_grep_stream`（`:453`）。
- 缓冲与超时：`MAX_BUFFER_SIZE = 20_000_000`（`:19`）、`parse_timeout_seconds`（`:198`）、`platform_default_timeout_seconds`（`:216`）、`default_timeout`（`:225`）。
- Windows 特有坑：`is_egain_error` + `EAGAIN_STDERR_RETRY_PREFIX`（`:282`、`:20`）——Windows 上读 stderr 可能拿到 EAGAIN，需要重试。**Hikari 在 Windows 上会遇到同样的问题，这个细节值得直接抄。**
- 首次探测：`run_ripgrep_first_use_probe`（`:367`）、`test_ripgrep_on_first_use`（`:416`）；`/doctor` 报告用的是哪个 rg（`README.md`）。
- 工具侧：`src/tools/grep_tool/mod.rs:113` `grep_tool_schema()`；`:110` `VCS_DIRECTORIES_TO_EXCLUDE = [".git",".svn",".hg",".bzr",".jj",".sl"]`；`:124` `DEFAULT_HEAD_LIMIT = 250`；`:199` `format_limit_info`（**显式报告截断**）；`:283` `read_deny_globs`；`:339` `sort_file_matches`；`:252` `to_relative_path`。
- `glob_tool`（`mod.rs` 990 行）是独立的模式匹配工具，Hikari 的 `find` 可以对齐它的 `**/*.tsx` 语义。
- 提示词：`grep_tool/prompt.rs:5-6`，明确要求"ALWAYS use Grep for search tasks. NEVER invoke `grep` or `rg` as a Bash command."——**Hikari 应该抄这句**，否则模型会绕过工具直接用 shell。

**不要抄的**：`is_javascript_whitespace`（`grep_tool/mod.rs:412`）、`javascript_parse_int_base10`（`:434`）、`javascript_locale_compare` 排序（`tools/mod.rs:183-185`）。这些是为了和 TS 行为逐字节一致，Hikari 没有这个约束。

**验收**

- 有 rg 时：`pattern: "fn \\w+_test"` 能命中（正则生效）。
- 无 rg 时（把 rg 移出 PATH 或改名）：回退到字面量搜索，结果里明确说明不支持正则。
- 截断场景：造 300 个匹配文件，断言结果里出现截断提示。
- 二进制场景：造一个含 NUL 的文件，断言它被报告为"跳过"而不是静默消失。
- 权限回归：`tests/execution.rs:87-175`（wrapper / compound / catastrophic）全过，且新增 rg 的 allow-list 用例。
- 中文场景：搜一个含中文的文件，断言输出不乱码（`rg` 输出是 UTF-8，不受 Windows 代码页影响——这顺带解决了 `shell` 的 GBK 问题，是接 rg 的额外好处）。

---
## 10. 工具层小批量修正

**目标**：一批独立的小改动，每个都是几行到几十行。

### 10.1 `read` / `read_file` 合并 + 截断信号

**现状**：`read_file`（`tool.rs:620-660` + `793-807`）整文件读、无分页、1 MB 上限、只看前 800 字节有无 NUL 判断二进制、`from_utf8_lossy`。`read`（`file_tools.rs:74-86`）分页、`offset.max(1)`、`limit.clamp(1,1000)` 默认 200、`format!("{}: {s}", i+1)`。**两者功能重叠，都暴露给模型只增加选择噪声。**

**方案**

1. 删 `read_file`（`tool.rs:620-660`、`793-807`、注册处 `tool.rs:104`）。
2. `read` 补截断信号：结尾加 `"\n（文件共 {total} 行，已显示第 {start}-{end} 行）"`；offset 越过 EOF 时明确说"偏移超过文件末尾（共 N 行）"，而不是返回空串。
3. `read` 补二进制/过大提示：把 `read_file` 的 800 字节 NUL 检测和 1 MB 上限搬过来，报明确原因。
4. 顺手：`limit` 的 schema 声明 `minimum: 1`、`maximum: 1000`，和代码的 `clamp(1,1000)` 对齐（现在 schema 只声明了 `minimum`）。

**CometixCode 对照**：`src/tools/file_read_tool/mod.rs`（3482 行）+ `limits.rs`（127 行）+ `ui.rs`（966 行）。关键行为：token 上限校验报 `"File content ({} tokens) exceeds maximum allowed tokens ({}). Use offset and limit parameters…"`（`mod.rs:407`）；`DEFAULT_MAX_OUTPUT_TOKENS = 25_000`（`limits.rs:12`）；图片支持（`IMAGE_EXTENSIONS`、`compress_image_buffer_with_token_limit`、`maybe_resize_and_downsample_image_buffer`，`mod.rs:47-53`）；PDF `pages` 参数（`mod.rs:36-39`）；`is_blocked_device_path`（`mod.rs:94`）、`is_unc_path`（`mod.rs:990`）。**抄：截断/超限的明确文案 + 图片通道（复用 Hikari 已有的 `AttachmentStore`）。不抄：PDF 分页、UNC、设备路径、JS `parseInt` 复刻。**

### 10.2 `write` 原子写 + 大小上限

**现状**：`file_tools.rs:87-98`，`create_dir_all(parent)` + `fs::write`，无原子性、无大小上限、不处理 BOM/换行（而 `edit` 处理了——不一致）。

**方案**

```rust
// 同目录临时文件 + rename，保证要么旧内容要么新内容
let tmp = path.with_extension("hikari-tmp");
fs::write(&tmp, content)?;
fs::rename(&tmp, &path)?;      // Windows 上 rename 到已存在文件是原子的
```

加大小上限（比如 16 MiB，和 `OUTPUT_CAP` 同量级），超限报明确原因。要不要统一 BOM/换行处理——**建议统一**：抽一个 `write_text_preserving(path, content)`，`write` 和 `edit` 都走它。

**CometixCode 对照**：`src/tools/file_write_tool/mod.rs`（3 文件 / 2284 行）。它的行数主要在做 diff 输出、read-state 更新、结构化 patch——这些 Hikari 不需要。原子写这件事 CC 靠 `src/utils/file_persistence/` 统一处理，值得看一眼它的临时文件命名与失败清理。

### 10.3 `edit` 加 `replace_all`

**现状**：`file_tools.rs:99-123` 要求恰好一次匹配（`"old_text 匹配 {count} 次，需要精确匹配一次"`）。

**方案**：schema 加 `replace_all: bool`（默认 false），`count > 1 && replace_all` 时全部替换。BOM/换行/并发检测逻辑不变。

**可选**：引号风格容错——匹配失败时把弯引号归一化后重试一次，命中就按归一化位置替换并保留原风格。CometixCode 的 `find_actual_string` + `preserve_quote_style`（`file_edit_tool/utils.rs`，613 行）是完整实现，Hikari 只需要最基础的一层。

**CometixCode 对照**：`src/tools/file_edit_tool/`（6 文件 / 3146 行）——`types.rs` 的 `FileEditInput { file_path, old_string, new_string, replace_all }`；`mod.rs:143-146` 的 `replace_all` 分支（`file.replace` vs `file.replacen(.., 1)`）；`mod.rs:317-321` 的多匹配报错文案；`mod.rs:71` `is_non_regular_existing_path`（拒绝非普通文件，**值得抄**）；`mod.rs:130` `validation_error(message, error_code)`（错误带码）；`utils.rs` 的 `find_actual_string` / `get_patch_for_edit` / `preserve_quote_style`；`mod.rs:137` `simulated_settings_edit`（对配置文件的模拟应用，**不需要**）。

### 10.4 `abort` 进 `Tool::call` 签名

**现状**：`fn call(&self, arguments: &str) -> std::result::Result<String, String>;`（`tool.rs:26`）没有 abort；只有 `call_with_abort`（`tool.rs:51-60`）的默认实现里检查。工具作者覆写 `call` 时很容易忘——`FileTool` 在遍历循环里**手动**查了（`file_tools.rs:152`），但这是自觉，不是约束。

**方案**：把 `abort: &AtomicBool` 提到 `call` 的必填参数：

```rust
fn call(&self, arguments: &str, abort: &AtomicBool) -> std::result::Result<String, String>;
```

改动是机械的（`tool.rs`、`file_tools.rs`、`execution.rs`、`delegate.rs`、`hikari-runtime/*`、`hikari-memory/tools.rs` 共约 15 个实现），但一劳永逸：编译器替你把"忘了检查取消"这件事兜住。同时可以删掉 `call_with_abort` 这层间接。

**CometixCode 对照**：`ToolCall::call` 的签名（`src/tool.rs:2367-2375`）把 `context: &ToolUseContext`（含 `abort_controller`）、`can_use_tool`、`parent_message`、`on_progress` 全部作为参数传入。**Hikari 只需要 abort 一个**，不用抄其它三个。

### 10.5 长工具进度反馈

**现状**：没有工具级进度通道。`shell` 的 stdout 在 8 MiB 上限内被两个线程捕获后**在进程结束时**才返回（`tool.rs:699-700`）。跑 10 分钟的命令，界面 10 分钟没有新信息。

**方案**：`Tool::call` 加一个进度回调（和 10.4 一起做，签名只改一次）：

```rust
pub enum ToolProgress { Text(String) }

fn call(
    &self,
    arguments: &str,
    abort: &AtomicBool,
    on_progress: &mut dyn FnMut(ToolProgress),
) -> std::result::Result<String, String>;
```

- `ToolRegistry::call_output` 把这个回调接到一个新的 `AgentEvent::ToolProgress { tool_call_id, text }`（`agent.rs:80-153` 加变体），`main.rs` 的 `apply_event` 更新对应 tool 行。
- `shell` 的 `read_capped` 线程（`tool.rs:759-787`）改成"边读边回调"；`delegate_task` 同理。
- 默认实现给个空闭包，这样大部分工具不用改。

**CometixCode 对照**：`src/types/tools.rs:6-95` 的 `ToolProgress` 枚举（7 个变体，含 `BashProgress`/`AgentProgress`/`WebSearch*`）；`ToolCallProgressFn<'a>`（`src/tool.rs:2019`）；context 级 `ToolProgressSink`（`src/tool.rs:366-369`）；`install_tool_progress_sink`（`query.rs:4491`）；跨 actor 边界走 `QueryEvent::ToolProgress`（`query.rs:355-359`）。**Hikari 只需要 `Text` 一个变体**，不要 7 个。

### 10.6 `forget` 之外的缓存失效范围

**现状**：`forget` 成功后只清 `cached_recall`（`agent.rs:637-639`）。但 `edit`/`write` 改了 `AGENTS.md` 角色卡、`skill_manage` 改了 skill 之后，同一轮内的召回和系统提示也是陈旧的。

**方案**：把"哪些动作要作废本轮缓存"集中成一个判断函数：

```rust
fn invalidates_turn_cache(name: &str, args: &str) -> bool {
    matches!(name, "forget" | "skill_manage" | "write" | "edit")
        // 或者更精确：只当目标路径命中 AGENTS.md / .hikari/skills 时
}
```

在 `agent.rs:637-639` 附近调用。小问题，但既然在重做 loop 状态管理，顺手集中比散在 loop 里好。

**CometixCode 对照**：CC 没有等价物（它的 system prompt 是每轮重新组装的），所以这条是 Hikari 特有的。参考 `query.rs:1103-1118`（autocompact 重建 read-file state 后清 `loaded_nested_memory_paths` 并推 `ToolContextUpdate`）——**同样的思路：状态被外部改动了就显式作废并广播**。

---

## 后续（需求驱动，不进本轮）

| 项 | 触发条件 | CometixCode 对照 |
|---|---|---|
| `LSP` 工具 | Hikari 要认真做代码任务，需要"改完知道对不对"的反馈闭环 | `src/tools/lsp_tool/`（6 文件 / 2497 行）+ `src/services/lsp/`（`manager.rs`、`client.rs`、`server_manager.rs`、`passive_feedback.rs`）。**注意**：这是唯一需要常驻外部进程的工具，成本不低 |
| `EnterPlanMode` / `ExitPlanMode` | 需要"先规划再动手"作为权限第三态 | `enter_plan_mode_tool/mod.rs`（499 行）、`exit_plan_mode_tool/mod.rs`（818 行） |
| 子 agent `resume` | 子任务中断后要能续 | `agent_tool/run_agent.rs` 的 resume 路径 |
| 任务依赖图 | `todo_list` 的平铺列表不够用 | `task_create_tool` / `task_get_tool` / `task_update_tool` / `task_list_tool`（约 2300 行，开关 `is_todo_v2_enabled`） |
| MCP resources | 需要读 MCP 的 resource 而不只是 tool | `list_mcp_resources_tool`（590 行）、`read_mcp_resource_tool`（507 行） |
| MCP OAuth | 需要交互式鉴权 | `mcp_auth_tool/mod.rs`（338 行） |
| `PowerShell` 独立工具 | `shell` 在 Windows 上跑 PowerShell 命令需要绕 wrapper（`permission.rs:412-440`） | `src/tools/powershell_tool/`（13 文件 / 8212 行，含独立 `mode_validation.rs` / `path_validation.rs`）。**不要抄 8212 行**，只要一个能正确调 `pwsh -Command` 并正确解码输出的工具 |
| `shell` 的 per-call 超时 + Windows 编码 | 现在 30 秒硬编码（`tool.rs:702`），输出 `from_utf8_lossy` 会乱码 GBK（`tool.rs:782`） | 超时：`utils/ripgrep.rs:198-236` 的 `parse_timeout_seconds` / `platform_default_timeout_seconds`。编码：**没有直接对照**，CC 是 Node 原生 UTF-8，Hikari 需要自己处理代码页（`chcp 65001` 或 `encoding_rs`） |
| 工具层输入 schema 快照测试 | `parameters()` 改动目前无人看守 | `src/tools/cc_input_schemas.json`（41 225 字节）+ `src/tools/input_schema_parity_test.rs`（535 行） |
| 每工具语义标记（`is_read_only` / `is_concurrency_safe` / `is_destructive`） | 需要只读工具批并发，或让权限判定不再依赖工具名 | `src/tool.rs:2183-2221` |
| 每工具 `check_permissions` | `permission.rs:92-131` 的工具名分支在加工具时必然要改中央代码 | `src/tool.rs:2320-2333` + `utils/permissions/permissions.rs:812/1218/1946` |
| 池子层 deny 过滤 | "不可用即不可见"比"调用时才报错"干净 | `src/tools/mod.rs:121-136` |

---

## 依赖关系与执行顺序

```
第 1 条（终态枚举）  ──┐
                      ├─→ 第 2、3 条的新错误类型依赖它
第 3 条（idle + 取消）─┴─→ 第 8 条（放开轮数）必须在它之后
第 4 条（缓存）       ──→ 第 5 条（token 计数）必须在它之后（否则两个变量混在一起）
第 6 条（response-driven 压缩）── 独立，但需要第 1 条的分类
第 7 条（写隔离）     ── 独立
第 9 条（rg）         ── 独立，随时可做
第 10 条（小批量）    ── 10.4 和 10.5 一起改 Tool::call 签名，只改一次
```

三条互相独立、可以立刻开工的：**第 1 条**、**第 3 条**、**第 9 条**。
