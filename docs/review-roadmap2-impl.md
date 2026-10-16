# roadmap2 实现审查

审查对象：工作区未提交改动（`git diff --stat` = 18 文件 / +496 / −147，另加未跟踪的 `crates/hikari-agent/src/fs_lock.rs`），对照 `roadmap2.md` 十条逐项核对。

验证基线：

- `cargo check --workspace --all-targets` → exit 0，6 条警告。
- `cargo test --workspace` → exit 0，全部用例通过（各 suite 0 failed）。
- 另有 3 个独立可复现实验（真实 `ureq` 2.12.1、真实 `rg`、真实文件系统），脚本建在 `%TEMP%` 下，未写入仓库。

一句话结论：**方向和顺序都对，item 7 做得比我预想的好，但 item 3 的实现有一个会让"本来能用的请求必然失败"的回归，item 10.2 的原子写改得比改前更不安全。这两处必须修，其余是收尾。**

---

## 一、P0：必须先修，否则这个版本不能发

### P0-1 `timeout_read(2s)` 会让所有"首字节超过 2 秒"的请求彻底失败

**观测**（`protocols.rs:297-301`）：

```rust
http: ureq::AgentBuilder::new()
    .timeout_connect(Duration::from_secs(15))
    .timeout_read(Duration::from_secs(2))     // ← 本次改动
    .timeout_write(Duration::from_secs(30))
```

**实测**（真实 `ureq` + 本地 TCP，服务端延迟 3 秒才写状态行）：

```
attempt 1: ERR after 2.0087428s -> Network Error: Error encountered in the status line: ...
attempt 2: ERR after 2.0041295s -> ...
attempt 3: ERR after 2.0033182s -> ...
```

也就是说：`ureq` 的 `timeout_read` 是 socket 级 `SO_RCVTIMEO`，**对状态行（首字节）同样生效**。它并不只是在"响应体读到一半"时生效。

**失败链路**：`send_json` 返回 `ureq::Error::Transport` → `is_retryable`（`transport.rs:34`）对**所有** Transport 错误返回 true → 重试 `max_attempts = 11`（`transport.rs:17`）→ 每次都在 2 秒处失败 → 退避累计 `0.5+1+2+4+8+16+32×4 ≈ 160` 秒 → 最后报"没有连上"。

**后果**：长 prompt（prompt 处理时间常在 2 秒以上）、思考型模型、跨区网络、任何有排队的上游——**必然失败**。改动前 `timeout_read` 用的是 `config.timeout_seconds`，默认 60（`git show HEAD:crates/hikari-agent/src/protocols.rs:288`），所以这是"从能用变成不能用"的回归，不是新功能没做好。

**roadmap2 里的前提是错的**。`roadmap2.md:174` 写：

> `ureq` 的 read timeout 按单次 `read` 系统调用生效（`set_read_timeout`），所以超时后连接仍然可用，**可以继续读**。这是本条方案能成立的前提。

这句话对**响应体**成立，对**首字节**不成立：首字节超时是由 `call()` 返回的 `Error::Transport`，根本进不到我们的读循环，也就没有"继续读"的机会。

**连带需要修正的文档**：`docs/cometixcode-loop.md` 的 §2.5 当时结论是"取消不需要 async 重写，把 read timeout 降到 2 秒 + 循环里查 abort 即可"。按上面的实测，这个结论不成立，§2.5 需要改写（这条是我当时写错的，不是你的实现问题）。

**修法**（两条路，取舍见文末问题 1）：

- **方案 A（最小、立刻恢复正确）**：`timeout_read` 取"能容忍的首字节上限"，例如复用 `timeout_seconds`（默认 60）或折中 30 秒；`idle_timeout_seconds` 继续只管"总静默"。代价：取消延迟上界 = 该值，拿不到 `roadmap2.md:226` 要求的"2 秒内进入已停下"。
- **方案 B（真正拿到 2 秒取消）**：不动同步接口，把读循环放进独立线程，用有界 channel + `recv_timeout(100ms)` 回主线程；主线程每 100ms 检查 abort 并 `join`。约 40 行，不引入 async 运行时。这样 read timeout 可以保持宽松（首字节安全），取消延迟由 `recv_timeout` 决定。

推荐：**先落 A 恢复正确性，再按 B 补齐取消延迟**。B 完全落在 `transport.rs` 内部，不影响 `provider.rs` 的签名。

### P0-2 超时后丢弃半行：`line.clear()` 的位置

**观测**（`transport.rs:79-96`）：

```rust
loop {
    if abort.load(Ordering::Relaxed) { return Ok(()); }
    line.clear();                                   // ← 每轮先清空
    let n = match reader.read_line(&mut line) {
        Ok(n) => n,
        Err(error) if is_timeout(&error) => {
            ...
            continue;                               // ← 半行已被 append 进 line，这里丢掉
        }
```

`BufRead::read_line` 的语义是"**出错时也已读到的字节保留在 `buf` 里**"（std 源码 `library/std/src/io/mod.rs`，`append_to_string` 的 `Guard` 只在 UTF-8 不合法时回滚长度）。所以当一帧被拆成两段、中间停顿超过 2 秒时：

1. 第一段 `data: {..."hel` 被 append 进 `line`；
2. `read_line` 超时返回 `Err`，`line` 里**已经有前半行**；
3. `continue` 回到循环顶部，`line.clear()` 把它清掉；
4. 第二段 `lo"}}` 被当成一个新行，而它在 SSE 里没有冒号 → `sse.rs` 的 `process_line` 落到 `_ => {}` 忽略；随后的空行因为没有 data 也不派发。

**实测**（真实 `ureq` 2.12.1，服务端把一帧拆成两段、中间 sleep 3 秒）：

```
real ureq timeout kind  -> caught by is_timeout (1 timeout(s))
lines recovered         -> ["lo\"}}\n", "\n"]
payload 'hello' lost?   -> true
```

对照实验（不清空、跨超时保留缓冲、自己按 `\n` 切行）：

```
B (persistent buffer)  -> ["data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hello\"}}\n", "\n"]
```

**触发频率的诚实说明**：它要求"**一帧内部**有超过 2 秒的停顿"。Anthropic / OpenAI 通常一次写完一整帧，所以常规情况下碰不到。会碰到的场景：大帧（长 `input_json_delta`、长文本 delta）跨多个 TCP 段、TLS 记录边界、中间代理分片或抖动。频率不高，但后果是**静默丢内容**，或者在流没有 `message_stop` 时变成 `protocols.rs:420-422` 的"模型流提前结束，未收到完成事件"——用户看到的是一个莫名其妙的收尾错误。

**为什么 roadmap 的验收测不出来**：`roadmap2.md:225` 的 mock 是"每 30 秒发**一个整帧**"。整帧到达时 `read_line` 阻塞在空缓冲上，超时时 `line` 是空的，丢弃无害。所以这个用例会通过，而缺陷仍在。**建议补一条验收**：mock 把一帧拆成两段、中间 sleep 3 秒，断言内容完整。

**归因**：`line.clear()` 的位置在 HEAD 里就是这样（`git show HEAD:.../transport.rs:34`），但 HEAD 的 read timeout 是 60 秒、且超时走的是致命错误分支（`map_err(...)?`），所以"丢弃半行"这条路径实际上走不到。是本次把 read timeout 降到 2 秒，把潜在缺陷变成了可触发缺陷。

**修法**：不要在循环顶部 `clear`。让 `read_line` 的追加语义跨超时保留缓冲，自己负责切分已完成的行：

```rust
let mut buf = String::new();
loop {
    if abort.load(Ordering::Relaxed) { return Ok(()); }
    match reader.read_line(&mut buf) {
        Ok(0) => { /* EOF：把 buf 残留 + parser.flush() 收尾 */ }
        Ok(_) => {
            last_data = Instant::now();
            while let Some(i) = buf.find('\n') {
                let line: String = buf.drain(..=i).collect();
                /* 原 feed 逻辑 */
            }
        }
        Err(error) if is_timeout(&error) => {
            if abort.load(Ordering::Relaxed) { return Ok(()); }
            if last_data.elapsed() >= idle_timeout {
                return Err(AgentError::new("流空闲超时，未收到新数据。"));
            }
            continue;      // buf 原样保留
        }
        Err(error) => return Err(AgentError::new(format!("流读取中断：{error}"))),
    }
}
```

注意 `Ok(0)` 分支要把 `buf` 里的残留交给 `parser.flush()`（当前实现靠 `line` 残留 + `flush()`，改成持久缓冲后要保持这个收尾）。

---

## 二、P1：明确是缺陷，建议本轮修掉

### P1-1 `atomic_replace` 的 Windows 分支既不原子、又可能丢文件（item 10.2）

**观测**（`file_tools.rs:305-317`）：

```rust
fn atomic_replace(tmp: &Path, target: &Path) -> Result<(), String> {
    match fs::rename(tmp, target) {
        Ok(()) => Ok(()),
        Err(first) => {
            if cfg!(windows) && target.exists() {
                fs::remove_file(target).map_err(|_| first.to_string())?;
                fs::rename(tmp, target).map_err(|e| e.to_string())
            } else { ... }
        }
    }
}
```

**这个分支是多余的，而且有害。** `std::fs::rename` 在 Windows 上就是 `MoveFileExW(old, new, MOVEFILE_REPLACE_EXISTING)`（std 源码 `sys/fs/windows.rs:1311`），它**本来就会覆盖已存在的目标**。所以：

1. `fs::rename` 成功时走不到这个分支——它只是在成功路径上多余；
2. `fs::rename` 失败（目标被占用、权限、跨卷）时，代码先 `remove_file(target)`：**制造了一个"文件不存在"的窗口**，并发执行的 `read` / `grep` 可能正好看到文件消失；
3. 如果第二次 `fs::rename` 也失败，目标已经被删、`tmp` 还在 → **原文件内容丢失**。这不是"原子性差一点"，是"把一次失败变成数据丢失"。

`roadmap2.md:677` 的注释本身是对的（"Windows 上 rename 到已存在文件是原子的"），实现画蛇添足。

**修法**：

```rust
fn atomic_replace(tmp: &Path, target: &Path) -> Result<(), String> {
    fs::rename(tmp, target).map_err(|e| {
        let _ = fs::remove_file(tmp);      // 失败时清理 tmp，不动 target
        e.to_string()
    })
}
```

### P1-2 临时文件名会撞名（item 10.2）

**观测**（`file_tools.rs:152`、`:191`）：`let tmp = path.with_extension("hikari-tmp");`

`with_extension` 是**替换**扩展名，不是追加。实测：

```
        foo.rs  ->  foo.hikari-tmp
        foo.py  ->  foo.hikari-tmp
    foo.tar.gz  ->  foo.tar.hikari-tmp
    .gitignore  ->  .gitignore.hikari-tmp
   a/b/note.md  ->  a/b/note.hikari-tmp
   a/b/note.txt  ->  a/b/note.hikari-tmp
```

于是同一个目录下 `note.md` 与 `note.txt` 共用同一个临时路径。`PathLocks` 是按**目标路径**加锁的（`file_tools.rs:138`/`:162`），这两种写法拿到的是两把锁，锁不住彼此 → 两个写/编辑并发时会互相覆盖 `note.hikari-tmp`，轻则 `rename` 报错，重则把 A 的内容写进 B。

顺带：`atomic_replace` 只有非 Windows 的错误分支才删 `tmp`，Windows 分支失败会留下垃圾文件。

**修法**：用唯一临时名。不想加依赖就用进程内计数器：

```rust
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
let tmp = path.with_file_name(format!(
    ".{}.hikari-tmp.{}.{}",
    path.file_name().and_then(|s| s.to_str()).unwrap_or("file"),
    std::process::id(),
    TMP_SEQ.fetch_add(1, Ordering::Relaxed),
));
```

（`roadmap2.md:675` 的示例代码是我写的，撞名这一点是我漏了，不是你抄错。）

### P1-3 `grep` 接 rg：只完成了"第一步"，而"第二步"缺的正是最关键的那部分（item 9）

**观测**（`file_tools.rs:12-43`）：直接 `Command::new("rg")`，**没有 `resolve_rg`**、没有 vendor 目录、没有 PATH 之外的解析、没有超时、没有 abort。

先肯定做对的部分：**item 9 第一步（静默显式化）完成得很好**——截断提示（`…结果被截断，共扫描 {visited} 个文件，仅显示前 200 条匹配`）和跳过提示（`另有 {skipped} 个文件因过大/二进制/编码问题被跳过`）都在（`file_tools.rs` grep 分支尾部），`limit` 的 schema 也补上了 `maximum: 1000`。正则本身在有 rg 时确实生效。

但下面这些是实测出来的硬差异，逐条都是"同一句话在不同机器上语义不同"：

**1. 正则 vs 字面量，且工具描述没改。** 实测 `pattern = "a.b"`：

```
rg 默认（正则）  ->  1:axb   2:a.b   3:literal a.b here
rg -F（字面量）  ->  2:a.b   3:literal a.b here
```

有 rg = 正则，无 rg = `line.contains()` 字面量。而工具描述仍写着"pattern 为字面文本"（`file_tools.rs:57`）。同一个工具调用在两台机器上给出不同结果，模型无法预知。

**2. 隐藏文件被静默跳过。** 实测（`rg` 默认 vs `--hidden`）：

```
默认      -> visible.txt:1:HIDDEN_MARKER_XYZ
--hidden  -> visible.txt / .env / .github\ci.yml
```

用 `grep` 搜 `.github/workflows`、`.env`、`.cursor` 会得到"没有匹配"，模型据此得出"不存在"的结论。内置回退实现会搜这些文件（它只按目录名跳过 `.git`/`target`/`node_modules`/`.hikari`）。

**3. `.gitignore` 生效范围不同。** 实测（真 git 仓库，`.gitignore` = `ignored/` + `*.log`）：

```
rg 默认（从 `.` 遍历）  -> keep.txt
rg --no-ignore          -> keep.txt / b.log / ignored\a.txt
rg <显式路径>            -> 显式点名的被忽略目录仍会搜
```

`rg` 从目标目录**向下遍历时应用 `.gitignore`**。本仓库 `.gitignore` 里有 `examples/`、`.hikari/`、`.workbuddy/`、`pages/dist` —— 意味着用 `grep` 搜 `examples/CometixCode`（你正在对照的那份参考实现）会**静默无结果**。这条对本项目的实际影响比其他几条都大。

**4. 输出路径没相对化，Windows 上还踩了盘符冒号。** `file_tools.rs:34-40` 用 `line.splitn(2, ':')` 切 `路径:行号:内容`。Windows 上实测：

```
file     = "E"
rest     = "\Hikari\crates\...\tool.rs:18:const OUTPUT_CAP: usize = 16_000;"
relative = "E"                       <- strip_prefix(root) 失败
FINAL    = E:\Hikari\crates\...      <- 绝对路径
```

最终那行**内容是对的**（`E` + `:` + `\Hikari\...` 拼回去恰好复原），但路径是绝对的、且与回退实现（相对路径）不一致。`roadmap2.md:620` 明确要求相对化，未做。

**5. 没有超时、没有 abort。** `command.output()`（`file_tools.rs:29`）阻塞到 rg 结束。回退实现有 20 000 文件上限和每轮 abort 检查；rg 路径两者都没有。灾难性正则或大目录上，rg 会把整轮卡住，而且**停止键无效**——这正好抵消了 item 3 想换来的取消能力。

**6. 非法正则静默退化成字面量搜索。** 实测 rg 对 `foo(bar` 返回 exit 2；`file_tools.rs:30` 的条件把非 0/1 的退出码判为失败 → 返回 `None` → 静默走内置字面量搜索。模型以为正则生效了，实际拿到的是字面量结果。应该把 exit 2 单独报成"正则表达式无效"。

**7. schema 承诺了回退不支持的参数。** `glob` / `case_sensitive` / `multiline` / `context` 只对 rg 生效；回退实现里 `case_sensitive` 被完全忽略（恒为大小写敏感）、`glob`/`context`/`multiline` 同理。而 `roadmap2.md:615` 要求"回退时在结果里说明'未找到 ripgrep，已使用内置字面量搜索（不支持正则）'"——这句没实现。

**结论**：item 9 大约完成 30%（第一步 100%，第二步只有"调一次 rg"）。而且注意：**在没有 rg 的机器上，这个改动比改前更差**——schema 多出四个不生效的参数，模型会以为自己能用正则。

**修法（按性价比排序）**：
1. `resolve_rg` 三级解析（`roadmap2.md:602-609` 已经写明）+ 回退时在结果里明说；
2. rg 分支加 abort 轮询与超时（复用 `shell` 的进程管理，或 `spawn` + 100ms 轮询）；
3. 输出相对化；
4. 显式决定隐藏文件与 `.gitignore` 策略（见文末问题 2），并把决定写进结果或描述；
5. exit 2 单独报"正则表达式无效"。

本轮若不做完，**至少要**把 `file_tools.rs:57` 的描述改回"字面文本"并注明"系统装有 ripgrep 时支持正则"，否则契约是错的。

### P1-4 压缩之后 token 锚点没有失效（item 5 × item 6 的交互）

**观测**：`count_context_tokens`（`provider.rs:96-113`）从后往前找**最后一条带 `usage` 的 assistant** 作为锚点，只估算它之后的消息：

```rust
let Some((index, usage)) = context.messages.iter().enumerate().rev().find_map(...)
let tail = &context.messages[index + 1..];
usage.input.max(usage.output)... .saturating_add(tail 的估算)
```

而 `maybe_compact` 压缩时只做两件事（`agent.rs:1002-1007`）：

```rust
request.messages.drain(..cut);
request.messages.insert(0, Message::ContextSummary { text: summary });
```

**保留区间里那些 assistant 的 `usage` 字段没有被清掉。** 保留的是最后 `COMPACT_KEEP = 8` 条（`agent.rs:12`），其中必然包含刚返回的那条 assistant，而它的 `usage.input` 是**压缩前**的 prompt 大小——也就是刚刚触发压缩的那个值。

**推断的后果**（这一步是 Inferred，需要一条集成测试确认）：下一轮 `input` 仍然 ≥ `available * 4/5`（`agent.rs:370`/`:382`）→ 再次进压缩 → `compaction_cut` 面对已经很短的 messages 可能返回 0（`agent.rs:869-879` 的 `cut == 0` 分支直接 `Ok(())`）→ `request.messages == before` 为真 → 走到 `if input >= available`（`agent.rs:393`）→ `TurnOutcome::ContextOverBudget`，**本轮直接结束**。表现出来就是"压缩成功了，但下一轮立刻说超预算"。

**验收建议**：mock 返回一个巨大的 `usage`，断言压缩之后第二轮不再触发压缩、且不报 `ContextOverBudget`。

**修法**：压缩成功后显式作废锚点。最省事的是在 `maybe_compact` 收尾处把保留区间内 assistant 的 `usage` 置为 `None`；更干净的是给 `TurnRequest` 加一个水位（例如 `token_anchor_floor: usize`），`count_context_tokens` 只认水位之后（`index >= floor`）的 usage。我倾向后者，因为它同时解决"`retain_new` 之后锚点也可能陈旧"这一类问题。

**附带一处**：`usage.input.max(usage.output)`（`provider.rs:111`）用 `max` 是不必要的防御。`input` 才是 prompt 大小；`output` 是本次补全长度。短 prompt + 长输出的场景（正是"让它写一段长代码"）会把锚点抬到完成长度，导致**高估**。建议直接用 `usage.input`。

（另注：Anthropic 的 usage 解码把 `cache_read_input_tokens` 和 `cache_creation_input_tokens` 都加进了 `input`（`protocols.rs:903-905`），Chat 侧读 `prompt_tokens`（`openai.rs:273`），Responses 侧读 `input_tokens`（`protocols.rs:874`）——三者都等于"真实 prompt 大小"，所以锚点口径是对的。这也验证了 roadmap 里"item 5 必须排在 item 4 之后"的顺序要求是必要的。）

### P1-5 进度回调不是流式的，item 10.5 的目标没达成

**观测**（`tool.rs:603-615`）：

```rust
pub fn call_output_with_progress(&self, name, arguments, abort, on_progress) -> ... {
    let output = self.call_output(name, arguments, abort)?;   // 先跑完
    if !output.text.is_empty() {
        on_progress(ToolProgress::Text(output.text.clone()));  // 再把整段发一次
    }
    Ok(output)
}
```

也就是说：没有任何增量信息，只是在工具**跑完之后**把完整结果又当"进度"发了一遍。实测确认没有任何 `Tool` 实现覆写这个 trait 方法——全仓库只有 `tool.rs:49` 的默认实现（**无人调用**）、`tool.rs:603` 的 registry 方法、以及 `agent.rs:652` 的调用点。

**后果**：
1. item 10.5 的目标（"跑 10 分钟的命令，界面 10 分钟没有新信息"）**没有解决**——`shell` 的 `read_capped`（`tool.rs:759-787`）依然是读完才返回；
2. 每个工具调用额外复制并发送一份完整输出（最大 16 KB）作为重复事件；UI 侧 `main.rs:1113-1120` 直接把 `detail` 覆盖成整段结果，随后 `ToolExecutionEnd` 再覆盖一次，表现为"工具好像提前结束了"的一闪。

**修法**：让 registry 真正分发到 trait 方法（`find(name)?.call_output_with_progress(...)`），并让 `shell` 的 `read_capped` 边读边回调（`roadmap2.md:726` 已经写明）。本轮若不做，**建议先把这条事件和 UI 处理去掉**——留着一个"看起来像进度但不是进度"的通道，比没有更容易误导后续维护者。

---

## 三、P2：细节与收尾

**先说一条我原本怀疑、核对后发现你做对了的**：item 7（子 agent 写隔离）**实现是正确且完整的**。`PathLocks` 定义在 `fs_lock.rs:5-14`，在 `tool.rs:211` 经 `fork_for_execution` 以 `self.locks.clone()` 传给子 agent（同一把 `Arc`，所以跨子 agent 生效），`write` 与 `edit` 分别在 `file_tools.rs:138` 和 `:162` 加锁，`_guard` 的作用域覆盖到 `fs::write` + `atomic_replace` 结束（即整个写盘过程），拿不到锁时 10 秒后返回"该文件正被另一个任务修改，请稍后重试"而不是死锁。顺序也对：`edit` 是先拿锁再读原文（`:162` → `:169`），避免了"读了之后被改"的丢失更新。唯一边角：锁键是 canonicalize 后的路径，对**尚不存在**的文件会保留用户输入的大小写，Windows 下 `write foo.txt` 与 `write FOO.TXT` 会拿到两把锁（同一文件被并发写）。要彻底解决就在 `acquire` 前对键做 `to_lowercase()`（仅 Windows）。

其余细节：

| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| 1 | `agent.rs:604-620`、`error.rs:10` | `TurnOutcome::ToolRoundLimit` **不可达**——轮数上限分支只 `push_assistant` 一句"工具一轮接一轮，先停在这里"，从不设置 `outcome`；全仓库没有该变体的构造点 | 要么在 `break` 前 `outcome = TurnOutcome::ToolRoundLimit { used: rounds, cap: request.max_tool_rounds }`，要么删掉这个变体（否则新枚举里有死变体，且用户拿不到"轮数已达上限"的明确原因） |
| 2 | `agent.rs:265/400/449/468` | 4 条 `unused variable: reason` 警告。根因是 `TurnOutcome::TokenBudgetExhausted` 不带载荷，**budget 的具体原因字符串被丢掉了**（HEAD 里是 `error = Some(reason)`） | 给变体加 `reason: Option<String>`，或至少 `let _ = reason;` 并把原因并进 `user_text()` |
| 3 | `transport.rs:50` | `unused mut emit`（`read_json_stream` 现在只做转发） | 去掉 `mut` |
| 4 | `iterations.rs:73-79` | `clamp_max_tool_iterations` 的 `Some(0)` 分支成了死代码——`agent.rs:217-221` 已经先把 0 转成 `usize::MAX` | 顺手删掉该分支或加注释说明由调用方处理 |
| 5 | `delegate.rs:109` | 子 agent 仍硬编码 `max_tool_rounds: 100` | 如果这是有意的（防止子 agent 无限跑）就在注释里写清，因为它和"去掉工具轮数上限"的叙述不一致 |
| 6 | `protocols.rs:661-667` | `system` 从字符串变成 block 数组、并在最后一个 user 块上加 `cache_control`，**但没有 `compatibility` 开关** | 走 Anthropic 协议形状的第三方网关/代理可能因此 400。`compatibility` 结构本来就是为这类场景准备的（对照 `responses_store` / `reasoning` / `system_role`），建议加 `prompt_cache: bool`（默认 true） |
| 7 | `transport.rs:34` | `is_retryable` 对**所有** `Transport` 错误返回 true，包括 DNS 失败、TLS 证书错误、连接被拒 | 这些重试 11 次（最坏退避 ~160 秒）才报错，用户会以为程序卡死。建议只对超时/连接重置类重试，其余快速失败 |
| 8 | `transport.rs:27` | 退避里的"抖动"是确定性的（以 `attempt` 为种子的线性同余，实测 attempt=1 恒为 +33ms） | 单客户端无害，但起不到抖动本意；改用 `rand` 或进程启动时间做种子 |
| 9 | `transport.rs:38-40` | `parse_retry_after` 只支持秒数 | `Retry-After` 允许 HTTP-date 形式，这种会被忽略而落回本地退避。可接受，但值得注释 |
| 10 | `protocols.rs:299`、`settings.rs:1784` | 改造后 `timeout_seconds` **只影响 `fetch_models`**（`protocols.rs:234`）；流式请求的 connect 超时是硬编码 15 秒（`:298`）。而设置界面把它标成"连接超时（秒）" | 标签与实际行为不符。要么让 `timeout_connect` 用 `timeout_seconds`，要么把标签改回"模型发现超时"并新增 idle 输入框（`idle_timeout_seconds` 目前无法在界面上配） |
| 11 | `mcp.rs:724`、`mcp.rs:609-611` | MCP 仍调用 90 秒默认 idle 版本的 `read_json_stream`，而它的 ureq agent 用 MCP 自己的 `timeout_seconds`（默认 30）做 read timeout | 同一份读循环在两个调用点语义不同。且 MCP 的 read(30s) < idle(90s)，停顿后会继续循环、最终以"流空闲超时"收尾而不是立刻报断线。行为变化未必是坏事，但没写进计划，建议在 MCP 侧也显式传 idle 参数 |
| 12 | `tool.rs:448`、`tool.rs:666`、`permission.rs:93` | `read_file` **仍然注册**（item 10.1 第 1 条要求删除） | 模型仍面对两个功能重叠的读文件工具。截断信号与 `limit` 上限（10.1 第 2/3/4 条）已做 |
| 13 | `tool.rs:49`、`tool.rs:603` | item 10.4（`abort` 进 `call` 签名）未做，`call_with_abort` 保留 | 编译器仍然兜不住"覆写 `call` 时忘了查取消" |
| 14 | `agent.rs:637-639` 附近 | item 10.6（`invalidates_turn_cache` 集中判断）未做 | 仍是 `forget` 专用失效 |
| 15 | `openai.rs:35` | `.timeout_read(Duration::from_secs(60))` 属死路径——`OpenAiProvider` 全仓库没有构造点，只有 `lib.rs:35` 的 re-export | 顺手删掉 `OpenAiProvider`，或把它统一到 `ProtocolProvider` |

补充一条**观察，不是缺陷**：现在同时存在两套 token 口径——压缩门槛用锚定的 `count_context_tokens`（`agent.rs:370`），预算记账用整体 body 估算的 `provider.input_tokens`（`agent.rs:457`）。这是合理的（预算必须保守），但意味着"预算先于上下文窗口被触发"是常见路径。若日后排查"为什么还没到窗口上限就停了"，先看 `execution.check_budget`。

---

## 四、逐条对照 roadmap2

| # | 项 | 状态 | 关键证据 | 待办 |
|---|---|---|---|---|
| 1 | 终态枚举化 | 基本完成 | `error.rs:7-29`；`agent.rs:120-131`、`:727` | `ToolRoundLimit` 不可达；budget `reason` 丢失 |
| 2 | 重试拆分 + `Retry-After` | 完成（偏激进） | `transport.rs:9-40`；`protocols.rs:379-402` | Transport 全量重试；抖动不随机；仅支持秒 |
| 3 | SSE 空闲独立化 + read timeout 降低 | **有回归** | `protocols.rs:299`；`transport.rs:79-96` | 见 P0-1 / P0-2 |
| 4 | `cache_control` + `ContextSummary` 改 user | 完成 | `protocols.rs:661-667`；`openai.rs:141`（在 `request_body` 内，是活路径） | 缺 `compatibility` 开关 |
| 5 | token 锚定真实 usage | 完成但有洞 | `provider.rs:96-113` | 压缩后不失效；`max(input,output)` |
| 6 | response-driven 压缩 | 完成 | `agent.rs:476-487` | 触发靠字符串匹配，且 `provider_outcome`（`agent.rs:754`）判 `ContextOverflow` 时**不看 `"tokens"`**，两处口径不一致 |
| 7 | 子 agent 写隔离 | **完成，实现正确** | `fs_lock.rs`；`file_tools.rs:138`/`:162`；`tool.rs:211` | 仅大小写边角 |
| 8 | 去轮数上限 + 清交班文案 | 完成 | `config.rs:43`；`agent.rs:217-221`/`:604`；`iterations.rs` 的告警与交班文案已删 | `ToolRoundLimit` 未接线；子 agent 仍 100 |
| 9 | `grep` 接 ripgrep | 部分（~30%） | `file_tools.rs:12-43` | 缺 `resolve_rg`/权限/超时/相对化/隐藏文件/`.gitignore` 一致性 |
| 10.1 | `read`/`read_file` 合并 | 未做 | `tool.rs:448`/`:666` 仍注册 | 删除 `read_file` |
| 10.2 | `write` 原子写 | **做了但更糟** | `file_tools.rs:152`、`:305-317` | 见 P1-1 / P1-2 |
| 10.3 | `edit` 加 `replace_all` | 完成 | `file_tools.rs:178-183` | — |
| 10.4 | `abort` 进 `call` 签名 | 未做 | `tool.rs:49`/`:603` | 仍是可选 |
| 10.5 | 长工具进度反馈 | 只接了事件，没有流式 | `tool.rs:603-615` | 见 P1-5 |
| 10.6 | 缓存失效集中 | 未做 | — | — |

## 五、建议的修复顺序

1. **P0-1**：`timeout_read` 抬回"可容忍首字节"的值（或改读线程方案）。**这一条不修，版本不能发**——它让所有慢一点的真实请求必然失败。
2. **P0-2**：去掉循环顶部的 `line.clear()`，让半行跨超时保留。
3. **P1-1 + P1-2**：`atomic_replace` 去掉 Windows 的 `remove_file` 分支；临时文件名唯一化。
4. **P1-4**：压缩成功后作废 token 锚点（顺手改掉 `max(input, output)`）。
5. **P2-6**：`cache_control` 参数化（`compatibility.prompt_cache`）。
6. **P1-3**：`resolve_rg` + 相对化 + 超时/abort；本轮做不完就先把描述改回去。
7. **收尾**：删 `read_file`、`ToolRoundLimit` 接线或删变体、清 5 条警告、修 `timeout_seconds` 的界面标签。

前 4 条互相独立，可以并行；第 5 条依赖第 4 条的结论（都要动 `count_context_tokens` 附近的语义）。

## 六、验证记录与未验证项

**已验证**

- `cargo check --workspace --all-targets` → exit 0，6 条警告（`agent.rs:265/400/449/468` 的 `reason`、`transport.rs:50` 的 `mut emit`，另有若干 `main.rs` 的既有死代码警告）。
- `cargo test --workspace` → exit 0，全部通过（各 suite：12/31/2/8/12/11/3/11/1/9/8/2/3/10/5/8/8/3，0 failed）。
- 实验一（真实 `ureq` 2.12.1 + 本地 TCP）：首字节延迟 3 秒 → `timeout_read(2s)` 在状态行处报错、连试 3 次都失败。
- 实验二（真实 `ureq`，一帧拆两段、中间 sleep 3 秒）：当前 `line.clear()` 实现丢失前半帧（`payload 'hello' lost? -> true`）；不清空的实现能完整还原。
- 实验三（真实 `rg`）：正则 vs 字面量、隐藏文件、`.gitignore`（真 git 仓库）、`exit 2`、Windows 盘符冒号切分，全部实测。
- 实验四（真实文件系统）：`with_extension` 撞名；`std::fs::rename` 在 Windows 上是 `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)`（std 源码 `sys/fs/windows.rs:1311`）。

**未验证 / 只有推断的**

- P1-4 的"压缩后连续触发"链条是**代码路径推断**，没有跑集成测试。要确认需要一条 mock 测试（返回巨大 usage → 断言压缩后第二轮不再压缩）。
- P0-1 / P0-2 的实测用的是自建 mock 服务，不是真实 provider。机制层面（`ureq` 的 socket 超时语义、`read_line` 的保留语义）已经由源码与实验双重确认，但真实 provider 上的触发概率没有实测数据。
- 未审查 `crates/hikari-runtime` 与 `crates/hikari` 界面层的全部改动（只看了 `main.rs` 的 `AgentEnd`/`ToolProgress` 处理、`settings.rs:979`/`:1433`/`:1784`、`config.rs:43`）。

## 七、需要确定的两件事

1. **取消延迟 vs 首字节容忍度**（P0-1 的方案 A / B）：选 A 是"改动最小、立刻正确，但停止键最多要等 30 秒"；选 B 是"多约 40 行读线程 + `recv_timeout`，能同时拿到宽松超时和 ~2 秒取消"。我倾向 A 先上、B 紧随。
2. **`grep` 的隐藏文件与 `.gitignore` 策略**（P1-3 第 2/3 条）：rg 默认跳过隐藏文件和 gitignore 命中项，回退会搜。要不要给 rg 加 `--hidden` 与 `--no-ignore`？加了召回更全但会把 `target/`、`node_modules/` 一起搜进来（慢）；不加则模型搜 `.github`、`.env`、gitignore 目录时会得到假阴性。折中方案是加 `--hidden` + 显式 `--glob '!target'` 之类的排除表，但需要一张排除表。

另外：`docs/cometixcode-loop.md` 的 §2.5 需要按 P0-1 的实测结论改写（原文说"降低 read timeout 即可，不必 async 重写"，这个论断在 `ureq` 的 socket 超时语义下不成立）。要不要我一并改掉？
