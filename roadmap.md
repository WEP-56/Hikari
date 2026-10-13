# Hikari 路线图

Hikari 是基于 Rust、GPUI Kit 的桌面 AI Agent，定位为注重长期记忆的个人助手。本阶段补强 harness 和必要工具，同时保持会话交互简单、执行边界明确。

## 实施优先级

| 阶段 | 内容 |
|---|---|
| 第一批 | 完整请求的上下文预算、MCP tool-search 及协议适配、web_search / web_fetch、session_search / session_read、长工具输出的保存与继续读取 |
| 第二批 | 会话内执行任务底座、todo_list、clarify、process_manage、权限门及前端交互 |
| 第三批 | delegate_task、统一附件与多模态工具结果、image_gen、skill_manage |
| 后置 | cronjob_manage；语音输入与输出、桌面悬浮窗、English 多语言支持 |

附件和工具结果的数据结构先确定，再接入图片生成。执行任务底座服务于后台进程和子 Agent，不扩展为跨会话任务管理系统。

## 工具

| 工具 | 范围与交互 |
|---|---|
| tool_search | 搜索尚未暴露的 MCP 工具，命中后使其在下一次模型请求中可调用；Hikari 内置工具直接暴露 |
| web_search | Hikari 不自建搜索后端，适配 Tavily、Exa、Serper 等提供商；增加提供商配置界面 |
| web_fetch | 获取网页正文，返回来源 URL；控制请求超时、内容大小及输出预算 |
| todo_list | 由 LLM 维护的会话内待办状态，前端展示进度；随会话保存和恢复，不做跨会话任务系统 |
| clarify | 向用户提问，阻塞当前会话的 Agent 执行，收到回答后返回工具结果；前端适配，超时、取消、失败返回简单明确的状态 |
| session_search / session_read | 搜索历史会话、按范围阅读会话原文；返回会话与消息来源，支持分页及输出预算 |
| process_manage | 管理本会话 / 本任务通过 shell 启动并登记的后台进程；查询状态、增量读取输出、停止进程 |
| delegate_task | 委派隔离的子 Agent，异步执行，支持 spawn / list / steer / stop |
| image_gen | 适配 OpenAI 标准图像 API；支持图片生成结果、聊天输入框图片粘贴 / 拖入、会话气泡图片展示与查看 |
| skill_manage | 安装、制作、修改和删除 Skill；复用现有发现、索引及 load_skill 能力 |
| cronjob_manage | 定时任务增删查改，后置实现 |

web_search 和 image_gen 的提供商配置放在同一设置页面，作为“其它提供商”。

## Harness

### MCP tool-search 与协议适配

将“全部已注册工具”与“当前会话对模型可见的工具”分开管理。MCP 工具默认延迟暴露，tool_search 搜索后激活命中工具，下一次请求真正携带可调用的工具定义，不能仅将 schema 写入结果文本。

- 索引包含工具名称、描述、参数名称与说明，以及 MCP 服务名称、描述 / instructions。
- 第一版使用 BM25，限制命中数量及新增 schema 的 token 总量；支持中英文检索，不直接照搬只处理英文的分词器。
- tool_search 的说明保持稳定，避免因 MCP 连接和工具数量变化而改变；可通过简短的 MCP 服务目录提示可用能力。
- 工具加载状态限于会话，随会话保存 / 恢复，压缩后仍能继续调用；子 Agent 使用自己的可见工具集。
- 区分没有匹配工具、服务不可用和执行失败；连接重建、工具列表变化时更新目录，并使已撤回工具不可调用。
- 工具可见性不代表执行授权，实际调用仍须经过权限检查。

参考实现：earendil-works/pi，检查版本 f13c00fb3a4d77ad3faa815057e31d614d42b3f1。本地 examples/pi_agent_rust 未找到 tool-search 实现。

| 协议 | 参考仓库的实现路径 |
|---|---|
| OpenAI Responses | 优先使用会话内的 additional_tools；其次使用客户端 tool_search_call + tool_search_output，输出工具定义带 defer_loading |
| Anthropic Messages | 使用 inline-tools-2026-09-15 beta，通过 tool_addition / tool_removal 更新工具；初始工具列表与 deferred placeholder 保持稳定 |
| 兼容路径 | 不支持原生增量披露时，下一次请求在顶部发送完整的当前可见工具列表，保留按需发现能力 |

按模型和实际接口配置能力，不能仅凭协议名称开启原生增量披露。参考仓库中 Responses 的移除 / 同名重定义会回退到当前完整工具列表。原生字段及 beta 支持需要在目标提供商上验证，不把参考实现视为所有兼容网关的能力保证。

实现与验收参考：

- [tool-search：BM25、搜索与激活](https://github.com/earendil-works/pi/blob/f13c00fb3a4d77ad3faa815057e31d614d42b3f1/packages/coding-agent/src/extensions/tool-search/tool.ts)
- [Responses：工具增量披露](https://github.com/earendil-works/pi/blob/f13c00fb3a4d77ad3faa815057e31d614d42b3f1/packages/ai/src/api/openai-responses-shared.ts)
- [Messages：会话内工具变更](https://github.com/earendil-works/pi/blob/f13c00fb3a4d77ad3faa815057e31d614d42b3f1/packages/ai/src/api/anthropic-messages.ts)
- [协议请求结构测试](https://github.com/earendil-works/pi/blob/f13c00fb3a4d77ad3faa815057e31d614d42b3f1/packages/ai/test/transcript-tool-changes.test.ts)

### 上下文预算与缓存

- 预算覆盖完整请求：system prompt、角色卡、Skill 索引、召回材料、工具 schema、消息和附件，并预留模型输出空间。
- 保持稳定前缀的内容顺序及序列化一致；区分基础规则、角色卡、技能索引、会话环境和当轮召回。
- Skill 索引不是唯一动态内容：记忆召回、工作区、角色卡修改、工具加载也会影响请求。
- volatile tier 只是内部内容组织方式，不等于 Provider 的缓存边界；按各协议的实际缓存机制适配并验证。

### 会话内执行任务底座

供后台进程及子 Agent 共用任务身份、状态、取消和结果投递机制。

- 任务绑定 session_id / task_id / parent_task_id，以及工作区、模型和权限配置。
- 支持排队、运行、等待用户、完成、失败、取消等基本状态；不引入跨会话任务列表或承诺管理。
- 同一会话的上下文写入串行化，后台任务可以并行执行，结果通过队列投递，避免覆盖用户的新消息。
- 确定用户正在发言、切换会话、父任务结束和应用重启时的结果处理规则。
- 增加耗时、token、并发数和子任务数量预算，取消能传递到所属子任务和进程。
- 执行记录与结果可以持久化，但崩溃恢复不能默认重跑可能已经成功的副作用动作。

todo_list 独立保存会话内的待办状态，不依赖长期记忆或压缩摘要恢复。clarify 只阻塞所属会话的执行，不阻塞 UI 或其它后台任务。

### 子 Agent

delegate_task 派生全新、隔离的 AI Agent 子实例。父代理只接收最终 summary，中间工具调用不进入父上下文；顶层调用通过后台异步派发，工具先返回任务身份 / 状态，最终结果作为后续一轮事件回到所属会话。

- schema 提供 tasks[]，每项包含 goal / context / output_schema / images / group；控制 action 为 spawn / list / steer / stop。
- 明确子实例接收的上下文、tools 和记忆访问边界，不自动复制全部父会话。
- 子任务不能扩大父任务授权范围，分别维护工具可见性和取消状态，共享总体预算约束。
- 隔离上下文不等于隔离文件系统；明确共享工作区及并发写入边界，避免多个子任务同时修改同一资源。
- summary 保留必要的证据和产物引用，区分已验证结果、推断和未完成事项。

### 工具结果、长输出与附件

- 工具结果统一支持文本、结构化数据、图片 / 文件引用；保留失败类型、截断状态及是否可重试。
- 长工具输出保存为 artifact，模型可分页或按范围继续读取；不能只有“已截断”而无法取得剩余内容。
- 聊天图片、MCP 图片结果、image_gen 和后续语音复用附件存储及消息内容结构。
- 会话保存资源引用，实际文件单独存储；定义资源删除和清理规则。
- 重试区分读取操作与有副作用的动作，不能对失败或超时的工具统一自动重放。

### Skill

现有 SkillCatalog 已将名称和描述放入 system prompt，并要求匹配任务时调用 load_skill，包含 Skill 相对资源读取能力。保留现有索引和加载工具，只新增 skill_manage；不重复增加 skills_list / skill_view。

- 控制索引成本，可将约 3k tokens 作为初始参考预算，根据实际 Skill 数量调整。
- 只有真实模型使用表现不足时，再补强主动使用 Skill 的提示。
- skill_manage 支持完整资源安装、制作、修改、删除；修改保留版本 / 回滚能力。
- 管理操作通过权限层，完成后在下一轮刷新目录，运行中的任务保持已确定的 Skill 版本。

### 历史检索与记忆来源

- session_search / session_read 返回原始来源；历史内容是资料，不能当作新的用户指令或授权。
- 明确关闭记忆、遗忘和删除会话的语义，避免历史检索结果将被遗忘内容重新写入长期记忆。
- 子 Agent summary 和历史检索结果属于派生 / 召回材料，不能直接当成新的已确认事实证据重复抽取。
- 验证用户纠正偏好后的新旧信息关系，避免同时召回互相矛盾的偏好。

## 权限门

确定实现两种模式，在输入框附近提供切换入口，审批 UI 提供“允许本次”和“拒绝”。

| 模式 | 对用户的说明 | 执行策略 |
|---|---|---|
| 审慎 | ta 会在关键行动前与你确认。 | 工作区外访问与高风险操作触发审批，工作区内常规操作自主执行 |
| 自主 | ta 会自主处理常规操作，仅在重要事项前确认。 | 放宽常规工作区外操作；删除、敏感资源修改及重要外部副作用等仍审批 |

### 共同约束

- 文件工具、shell、MCP、Skill 管理等执行入口统一经过权限层，后台任务和子 Agent 不得绕过。
- 授权绑定实际工具、命令 / 参数、工作目录和目标资源，审批中展示这些信息；参数或目标变化时重新判断。
- 用户偏好和长期记忆不构成执行授权。第一版不做复杂的永久授权规则。
- 审批拒绝、取消或超时则不执行，返回明确工具结果。
- cwd 只表示命令启动目录，不构成文件系统隔离；命令规则属于策略检查，不能宣称提供了 OS 沙箱。
- 文件路径检查解析符号链接和 Windows junction；审慎模式的工作区外审批通过后，工具也应能在本次授权范围内执行，而不是仍被旧的固定路径限制拒绝。
- MCP readOnlyHint / destructiveHint 等元数据仅辅助判断，不作为可信授权；工具搜索和加载本身不会授予执行权限。

### 触发审批的操作

以下是识别类别与示例，不把简单字符串匹配当成完整安全边界。优先适配当前 Windows 执行环境（shell 当前为 cmd.exe /C），同时覆盖用户通过 PowerShell、Git Bash、WSL 等入口执行的命令。

| 类别 | 示例与判断依据 |
|---|---|
| 工作区外访问 | 审慎模式下检查实际解析后的目标路径；自主模式下的敏感目标仍审批 |
| 删除与递归删除 | rm -r / --recursive、rd / rmdir /s、del、Remove-Item -Recurse，以及 xargs rm、find -exec rm / -delete；结合目标和范围判断 |
| 危险权限修改 | chmod 777 / 666 / o+w / a+w，递归不安全权限，chown -R root，以及 Windows ACL 的广泛写权限变更 |
| 磁盘与设备写入 | mkfs、format、diskpart、dd of=、重定向写入块设备；dd 主要检查写入目标，不能只匹配 if= |
| 数据库破坏性操作 | DROP TABLE / DATABASE、TRUNCATE TABLE、无 WHERE 的 DELETE FROM |
| 敏感文件写入 | tee、重定向、cp / mv / install、sed -i 等修改系统配置、SSH、凭据文件或 Hikari 配置；覆盖 Windows 对应路径 |
| 系统服务与进程控制 | systemctl stop / restart / disable / mask，sc stop / delete，Stop-Service，以及大范围强制终止进程；区分本任务登记进程与无关进程 |
| 远程脚本执行 | curl / wget 管道或进程替换交给 shell，PowerShell 下载后执行等 |
| 发送和提交等外部副作用 | 对外发送消息、提交或发布、覆盖远程资源等，结合具体目标及本任务已有授权判断 |
| 包装执行与无法判断的命令 | 解析 cmd /c、PowerShell -Command / -EncodedCommand、bash / sh / zsh / ksh -c（含 -lc）、python -c、perl / ruby / node -e 的内部动作；无法可靠判断时审批 |

包装执行本身不等于破坏性操作。Node 的 -c / --check 是语法检查，不作为内联脚本执行规则。复合命令、管道、重定向和脚本需按实际动作检查，不能只检查第一个可执行文件名。

删除原先 Hermes / gateway 的专属规则，改为防止工具误终止 Hikari 主进程或绕过其运行管理。

### 硬性拦截

两种模式都不允许通过审批执行以下明确的灾难性动作。

| 动作 | 示例 |
|---|---|
| 清除文件系统根目录或系统盘 | rm -rf /、--no-preserve-root 及明显变体；Windows 系统盘 / 系统目录的明确整体清除 |
| 明确的 fork 炸弹 | Bash 的 :(){ :\|:& };: 及可识别等价形式 |
| 格式化正在使用的系统根设备 | mkfs.* 作用于已挂载根设备，以及 Windows 对应系统盘格式化动作 |
| 明确清零物理磁盘 | dd if=/dev/zero of=/dev/sd* 及可识别的等价全盘覆写 |

硬拦截需要结合真实目标判断，不能将普通文件写入误认为物理磁盘覆写。

## 后续能力

- cronjob_manage 后置。实现时再确定时区、睡眠唤醒补跑、错过多次是否合并、应用退出后是否执行，以及提醒是否需要启动完整 Agent。
- 语音输入、声音输出。
- 桌面悬浮窗绑定会话；支持直接呼出时必须创建新会话。
- 多语言，优先 English。

## Harness 验收场景

- 大量 MCP 工具下能否找对工具，中文检索是否有效，命中工具能否在下一次请求实际调用。
- 验证各协议原生披露与兼容路径的请求结构、能力开关、完整预算和缓存效果。
- 工具加载状态在会话恢复、压缩和 MCP 重连后是否正确，已撤回工具是否停止可用。
- todo 状态能否在会话内跨轮次维护，clarify 回答 / 拒绝 / 取消后能否正确继续或结束。
- 异步结果是否投递到正确会话，是否覆盖用户新输入；取消后是否仍产生可避免的动作。
- 崩溃恢复是否重复执行副作用动作，父子任务是否突破权限或预算。
- 权限检查覆盖 Windows / Unix、复合命令、包装执行、符号链接 / junction，审批通过后只执行被批准的动作。
- 长输出能否继续读取，附件能否保存与恢复。
- 用户纠正或遗忘后，历史检索和子 Agent summary 是否造成错误召回或重复记忆写入。
