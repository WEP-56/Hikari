# Pi 改造方案：接入 Hindsight

记忆怎么接进 agent loop。存储语义以 [hindsight-research.md](hindsight-research.md) 为准，这里只写 Pi 侧的缝。前端按文末的事件画，不必等 Memory Core 落地。

参考实现是裁过的 [agent.rs](../examples/pi_agent_rust/src/agent.rs)、[provider.rs](../examples/pi_agent_rust/src/provider.rs)、[tools.rs](../examples/pi_agent_rust/src/tools.rs)。新代码不要把这三份文件原样搬进来再改，按下面的边界重写一版小的。

## 留什么

| 留下 | 原因 |
| --- | --- |
| `run_loop` 这种「流式补全 → 执行工具 → 再补全」的循环 | 编程、浏览器、computer use 都靠它 |
| `Provider` | 只负责把 `Context` 变成 `StreamEvent`。记忆不进这里 |
| `Tool` / `ToolRegistry` | bash、编辑、浏览器仍是工具。记忆的自动读写不是工具 |
| `Message`：User / Assistant / ToolResult | 会话原文。retain 的输入就是这个 |
| `AgentEvent` | 前端唯一的事实来源。不另开一条记忆通道 |
| compaction 的触发点 `maybe_compact` | 窗口满了要压缩。压缩前必须先 retain |

砍掉、也不要在新 loop 里复活：扩展宿主、QuickJS、TUI、magic keyword 对 `system_prompt` 的永久改写。召回如果写进 `AgentConfig.system_prompt`，会粘到后面每一轮，并且被下一轮 retain 当成新事实再存一遍。

## 一轮里面记忆在哪

```
用户消息进入 session
        |
        v
retain 尚未覆盖的旧原文          （若窗口即将满：先 retain，再 compact）
        |
        v
recall(本轮 query)               只附加在这一次 Provider 请求上
        |
        v
Provider.stream(system + 历史 + tools)
        |
        +-- 模型调用 bash / 编辑 / 浏览器 --> ToolExecution*  --> 结果写回 session，回到 stream
        |
        v
TurnEnd
        |
        +-- 异步 retain(这一轮新增的 User / Assistant / ToolResult)
                 |
                 +-- 异步 consolidation（不挡下一轮）
```

两次模型调用和对话那次不是一回事。抽取、沉淀用同一个 `Provider`，但是辅助调用：不发 `MessageStart`，不进 session，前端看不到它在「说话」。前端只在调用结束时收到一条记忆事件。

## 三条缝

### 1. 读：每次 `Provider.stream` 之前

主机做，不交给模型记得调用。

- query 用本轮用户的话。如果这轮是在继续干活（同一仓库、同一个网站、同一段桌面操作），把任务描述并进 query。
- 默认只取 `observation`，小预算。分数不够就什么都不附。
- 附在这一次请求的系统提示末尾，标题固定，例如 `Recalled memory`。不写入 session 的 `messages`。
- 下一轮重新召回。上一轮注入的文本不是对话的一部分。

模型中途想看步骤（observation 只有结论，experience 里才有「改了哪个文件」）时，再给一个普通工具 `recall`。它走 `Tool`，所以前端用现成的工具卡片。自动召回不是这个工具，不要画成模型主动调用。

`recall` 工具只读。参数：`query`、可选 `include_source_facts`。返回 observation 或展开后的 experience / world。它不能改记忆。

### 2. 写：`TurnEnd` 之后

主机把这一轮新进 session 的消息交给 retain。包括工具结果，否则 experience 里不会有编程和 computer use。

- 输入是一个 document，`document_id` 用 `session_id` 加这段消息的区间，不是每个 token 一篇。
- `content_hash` 挡住同一次重复提交。
- retain 失败只重试，不回滚已经回复给用户的话。记忆落后于对话是可以的，对话落后于记忆不行。
- 不提供 `retain` 工具。模型不能自己写入。它会把猜测、终端全文和本轮注入的召回再存进去。

抽取规则仍是调研文档里的那刀：用户和世界进 `world`，Hikari 自己做成的事进 `experience`，observation 只有沉淀能写。一条 experience 是一个动作的结果，不是 stdout 的每一行。

### 3. 沉淀：retain 成功之后的后台

不在 loop 里等。待处理的是 `consolidated_at` 为空的 world / experience。它自己会再做一次只针对 observation 的召回，融合用 interleave，不用回答用户那次的 RRF。细节在调研文档，loop 只负责在 retain 完成后丢一个任务。

## session 和记忆

两套东西，不要合成一个库。

| | session | Memory Core |
| --- | --- | --- |
| 存什么 | 这次对话的原文，含工具结果 | 抽出的事实和 observation |
| 寿命 | 这次会话，compaction 会丢掉旧原文 | 跨会话 |
| 谁读 | 模型每一轮的 `Context.messages` | recall，按 query |
| 前端 | 聊天气泡、工具卡片 | 本轮「想起了什么」，不是气泡 |

compaction 不能代替 retain。`maybe_compact` 一旦把旧消息换成摘要，原文就没了。顺序固定：

1. 找出还没 retain 的消息
2. retain 它们（至少要提交成功，抽取可以仍在后台）
3. 再发 `AutoCompactionStart`，再压缩

摘要本身不要再 retain。它是原文的派生物，再抽一次会制造第二份事实。

## 给前端的事件

沿用 `AgentEvent`。对话和工具已经有事件，记忆补三条。三条都是主机发的，不是模型输出。

```
MemoryRecall {
  query: string
  items: { id, fact_type, text, proof_count }[]   // 空数组 = 这一轮没注入
}

MemoryRetain {
  document_id: string
  state: "queued" | "done" | "failed"
  kept: number          // 抽中的 fact 数，done 才有
  dropped: number       // 寒暄、空话丢掉的数，done 才有
  error?: string
}

MemoryConsolidation {
  state: "running" | "done" | "failed"
  created: number
  updated: number
  error?: string
}
```

前端可以先按这个假数据画：

- `MemoryRecall` 出现在这一轮第一条 `MessageStart` 之前。空的就不占地方。有内容时是这一轮的旁注（「想起了」），不是用户气泡，也不是助手正文。不要展示向量分。
- `ToolExecution*` 只留给 bash、编辑、浏览器、computer use，以及模型主动调用的 `recall`。自动读写不要占用工具卡片。
- `MemoryRetain` 跟在 `TurnEnd` 后面，慢一步是正常的。`queued` 一个小状态即可，`failed` 要能看见，因为压缩前的失败会丢原文。
- `MemoryConsolidation` 更慢，和某一条气泡无关。适合放在会话状态上，而不是插进消息流中间。
- `AutoCompactionStart` / `End` 保持原样。它前面如果 retain 还是 `queued`，界面上应能看出来压缩在等记忆，而不是卡住。

辅助抽取那次模型调用没有对应的 `MessageUpdate`。如果画布上出现「助手突然又说了一段不像回复的话」，就是接错了，那段不该进 session。

## 落地顺序

1. 先定事件。前端用假的 `MemoryRecall` / `MemoryRetain` / `MemoryConsolidation` 画。
2. `MemoryCore` 三个方法：`recall`、`retain`、`consolidate_pending`。存储按调研文档，先可以是进程内的。
3. loop 在 `stream` 前调用 `recall`，只改这一次的系统提示。
4. `TurnEnd` 后投递 `retain`；`maybe_compact` 前等待尚未提交的 document。
5. retain 完成后投递 consolidation。
6. 最后才加只读的 `recall` 工具，给继续编程或 computer use 时展开 experience。