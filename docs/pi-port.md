# Pi 接入：第一阶段

Hikari 的 agent 是 Pi 的改造版。桌面 agent、模型协议、MCP、Skill 与 Hindsight 风格 Memory Core 已接入现有 `Memory` 边界；未启用记忆的测试与会话仍使用 `NullMemory`。

本地参考 `examples/pi_agent_rust` 来自 `b6fa9e18b`，是裁剪后的源码集合，缺少 Cargo manifest、HTTP/config/workspace/extension 等依赖模块，不能直接作为 path dependency 编译。使用可独立移植的模块和 Pi 的协议、执行边界，保留 GPUI 所需的薄适配。引用文件均保留来源注释，上游许可证全文位于 `licenses/pi_agent_rust.LICENSE`；它包含 OpenAI/Anthropic rider，分发时必须保留原条件。

| Hikari 文件 | Pi 来源 | 复用方式 |
| --- | --- | --- |
| `crates/hikari-agent/src/pi/sse.rs` | `src/sse.rs` | 移植 `SseEvent` / `SseParser`；去掉 asupersync/futures stream wrapper，增加同步 HTTP reader |
| `crates/hikari-agent/src/pi/turn_recovery.rs` | `src/turn_recovery.rs` | 完整移植分类、恢复预算与原有测试；默认 conservative |
| `crates/hikari-agent/src/pi/iterations.rs` | `src/agent.rs` | 移植轮数默认值、上限、参数解析、80% 提示逻辑 |
| `crates/hikari-agent/src/pi/compaction.rs` | `src/compaction.rs` | 原样移植首次摘要和增量摘要提示词 |
| `crates/hikari-agent/src/pi/edit.rs` | `src/tools.rs` | 移植 BOM、换行检测、LF 归一化函数 |
| `crates/hikari-agent/src/agent.rs` | `src/agent.rs` | 适配当前 `Message`/GPUI 事件，保留 stream → tools → stream、取消、工具配对、pause_turn、恢复、steering/follow-up 边界 |
| `crates/hikari-agent/src/protocols.rs` | `src/providers/openai_responses.rs`、`src/providers/anthropic.rs` | 适配器采用 Pi 的稳定 tool slots、terminal snapshots、原生 reasoning replay；HTTP 使用现有 ureq |
| `crates/hikari-agent/src/file_tools.rs` | `src/tools.rs` | 基于现有工具接口适配 read/write/edit/ls/find/grep；edit 复用上游换行函数 |

MCP 和 Skill 的上游实现已从本地参考目录删除，本阶段在 `hikari-runtime` 补齐，全部通过 `ToolRegistry` 暴露给同一个 loop。

本轮另参考 [earendil-works/pi](https://github.com/earendil-works/pi/tree/f13c00fb3a4d77ad3faa815057e31d614d42b3f1)：Hikari 采用自有 Rust BM25 发现和 ToolCatalog，按配置转换 Responses `additional_tools` / 客户端 `tool_search_call` + `tool_search_output`，或 Messages `tool_addition` / `tool_removal`；默认兼容路径发送当前完整定义，不自动启用服务端原生搜索。MCP 重连和每轮 tools/list 刷新目录，调用前复核工具仍存在且 schema 未改变。

## 运行

在 `E:\Hikari` 执行：

```powershell
cargo run
```

桌面应用位于 `crates/hikari`，是根工作区的默认启动目标；也可使用 `cargo run -p hikari` 显式选择。编译产物统一位于根目录 `target`，发布构建命令为 `cargo build --release`。

设置 → 提供商：选择 Chat Completions、Responses 或 Anthropic，填写服务地址和模型名字。API key 按提供商明文保存到本地配置，重启后恢复；留空时读取 `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` 或配置中的自定义环境变量。点击“保存设置”持久化密钥、协议、地址、模型、记忆预算和主题。人格页独立保存全局 `.hikari/AGENTS.md` 角色卡。

现在可添加、切换、重命名或删除多个提供商，分别保存模型列表与元数据。获取模型列表复用当前地址、认证、请求头和超时；接口只返回 ID 时不猜测模型能力。支持配置思考/图片能力、可用思考挡位、上下文与最大输出。图片能力仅作为元数据，聊天附件尚未实现。思考挡位映射到实际 `reasoning_effort`；上下文用量使用当前保存模型的上限。请求头为 JSON 字符串映射，认证值必须使用 `${ENV_NAME}`；Chat 的 token 参数名称、系统角色、流用量，以及 Responses 的服务端存储可配置。

普通设置保留草稿，保存前验证并在失败时继续使用原配置；可恢复已保存设置。聊天模型菜单使用已保存提供商/模型及支持的思考挡位，选择即保存。界面可分别设置正文、代码/工具输出、终端字体与字号，以及界面字号和正文行高。关于页面提供仓库/Issues/Releases 链接和 GitHub latest 正式 Release 的语义版本比较，不自动下载或安装。

本地数据位于 `.hikari/config.json` 和 `.hikari/sessions.json`，已 gitignore。会话保存完整 transcript、当前模型 context、usage 和 retain 游标；上下文压缩生成独立 `ContextSummary`，界面历史保留完整原文。

运行中，“插入当前任务”在工具批次完成后的下一次模型请求生效；“排队”或 Enter 在本轮回复后进入下一轮。停止按钮取消运行；未消费的排队输入保留为会话旁注。通常配置/工具集合在一次运行开始时固定，新配置在下一次运行应用；全局关闭 Memory Core 保存后即时切断自动召回、记忆工具和原文提交，已运行的后台任务继续完成。

## MCP

设置 → MCP：可添加本机 stdio 命令或远程 Streamable HTTP URL，显示连接状态和已发现的工具，支持启停、重连、断开、移除。高级 JSON 可编辑参数、认证环境变量及超时，例如：

可先扫描 Claude Desktop/CLI、Codex 和 Hermes 的常用配置，预览来源和启动命令/URL，勾选导入并决定是否覆盖同名配置。扫描只检查文件，不启动服务；导入服务默认停用。无效条目单独报告，所选来源重名时需要只选一个。HTTP 请求头支持字符串映射和 `${ENV_NAME}`，Codex 的 `env_http_headers`、`bearer_token_env_var` 会转换为 Hikari 格式；包含明文认证头或旧版 SSE 的条目会报告诊断。

```json
[
  {
    "name": "filesystem",
    "enabled": true,
    "timeout_seconds": 30,
    "transport": {
      "type": "stdio",
      "command": "npx.cmd",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "E:\\Hikari"],
      "env": {}
    }
  },
  {
    "name": "remote",
    "enabled": false,
    "timeout_seconds": 30,
    "transport": {
      "type": "streamable_http",
      "url": "https://example.com/mcp",
      "bearer_token_env": "HIKARI_MCP_TOKEN"
    }
  }
]
```

工具名称映射为 `mcp_<server>_<tool>`。执行 initialize、initialized notification、分页 tools/list 和 tools/call；传递 HTTP session/version headers，支持 JSON/SSE 响应和 isError。stdio 取消会发送 `notifications/cancelled`。Windows 命令必须可直接执行，例如 `npx.cmd`，不是 shell 命令字符串。

当前范围是 MCP tools。资源、prompt、OAuth 登录、server sampling/elicitation、HTTP GET 通知订阅和动态 tools/list_changed 刷新尚未接入；HTTP session 失效需点击重连。HTTP 取消在读取边界生效，正在等待网络响应时受配置超时限制。

## Skill

设置 → Skill：添加一个 Skill 目录或包含多个 Skill 的目录，重新扫描、逐项启停或移除目录。默认扫描 `.hikari/skills`。`SKILL.md` 必须有 YAML frontmatter 的 `name`、`description`。支持多行描述，重复名称和无效文件显示诊断。

Hikari 自己的 `.hikari/skills` 始终优先扫描。可扫描 Codex/Claude/Hermes/Agents 的用户和项目 Skills，包括自定义 `CODEX_HOME`、`HERMES_HOME`，或填写来源目录单独扫描。勾选后复制 SKILL.md、脚本和参考资料到 Hikari 目录，不改变来源，不覆盖已有同名 Skill，不复制符号链接。也可继续使用“添加目录”直接挂载。启停和目录管理单独持久化。

系统提示仅提供已启用 Skill 的元信息；模型调用 `load_skill(name)` 读取完整指令。`load_skill(name, resource)` 可继续读取该 Skill 目录内的文本资源。模型使用 shell 执行 Skill 脚本，或使用工具读取其他文件。当前不提供在线安装市场。

## 验证与后续

```powershell
cargo test -p hikari-agent -p hikari-runtime -p hikari-memory -p hikari
cargo check -p hikari
cargo fmt --all -- --check
```

测试使用本地 HTTP 与实际 stdio 测试进程，不请求付费模型。覆盖流式 framing、三种协议的关键工具回传、原生 reasoning/签名保存、Pi 恢复策略、BOM/CRLF 编辑、会话/config 重写、Skill 资源边界、MCP 握手/分页/调用/断开。

需要后续真实服务验收：各模型网关、实际 MCP 服务、GPUI 交互与视觉检查。当前是文本及工具场景；图片、附件、MCP 非文本内容原生渲染和协议跨模型 reasoning 兼容留待后续。Memory Core 使用本地 SQLite/WAL、后台抽取/沉淀、中文轻量 FTS5 索引和可恢复任务队列；支持自动 recall、search_observations、recall、expand、forget 及全局/会话开关。默认不加载 embedding 模型。

协议核对使用了 [OpenAI streaming](https://developers.openai.com/api/docs/guides/streaming-responses)、[function calling](https://developers.openai.com/api/docs/guides/function-calling) 及 [MCP transports](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)。
