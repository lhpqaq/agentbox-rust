# AgentBox Rust

**Rust 重写源码草案，尚未编译验证。** 当前交付环境没有 `rustc` / `cargo`；尚未安装工具链、解析 Cargo 依赖或执行 Rust 测试。本包不含可执行文件或已验证的 Cargo.lock。已实际运行的是 6 个前端 JavaScript 测试文件。详细状态见 [验证记录](VERIFICATION.md)。不要把这份源码包视为已经跑通的发行版。

A Rust rewrite of AgentBox's local persistent terminal workspace. The backend, CLI, broker and MCP bridge are Rust. The existing browser UI remains HTML/CSS/JavaScript, embedded into the executable at compile time. No Python runtime is required by the application.

## 实现范围

- 真实 Bash + PTY，多会话独立目录、变量和函数状态。
- 独立 broker 持有 Shell，HTTP 服务重启不主动终止 Shell。
- 私有控制 FD 记录 START / DONE / PROMPT，终端里打印“完成”不会完成任务。
- SQLite 命令与事件记录、重放游标、幂等 request_id，以及 broker 重启后的 lost 状态。
- 本机 HTTP API、原有 Web UI、CLI、8 个 stdio MCP 工具。
- 私有状态目录和令牌、loopback Host/Origin 防护、请求和输出上限。

这些是源码中实现的设计，不是全部已通过运行验证的声明。

## 平台和构建

目前仅针对 **Linux**。需要真实 PTY、允许本机 Unix/TCP socket、Bash 4+、`/bin/stty`。建议 Linux 5.3+ 以使用 pidfd；没有验证 macOS、原生 Windows、WSL2、SSH 或实际 MCP 宿主。其他系统可自行在 Linux 虚拟机中验证。

构建需要当前稳定版 Rust / Cargo、C 编译器及常规 Linux 开发工具。SQLite 通过 rusqlite 的 bundled 功能编译。首次 Cargo 构建需要访问 crates.io。使用自己认可的官方工具链安装方式，本包不会自动下载安装工具。

```sh
cd agentbox-rust
cargo fmt
cargo build --release
./target/release/agentbox --help
./scripts/verify.sh
```

本源码草案尚未由 rustfmt 格式化，未验证依赖解析、编译、测试或 clippy；首次在获准环境执行上面的步骤可能需要修正问题。构建成功后保存生成的 Cargo.lock，后续使用 `cargo build --release --locked` 固定依赖。

## 启动

```sh
./target/release/agentbox serve
```

打开 `http://127.0.0.1:8765`。另一个终端读取 token，再粘贴到解锁表单：

```sh
./target/release/agentbox token
```

**token 等同于该本机账户的终端权限。不要贴到聊天、公开日志、Git 仓库或 URL。**

默认状态目录：`AGENTBOX_HOME`，未设置时为 `~/.local/state/agentbox-rust`。使用新的目录，不会自动读取或迁移 Python 版 `~/.local/state/agentbox` 的数据库。状态目录必须由当前用户拥有、权限为 0700，路径应足够短以容纳 Unix socket；不要指定整个 HOME 或 /tmp。

```sh
./target/release/agentbox serve --home "$HOME/.local/state/agentbox-rust" --port 8766
```

`--home` 和 `--port` 是全局参数，可放在子命令前后。CLI / MCP 通过 `--home` 直接访问 broker，不依赖 HTTP 端口；`stop` 关闭 HTTP 时需要正确 `--port`。

## 持久状态示例

```sh
AB=./target/release/agentbox
SESSION_ID=$($AB create work --cwd "$HOME")
$AB run "$SESSION_ID" 'export DEMO_VALUE=hello; cd /tmp; pwd' --request-id demo:1
$AB run "$SESSION_ID" 'printf "%s\n" "$DEMO_VALUE"' --request-id demo:2
$AB history "$SESSION_ID"
$AB logs "$SESSION_ID" --follow
```

命令用引号包住，避免调用方 Shell 提前展开变量。`run` 输出 JSON，包括命令 ID、退出状态和最多 1 MiB 的输出尾部。`run --no-wait` 只提交并返回记录；`wait SESSION_ID COMMAND_ID` 仅观察原命令。等待超时不会中断程序，也不会重发。命令失败、未知状态或等待未完成时 CLI 返回非零退出码。

`request_id` 为每个会话内的 1–128 个 ASCII 字母、数字、`.`、`_`、`:`、`-`。同一键和相同完整文本返回原记录，不重复提交；不同文本产生冲突。幂等键不保证外部副作用恰好发生一次，也不提供回滚。

## 交互输入和主动中断

```sh
$AB run "$SESSION_ID" 'read -r answer; printf "answer=%s\n" "$answer"' --no-wait
$AB input "$SESSION_ID" hello --newline
$AB interrupt "$SESSION_ID"
$AB close "$SESSION_ID"
```

本版采用保守的边界：**一旦发送非空原始输入，该会话继续支持交互和回放，但不再接受后续结构化 run；请新建会话。** 原始输入可能有未被程序消费的剩余字节，因此不能安全地假设 Shell 已重新可用。显式 interrupt 不会单独触发这个限制。修改 PROMPT_COMMAND 等跟踪设置，也可能把会话永久标成 interactive；DONE 可以完成原命令，但不会冒险允许新命令。

自动终端设备应答已禁用，以免历史输出触发输入。某些依赖光标位置查询的全屏程序可能受影响。不要把这个版本当作已验证的完整终端模拟器替代品。

## 生命周期

```sh
# 停止 HTTP，保留 broker 和 Shell
$AB stop
# 重新启动 HTTP，连接同一个 broker
$AB serve
# 明确停止 broker 以及它能清理的会话进程
$AB stop --shutdown-sessions
# 只启动后台 broker，不提供 Web 服务
$AB start
```

HTTP 进程的 Ctrl-C 只关闭 HTTP。关闭浏览器、CLI 或 MCP 不会结束 broker。机器重启或 broker 崩溃后不会恢复进程状态；元数据和历史仍在，未确认命令会标记 lost。正常清理会尝试结束 Linux Unix-session 内的进程并回收 Shell；刻意 setsid/daemonize 逃离的程序不在此范围。权限拒绝或清理失败必须按报告处理，不应假设所有后代都已结束。

## 接入编码 Agent

先执行 `agentbox start` 或 `agentbox serve`，然后让 MCP 宿主启动：

```json
{
  "mcpServers": {
    "agentbox": {
      "command": "/absolute/path/to/agentbox",
      "args": ["mcp", "--home", "/absolute/private/agentbox-rust"]
    }
  }
}
```

Codex 风格配置示例：

```toml
[mcp_servers.agentbox]
command = "/absolute/path/to/agentbox"
args = ["mcp", "--home", "/absolute/private/agentbox-rust"]
```

这是配置示例，不代表已与具体版本宿主联调。注册工具不等于授权任意命令，宿主仍须遵守用户权限和确认规则。详情见 [MCP](MCP.md)。

## 重要差异和文档

- [迁移和功能差异](MIGRATION.md)
- [HTTP API](API.md)
- [MCP 协议](MCP.md)
- [安全边界](SECURITY.md)
- [验证记录与本机检查](VERIFICATION.md)

AgentBox 不是容器、虚拟机或权限沙箱。执行权限等同 broker 的 OS 账户；cwd 不限制文件访问范围。终端内容和命令历史可能含秘密，令牌保护不等于日志脱敏。
