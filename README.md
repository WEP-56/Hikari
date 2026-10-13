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
- `assets`：README 页面截图。
- `docs`：设计与接入文档。
- `examples`：本地参考实现。

工作区共用根目录的 `Cargo.lock` 和 `target`。


## visual
![page1](assets/page1.png)

## License
MIT

移植的 Pi 代码沿用其 [上游许可证](licenses/pi_agent_rust.LICENSE)，包括原有 rider 条款。

## 致谢
- 长期记忆计划参考 [hindsight](https://github.com/vectorize-io/hindsight)，以 Rust 实现简版 memory core；
- agent core基于 [pi_agent_rust](https://github.com/Dicklesworthstone/pi_agent_rust1) 二次开发
