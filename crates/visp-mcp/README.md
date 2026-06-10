# visp-mcp — MCP 客户端

MCP（Model Context Protocol）客户端封装，使 visp daemon 能够连接外部 MCP 服务器，将其工具动态集成到 Tool Registry 中。

## 功能

- **McpSession** — 单服务器连接生命周期管理（connect / list_tools / call_tool / shutdown）
- **McpToolAdapter** — 将 MCP 工具适配为 visp `Tool` trait，与内置工具无缝集成
- **McpManager** — 多服务器管理，支持自动重连（指数退避 → 30s 轮询）
- **双传输支持** — stdio（子进程）和 SSE（HTTP）
- **工具名前缀** — 防止不同服务器的工具名冲突
- **fail-fast** — 配置错误的 header 在启动时即报错退出

## 配置

在 daemon 配置文件中添加 `[mcp]` 段即可启用，详见根目录 README。
