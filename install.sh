#!/usr/bin/env bash
#
# visp 一键安装 / 卸载脚本
# ---------------------------------------------------------------------------
# 从 GitHub Releases 下载预编译二进制（visp / visp-daemon / visp-tui），
# 安装到指定目录，并可选地初始化配置骨架与安装 herdr hook。
# 亦支持 --uninstall 卸载二进制、--purge-config 清理配置目录。
#
#   用法：  ./install.sh [选项]
#   远程：  curl -fsSL https://raw.githubusercontent.com/laonger/visp/master/install.sh | bash
#
# 本脚本不联网以外的副作用：不调用 sudo、不修改任何 shell 配置（仅打印指引）。
# ---------------------------------------------------------------------------
set -euo pipefail

REPO_SLUG="laonger/visp"
REPO_URL="https://github.com/${REPO_SLUG}"
DEFAULT_MODEL="claude-sonnet-4-20250514"

# ---------------------------------------------------------------------------
# 日志
# ---------------------------------------------------------------------------
info() { printf '[visp] %s\n' "$*"; }
warn() { printf '[visp] 警告：%s\n' "$*" >&2; }
die() {
    printf '[visp] 错误：%s\n' "$*" >&2
    exit 1
}

# ---------------------------------------------------------------------------
# 参数解析（支持 --opt value 与 --opt=value）
# ---------------------------------------------------------------------------
BIN_DIR="${VISP_BIN_DIR:-$HOME/.local/bin}"
TAG="latest"
REF=""
DO_CONFIG=1
DO_HERDR=1
DRY_RUN=0
# 非交互配置（CLI 值；优先级见 init_config）
MODEL_ARG=""
API_KEY_ARG=""
# 卸载
UNINSTALL=0
PURGE_CONFIG=0
ASSUME_YES=0

usage() {
    cat <<'EOF'
visp 安装 / 卸载脚本 —— 从 GitHub Releases 安装预编译二进制并初始化配置。

用法：
  install.sh [选项]

模式：
  （默认）          安装 / 升级二进制并初始化配置
  --uninstall       卸载二进制（默认保留配置），可重复执行

安装选项：
  --bin-dir DIR     安装目录（默认 $VISP_BIN_DIR 或 ~/.local/bin）
  --tag TAG         指定 Release 版本号，如 v0.1.0（默认 latest）
  --ref REF         拉取 herdr hook 脚本所用 git ref
                    （默认：--tag 非 latest 时用该 tag，否则 master）
  --no-config       跳过配置初始化（不创建 ~/.config/visp 与 daemon.toml）
  --no-herdr        跳过 herdr hook 安装

配置写入（仅在本次新建 daemon.toml 时生效；已存在则不改）：
  --model MODEL     默认模型；回退环境变量 VISP_MODEL
  --api-key KEY     API key；回退环境变量 VISP_API_KEY
                    优先级：CLI 参数 > 环境变量 >（tty）交互提问 > 骨架默认
                    非交互环境（如 curl|bash）未提供时保持骨架默认、不提问

卸载选项：
  --uninstall       删除 <bin-dir> 下的 visp / visp-daemon / visp-tui
  --purge-config    配合 --uninstall，额外删除整个配置目录（含 daemon.toml、hooks/）
                    tty 下需确认；非 tty 必须显式 --yes，否则跳过
  --yes             跳过删除配置目录的确认

其它：
  --dry-run         仅打印将要执行的动作，不下载、不写盘
  -h, --help        显示本帮助

示例：
  ./install.sh
  ./install.sh --bin-dir /usr/local/bin --tag v0.1.0
  ./install.sh --model claude-sonnet-4-20250514 --api-key sk-ant-xxx
  ./install.sh --no-config --no-herdr
  ./install.sh --uninstall
  ./install.sh --uninstall --purge-config --yes
  ./install.sh --dry-run
EOF
}

parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --bin-dir)
                [ $# -ge 2 ] || die "选项 --bin-dir 缺少参数"
                BIN_DIR="$2"
                shift 2
                ;;
            --bin-dir=*)
                BIN_DIR="${1#*=}"
                shift
                ;;
            --tag)
                [ $# -ge 2 ] || die "选项 --tag 缺少参数"
                TAG="$2"
                shift 2
                ;;
            --tag=*)
                TAG="${1#*=}"
                shift
                ;;
            --ref)
                [ $# -ge 2 ] || die "选项 --ref 缺少参数"
                REF="$2"
                shift 2
                ;;
            --ref=*)
                REF="${1#*=}"
                shift
                ;;
            --model)
                [ $# -ge 2 ] || die "选项 --model 缺少参数"
                MODEL_ARG="$2"
                shift 2
                ;;
            --model=*)
                MODEL_ARG="${1#*=}"
                shift
                ;;
            --api-key)
                [ $# -ge 2 ] || die "选项 --api-key 缺少参数"
                API_KEY_ARG="$2"
                shift 2
                ;;
            --api-key=*)
                API_KEY_ARG="${1#*=}"
                shift
                ;;
            --uninstall)
                UNINSTALL=1
                shift
                ;;
            --purge-config)
                PURGE_CONFIG=1
                shift
                ;;
            --yes)
                ASSUME_YES=1
                shift
                ;;
            --no-config)
                DO_CONFIG=0
                shift
                ;;
            --no-herdr)
                DO_HERDR=0
                shift
                ;;
            --dry-run)
                DRY_RUN=1
                shift
                ;;
            -h | --help)
                usage
                exit 0
                ;;
            *)
                printf '[visp] 错误：未知参数：%s\n' "$1" >&2
                usage >&2
                exit 2
                ;;
        esac
    done

    [ -n "$BIN_DIR" ] || die "--bin-dir 不能为空"
    [ -n "$TAG" ] || die "--tag 不能为空"

    if [ -z "$REF" ]; then
        if [ "$TAG" = "latest" ]; then
            REF="master"
        else
            REF="$TAG"
        fi
    fi

    # 与 daemon 的 global_config_dir() 行为保持一致：
    # VISP_CONFIG_DIR 优先，否则 ~/.config/visp。
    CONFIG_DIR="${VISP_CONFIG_DIR:-$HOME/.config/visp}"
    CONFIG_TOML="$CONFIG_DIR/daemon.toml"
}

# ---------------------------------------------------------------------------
# 平台检测
# ---------------------------------------------------------------------------
detect_target() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux) os="unknown-linux-gnu" ;;
        Darwin) os="apple-darwin" ;;
        *) return 1 ;;
    esac

    case "$arch" in
        x86_64 | amd64) arch="x86_64" ;;
        arm64 | aarch64) arch="aarch64" ;;
        *) return 1 ;;
    esac

    case "${arch}-${os}" in
        x86_64-unknown-linux-gnu | aarch64-apple-darwin)
            printf '%s' "${arch}-${os}"
            ;;
        *) return 1 ;;
    esac
}

print_supported_matrix() {
    printf '[visp] 当前仅支持以下平台：\n' >&2
    printf '  - x86_64-unknown-linux-gnu  (Linux x86_64)\n' >&2
    printf '  - aarch64-apple-darwin     (macOS Apple Silicon)\n' >&2
    printf '[visp] 其它平台请从源码编译：cargo build --release\n' >&2
}

# ---------------------------------------------------------------------------
# 下载
# ---------------------------------------------------------------------------
TMPDIR_DL=""
cleanup() {
    if [ -n "$TMPDIR_DL" ] && [ -d "$TMPDIR_DL" ]; then
        rm -rf "$TMPDIR_DL"
    fi
}

has_downloader() {
    command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1
}

# 下载 url 到 dest。成功返回 0，失败返回非 0（不退出）。
fetch_url() {
    local url="$1" dest="$2"
    if command -v curl >/dev/null 2>&1; then
        curl -fL --retry 3 --connect-timeout 15 -o "$dest" "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$dest" "$url"
    else
        return 127
    fi
}

# ---------------------------------------------------------------------------
# 安装二进制
# ---------------------------------------------------------------------------
extract_pkg() {
    local archive="$1"
    tar -xzf "$archive" -C "$TMPDIR_DL" || die "解包失败：$archive"
}

install_binaries() {
    local target="$1"
    local srcdir="$TMPDIR_DL/visp-$target"
    [ -d "$srcdir" ] || die "解包结果缺少目录：$srcdir"

    mkdir -p "$BIN_DIR"
    local b src dest
    for b in visp visp-daemon visp-tui; do
        src="$srcdir/$b"
        [ -f "$src" ] || die "解包结果缺少二进制：$src"
        dest="$BIN_DIR/$b"
        if [ -e "$dest" ]; then
            info "覆盖已存在二进制（视为升级）：$dest"
        fi
        cp -f "$src" "$dest"
        chmod +x "$dest"
        info "已安装：$dest"
    done
}

path_check() {
    case ":${PATH:-}:" in
        *":$BIN_DIR:"*) return 0 ;;
    esac
    warn "$BIN_DIR 不在 PATH 中，请手动加入："
    printf '  # bash（追加到 ~/.bashrc）\n'
    printf '  export PATH="%s:$PATH"\n' "$BIN_DIR"
    printf '  # zsh（追加到 ~/.zshrc）\n'
    printf '  export PATH="%s:$PATH"\n' "$BIN_DIR"
    printf '  然后重开终端或执行：source ~/.zshrc（或 ~/.bashrc）\n'
}

# ---------------------------------------------------------------------------
# 配置初始化
# ---------------------------------------------------------------------------
write_skeleton() {
    local model="$1" api_key="$2"
    local model_esc api_esc
    model_esc="${model//\\/\\\\}"
    model_esc="${model_esc//\"/\\\"}"

    {
        printf '%s\n' '# ============================================================================='
        printf '%s\n' '# visp 全局配置（由 install.sh 生成）'
        printf '%s\n' '# ============================================================================='
        printf '%s\n' "# 完整注解模板：${REPO_URL}/blob/master/docs/daemon.example.toml"
        printf '%s\n' "# 仓库：${REPO_URL}"
        printf '%s\n' '#'
        printf '%s\n' '# 所有字段均有默认值，按需取消注释 / 修改即可。'
        printf '%s\n' ''
        printf '%s\n' '[llm]'
        printf 'model = "%s"\n' "$model_esc"
        printf '%s\n' '# api_key 缺省时依次尝试: 字段 api_key → 环境变量 ANTHROPIC_API_KEY'
        if [ -n "$api_key" ]; then
            api_esc="${api_key//\\/\\\\}"
            api_esc="${api_esc//\"/\\\"}"
            printf 'api_key = "%s"\n' "$api_esc"
        else
            printf '%s\n' '# api_key = "sk-ant-..."'
        fi
    } >"$CONFIG_TOML"
}

init_config() {
    # 解析优先级：CLI 参数 > 环境变量 >（tty）交互提问 > 骨架默认。
    local model="$DEFAULT_MODEL"
    local api_key=""
    if [ -n "${VISP_MODEL:-}" ]; then model="$VISP_MODEL"; fi
    if [ -n "${VISP_API_KEY:-}" ]; then api_key="$VISP_API_KEY"; fi
    if [ -n "$MODEL_ARG" ]; then model="$MODEL_ARG"; fi
    if [ -n "$API_KEY_ARG" ]; then api_key="$API_KEY_ARG"; fi

    info "初始化配置目录：$CONFIG_DIR"
    if [ "$DRY_RUN" -eq 1 ]; then
        info "[dry-run] mkdir -p $CONFIG_DIR/{rules,skills,agents,hooks}"
        if [ -e "$CONFIG_TOML" ]; then
            info "[dry-run] 配置已存在，将跳过（不覆盖）"
        elif [ -n "$api_key" ]; then
            info "[dry-run] 写入骨架：model=${model} api_key=sk-***（权限 600）"
        else
            info "[dry-run] 写入骨架：model=${model}"
        fi
        return 0
    fi

    mkdir -p "$CONFIG_DIR"/{rules,skills,agents,hooks}

    if [ -e "$CONFIG_TOML" ]; then
        info "配置已存在，未修改：$CONFIG_TOML"
        printf '  如需更新请手动编辑，或执行后重装：install.sh --uninstall --purge-config --yes\n'
        if [ -n "$MODEL_ARG$API_KEY_ARG" ] || [ -n "${VISP_MODEL:-}${VISP_API_KEY:-}" ]; then
            warn "已传入的 --model/--api-key（或 VISP_MODEL/VISP_API_KEY）因配置已存在而未应用"
        fi
        return 0
    fi

    # 交互式提问仅针对 CLI / 环境变量均未提供的字段。
    local have_model=0 have_key=0
    if [ -n "$MODEL_ARG" ] || [ -n "${VISP_MODEL:-}" ]; then have_model=1; fi
    if [ -n "$API_KEY_ARG" ] || [ -n "${VISP_API_KEY:-}" ]; then have_key=1; fi

    local input=""
    if [ -t 0 ] && [ -t 1 ]; then
        if [ "$have_model" -eq 0 ]; then
            printf '默认模型 [%s]（回车使用默认）: ' "$DEFAULT_MODEL"
            read -r input || input=""
            if [ -n "$input" ]; then model="$input"; fi
        fi
        if [ "$have_key" -eq 0 ]; then
            printf 'API key（回车跳过，之后也可用 ANTHROPIC_API_KEY 环境变量）: '
            read -rs input || input=""
            printf '\n'
            if [ -n "$input" ]; then api_key="$input"; fi
        fi
    fi

    write_skeleton "$model" "$api_key"
    if [ -n "$api_key" ]; then
        chmod 600 "$CONFIG_TOML"
        info "已生成配置骨架：${CONFIG_TOML}（model=${model}，api_key=sk-***，权限 600）"
    else
        info "已生成配置骨架：${CONFIG_TOML}（model=${model}）"
    fi
}

# ---------------------------------------------------------------------------
# herdr hook 安装
# ---------------------------------------------------------------------------
append_herdr_rule() {
    local hook_path="$1"

    if [ -e "$CONFIG_TOML" ] && grep -Eq '^[[:space:]]*id[[:space:]]*=[[:space:]]*"herdr"' "$CONFIG_TOML"; then
        info "daemon.toml 已存在 id=\"herdr\" 规则，跳过追加"
        return 0
    fi

    # 确保已有内容以换行结尾，避免与新增规则拼接。
    if [ -s "$CONFIG_TOML" ] && [ -n "$(tail -c 1 "$CONFIG_TOML")" ]; then
        printf '\n' >>"$CONFIG_TOML"
    fi

    {
        printf '\n'
        printf '%s\n' '# herdr 状态上报（由 install.sh 添加；脚本仅在 herdr pane 内生效）'
        printf '%s\n' '[[hooks.rules]]'
        printf '%s\n' 'id = "herdr"'
        printf '%s\n' 'event = ["SessionStart", "UserPromptSubmit", "PermissionRequest", "Stop", "StopFailure", "AgentRunEnd", "SubagentStop"]'
        printf 'command = "%s"\n' "$hook_path"
        printf '%s\n' 'on_full = "coalesce_latest"'
        printf '%s\n' 'timeout_ms = 2000'
    } >>"$CONFIG_TOML"

    info "已向 daemon.toml 追加 herdr hook 规则"
}

install_herdr() {
    local hook_dir="$CONFIG_DIR/hooks"
    local hook_path="$hook_dir/herdr.hook.sh"
    local raw_url="https://raw.githubusercontent.com/${REPO_SLUG}/${REF}/assets/hooks/herdr.hook.sh"

    info "安装 herdr hook：$hook_path"
    if [ "$DRY_RUN" -eq 1 ]; then
        info "[dry-run] mkdir -p $hook_dir"
        info "[dry-run] 下载 $raw_url -> $hook_path"
        info "[dry-run] 追加 [[hooks.rules]] id=\"herdr\"（若不存在）"
        return 0
    fi

    mkdir -p "$hook_dir"
    if ! fetch_url "$raw_url" "$hook_path"; then
        warn "下载 herdr hook 脚本失败，跳过（不影响 visp 安装）：$raw_url"
        return 0
    fi
    chmod +x "$hook_path"
    info "已安装 herdr hook 脚本：$hook_path"

    append_herdr_rule "$hook_path"
}

# ---------------------------------------------------------------------------
# 收尾提示
# ---------------------------------------------------------------------------
print_next_steps() {
    printf '\n'
    info "完成 ✅"
    printf '  二进制目录：%s\n' "$BIN_DIR"
    printf '  配置文件：  %s\n' "$CONFIG_TOML"
    printf '\n下一步：\n'
    printf '  1. 在项目目录启动： visp -p /path/to/project\n'
    printf '  2. 检查 hook 状态： visp hooks doctor\n'
    printf '  3. 完整配置模板：   %s/blob/master/docs/daemon.example.toml\n' "$REPO_URL"
}

# ---------------------------------------------------------------------------
# 卸载
# ---------------------------------------------------------------------------
purge_config_dir() {
    if [ ! -e "$CONFIG_DIR" ]; then
        info "配置目录不存在，无需清理：$CONFIG_DIR"
        return 0
    fi

    info "准备删除配置目录：$CONFIG_DIR"

    if [ "$ASSUME_YES" -eq 0 ]; then
        if [ -t 0 ] && [ -t 1 ]; then
            local ans=""
            printf '确认删除 %s ？（y/N）: ' "$CONFIG_DIR"
            read -r ans || ans=""
            case "$ans" in
                y | Y | yes | YES | Yes) ;;
                *)
                    info "已跳过配置清理（未确认）"
                    return 0
                    ;;
            esac
        else
            warn "非交互环境需显式 --yes 才会删除配置目录；已跳过"
            return 0
        fi
    fi

    if [ "$DRY_RUN" -eq 1 ]; then
        info "[dry-run] rm -rf $CONFIG_DIR"
        return 0
    fi
    rm -rf "$CONFIG_DIR"
    info "已删除配置目录：$CONFIG_DIR"
}

do_uninstall() {
    info "卸载 visp（二进制目录：${BIN_DIR}）"
    local any=0 b dest
    for b in visp visp-daemon visp-tui; do
        dest="$BIN_DIR/$b"
        if [ -e "$dest" ]; then
            any=1
            if [ "$DRY_RUN" -eq 1 ]; then
                info "[dry-run] 删除 $dest"
            else
                rm -f "$dest"
                info "已删除：$dest"
            fi
        else
            info "未安装：$dest"
        fi
    done
    if [ "$any" -eq 0 ]; then
        info "本地未发现已安装的 visp 二进制"
    fi

    if [ "$PURGE_CONFIG" -eq 1 ]; then
        purge_config_dir
    fi

    printf '\n'
    printf '  注意：~/.visp（日志 / 会话 DB）未删除，如需清理请手动执行：rm -rf ~/.visp\n'
}

# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------
main() {
    parse_args "$@"

    # --purge-config 仅在卸载时生效。
    if [ "$PURGE_CONFIG" -eq 1 ] && [ "$UNINSTALL" -eq 0 ]; then
        warn "--purge-config 仅在 --uninstall 时生效，已忽略"
        PURGE_CONFIG=0
    fi

    # 卸载优先：忽略一切安装步骤。
    if [ "$UNINSTALL" -eq 1 ]; then
        do_uninstall
        return 0
    fi

    local target asset url archive_path
    if ! target="$(detect_target)"; then
        print_supported_matrix
        exit 1
    fi
    asset="visp-${target}.tar.gz"
    if [ "$TAG" = "latest" ]; then
        url="https://github.com/${REPO_SLUG}/releases/latest/download/${asset}"
    else
        url="https://github.com/${REPO_SLUG}/releases/download/${TAG}/${asset}"
    fi

    info "平台：$target"
    info "版本：${TAG}（herdr ref：${REF}）"

    TMPDIR_DL="$(mktemp -d)"
    trap cleanup EXIT

    archive_path="$TMPDIR_DL/$asset"

    if [ "$DRY_RUN" -eq 1 ]; then
        info "[dry-run] 下载 $url -> $archive_path"
        info "[dry-run] 解包并安装 visp / visp-daemon / visp-tui -> $BIN_DIR"
    else
        if ! has_downloader; then
            die "未找到 curl 或 wget，无法下载。请先安装其中之一。"
        fi
        if ! fetch_url "$url" "$archive_path"; then
            die "下载失败：${url}（请检查 tag 是否正确及网络连通性）"
        fi
        extract_pkg "$archive_path"
        install_binaries "$target"
    fi

    path_check

    if [ "$DO_CONFIG" -eq 1 ]; then
        init_config
    fi
    if [ "$DO_HERDR" -eq 1 ]; then
        install_herdr
    fi

    print_next_steps
}

main "$@"
