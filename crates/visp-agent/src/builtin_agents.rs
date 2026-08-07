use visp_core::agent_definition::{AgentDefinition, AgentMode, PermissionAction, PermissionRule};
use visp_core::agent_registry::AgentRegistry;

/// Register all built-in agents into the registry.
///
/// These are the lowest-priority agents — file-based agents loaded later
/// via `load_agents` will overwrite them.
pub(crate) fn register_builtin_agents(registry: &mut AgentRegistry) {
    register_default(registry);
    register_explorer(registry);
    register_fixer(registry);
    register_painter(registry);
    register_vision(registry);
}

fn register_default(registry: &mut AgentRegistry) {
    let default_agent = AgentDefinition {
        name: "default".to_string(),
        description: "通用 AI 编程助手".to_string(),
        mode: AgentMode::All,
        model: None,
        temperature: None,
        steps: None,
        permission: Vec::new(),
        allowed_sub_agents: Vec::new(),
        system_prompt: String::new(),
    };
    registry.register(default_agent).ok();
}

fn register_explorer(registry: &mut AgentRegistry) {
    let explorer = AgentDefinition {
        name: "explorer".to_string(),
        description: "快速代码库搜索专家。用于查找文件、定位代码模式、回答\"X 在哪里？\"等问题。"
            .to_string(),
        mode: AgentMode::Subagent,
        model: None,
        temperature: Some(0.1),
        steps: None,
        permission: vec![
            PermissionRule {
                permission: "read_file".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "grep".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "glob".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "fetch_web".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "codegraph_search".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "codegraph_get_details".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "codegraph_context".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "codegraph_trace".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "codegraph_impact".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
        ],
        allowed_sub_agents: Vec::new(),
        system_prompt: concat!(
            "你是 Explorer -- 快速代码库导航专家。\n",
            "\n",
            "**角色**：代码库侦察兵。回答\"X 在哪里？\"\"找到 Y\"\"哪个文件有 Z\"。\n",
            "\n",
            "**工具选择**：\n",
            "- 文本/正则搜索（字符串、注释、变量名）：grep\n",
            "- 文件发现（按名称/扩展名查找）：glob\n",
            "- 结构性查询（符号定义、调用关系、影响分析）：codegraph_search、codegraph_get_details、codegraph_context、codegraph_trace、codegraph_impact\n",
            "- 读取文件内容：read_file\n",
            "\n",
            "**行为准则**：\n",
            "- 快速且彻底\n",
            "- 需要时并行发起多个搜索\n",
            "- 返回文件路径和代码片段（含行号）\n",
            "\n",
            "**输出格式**：\n",
            "<results>\n",
            "<files>\n",
            "- 路径:行号 - 简要说明\n",
            "</files>\n",
            "<answer>\n",
            "简洁回答\n",
            "</answer>\n",
            "</results>\n",
            "\n",
            "**约束**：\n",
            "- 只读--搜索和报告，不修改文件\n",
            "- 详尽但简洁\n",
            "- 包含行号\n",
        )
        .to_string(),
    };
    registry.register(explorer).ok();
}

fn register_fixer(registry: &mut AgentRegistry) {
    let fixer = AgentDefinition {
        name: "fixer".to_string(),
        description: "快速实现专家。Prefer this for all non-trivial code implementation tasks."
            .to_string(),
        mode: AgentMode::Subagent,
        model: None,
        temperature: Some(0.2),
        steps: None,
        permission: vec![
            PermissionRule {
                permission: "task".into(),
                pattern: "*".into(),
                action: PermissionAction::Deny,
            },
            PermissionRule {
                permission: "*".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
        ],
        allowed_sub_agents: Vec::new(),
        system_prompt: concat!(
            "你是 Fixer —— 快速、专注的实现专家。\n",
            "\n",
            "**角色**：高效执行代码变更。你从 Orchestrator 收到完整上下文和明确任务规格。你的工作是实现，不是规划或调研。\n",
            "\n",
            "**行为准则**：\n",
            "- 执行 Orchestrator 提供的任务规格\n",
            "- 使用提供的研究上下文（文件路径、文档、模式）\n",
            "- 修改文件用 edit_file（精确替换）或 write_file（整体重写）；edit_file 前先 read_file 确认上下文\n",
            "- 快速直接——不做调研，不委托，不多步规划\n",
            "- 需要时编写或更新测试\n",
            "- 完成后报告变更摘要\n",
            "\n",
            "**约束**：\n",
            "- 不做外部调研（不使用 fetch_web）\n",
            "- 不委托或生成子 agent（不使用 task）\n",
            "- 禁止用 bash 修改文件（不使用 sed/echo/cat > 等）；文件读写一律用 read_file/edit_file/write_file\n",
            "- bash 仅用于：运行构建命令、测试、git 操作\n",
            "- 不做多步研究/规划；最小执行序列即可\n",
            "- 上下文不足时：直接用 grep/glob/read_file 获取，不委托\n",
            "- 只在真正无法自行获取时才请求补充输入\n",
            "\n",
            "**输出格式**：\n",
            "<summary>\n",
            "实现内容简述\n",
            "</summary>\n",
            "<changes>\n",
            "- file1.rs: 将 X 改为 Y\n",
            "- file2.rs: 新增 Z 函数\n",
            "</changes>\n",
            "<verification>\n",
            "- 测试通过: [是/否/跳过原因]\n",
            "- 验证: [通过/失败/跳过原因]\n",
            "</verification>\n",
        )
        .to_string(),
    };
    registry.register(fixer).ok();
}

fn register_painter(registry: &mut AgentRegistry) {
    let painter = AgentDefinition {
        name: "painter".to_string(),
        description: "文生图专家。根据用户的文字描述生成图片。".to_string(),
        mode: AgentMode::Subagent,
        model: None, // 由 daemon.toml 的 llm.image_generation_model 覆盖
        temperature: None,
        steps: Some(1),
        permission: Vec::new(),
        allowed_sub_agents: Vec::new(),
        system_prompt: concat!(
            "你是 Painter -- 文生图专家。\n",
            "\n",
            "**角色**：根据用户的文字描述生成图片。\n",
            "\n",
            "**行为准则**：\n",
            "- 将用户的描述作为 prompt 直接发送给文生图模型\n",
            "- 不使用任何工具\n",
            "- 如果描述不够清晰，可以适度补充细节（风格、构图、光线等）\n",
            "- 生成结果直接返回，不做额外解释\n",
        )
        .to_string(),
    };
    registry.register(painter).ok();
}

fn register_vision(registry: &mut AgentRegistry) {
    let vision = AgentDefinition {
        name: "vision".to_string(),
        description: "识图专家。分析图片内容并回答用户问题。".to_string(),
        mode: AgentMode::Subagent,
        model: None, // 由 daemon.toml 的 llm.vision_model 覆盖
        temperature: Some(0.1),
        steps: Some(3),
        permission: vec![PermissionRule {
            permission: "read_file".into(),
            pattern: "*".into(),
            action: PermissionAction::Allow,
        }],
        allowed_sub_agents: Vec::new(),
        system_prompt: concat!(
            "你是 Vision -- 识图分析专家。\n",
            "\n",
            "**角色**：分析用户提供的图片，回答关于图片内容的问题。\n",
            "\n",
            "**行为准则**：\n",
            "- 仔细观察图片中的所有细节\n",
            "- 用清晰准确的语言描述图片内容\n",
            "- 回答用户的具体问题，不做无关推测\n",
            "- 如果图片不清晰或无法识别，如实说明\n",
        )
        .to_string(),
    };
    registry.register(vision).ok();
}
