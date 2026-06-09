# Launcher 设计

## 目标

一个命令完成"启动 daemon → 启动 CLI → CLI 退出 → 关闭 daemon"全流程。

## 架构

```
           ┌──────────────────┐
           │   vbw (launcher) │
           └────────┬─────────┘
                    │
         spawn ─────┼──── spawn
         │          │          │
         ▼          │          ▼
  ┌──────────┐     CLI       ┌──────────┐
  │ visp-daemon│   退出信号    │ visp-cli  │
  └──────────┘              └──────────┘
         │
  gRPC shutdown
```

## 改动范围

| 层次 | 改动内容 |
|------|---------|
| **vbw**（新建） | Launcher crate，管理 daemon + cli 生命周期 |
| Cargo.toml | workspace 添加 `vbw` crate |
| **visp-daemon** | 无改动 |
| **visp-cli** | 无改动 |

## Launcher 行为

```
1. 解析 CLI 参数
2. 创建 ~/.visp/logs/ 目录
3. 启动 visp-daemon 子进程（默认地址 [::1]:50051），stdout/stderr 写入日志文件
4. 轮询 health check（最多 30 次，间隔 500ms，共 15s 超时）
5. 超时 → 打印错误 → kill daemon → 退出码 1
6. 启动 visp-cli 子进程，传递 CLI 参数，CLI 的 stdout/stderr 直通终端
7. 等待 visp-cli 退出
8. 向 daemon 发送 gRPC Shutdown 请求
9. 等待 daemon 进程退出（超时 5s，超时则 kill）
10. 以 CLI 的退出码退出
```

## 日志

daemon 的 stdout/stderr 写入 `~/.visp/logs/daemon-{timestamp}.log`，按日期时间命名。CLI 的 stdout/stderr 直通终端（用户交互）。

日志文件自动轮转：每次启动生成新文件，不清理旧文件（用户手动管理）。

## 超时

| 阶段 | 超时 | 行为 |
|------|------|------|
| daemon 启动 + health check | 15s（30 × 500ms） | 超时 → 打印错误 → kill daemon → 退出码 1 |
| daemon 优雅关闭 | 5s（等待进程退出） | 超时 → kill -9 |

## 启动器 CLI 参数

```
Usage: vbw [OPTIONS]

Options:
  -p, --project <PATH>      项目路径 [default: .]
  -a, --addr <ADDR>         Daemon 监听地址 [default: [::1]:50051]
      --model <NAME>        LLM 模型
      --temperature <VAL>   温度
      --thinking-budget <N> 思考预算 token 数
```

## 不做什么

- ❌ 不做 daemon 的持久化运行（launcher 退出 daemon 即停）
- ❌ 不做 daemon 的多会话管理
- ❌ 不做 daemon 的日志持久化

## 验收标准

1. `cargo run --bin vbw` 启动 launcher，自动启动 daemon + CLI
2. daemon health check 通过前 CLI 不会启动
3. CLI 退出后 daemon 自动关闭
4. daemon 启动失败或 health check 超时给出清晰错误
5. CLI 的退出码透传到 launcher
