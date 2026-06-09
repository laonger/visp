# visp-llm — LLM 提供器

封装 Anthropic Claude API 调用，支持 SSE 流解析、消息转换与重试。通过自定义 `base_url` 可兼容 OpenAI 兼容 API。

## 关键文件

- `anthropic.rs` — Anthropic API 集成（消息转换、SSE 流解析、重试）
- `streaming.rs` — SSE 事件解析器
- `mock.rs` — 测试用 Mock Provider

## 依赖

- `visp-core`（LlmProvider trait）

## 测试

```bash
cargo test -p visp-llm
```
