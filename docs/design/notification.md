# visp 终端通知功能设计

## 1. 目标

当 visp 的 agent 处于"等用户"状态而用户不在看终端时，通过**终端桌面通知**提醒用户返回：

- 一轮任务执行完毕（等待下一次输入）
- agent 需要用户介入（工具审批、agent 向用户提问）

设计哲学：visp 是通用软件，只按**标准终端通知协议**说话，不针对任何复用器做适配或绕行。复用器是否透传（tmux 的 allow-passthrough、rmux 的透传实现）是复用器自己的职责。

### 支持的通知协议（按能力排序）

| 协议 | 语法形态 | 能力 | 已核实支持方 |
|---|---|---|---|
| kitty 通知协议 (OSC 99) | `OSC 99 ; 元数据 ; 正文 ST` | 标题+正文+扩展（点击动作等，v1 不用） | kitty |
| OSC 9 (iTerm2) | `OSC 9 ; 正文 BEL/ST` | 仅正文（标题取窗口标题） | ghostty、iTerm2 |
| OSC 777 (urxvt) | `OSC 777 ; notify ; 标题 ; 正文 ST` | 标题+正文 | WezTerm（官方文档已核实）、urxvt 系；foot/konsole 待核实 |
| BEL (`0x07`) | 单字节控制字符 | 无文本负载（仅响铃/提示，标题与正文均无） | 兜底：未识别终端（含 rmux / tmux 等复用器），见下方「实现偏离说明」 |

已核实的 kitty OSC 99 语法要点（官方规范，编码器需遵守）：

- 形态 `OSC 99 ; metadata ; payload ST`，两个分号必须存在；metadata 为 `key=value` 冒号分隔
- 标题/正文分块发送：`p=title`（`d=0` 未完）→ `p=body`（`d=1` 结束）；最简形式 `\x1b]99;;Hello world\x1b\\`
- payload 单块 ≤ 2048 字节（编码前）→ 超长正文需截断
- payload 须为 safe-UTF-8（无控制字符）或 base64（`e=1`）→ 编码器必须清洗控制字符
- `f`（应用名）/ `t`（类型）为 base64 值，供用户侧过滤 → visp 应带 `f=visp`

### 实现偏离说明（2026-09-18）：未识别终端回退 BEL

**原决策**：未知终端 → 默认盲发 OSC 9。理由是支持 OSC 9 的终端照常弹通知，不支持的终端会静默忽略该序列、无副作用（alacritty、Terminal.app 等已确认忽略未知 OSC）。

**为何修订**：用户实际链路为 Ghostty → rmux → visp。rmux 的 `osc_notification` 为空实现（拦截并丢弃 OSC 9/777/99），而 rmux 不在已知终端映射表内 → 命中「盲发 OSC 9」→ 序列被复用器吃掉 → **用户完全收不到任何提示**。原决策"无副作用"的前提在复用器场景下等价于"无任何效果"。

**新决策**：未识别终端（含 rmux / tmux 等复用器）统一回退 **BEL（`0x07`）** 兜底 —— 保住响铃/🔔 提示，代价是放弃标题+正文横幅。

**已知取舍（明示接受）**：
- 这是**事实上的复用器适配**，与本文档开头的设计哲学（"不针对任何复用器做适配或绕行"）相悖。可接受的理由：该兜底是**通用的「未识别 → BEL」**，代码中不存在针对 rmux 的识别或特判；且配置项 `protocol` 可强制覆盖探测结果，行为对用户可见、可改。
- BEL 只是响铃/提示音：是否弹桌面通知、是否有横幅，取决于用户终端与系统设置，visp 无法控制。
- 未识别终端中本就支持 OSC 9 的（如 alacritty）会因此失去文本横幅，只剩响铃 —— 这是本次偏离的主要代价。

**影响面**：§1 协议表（新增 BEL 行）、§2.1 探测器映射表兜底行、§4 边界表（SSH / 复用器 / 未识别终端三行）、§6 验收标准（新增 BEL 项）、`crates/visp-cli/src/notify.rs`（`Protocol::Bel`）、`docs/todo/TODO.md`「已知限制」。

**后续方案**：系统通知 fallback 后端（§7）；kitty OSC 99 能力探测（`a=q`）以校准环境变量判定。

## 2. 模块划分

| 模块 | 职责 | 变更性质 |
|---|---|---|
| visp-cli（新模块 `notify/`） | 通知引擎：协议探测、序列编码、开关与节流 | 新增 |
| visp-cli `event.rs` | 在 Done / UserQuery 两个事件处理点挂接通知调用；**会话过滤（仅根 session）在挂接点完成** | 小改 |
| visp-config | `DaemonConfig` 新增 `[notification]` 配置段 | 小改 |

不做新 crate：通知逻辑只被 visp-cli 消费，先以模块形式落地，未来 daemon 需要时再抽 crate。

### 2.1 notify 模块（visp-cli 内）

三个内部组件：

- **探测器（Detector）**：启动时运行一次。前置守卫：`stdout().is_tty()` 不满足（管道/重定向运行）时整模块禁用；通过后依据环境变量判定协议，产出单一选定协议
  - 已知终端映射表（依据已核实支持矩阵）：`KITTY_WINDOW_ID`/`TERM=xterm-kitty` → OSC 99；`TERM_PROGRAM=WezTerm` 等 777 系终端 → OSC 777；`TERM_PROGRAM=ghostty`/`iTerm.app` → OSC 9
  - 未识别终端（含 rmux / tmux 等复用器）→ **回退 BEL（`0x07`）兜底**：仅响铃/提示音，无文本横幅。（原定「未知终端盲发 OSC 9」已于 2026-09-18 修订，理由与取舍见 §1「实现偏离说明」）
  - 用户配置可强制覆盖探测结果
- **编码器（Encoder）**：按选定协议生成转义序列字节串。纯函数、无 IO，便于单测。正文/标题中的控制字符（ESC、BEL、C0/C1）按各协议规范转义
- **节流器**：事件分两类（Done / UserQuery），按类独立计时，同类最小间隔内（默认 3s）不重复发。UserQuery 连续多步审批的高频触发即被压制

会话过滤（仅根 session）不属 notify 内部组件，见 §2.2 挂接点。

通知文案（v1 固定，不做模板）：

- 标题：`visp`（OSC 9 无标题字段，仅发正文，标题由终端取窗口标题）
- Done 正文：固定文案「任务完成，等待输入」
- UserQuery 正文：`uq.message` 截断至 200 字符（审批场景 message 即具体工具说明）；控制字符由 Encoder 清洗兜底

写入方式：直接向 stdout 写入转义序列字节（crossterm 已是 visp-cli 直接依赖，Cargo.toml:19，无需新增）。OSC 序列由终端模拟器自身处理、不进入备用屏缓冲，ratatui 全屏模式下通知照常弹出。硬约束：

- 写入必须在主事件循环线程**内联同步**完成，禁止 spawn 异步任务——否则与 ratatui 渲染写 stdout 产生字节交错
- 仅 `write_all(bytes) + flush()`，序列以 BEL/ST 结尾、无尾随字节；禁止 `println!`（换行会破坏 TUI 布局）
- fire-and-forget 指「不等待、不处理写入结果」，而非异步写；tty 写入为非阻塞，几十字节无性能影响
- 通知路径任何失败静默降级为 debug 日志，不影响 TUI 主流程

### 2.2 挂接点（visp-cli/event.rs）

- **回合完成**：`Payload::Done` 处理分支（event.rs:1065）。⚠️ 必须挂在 `stale_done_expected` 提前 return 分支（紧随其后，主 session Cancel 产生的 Done）**之后**——否则用户取消任务也会弹「完成」通知
- **需要用户输入**：`Payload::UserQuery` 处理分支（event.rs:1006），读 `uq.session_id` 做根会话判别（proto 字段 visp.proto:187，实现中已启用）
- 挂接点先做会话过滤：`is_main = session_id.is_empty() || session_id == app.main_session_id`（与 Error/Done 分支同款判别；UserQuery 的 session_id 见 visp.proto:187），仅在 `is_main` 为真时调用 `notify::on_event(kind, body)`
- notify 内部只做 开关 → 节流 → 编码，不感知 session；子 agent 的完成与审批一律不通知（过滤在挂接点）

### 2.3 配置（visp-config）

`DaemonConfig` 新增 `notification` 段（serde 默认值兜底，老配置文件无需改动）：

| 字段 | 含义 | 默认值 |
|---|---|---|
| enabled | 总开关 | true |
| on_complete | 回合完成时通知 | true |
| on_user_input | 需要用户输入时通知 | true |
| protocol | 协议选择：`auto` / `osc9` / `osc777` / `kitty99` | auto |
| min_interval_secs | 节流间隔秒数 | 3 |

配置读取：visp-cli 启动时调用现有 `load_config()` 读取同一份 daemon.toml（CLI 目前只用 visp-config 的路径能力，经核验现无任何 load_config 调用——此为全新初始化步骤，加载点位于 main 启动早期、TUI 初始化之前，代价为一次 TOML 解析）。备选方案（daemon 经 gRPC 下发）引入协议变更，v1 不采用。

## 3. 核心数据流

```
[用户提交任务] → daemon agent loop 执行
     │
     ├─(完成)─→ gRPC Done ────┐
     │                        ├─→ visp-cli 事件循环
     └─(需输入)→ gRPC UserQuery┘        │
                                        ▼
                          notify::on_event(kind, body)
                                        │
                          根会话？→ 开关？→ 同类节流通过？
                                        │
                                        ▼
                          Encoder（启动时已由 Detector 选定协议）
                                        │
                                        ▼
                          crossterm 写 stdout → 终端弹桌面通知
```

## 4. 边界情况

| 场景 | 行为 |
|---|---|
| stdout 非 tty（管道/重定向运行） | is_tty 守卫：整模块禁用，零输出副作用 |
| SSH 远端运行（TERM 透传、TERM_PROGRAM 不透传） | TERM 命中映射（`xterm-kitty`/`xterm-ghostty`）走精确协议；未命中→回退 BEL 兜底（仅响铃），不依赖 TERM_PROGRAM —— SSH 下自洽，无需特殊处理 |
| 未识别终端（alacritty、Terminal.app 等不在映射表内） | 回退 BEL 兜底：响铃/提示音；不发 OSC，故无未知序列风险 |
| 协议选择与实际终端不符（探测/配置失误） | 静默无害（终端忽略未知 OSC），可改 protocol 强制修正；若落到 BEL 则仅响铃 |
| 复用器（tmux/rmux）拦截序列 | 不做任何绕行；且复用器通常不在映射表内 → 命中 BEL 兜底保住响铃（见 §1「实现偏离说明」）。OSC 是否透传由复用器自身负责 |
| 正文含 ESC/BEL/换行等控制字符 | Encoder 按协议规范转义或剥离 |
| 连续多个审批请求 | 节流器压制 |
| 子 agent tab 完成 | 不通知 |
| 配置文件无 [notification] 段 | serde 默认值，行为=开 |
| daemon.toml 解析失败 | 沿用 visp 现有配置加载的错误策略 |

## 5. 不做什么（v1）

- 不做系统通知后端（osascript/notify-rust）——纯协议通道；未来可作为"协议不可用时的 fallback"扩展
- 不做 kitty OSC 99 的高级特性（点击动作、关闭回调、图标、进度）
- 不做 kitty OSC 99 能力探测（`a=q` 查询等待响应）——v1 用环境变量判定
- 不做 tmux `DCS tmux;` 包裹（tmux 用户可自行开启 allow-passthrough）
- 不做终端聚焦检测、通知文案模板、点击跳转、通知历史

## 6. 验收标准

1. ghostty 直连环境，长任务完成后收到桌面通知（OSC 9 路径）
2. agent 请求工具审批时收到通知
3. kitty 环境下走 OSC 99，标题+正文正确显示
4. `protocol = "osc777"` 强制指定时按 777 编码
5. `[notification] enabled = false` 完全静默；on_complete / on_user_input 可独立开关
6. 子 agent 会话的完成与审批请求均不触发（挂接点统一会话过滤，notify 不感知 session）
7. 连续多个审批请求只发一条（节流）
8. 单测覆盖：Detector 映射表与 is_tty 守卫、三种 Encoder 的序列正确性（含控制字符转义）、节流器（按类独立计时）、会话过滤（挂接点层，见 event_tests）
9. stdout 重定向到文件时无任何转义序列泄漏（is_tty 守卫生效）
10. 主 session 取消（Cancel）不触发「完成」通知（stale_done_expected 路径避让生效）
11. 未识别终端（含 rmux / tmux 复用器）下，任务完成与审批请求仍可感知（BEL 响铃/🔔；§1「实现偏离说明」的偏离行为）

## 7. 未来扩展（不在本期）

- kitty OSC 99 能力探测（a=q 查询/响应），替代或校准环境变量探测
- 系统通知 fallback 后端（协议不可用或复用器拦截场景）
- tmux DCS 包裹、聚焦检测、通知文案带任务摘要
