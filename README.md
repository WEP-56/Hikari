![icon](crates/hikari/resources/branding/portrait.png)

# Hikari

基于 Rust、[Gpui_kit](https://gpui-kit.com/) 实现的伴侣型个人助手，定义它的性格，在所有对话中养成你们的关系吧。


## 运行

在项目根目录 `E:\Hikari` 执行：

```powershell
cargo run
```

发布构建使用 `cargo build --release`，程序位于 `target/release/hikari.exe`。

## 项目结构

- `crates/hikari`：GPUI 桌面应用；形象资源位于 `resources/branding`。
- `crates/hikari-agent`：Pi agent core 与模型协议。
- `crates/hikari-runtime`：配置、会话、MCP 与 Skill 管理。
- `crates/hikari-memory`：本地 SQLite 长期记忆、抽取、沉淀与检索。
- `assets`：README 页面截图。
- `docs`：设计与接入文档。
- `examples`：本地参考实现。

工作区共用根目录的 `Cargo.lock` 和 `target`。

## 设置与本地数据

设置页可管理多个提供商及模型，配置请求头、三种协议的兼容项、思考支持/挡位、图片能力元数据、上下文与输出限制。支持获取模型 ID 列表、正文/代码/终端字体与字号预览，关于页可打开仓库、Issues 和 Releases，并检查正式版本更新。

Skills 可从 Codex、Claude、Hermes 和通用 Agents 的资源目录扫描、勾选并复制到 `.hikari/skills`；也支持直接挂载目录。MCP 可从其他客户端的 JSON/TOML/YAML 配置扫描、勾选导入，导入服务默认停用，启用后才连接。

长期记忆跨会话共享，新会话默认开启，也可单独关闭。记忆页可配置抽取/沉淀模型、查看任务状态、重试失败任务、搜索和遗忘记忆。数据位于 `.hikari/memory.db`，无需 embedding 模型或额外服务。关闭 Memory Core 停止自动召回、记忆工具和新原文提交；正在执行的后台任务会完成。

人格页编辑全局 `.hikari/AGENTS.md` 角色卡，包含描述、性格、背景、关系、说话方式、场景与示例。保存后下一轮生效，也可直接用文本编辑器修改文件；角色卡不写入长期记忆。

设置保存在 `.hikari/config.json`，支持旧版迁移、校验、备份与原子替换。API key 按提供商明文保存，重启后恢复；留空时使用配置的环境变量。认证请求头用 `${ENV_NAME}` 引用。会话保存在 `.hikari/sessions.json`，包含工作区与置顶状态；图片附件和 SQLite 会话存储尚未接入。详细说明见 [Pi 接入文档](docs/pi-port.md)。

新建会话默认“不选择工作区”，可在欢迎页下拉选择历史路径或通过系统窗口选择文件夹。普通对话固定使用 `.hikari/chat`；工作区会话的 Agent、终端及外部打开操作使用该会话的路径。未选择工作区时顶部终端和外部打开入口禁用。工作区历史保存到 `.hikari/workspaces.json`。侧栏会话支持右键重命名、置顶和删除；运行中的会话暂不可删除。


## visual
![page1](assets/page1.png)

## License
MIT

移植的 Pi 代码沿用其 [上游许可证](licenses/pi_agent_rust.LICENSE)，包括原有 rider 条款。

## 致谢
- 长期记忆计划参考 [hindsight](https://github.com/vectorize-io/hindsight)，以 Rust 实现简版 memory core；
- agent core基于 [pi_agent_rust](https://github.com/Dicklesworthstone/pi_agent_rust1) 二次开发
