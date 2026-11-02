# phi-agent-tui

[![Crates.io](https://img.shields.io/crates/v/phi-agent-tui.svg)](https://crates.io/crates/phi-agent-tui)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

面向 LLM agent 产品的可复用终端应用壳，基于 ratatui + crossterm 与 phi 家族的
agent 运行时。它把「人跟 agent 在终端里对话」的整个应用框架交给产品：事件循环与
输入泵、转录翻译与渲染、语义主题、审批/问询交互机制、会话 picker 与 resume 骨架，
以及 `ShellSpec` / `ShellHandles` 接线——而所有产品决策都留在产品那一侧。

**这里不放任何产品策略。** 命令语义、审批分级表、风险文案、数据目录布局、品牌
措辞、更新/提示分发，全部经 `ShellSpec` 数据或 `ShellHandles` fn 槽进入；默认值
复刻参考产品（phimint）的口吻，未填的 spec 也能跑。产品自己组装
`phi_agent::PhiAgent` 交进来——壳从不建造 agent。

## 里面有什么

| 模块 | 能力 |
|---|---|
| `run` | 事件循环：输入泵、事件排空、turn 提交、合成 turn 注入、更新/提示/标题钩子、尺寸与 raw-mode 自愈 |
| `shell` | 接线：`ShellSpec`（数据）+ `ShellHandles`（活对象）、措辞槽（`CopyTable` / `ApprovalCopy`）、`Keymap`、`CommandSpec`、`SessionIo`、面板/picker 钩子 |
| `app` | 会话状态：转录提交、流式尾部、composer + 排队 steering、提交纪律（Esc/Enter 对偶）、状态面 |
| `render` | 帧合成与命中测试协议（鼠标 → 语义命中）、审批弹窗/picker/任务面板帧、状态栏 |
| `theme` | 语义 `Theme`（类名 token 渲染期解析）+ 品牌行 |
| `approval` | 审批状态机：队列、弹窗、超时、Esc/Enter 纪律、两键同意门——只有机制；分级表是产品策略 |
| `interaction` | 问询/同意词汇：`AskItem` / `AskAnswer` / `AskOption`、picker 队列、`LiveInteractionChannel` |
| `picker` | 深 picker 骨架（行、过滤、resume/reopen 消费者） |
| `task_panel` | 深多任务面板骨架（子 agent + 后台 shell 任务） |
| `bg_wake` / `child_results` | 合成 turn 投递政策：turn 中不注、攒批、静默窗、只报一次 |
| `frame_log` / `exit_dump` | 翻页簿三件套（frames / perf / exit dump），事后排障 |
| `term_guard` / `tips` | Unix termios 自愈防外部破坏；轮换提示带机制（内容是产品数据） |

## 面板三档

产品按三个深度扩展 UI（每一档在 phimint 里都有活客户）：

- **浅档——事件合成。** 产品保留自己的面板，向壳注入合成 turn（点击答案、
  后台任务唤醒、子代报告批）。
- **中档——布局槽位。** 预留带（提示条、状态带面），产品每 tick 填数据。
- **深档——视图模式。** 壳管焦点与滚动切换，产品注册内容（`task_panel`、picker）。

## 用法

```toml
[dependencies]
phi-agent-tui = "0.1"
```

```rust
use phi_agent_tui::shell::{ShellSpec, ShellHandles, CopyTable, Keymap, Theme};
use phi_agent_tui::theme::ColorScheme;

// 数据侧——每个槽都写明默认值。`CopyTable::default()`、`Keymap::default()`、
// `Theme::for_scheme(..)` 携带参考产品（phimint）的口吻：只覆写你的产品措辞
// 不同的部分。其余槽（wordmark、title_gen、update、tips、notices、skills、
// tick、on_submit、on_reopen、exit_dump……）都是可选 fn 槽，`None` = 关。
let spec = ShellSpec {
    data_dir: home.join(".myagent"),
    workspace: std::env::current_dir()?,
    name: "myagent",
    version: env!("CARGO_PKG_VERSION").into(),
    theme: Theme::for_scheme(ColorScheme::Dark),
    copy: CopyTable::default(),
    keymap: Keymap::default(),
    commands: my_commands(),   // Vec<CommandSpec>——产品语义
    session_io: None,          // 非会话产品：/resume 保持关闭
    // ...
};

// 活对象侧——产品自己组装 agent 交进来。
let handles = ShellHandles {
    session_state, bg_registry, approval_rx,
    approval_mode, log_sinks, session_boot, session_end,
};

run(spec, agent, handles).await?;
```

参考消费者是 **phimint**（家族的编码 agent）——它的 `src/main.rs` +
`src/commands.rs` 就是一套生产级完整接线。

## 设计要点

- **机制 vs. 措辞。** 机制（事件循环、状态机、颜色、布局拼接）归壳；措辞与策略
  归产品。风险的*颜色*是壳的，风险的*字*是产品的；选项标签带按键和弦，因为
  按键→决策的映射是产品语义。
- **合成 turn 时序不变式。** 合成 turn 仅在无活动 turn 时注入，并落成一个真正的
  user turn——绝不注进进行中的 turn（turn 中注入会静默污染活动 turn 的上下文）。
  声明在 `shell::Synthetic`，由参考产品的 PTY 冒烟门在屏幕级钉住。
- **钩子降级，不崩产品。** 产品 fn 槽都在守卫后运行：panic 的钩子回退并在翻页簿
  留痕，而不是杀死事件循环。
- **fn 字段，不开 trait。** 接线缝形是 `Arc<dyn Fn…>` 槽——家族既定缝形，没有
  要实现的插件 trait。

## 家族

- [phi-agent](https://crates.io/crates/phi-agent) — 本壳包装的 agent 运行时
- [phi-tui](https://crates.io/crates/phi-tui) — 聊天式 TUI 组件（纯控件，零 agent 语义）
- [phi-kernel-tools](https://crates.io/crates/phi-kernel-tools) — 内核工具（文件 / shell / 后台任务）
- [code-intel](https://crates.io/crates/code-intel) — 语言智能（LSP、repo map、ripgrep 内核）
- [phimint](https://crates.io/crates/phimint) — 建在本壳上的编码 agent

## 许可证

MIT — 见 [LICENSE](LICENSE)。
