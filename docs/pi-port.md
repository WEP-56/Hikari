# Pi 接入：第一阶段

Hikari 的 agent 是 Pi 的改造版。第一阶段先接通桌面 agent、模型协议、MCP 和 Skill；Hindsight Memory Core 后续接入现有 `Memory` 边界。当前运行时使用 `NullMemory`。

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

## 运行

在 `E:\Hikari` 执行：

```powershell
cargo run -p hikari
```

设置 → 提供商：选择 Chat Completions、Responses 或 Anthropic，填写服务地址和模型名字。密钥输入框只用于本次运行；可在启动程序前设置 `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` 或配置中的自定义环境变量。点击“保存设置”持久化协议、地址、模型、人格、主题及管理设置，密钥值不写入配置。

本地数据位于 `.hikari/config.json` 和 `.hikari/sessions.json`，已 gitignore。会话保存完整 transcript、当前模型 context、usage 和 retain 游标；上下文压缩生成独立 `ContextSummary`，界面历史保留完整原文。

运行中，“插入当前任务”在工具批次完成后的下一次模型请求生效；“排队”或 Enter 在本轮回复后进入下一轮。停止按钮取消运行；未消费的排队输入保留为会话旁注。配置/工具集合在一次运行开始时固定，新配置在下一次运行应用。

## MCP

设置 → MCP：可添加本机 stdio 命令或远程 Streamable HTTP URL，显示连接状态和已发现的工具，支持启停、重连、断开、移除。高级 JSON 可编辑参数、认证环境变量及超时，例如：

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

系统提示仅提供已启用 Skill 的元信息；模型调用 `load_skill(name)` 读取完整指令。`load_skill(name, resource)` 可继续读取该 Skill 目录内的文本资源。模型使用 shell 执行 Skill 脚本，或使用工具读取其他文件。当前不提供在线安装市场。

## 验证与后续

```powershell
cargo test -p hikari-agent -p hikari-runtime
cargo check -p hikari
cargo fmt --all -- --check
```

测试使用本地 HTTP 与实际 stdio 测试进程，不请求付费模型。覆盖流式 framing、三种协议的关键工具回传、原生 reasoning/签名保存、Pi 恢复策略、BOM/CRLF 编辑、会话/config 重写、Skill 资源边界、MCP 握手/分页/调用/断开。

需要后续真实服务验收：各模型网关、实际 MCP 服务、GPUI 交互与视觉检查。当前是文本及工具场景；图片、附件、MCP 非文本内容原生渲染和协议跨模型 reasoning 兼容留待后续。Memory Core 的后台 retain/consolidation、SQLite 存储与 recall 尚未实现。

协议核对使用了 [OpenAI streaming](https://developers.openai.com/api/docs/guides/streaming-responses)、[function calling](https://developers.openai.com/api/docs/guides/function-calling) 及 [MCP transports](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)。
