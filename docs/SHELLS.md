# 复用终端环境与 Zsh 状态

## 直接使用自己的 shell

```sh
agentbox attach --home "$PWD/.agentbox"
# 明确指定 Zsh，或需要 .zprofile/.zlogin 时使用登录模式
agentbox attach --shell /bin/zsh --login --home "$PWD/.agentbox"
```

新建 `attach` 默认选择发起终端的 `$SHELL`，支持 Bash 和 Zsh。正常加载用户
启动文件，因此主题、Oh My Zsh、补全和 ZLE 插件照常初始化。程序不修改你的
点文件，不覆盖提示符或键位，也不关闭原生 shell 的历史功能。

默认传递**当前客户端**的完整导出环境，而不是后台服务启动时的旧环境。
可以使用多次 `--env NAME=VALUE` 修改传入值；`--broker-env` 则明确选择后台服务
环境。启动文件仍可按自身逻辑修改这些值。`AGENTBOX_TOKEN` 不传入子 shell。
环境值只在启动过程中传递，不作为会话元数据写入数据库；启动脚本或用户命令
如果主动打印这些值，打印结果仍属于终端输出。

`--no-rc` 跳过用户配置，用于排查启动问题；Zsh 的系统 `zshenv` 仍可能执行。
`--login` 按 shell 的登录规则加载配置。Zsh 的顺序参见
[官方启动文件文档](https://zsh.sourceforge.io/Doc/Release/Files.html)。

## 带入当前 Zsh 的临时定义

普通子进程只能继承导出的环境变量。当前终端临时定义的普通变量、数组、函数
和别名，不会因为启动了新的 Zsh 自动复制。可选集成专门处理这部分：

```zsh
eval "$(agentbox shell-init zsh)"

PROJECT_NOTE='这里包含空格和中文'
typeset -a TARGETS=(api worker)
my_status() { print -r -- "$PROJECT_NOTE"; }
alias ll='ls -lah'

agentbox-here --var PROJECT_NOTE --var TARGETS --function my_status \
  --option autocd -- --home "$PWD/.agentbox"
```

这条 `eval` 定义当前 shell 的 `agentbox-here` 函数，并记录生成它的可执行文件路径；即使用绝对路径调用未安装的构建产物，也无需额外修改 PATH。若希望以后自动提供此函数，
可以自行把它加到 `.zshrc`，AgentBox 不会自动改写配置。

- 默认复制当前普通别名、全局别名和后缀别名；`--no-aliases` 可关闭。
- `--var NAME` 复制指定的全局普通变量、数组或关联数组。特殊参数、函数局部变量
  和集成保留名称会拒绝，避免导入 shell 内部状态。
- `--export NAME` 在启动文件执行后恢复指定的已导出标量。适合 `PATH`、
  `VIRTUAL_ENV` 等需要优先于新 shell 启动配置的值；不会复制 AgentBox 控制变量。
- `--function NAME` 复制指定的函数定义。
- `--option NAME` 复制指定 Zsh 选项的当前开/关状态。名称采用规范小写形式，例如
  `autocd`、`extendedglob`。启动身份等不可复制选项会拒绝。
- `--` 后传给新建 `attach`，例如 `--home`、`--cwd`、`--login`。

新 shell 先加载配置和插件，再导入选定状态。客户端会立即连接，因此启动脚本
需要输入时可以直接回答，不会因为等待输入而阻塞整个 broker。会话元数据
`shell.state_import_requested` 表示请求了导入，`shell.state_imported` 只有在导入
真正完成后才会变成 `true`；`create` 返回会话 ID 不等于配置已加载完成。导入失败
会关闭新会话，`attach` 返回错误，可通过保留输出查原因。

临时定义可以覆盖启动文件中的
同名定义。状态通过认证本地 socket 传入，临时文件只允许本人读取，读取/导入后
清理，源代码和变量值不写入会话元数据。`--state-file` 是底层入口，接受本人拥有、
权限私密的 UTF-8 Zsh 源文件，最多 128 KiB；它会执行文件中的代码，应只传入
自己生成或明确授权的文件。导入配置使用临时 ZDOTDIR 转发文件，用户启动文件
执行时会恢复真实 ZDOTDIR，支持用户 `.zshenv` 修改 ZDOTDIR 的情况。

这会创建一个新持久 shell，不会把原 shell 的进程、任务、打开的文件描述符、
完整命令历史或任意插件内部运行状态搬走。已有插件按正常启动流程重新加载；
需要复制的临时定义按名称选择。原终端保持可用。

## 重连保留的是同一个进程

进入共享 shell 后，修改目录、普通变量、函数、选项和后台任务，均发生在持久的
shell 进程里。`Ctrl-]` 只断开客户端；随后：

```sh
agentbox attach SESSION_ID --home "$PWD/.agentbox"
```

继续连接相同 PID，而不是创建另一个 shell。重新连接不会重新加载 `.zshrc`，
也不会把新客户端的环境强行覆盖进去。连接已有会话时不能附带 shell 或环境
创建参数。`exit`、关闭会话、停止 broker 或机器重启会终止这份运行状态。

原生 shell 使用 `input` / `read`：

```sh
agentbox input SESSION_ID 'pwd; print -r -- "$PROJECT_NOTE"' --newline --home "$PWD/.agentbox"
agentbox read SESSION_ID --home "$PWD/.agentbox"
```

`run` / `wait` 的结构化命令协议继续使用 `create` 默认的 managed Bash。
原生会话会明确拒绝结构化 `run`，避免向 ZLE、SSH 或前台程序误插入控制脚本。
原生会话的 `cwd` 元数据只代表初始目录；进入 SSH 后尤其应在共享终端运行 `pwd`
判断当前远端位置。

## 升级与排查

旧 broker 不支持新 shell 功能时，客户端会在创建之前拒绝，现有会话不变。
可使用新的 `--home` 先体验，待旧任务结束后再计划停止和重启旧 broker。

`doctor` 会同时报告 managed Bash 与默认原生 shell 的解析结果。Ubuntu 的系统
Zsh 配置可能在用户 `.zshrc` 之前调用补全初始化；若它提示检查目录权限，应检查
该配置和 `compaudit` 输出，而不是把等待用户回答误认为 AgentBox 卡死。

测试入口：`scripts/test-shell.py`、`scripts/test-shell-state.py`，均使用临时目录和
独立 broker，不会加载或修改你自己的点文件，不会干扰现有共享终端。


实际插件验收还可运行 `python3 scripts/test-zsh-plugins.py`。默认使用本机
`~/.oh-my-zsh` 和 Homebrew 的插件目录，也可通过 `AGENTBOX_TEST_OMZ`、
`AGENTBOX_TEST_AUTOSUGGEST`、`AGENTBOX_TEST_HIGHLIGHT` 指向已安装的插件。
测试用临时 HOME、缓存和补全文件，关闭框架自动更新，不修改原插件或个人配置。
