# visp 工作计划：跨平台文件监听模块 `visp-fs` 与 AGENTS.md 向上边界

- 日期：2026-09-25
- 依据设计：`docs/design/2026-09-25-fs-watcher-and-agents-bound-design.md`（**v5**，四轮独立评审通过后定稿）
- 质量门：`cargo test`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt -- --check`

## 概述

按设计 v5 实施四件事：(1) 新建跨平台监听模块 `visp-fs`（结果级契约 G1–G5 + 目录/文件双监听 + 幂等重挂 + 细则 1/2a/2b/3/4/5 + 事件归一化 + 缺目录降级 + 生命周期）；(2) 迁移 `visp-daemon/src/watch.rs`；(3) 迁移 `visp-codegraph/src/watcher.rs`；(4) `AGENTS.md` 向上查找止于 git 根，并把祖先链解析抽为 `visp-config` 单一函数供加载器与监听计划共用。

**关键落位决策（实施前约定）**：

- `visp-fs` **不得**依赖 `visp-core` / `visp-config` / `visp-daemon` / `visp-codegraph`；契约只含「路径 + 事件类型」。
- 纯逻辑（目标声明、匹配/排除、归一化、降级链）与运行时（notify 接线）**分文件**，前者单测、后者集成测试。
- 集成测试**后端无关**（有限时间轮询的最终收敛断言，无 kqueue 特化断言），以便 Linux（CI）与 macOS（开发机）双方言跑同一组用例。
- 迁移后 `visp-daemon`、`visp-codegraph` 的 Cargo.toml **移除对 `notify` 的直接依赖**。

### 设计中的关键陷阱（实施时不得偏离）

1. **§4.3 细则 2a 是核心修复的必需主路径**（非防御性）：Remove 命中**仍存在**的路径 ⇒ 原子替换 ⇒ **立即重挂 + 投递「创建」**。kqueue 的目录 diff 找的是「不在 watch 集合里的条目」，被覆盖的同名路径仍在集合中，**不会补发 Create**。
2. **验收用例必须让文件「在监听启动之前已存在」**：kqueue 会对目录 diff 发现的新文件自动补挂，否则缺陷实现也会通过（虚假覆盖）。
3. **幂等重挂**：对 Create/Rename 涉及路径**无条件 `unwatch + watch`**，**不得**「路径在集合就跳过」。
4. **目录 Modify 抑制范围**：仅**模块直接挂载的目录**；递归子树目录**不保证抑制**（用模块自身目录集合判定，**不得**用 `path.is_dir()`）。
5. **事件形态归属**：kqueue `Any`（单路径旧）；inotify `From`/`To`/**`Both`**；Windows 仅 `From`/`To`（无 `Both`）。

---

## 步骤 1：`visp-fs` 纯逻辑核心（新 crate）

### 1a：crate 骨架 + 目标声明与匹配/排除规则

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 两种模式表达 | 目标声明可表达「递归」与「直接子级（文件与目录）」 |
| 2 | 包含-文件名 | 文件名精确匹配命中/未命中 |
| 3 | 包含-扩展名双写法 | 扩展名「含点」（`.md`）与「不含点」（`md`）两种书写均可匹配（codegraph 现状两种都用） |
| 4 | 包含-前缀/子树 | 前缀匹配与子树匹配 |
| 5 | 排除-路径组件级 | 排除路径组件（如 `node_modules`、`.git`）命中子路径时被排除 |
| 6 | 组合边界 | 同时给包含与排除时，排除优先；大小写按精确匹配（不敏感平台列为不承诺） |

#### 🟢 绿 — 实现
新建 `visp-fs` crate（`Cargo.toml` + `src/lib.rs`），实现目标声明（根、模式、过滤规则）与匹配/排除判定。**不引入新依赖**（除既有 `notify`、`tokio` 按需）。判定为**纯函数**，无 IO。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-fs` / `cargo clippy -p visp-fs --all-targets -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无（新 crate）。

#### 📦 提交
`git commit -m "feat(fs): visp-fs crate with watch target spec and match/exclude rules"`

### 1b：事件类型归一化（纯函数）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 新建 → 创建 | 底层 create → `创建` |
| 2 | 内容写入 → 修改 | write / truncate / O_TRUNC → `修改` |
| 3 | 元数据变更 → 修改 | chmod / mtime（如 touch）→ `修改`（保守） |
| 4 | 删除 → 删除 | remove → `删除` |
| 5 | kqueue `Any` | 单事件、仅旧路径 → `删除`(旧) |
| 6 | inotify `From`/`To` | 两事件 → `删除`(旧) + `创建`(新) |
| 7 | inotify `Both` | 单事件含旧+新 → 拆为 `删除`(旧) + `创建`(新) |
| 8 | Windows `From`/`To` | 两事件 → `删除`(旧) + `创建`(新)（**无 `Both`**，不得为它造用例） |
| 9 | 目录条目级 Modify（在已直接挂载目录集合中） | → **抑制、不下发** |
| 10 | 目录条目级 Modify（不在集合中，如递归子树目录） | → **不抑制**，作为 `修改` 下发（§4.6 范围限定） |
| 11 | 临时文件不做特殊抑制 | 命中过滤的 tmp 事件按普通事件处理 |

#### 🟢 绿 — 实现
实现归一化纯函数：输入底层事件 → 输出零或多个 `(路径, 类型)`；目录 Modify 抑制以**模块自身维护的「已直接挂载目录」集合**判定（无 IO、无 TOCTOU）。

#### 🧪 测试 → 🔍 类型检查
同 1a

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "feat(fs): event normalization for three backends with scoped dir-Modify suppression"`

### 1c：缺目录降级链解析（纯函数）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 根存在 | 直接监听根 |
| 2 | 根不存在 | 解析出「最近存在祖先 + 前缀过滤」 |
| 3 | 多级缺失 | 逐级向上找到最近存在祖先 |
| 4 | 无任何可挂载祖先 | 返回「不可挂载」终止语义（供细则 5 显式上报） |
| 5 | 祖先链解析可复用 | 输出目录列表，供加载器/监听计划共用（步骤 5） |

#### 🟢 绿 — 实现
实现降级链解析纯函数；输出既可驱动监听（最近存在祖先 + 前缀），也可作为祖先链供复用。

#### 🧪 测试 → 🔍 类型检查
同 1a

#### ♻️ 重构
若与 1a 的匹配逻辑重叠，收敛为单一判定入口。

#### 📦 提交
`git commit -m "feat(fs): missing-dir degradation chain resolution"`

---

## 步骤 2：`visp-fs` 运行时

### 2a：watcher 运行时（notify 接线 + 目录/文件双监听 + 动态补挂）

#### 🔴 红 — 测试（集成，临时目录，后端无关）
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | **启动前已存在文件的原地覆写** | 文件先存在 → 启动监听 → 原地覆写 → 收到 `修改`（**核心修复项**；不得写成「启动后创建再覆写」） |
| 2 | 新建文件 → `创建` | 目录中出现新文件 |
| 3 | 删除文件 → `删除` | |
| 4 | 直接子级模式下的**子目录创建** | 因模式包含目录，子目录创建须上报（G5/daemon #6 依赖） |
| 5 | 动态补挂 | 目标目录不存在 → 运行中创建并写入 → 收到事件 + 一次重扫信号 |
| 6 | 递归目标下的内容改写 | 子树内既有文件覆写 → `修改` |
| 7 | 过滤生效 | 未命中包含规则 / 命中排除规则的事件不下发 |

#### 🟢 绿 — 实现
实现运行时：notify watcher 创建（多目标挂载）、直接子级目标**额外对每个匹配子文件挂文件级监听**、动态补挂、事件投递（不阻塞回调线程）、重扫信号。断言用有限时间轮询。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-fs` / `cargo clippy -p visp-fs --all-targets -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
把「挂载/卸载」收敛为单一路径，避免多处重复。

#### 📦 提交
`git commit -m "feat(fs): watcher runtime with dir+file dual watch and dynamic attach"`

### 2b：幂等重挂与细则 1 / 2a / 2b / 3 / 4 / 5

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | **原子替换后仍可感知（细则 2a 直接验收）** | 启动前已存在且已监听的文件 → tmp+rename 原子替换 → **再原地覆写** → 仍收到 `修改`（kqueue 下目录 diff 不会补发 Create，只能靠 2a 恢复） |
| 2 | 细则 1：`watch_not_found` 忽略 | 二次卸载不报错 |
| 3 | 幂等重挂 | 对已挂路径的 Create/Rename 无条件重挂，状态不重复、不丢失 |
| 4 | 细则 4：单文件级失败不污染 | 单文件挂载失败 → 忽略 + 不入集合 + warn；后续 Create 可恢复 |
| 5 | 细则 5：根/最近祖先级失败 | 无可权限/资源耗尽 → **显式上报降级**（不静默），调用方启动不受影响 |
| 6 | 细则 2b：乱序互斥（防御性，假体） | 「Create 先 / Remove 后」不拆掉刚挂的 watch |
| 7 | 细则 3：不依赖 Link 内部重挂 | 断言仅依赖模块自身重挂逻辑（可用假体或行为断言） |

#### 🟢 绿 — 实现
实现细则 1/2a/2b/3/4/5：幂等重挂；Remove 前存在性探测（2a）；乱序互斥（2b）；忽略 notify 的 Link 内部行为（3）；单文件级失败忽略（4）；根级失败显式降级上报 + 有界重试 + 无祖先时终止上报（5）。

#### 🧪 测试 → 🔍 类型检查
同 2a

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "feat(fs): idempotent remount with 2a recovery and explicit root-level degradation"`

### 2c：生命周期与监听集合收敛

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | stop 后不再投递 | `stop` 后无事件、监听释放 |
| 2 | 去重收敛 | 重复 create/delete 风暴下监听状态收敛、**无句柄泄漏**（以监听集合大小/挂载次数断言） |
| 3 | 接收端 drop 不 panic | 消费者接收端被 drop 后模块不 panic |
| 4 | 目录目标重挂成本约束 | 目录级重挂仅在目录事件时发生（断言不因文件事件触发目录重挂） |

#### 🟢 绿 — 实现
实现 `stop`、监听集合维护与释放、接收端 drop 容错、目录/文件重挂路径分离。

#### 🧪 测试 → 🔍 类型检查
同 2a

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "feat(fs): lifecycle stop and watch-set convergence"`

---

## 步骤 3：迁移两处 watcher（与步骤 5a 并行）

### 3a：`visp-daemon/src/watch.rs` 迁移到 `visp-fs`

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 领域分类保留 | 路径 → rules/skills/agents 的判定不变 |
| 2 | debounce 聚合保留 | 窗口重置式、按领域聚合不变 |
| 3 | **原地覆写产生事件**（新增显式断言） | 修正既有 `rapid_writes` 掩蔽的盲区（**启动前已存在**的文件） |
| 4 | 既有 8 个集成用例结构性改写后等价 | 写 AGENTS.md 触发 rules 重载 / debounce 合并 / 原子写收敛 / 跨领域聚合 / 无关事件过滤 / 缺目录补挂 / 风暴收敛 / stop 清理 |
| 5 | 移除直接 notify 依赖后编译通过 | Cargo.toml 依赖调整 |

#### 🟢 绿 — 实现
daemon watch 改为：声明既有 9 类目标 → 消费 `visp-fs` 事件 → 保留领域分类与 debounce；移除对 `notify` 的直接依赖。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-daemon` / `cargo clippy -p visp-daemon --all-targets -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
删除 daemon 侧已下沉的挂载/降级/去重代码。

#### 📦 提交
`git commit -m "refactor(daemon): migrate file watcher onto visp-fs"`

### 4a：`visp-codegraph/src/watcher.rs` 迁移到 `visp-fs`（可与 3a 并行：不同 crate）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 内容改写触发索引 | 子树内既有源文件原地覆写 → 索引更新 |
| 2 | 原子替换触发索引 | tmp+rename |
| 3 | **重命名语义** | 旧路径的 dangling `edges`/`imports`/`exports`/`files` 行被清理、新路径被正确索引 |
| 4 | exclude 目录内变更零触发 | `node_modules`/`.git` 内变更不触发 |
| 5 | 无关扩展名零触发 | 非目标扩展名事件不下发 |
| 6 | 忽略重扫信号 | codegraph **不响应** `visp-fs` 的重扫信号（设计 §4.7 决策） |
| 7 | 移除直接 notify 依赖后编译通过 | Cargo.toml 依赖调整 |

#### 🟢 绿 — 实现
codegraph watcher 改为：一个递归目标 + 包含（扩展名）+ 排除（exclude 目录）；消费事件按「删除(旧) → 删除索引条目；创建(新) → 重插」处理；忽略重扫信号。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-codegraph` / `cargo clippy -p visp-codegraph --all-targets -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
删除 codegraph 侧已下沉的 notify 使用。

#### 📦 提交
`git commit -m "refactor(codegraph): migrate indexer watcher onto visp-fs"`

### 5a：`visp-config` 祖先链抽取 + git 根边界（可与 3a/4a 并行：不同 crate）

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 止于 git 根 | git 根之上人为放置 AGENTS.md → 不被加载 |
| 2 | monorepo | 子目录项目加载至 git 根的各层 |
| 3 | `.git` 是文件（worktree） | 同样视为边界 |
| 4 | 非 git + `$HOME` 之下 | 止于 `$HOME`（**含**该层） |
| 5 | 非 git + `$HOME` 之外 | **不向上** |
| 6 | `$HOME` 不可解析 | **只加载项目层** |
| 7 | 祖先链函数可被复用 | 输出祖先目录列表，供监听计划使用 |
| 8 | 既有规则加载回归 | 其余 rules 加载用例不回归（含全局 AGENTS.md 通道不变） |

#### 🟢 绿 — 实现
`discover_agents_md` 加边界（git 根 → `$HOME` → 不向上 → `$HOME` 不可解析只项目层）；把「祖先链（目录列表）」抽为**单一函数**，加载器在其上查存在的 AGENTS.md。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-config` / `cargo clippy -p visp-config --all-targets -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
加载器与祖先链函数共用扫描顺序，避免两处漂移。

#### 📦 提交
`git commit -m "feat(config): bound AGENTS.md discovery at git root with shared ancestor-chain resolver"`

---

## 步骤 4：监听计划与加载器保持一致（依赖 3a + 5a）

### 5b：daemon 监听计划目标 #1 复用共享祖先链

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 监听计划 = 加载器祖先链 | 同一项目下，监听目标 #1 的目录集合与加载器解析出的祖先链一致（无「监听但不加载」或反之） |
| 2 | 止于 git 根 | 计划不含 git 根之上的目录 |
| 3 | 既有目标 #2–#9 回归 | 其余监听目标不受影响 |

#### 🟢 绿 — 实现
daemon 监听计划的 #1 改为调用 `visp-config` 的共享祖先链解析。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-daemon` / `cargo clippy -p visp-daemon --all-targets -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "refactor(daemon): use shared ancestor-chain resolver for AGENTS.md watch targets"`

---

## 步骤 5：端到端回归与后端无关验证

### 6a：workspace 全量质量门 + 后端验证 + 验收核对

#### 🔴 红 — 测试
| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | workspace 全量回归 | `cargo test --workspace` 全绿，既有测试零回归 |
| 2 | 静态检查全绿 | `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt -- --check` |
| 3 | 后端无关核对 | 确认 `visp-fs` 集成测试中**无 kqueue 特化断言**（可在 Linux/CI 与 macOS/开发机双方言跑） |
| 4 | 设计验收映射 | 对照设计 §7 的 18 条验收逐条标注「已被哪个子步骤的测试覆盖 / 需手工验收」 |
| 5 | daemon 冒烟（手工辅助） | 启动 daemon，**原地覆写** `AGENTS.md` → 观察 rules 热重载日志（本次缺陷的端到端确认） |

#### 🟢 绿 — 实现
修复回归暴露的问题（各自小提交）；无实现性新增。

#### 🧪 测试 → 🔍 类型检查
`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无。

#### 📦 提交
`git commit -m "chore: full regression and backend-agnostic verification for visp-fs migration"`

---

## Wave 并行策略

### Wave 1：`visp-fs` 纯逻辑（1 条串行链，同一新 crate）
任务 A: 1a → 1b → 1c

### Wave 2：`visp-fs` 运行时（1 条串行链，同 crate）
任务 A: 2a → 2b → 2c

### Wave 3：迁移与边界（**3 个并行任务**，不同 crate，互不重叠）
任务 A: 3a（`visp-daemon`）
任务 B: 4a（`visp-codegraph`）
任务 C: 5a（`visp-config`）

### Wave 4：一致性收口（1 个任务，依赖 3a + 5a）
任务 A: 5b（`visp-daemon`）

### Wave 5：收尾（1 个任务，依赖全部）
任务 A: 6a（workspace）

> **资源互斥**：`visp-daemon/src/watch.rs` 被 3a 与 5b 先后修改 → **Wave 3 的 3a 与 Wave 4 的 5b 必须串行**（已按波次保证）。

## 依赖关系总览

```
Wave1: 1a ─► 1b ─► 1c ─┐
Wave2: 2a ─► 2b ─► 2c ─┼─► (visp-fs 就绪)
                        │
Wave3: 3a (daemon) ◄────┤ (依赖 visp-fs)
       4a (codegraph) ◄─┤
       5a (config)      │ (不依赖 visp-fs)
                        │
Wave4: 5b (daemon) ◄────┘ (依赖 3a + 5a)
Wave5: 6a (全部)
```

## 测试覆盖汇总

| Wave | 并行数 | crate | 步骤 | 测试用例数 |
|---|---|---|---|---|
| 1 | 1（串行链） | visp-fs（新） | 1a；1b；1c | 6；11；5 |
| 2 | 1（串行链） | visp-fs（新） | 2a；2b；2c | 7；7；4 |
| 3 | 3（并行） | visp-daemon；visp-codegraph；visp-config | 3a；4a；5a | 5；7；8 |
| 4 | 1 | visp-daemon | 5b | 3 |
| 5 | 1 | workspace | 6a | 5 |
| **合计** | — | 4 个 crate（含 1 个新） | 10 子步骤 | **约 68** |

## 备注

1. **后端承载面不对称（必须在实施计划中体现）**：CI（`.github/workflows/rust.yml`）为 `ubuntu-latest` → inotify 有现成承载面；**macOS（kqueue，主交付后端）无 CI runner**（`release.yml` 的 macos runner 只 build 不 test），由**开发机**承担 kqueue 验证。
2. **集成测试禁止 kqueue 特化断言**：一律「有限时间轮询的最终收敛」，否则无法在 Linux/CI 与 macOS/开发机双方言跑同一组用例。
3. **验收 7.1.1/7.1.2 的用例前提**：文件必须**在监听启动之前已存在**——kqueue 会对目录 diff 发现的新文件自动补挂，否则缺陷实现也会通过（虚假覆盖）。
4. **细则 2a 不是防御性代码**：实施时若省略「Remove 命中仍存在路径 → 重挂」，**核心修复（编辑器原子保存）在 kqueue 上失效**。
5. **目录 Modify 抑制范围**：仅直接挂载目录；递归子树目录不保证（判定用模块自身集合，**不得**用 `path.is_dir()`）。
6. **事件形态**：kqueue `Any`（单路径旧）；inotify `From`/`To`/**`Both`**；Windows 仅 `From`/`To`——为 Windows 造 `Both` 用例是错的（该事件永不出现）。
7. **迁移非等价性**：codegraph 的 rename 语义由「归为 Modified」变为「删除 + 创建」，顺带清理 dangling `edges`/`imports`/`exports`/`files` 行（**不是**符号残留）；须专项回归。
8. **codegraph 无重扫兜底**：kqueue 多文件同窗口创建漏报对纯增量消费者存在已知残余风险（兜底为手动重建索引），设计已文档化，实施时不为它新增机制。
9. **移依赖**：3a/4a 完成后，`visp-daemon`、`visp-codegraph` 的 Cargo.toml 移除 `notify.workspace = true`（确认无其它用途）。
10. **行号会漂移**：设计中的行号（`watch.rs:246-254`、`rules.rs:156-174`、`kqueue.rs:*`）为评审时快照，实施时以 grep / 读源码重新定位为准。
11. **提交纪律**：每子步骤一个 commit；发现设计偏差时停下更新设计文档，不带病推进。
12. **手工验收项**（无法自动化）：真实编辑器原子保存的热重载体感；CI（Linux）与开发机（macOS）双后端实测。

---

## 附：实施顺序建议（与设计风险排序一致）

`visp-fs` 契约实现（Wave 1–2，尤其 **2a/2b**）是全局语义基线，先做透；再并行迁移；最后收口。**codegraph 迁移风险最高**（影响索引新鲜度且无兜底），其回归用例须在 4a 内完成，不留给 6a。
