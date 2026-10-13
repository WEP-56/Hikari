# Hindsight 调研

给 Hikari 的轻量 Memory Core 用。只记现在要落地的部分：存什么、何时写、何时读。上游是 [vectorize-io/hindsight](https://github.com/vectorize-io/hindsight) `a9eeed77f`，裁过的源码在 `examples/hindsight`（已 gitignore）。这里写的是方案，不是服务架构。

Hikari 的形态接近 [Hermes](https://github.com/NousResearch/hermes-agent)（「The agent that grows with you」）：一个能写代码、用浏览器、做 computer use 的个人 agent。差别只在成长的载体。Hermes 把成长放在学习和技能上；Hikari 把这一格换成 hindsight 的长期记忆。伴侣是相处的语气，不是把事务能力拿掉。所以下面的 `world` / `experience` 划分照搬，不改成「共同经历」。

HTTP、鉴权、多租户、Postgres、mental model、directives、附件，第一版都不做。

## 存储本身

记忆不是一段对话原文。原文只是来源，真正被检索和注入的是从原文抽出的事实，以及事后沉淀出的观察。

三层：

| 层 | 是什么 | 谁写入 |
| --- | --- | --- |
| document | 一段对话或文档的原文，带 `content_hash` | retain 开始时 |
| memory unit，`world` / `experience` | 一句可独立回忆的事实 | 抽取 |
| memory unit，`observation` | 多条事实收成的一个面 | consolidation，抽取不许直接写 |

`fact_type` 只有这三种。`opinion` 和列 `confidence_score` 已从现行 schema 删除，不要抄回来。

### 一条记忆上的字段

以 [models.py](../examples/hindsight/hindsight-api-slim/hindsight_api/models.py) 的 `MemoryUnit` 和 [base.py](../examples/hindsight/hindsight-api-slim/hindsight_api/engine/memories/base.py) 的 `FactRecord` 为准。

- `text`：事实正文。concise 模式是一两句，不是原话照抄。
- `context`：可选的周围语境。
- `fact_type`：`world` | `experience` | `observation`。
- 时间，四个意思不要混：
  - `occurred_start` / `occurred_end`：事情发生的区间。点事件两端相同。只说了「2015 年」就覆盖整年，不要收成 1 月 1 日。
  - `mentioned_at`：这句话被说出来的时间。旧文档今天才入库，这个时间仍是文档自己的时间。
  - `created_at`：第一次写入。之后不动。
  - `updated_at`：最后一次写入。召回的时间窗和 consolidation 的「读完之后有没有被改」都看它。
- `metadata`：调用方的 str→str 袋。不是抽取模型自己发明的字段。
- 来源：`document_id`、`chunk_id`。observation 另有 `source_memory_ids` 和 `proof_count`。
- `embedding`：这条 `text` 的向量。
- `consolidated_at`：这条 world/experience 被折进 observation 的时间。空着表示还在待沉淀队列里。observation 自己没有这个标记。

原文留在 document 上，供事后展开。平时不进提示词。

### 类型怎么分

抽取提示词只让模型输出两种，见 [fact_extraction.py](../examples/hindsight/hindsight-api-slim/hindsight_api/engine/retain/fact_extraction.py)：

- `world`：外部事实，以及用户的偏好、规则、纠正、计划、特质。即使用户是在和助手说话时说的，也是 world。
- `assistant`：助手自己做过、试过、发现过的事。入库时改名成 `experience`。

observation 不在抽取结果里。它只能由后面的沉淀生成。

Hikari 照搬这刀，不改：

- `world`：用户和世界。名字、偏好、约束、关系、项目事实、对方纠正过的规则。即使用户是在闲聊里说的，也是 world。
- `experience`：Hikari 自己做过的事。编程、浏览器、computer use 的流程落在这里：改了哪个文件、为什么这么改、命令的结果、页面走到哪、桌面操作做成了什么。这是它下次接着干同一件事时要召回的记录。
- 不存：寒暄、纯过程话（「等一下」「好的」）、没有结果的尝试性自语。

一条 experience 记一个动作或一段流程的结果，不记终端滚动。例如「把登录改成 cookie，因为刷新后 session 丢失」，而不是把 `bash` 的每一行 stdout 各存一条。共同经历如果只是用户陈述的事实，仍进 world；只有 Hikari 自己执行过，才进 experience。

### 关系

四样，重要程度不一样。

1. **来源边（最重要）**。observation → 它吃掉的 fact，存在 `source_memory_ids`，不是 `memory_links` 的一种。`proof_count` 是这个列表的规模。读 observation 时可以顺着它把原始事实展开。
2. **实体**。`Entity`（规范名、`mention_count`、`first_seen` / `last_seen`）和 `UnitEntity`（记忆 ↔ 实体）。抽取时模型给出名字字符串，写入前收成同一个实体。人、组织、地点、关键物件都算。关于用户的事实要带上 `user`。
3. **因果边**。retain 只写 `caused_by`，权重默认 1。同一批抽出的事实之间，用下标互相指向，写入时换成 unit id。历史类型 `causes` / `enables` / `prevents` 只为旧数据保留。
4. **近邻边，写入时算，不是模型抽的**。见 [link_creation.py](../examples/hindsight/hindsight-api-slim/hindsight_api/engine/retain/link_creation.py)：
   - `temporal`：发生时间靠近的事实互连
   - `semantic`：向量相似度过阈值的事实互连
   - `entity`：共享实体的事实互连（表结构在 `MemoryLink`，权重 0–1）

`EntityCooccurrence` 是共现计数缓存。第一版可以不建，图检索真用到再算。

`MemoryLink` 的主键是 `(from, to, link_type, entity_id)`。同一对记忆可以同时有时间边和语义边。

### 把握不放在行上

现行行里没有 confidence。代替物：

- 召回分 `RecallScores`：`semantic`、`keyword`、`reranker`、`final`。`final` 和 `reranker` 可以当弃权线，低过线就不注入。
- observation 的 `proof_count`。1 是中性，大约 150 封顶。它只轻微改排序（默认 ±5%），不是真假开关。
- 证据时间算出的 trend：`new` / `strengthening` / `stable` / `weakening` / `stale`。近 30 天密度相对更早的密度，比值 > 1.5 增强，< 0.5 减弱，近窗没有证据就是 stale。这是读的时候算的，不落库。

## 何时存储

写入分两拍。第一拍抽事实，第二拍在后台把事实收成观察。对话进行中不写。

### 第一拍：一段对话结束之后

retain 是一次显式提交，输入是一段 document（一次会话、或会话里的一个 chunk），不是每个 token。

建议的触发：

- 一轮用户话轮结束，且这轮有实质内容
- 或者会话闲下来、窗口要压缩之前，把还没 retain 的原文交出去

不要在生成每个字时写。compaction 丢掉的原文，如果还没 retain，就真丢了，所以压缩之前要先走完这一拍。

同一 document 用 `content_hash` 认出重复提交。追加时如果哈希已经变了，整次重做，不要把新事实接到旧正文上。

### 抽什么，不抽什么

默认 concise。标准是一句：「六个月后还值得想起来吗」。提示词里的正反例：

要：

- 人名、关系、角色、背景
- 偏好、习惯、对方纠正过的做法
- 里程碑、决定、变化
- 计划、期限、承诺
- 技能和长期约束
- Hikari 自己完成的流程：改过的代码、跑通或失败的命令、浏览器走到的页面、computer use 的关键步骤和结果
- 能代表一段经历的感官或情绪细节

不要：

- 问候、谢谢、好的、知道了
- 「我去看一下」这种还没有结果的过程话
- 工具输出的逐行日志。收成一条 experience，留下结果和原因
- 同一段里已经抽过的重复句

相关的几句收成一条。相对时间写进正文时改成绝对日期（「昨天」写成那天的日期）。持续状态不填 `occurred_*`。指代要解开：「我室友」和后文的 Emily 写成 `Emily (user's roommate)`。

空话、纯标点、一两个字符的抽取结果直接丢掉，不入库。

### 第二拍：retain 之后，异步

`enable_observations` 打开时，retain 完成后跑 consolidation。它只拉 `consolidated_at` 为空的 `world` 和 `experience`，按 `created_at` 从旧到新。没有待处理行就直接返回。

对每一批，先把已有 observation 召回给模型，模型返回三种动作：

- create：新开一条 observation，挂上 `source_fact_ids`
- update：改写一条已有 observation 的 `text`，把新 fact id 并进 `source_memory_ids`，`proof_count` 增加
- delete：极少用。提示词要求几乎不删

然后这些 fact 标上 `consolidated_at`。中途崩溃只丢掉还没标的，已写入的 observation 留下。

沉淀规则里，伴侣实现要守住的是这几条（原文在 [prompts.py](../examples/hindsight/hindsight-api-slim/hindsight_api/engine/consolidation/prompts.py)）：

- 同一件事更新旧观察，不要新生一条近重复。没有对上的面才新建。
- 一条 observation 只讲一个面：一个人、一段关系、一个决定、一次事件。不按大主题合并。
- 状态变了就改写那一条，带上日期（「养过一只叫 Rex 的狗，2025-03-15 去世」）。不另起一条矛盾记录，也不删旧历史。
- 不做算术。先说「有两条狗」再说「有只狗叫 Rex」，不要把数量改成 3。只有对方明确说出新数字才改数字。

observation 的 `mentioned_at` 取来源事实里最新的那个；`occurred_*` 取来源里最早的开始和最晚的结束。

## 何时读取

有两个读者，策略相反。不要合成一次召回打天下。

### 回复用户之前

这是注入进 agent 上下文的那次读。上游的 reflect 不在每轮开始时自动塞记忆。它强制模型先调用 `recall` / 观察列表，没搜过不许给最终答案。

Hikari 更适合固定在生成前做一次，而不是指望模型记得调用：

1. 用本轮用户的话做 query。接着做同一件事时（继续改这个仓库、再开那个网站、接着上次的桌面操作），query 里带上任务本身，好让 experience 沉淀出的 observation 被召回，而不是只靠闲聊句。
2. 默认只召回 `observation`，token 预算小（上游默认事实预算 4096，日常可以更小）。
3. 分数过低就什么都不注入。没有记忆好过注入一条擦边的。
4. 用户在问过去，或正在继续一段未做完的流程，或 observation 不够，再把 `include_source_facts` 打开，把 `source_memory_ids` 指向的原文事实展开。编程和 computer use 经常需要这一步，因为 observation 只有结论，步骤在 experience 里。
5. 仍不够，最后才按 `document_id` / `chunk_id` 翻原文。原文不进默认提示词。

注入的是 observation 的 `text`，带上时间和 `proof_count`。不要把向量分、图遍历细节写进提示词。

### 沉淀的时候

consolidation 给模型看「已有观察」时，自己先召回一次。这次和回答用户不是同一套：

- 只要 `fact_type=observation`
- 预算用低档
- 融合用 interleave（各路轮流取），不用 RRF，也不用 cross-encoder

原因：要合并的那条旧观察往往只在语义路排第一，词法和图路上都没有。RRF 会把它平均掉，模型看不见，就会再 create 一条重复。interleave 保证每一路的第一名都占一个位子。

### 召回怎么排

回答用户时的排序，四路并行再融合，见 [fusion.py](../examples/hindsight/hindsight-api-slim/hindsight_api/engine/search/fusion.py) 和 [reranking.py](../examples/hindsight/hindsight-api-slim/hindsight_api/engine/search/reranking.py)。

四路：

- 语义：query 向量对记忆向量
- 关键词：BM25。索引文本额外拼上实体名和日期的英文写法，所以「Alice」和「三月」能命中没写在正文里的信号
- 图：从种子记忆沿着 entity / temporal / causal 边扩。找的是向量和关键词都漏掉、但连在同一人同一件事上的事实
- 时间：query 里有日期，或调用方给了时间窗时，抬高落在窗里的记忆。这不是过滤器，窗外的结果仍可从别的路进来

融合用 RRF，`score = Σ 1/(60+rank)`。每一路先截断，避免一路刷屏。

之后的 `final`：

```
final = relevance
      * (1 + 0.2 * (recency - 0.5))
      * (1 + 0.2 * (temporal - 0.5))
      * (1 + 0.1 * (proof_norm - 0.5))
```

`relevance` 是 cross-encoder。没有它时，用 RRF 名次映射到 0.1–1，不要退化成纯按时间排序。recency 默认一年线性衰减到地板；没有日期的记忆这三项都是 1，不奖不罚。`proof_norm` 只对 observation 偏离 0.5。

`prefer_observations` 为真时，一条原始事实如果已经被返回的某条 observation 收进 `source_memory_ids`，就把原始事实拿掉，空位用后面的结果补。日常回答走这条，避免同一件事说两遍。沉淀自己去召回原始事实时必须关掉，否则待合并的事实会消失。

## 第一版建议砍掉的

**Hikari 已确定并实现的取舍（2026-10-13）**：本地 Rust/SQLite 单用户记忆库，不加载 embedding，不部署 Hindsight 服务。下文向量、图与融合是上游研究，不是 Hikari 当前依赖；实际检索为轻量中文词项、FTS5/BM25、实体线索和可选时间过滤。保留 document → world/experience → observation 架构、证据来源、后台沉淀、有限召回和按需展开。完整契约见 [pi-memory-plan.md](pi-memory-plan.md)。

这些在上游是真的，但不是 Memory Core 的第一刀：

- mental model：人预先写的一个问题，到点用 reflect 刷新成一篇文章。observation 已经是自动长出来的灵魂。mental model 以后再加。
- directives、disposition（怀疑 / 字面 / 共情 1–5）。那是 reflect 的语气旋钮，不是记忆。
- 多 bank、tag 可见性。Hikari 一个用户一条记忆就够了。`bank_id` 留着，固定成这一个关系。
- 图扩散作为独立的第四路。实体边和时间边先存着，检索先做语义 + 关键词。图路等边真的有了再接上，融合接口不用改。
- cross-encoder。没有它就用上面的 RRF 名次当 relevance。
- 附件、文件解析、webhook、操作队列。

## 和 agent loop 的接缝

三处调用，都在 pi 的工具 / 轮次边界上，不进 Provider：

1. 轮次结束、以及 compaction 之前：retain。输入是这段新增对话。
2. 下一轮生成之前：recall。输出截断后放进本轮上下文。
3. retain 返回后，后台：consolidation。不阻塞回复。

抽取、沉淀、召回的排序都可以是普通函数。只有「这段文字算不算一条事实」和「该更新哪条 observation」需要一次模型调用。
