# macOS 适配与验证

验证日期：2026-11-09。环境：macOS 15.1.1、Apple Silicon arm64、Rust/Cargo 1.93.0、系统 Bash 3.2.57。

本目录是原源码 ZIP 的独立副本，原压缩包未修改。本地已生成调试构建 `target/debug/agentbox`，仓库保留固定依赖版本的 `Cargo.lock`，不包含构建产物，未构建 release 版。

## 修改内容

- 允许在 macOS 上构建，保留 Linux 条件编译分支；修改后的 Linux 分支尚未重新构建验证。
- `src/platform.rs` 集中实现排他重命名、close-on-exec 管道及会话进程枚举；macOS 使用 `renamex_np`、`pipe`/`fcntl` 和 libproc。
- PTY 的指针签名及 ioctl 类型适配 Darwin；Bash 完成协议使用 `${BASHPID:-$$}`，支持系统 Bash 3.2。
- macOS 进程身份由 session ID 与启动时间构成，发信号前重新核对；不承诺 Linux pidfd 的原子身份绑定，也不覆盖刻意 setsid/daemonize 逃离的进程。
- 显式把 accept 得到的 Unix socket 连接设为阻塞，修复 Darwin 继承监听 socket 的非阻塞状态、大事件响应被提前断开的问题。
- 原始 Ctrl-C 测试使用 `dd` 读取单字节，验证关闭 ISIG 的程序能收到字节 3；不依赖 Bash 4 的 `read -N` 或 Bash 3 `read -n` 的终端行为。
- 执行 rustfmt 并修正 clippy 报告的警告。

## 实际验证

- `cargo check --all-targets --offline`：通过。
- `cargo fmt --check`：通过。
- `cargo clippy --all-targets --locked --offline -- -D warnings`：通过。
- Rust 默认测试：46 个单元测试及 4 个 Core 集成测试通过。
- 6 个 `web/test-*.mjs` 测试文件通过；这些是 JS/parser/mock 测试，不等于浏览器端验证。
- 2026-11-07 的真实 socket 集成测试通过：鉴权、64 KiB 输出、HTTP 重启后同一 broker/Shell 状态保留、read 输入恢复及 MCP stdio 工具发现/调用。
- 2026-11-09 的纯 CLI 场景全部通过，见下文。

默认检查的本地输出保存在 `macos-checks.txt`，该机器相关日志未提交到仓库。socket/CLI 测试需要允许本机 Unix/TCP socket 的执行环境；受限执行环境中的 EPERM 不是项目功能测试结果。

## 纯 CLI 场景

运行 `python3 scripts/test-macos-cli.py`，在 `/private/tmp` 创建独立私有状态目录，只启动 broker，不启动 HTTP 或浏览器，结束后关闭测试 broker。

1. 不同 CLI 进程之间保留 cwd、导出变量和函数。
2. 相同 request_id 返回原命令，不重复增加计数；不同命令文本冲突被拒绝。
3. 命令退出码 7 被保存，CLI 返回非零状态。
4. 128 KiB 输出完整返回，不因 Unix socket 响应较大而断开。
5. 零超时 wait 只观察；interrupt 中断 sleep，Shell 后续仍可执行命令。
6. input 能回答 read，之后该会话按原设计拒绝新的结构化 run。
7. broker 正常关闭/重启后保留历史和去重记录，不复活已关闭的 Shell。

另外验证过关闭 HTTP 服务后 CLI 仍可读取原 Shell 的环境变量。网页浏览曾被 Host/Origin 防护拒绝，未完成网页端人工验收；未为此放宽防护。

## 使用

参见根目录 README 的 CLI 命令。CLI 不需要浏览器、HTTP 服务或手动输入 token。项目以当前用户权限执行命令，不提供权限隔离。

`SOURCE-MANIFEST.sha256` 是原包内容清单；已移到 `docs/UPSTREAM-SOURCE-MANIFEST.sha256` 保留原始依据。不要用它校验修改后的副本。

## 2026-11-10：原生共享终端 attach

新增 `attach [SESSION_ID] [--cwd DIR] [--no-replay]`。无 ID 时创建 Shell 并启用 readline；有 ID 时接入原 Shell，不注入初始化命令。CLI 使用本地 raw tty，转发键盘并同步窗口尺寸；Ctrl-] 仅断开连接，exit 关闭 Shell。另一操作方仍使用同一 home 的 logs/input 接口，双方轮流输入。

`python3 scripts/test-attach.py` 已用真实 PTY 和独立 broker 通过：本地键盘执行、另一 CLI 查看/操作同一 Shell 与环境变量、尺寸同步、Ctrl-C、终端查询过滤、断开恢复本地 termios、重新连接及 exit 关闭。macOS raw/canonical 切换会由内核设置 PENDIN，恢复检查仅忽略该内核管理的标志。两个新增单元测试覆盖分片 Unicode 输入、脱离快捷键和输出控制序列过滤。

本功能不是完整屏幕状态同步器；历史有保留上限，--no-replay 不补画已有屏幕。OSC/DCS、设备查询和未知 CSI 会过滤，依赖这些功能的全屏程序可能受限。首次非空原始输入后，仍按原设计拒绝结构化 run，操作方需使用 input/logs。

### Bash 3.2 连续命令提示符修复

真实使用发现，在结构化 run 的 source 脚本中执行 `set -o emacs` 会使系统 Bash 3.2 后续不再显示提示符，虽然命令仍能执行。已在独立 PTY 中复现；将新建共享 Shell 的 readline 初始化改为顶层 input，避免 source 路径。已有受影响会话可在顶层执行 `set +o emacs; set -o emacs` 恢复，保留原 Shell 状态。

原生 attach 回归测试新增“每一条连续命令后必须出现新提示符”的断言；该断言在旧二进制上复现失败，在修复后通过。之前的仅输出匹配不能证明交互提示符正常。

## 2026-11-12：共享会话读取与大输出修复

- `attach SESSION --read-only` 只旁观，不转发输入或修改共享窗口尺寸。
- 会话参数支持完整 ID、唯一名称或至少六字符的唯一 ID 前缀；歧义直接报错，关闭的会话仍参与歧义检查。
- `read SESSION --after CURSOR --max-bytes N` 返回有大小上限的 JSON 终端记录，支持续读及保留历史缺失提示。它不会执行命令，不是终端屏幕重建器。
- `stop --shutdown-sessions --broker-only` 不再尝试关闭 HTTP。
- 修复 macOS 上服务端关闭 Unix socket 后，设置 SO_RCVTIMEO 返回 EINVAL、但接收缓冲区仍有完整响应的竞态。客户端改用非阻塞读取和绝对截止时间 poll；不重发请求。
- 等待响应区分 `timeout` 与 `observation_incomplete`，后者保留 `observation_error`；已知命令完成状态不会丢失。
- 为事件保留统计增加覆盖索引，避免重复读取包含大段输出的数据库页。

`python3 scripts/test-bulk-output.py` 在独立 broker 中验证 9 MiB 输出、1 MiB 客户端尾部上限、8 MiB 服务端保留上限和 read 的缺失提示。本机 debug 构建一次实测约 14 秒，不代表其他机器或 release 性能。新增 socket 测试覆盖发完响应立即关闭、响应不完整和截止时间；全部为临时状态目录，不操作用户现有会话。

### 输出未排空时的关闭竞态

macOS CI 曾在立即输入/关闭场景报告 Shell 未在清理截止时间内被回收。本机复现后，发现等待子进程期间不读 PTY 会使终端尾部输出阻碍退出。清理循环现在持续进行有上限的 PTY 读取，同时保持原有退出时限和进程身份检查。修复后原场景连续 100 次通过；新增 16 次输出中关闭的回归，使用 waitpid/ECHILD 确认子进程已由拥有者回收。


### 0.4.0 原生终端与状态迁移（2026-11-13）

无 ID 的 `attach` 现在默认使用当前终端的 `$SHELL`（Bash/Zsh），加载正常配置、
插件和客户端导出环境。此前干净 Bash 的行为可通过 `--managed` 使用；已有会话
仍不注入初始化指令。`shell-init zsh` 提供可选状态迁移函数，详见
[终端环境与 Zsh 状态](SHELLS.md)。本机实测了 Oh My Zsh、自动建议接受、语法高亮、
变量/函数/别名迁移、Python 虚拟环境恢复和 pending 状态下的交互启动。

发布 ZIP 在含空格及单引号的临时路径中通过原生 shell、状态迁移和真实插件测试；
源码 ZIP 独立离线构建后通过 CLI 与状态迁移测试。具体提交与 CI 记录以
[当前验证记录](VERIFICATION.md) 为准，以上旧版测试保留作历史记录。
