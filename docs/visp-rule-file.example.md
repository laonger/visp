---
# 规则文件的 YAML frontmatter
# alwaysApply 是必填字段，必须在文件前 5 行内。
# true  = 规则始终生效（推荐）
# false = 规则默认不生效
alwaysApply: true
---

# 规则标题

规则内容使用 Markdown 编写，支持标准 Markdown 语法。

## 放置位置

规则文件有两种放置位置：

- **项目级规则**：`<项目根目录>/.visp/rules/*.md`
  - 随项目版本控制，团队成员共享
  - 例如：`.visp/rules/coding-style.md`

- **全局规则**：`~/.config/visp/rules/*.md`
  - 用户个人偏好，所有项目共享
  - 例如：`~/.config/visp/rules/language.md`

## 加载规则

- 仅加载扩展名为 `.md` 的文件
- 按文件名**字母序**加载（项目级优先于全局级）
- 仅加载 `alwaysApply: true` 的规则
- 规则内容会按顺序拼接后注入 LLM 的系统提示中

## 示例

```markdown
---
alwaysApply: true
---

# 代码风格

1. 使用 2 空格缩进
2. 函数命名使用 camelCase
3. 禁止使用 `any` 类型
```

## 注意

- 避免规则过多（建议不超过 10 条），否则会稀释关键指令
- 规则过长会占用 LLM 上下文窗口，保持精简
