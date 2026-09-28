#!/usr/bin/env sh
# visp → herdr 状态上报参考脚本（外置；随包发布，无安装器）。
#
# herdr 集成的外置形态（herdr 设计 §11.1）：用户在 visp 配置中自行添加一条
# [hooks] 规则指向本脚本，visp 不内置任何 herdr Rust 代码。
#
# 规则示意（用户按文档自行添加，真实字段以 [hooks] 文档为准）：
#   id      = "herdr"
#   event   = ["UserPromptSubmit", "PermissionRequest", "Stop", "StopFailure",
#              "AgentRunEnd", "SubagentStop", "SessionStart"]
#   command = "sh"
#   args    = ["/path/to/visp/assets/hooks/herdr.hook.sh"]
#
# 依赖环境变量（visp hook 执行器白名单继承 + herdr pane 注入）：
#   VISP_HOOK_EVENT  事件名（PascalCase）——仅以此判定，**不解析 stdin JSON**
#   HERDR_ENV        herdr pane 标记；!= 1 时静默退出
#   HERDR_BIN_PATH   herdr CLI 路径
#   HERDR_PANE_ID    目标 pane id
#
# 设计约束：失败 / 缺失 / 未映射一律静默降级，绝不阻断 agent 主流程。
# 本脚本不读取 stdin（visp 写入 JSON 后即关写端，未读取无碍）。

set -eu

# 护栏 1：不在 herdr pane 内 → 静默退出。
if [ "${HERDR_ENV:-}" != "1" ]; then
    exit 0
fi

# 护栏 2：缺少 herdr CLI 路径或 pane 身份 → 无法上报，静默退出。
if [ -z "${HERDR_BIN_PATH:-}" ] || [ -z "${HERDR_PANE_ID:-}" ]; then
    exit 0
fi

state=""
message=""

# 状态映射（仅依赖 VISP_HOOK_EVENT）。
case "${VISP_HOOK_EVENT:-}" in
    UserPromptSubmit)
        state="working"
        ;;
    PermissionRequest)
        state="blocked"
        message="visp: 等待用户处理"
        ;;
    Stop|StopFailure|AgentRunEnd|SubagentStop|SessionStart)
        state="idle"
        ;;
    *)
        # 未映射事件不上报。
        exit 0
        ;;
esac

# 上报：失败静默（fire-and-forget）。
if [ -n "$message" ]; then
    "$HERDR_BIN_PATH" pane report-agent "$HERDR_PANE_ID" \
        --source custom:visp --agent visp --state "$state" --message "$message" \
        >/dev/null 2>&1 || true
else
    "$HERDR_BIN_PATH" pane report-agent "$HERDR_PANE_ID" \
        --source custom:visp --agent visp --state "$state" \
        >/dev/null 2>&1 || true
fi

exit 0
