use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::AgentLoopContext;
use crate::agent::Envelope;
use crate::agent::OrchestratorMessage;
use crate::agent_definition::PermissionRule;
use crate::error::SessionError;
use crate::message::Message;
use crate::message::Role;
use crate::provider::LlmConfig;

/// 会话状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    Idle,
    Running,
    Completed,
    Error,
}

/// 会话
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub id: String,
    pub project_path: PathBuf,
    pub status: SessionStatus,
    pub created_at: Instant,
    /// Unix 毫秒时间戳，用于 DB 持久化（Option 以兼容运行时无法确定时间的场景）
    pub created_at_unix: Option<i64>,
    pub history: Vec<Message>,
    /// 最后一条用户消息内容（用于 /list 显示），截断到 80 字符
    pub last_user_message: Option<String>,
    pub config: LlmConfig,
    pub system_prompt_template: String,
    /// 已审批的工具名称集合（Always Allow）
    pub approved_tools: HashSet<String>,
    /// 当前使用的 Agent 名称
    pub agent_name: String,
    /// 父 Session ID（子 Session 用）
    pub parent_id: Option<String>,
    /// 运行时权限规则集
    pub permission: Vec<PermissionRule>,
}

/// 会话存储抽象 trait
pub trait SessionStore: Send {
    fn create(&mut self, session: Session) -> Result<(), SessionError>;
    fn get(&self, session_id: &str) -> Result<Session, SessionError>;
    fn list(&self) -> Result<Vec<Session>, SessionError>;
    fn delete(&mut self, session_id: &str) -> Result<(), SessionError>;
    fn update(&mut self, session: Session) -> Result<(), SessionError>;

    /// 获取会话的全部消息
    fn get_messages(&self, session_id: &str) -> Result<Vec<Message>, SessionError>;
    /// 追加一条消息到会话
    fn append_message(&mut self, session_id: &str, message: Message) -> Result<(), SessionError>;
    /// 按项目路径过滤会话列表
    fn list_by_project(&self, project_path: &str) -> Result<Vec<Session>, SessionError>;
    /// 获取指定父会话的所有子会话
    fn list_child_sessions(&self, parent_id: &str) -> Result<Vec<Session>, SessionError>;
}

/// 内存会话存储
pub struct InMemorySessionStore {
    sessions: HashMap<String, Session>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }
}

impl Default for InMemorySessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore for InMemorySessionStore {
    fn create(&mut self, session: Session) -> Result<(), SessionError> {
        if self.sessions.contains_key(&session.id) {
            return Err(SessionError::AlreadyExists(session.id));
        }
        self.sessions.insert(session.id.clone(), session);
        Ok(())
    }

    fn get(&self, session_id: &str) -> Result<Session, SessionError> {
        self.sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| SessionError::NotFound(session_id.to_string()))
    }

    fn list(&self) -> Result<Vec<Session>, SessionError> {
        let mut sessions: Vec<Session> = self.sessions.values().cloned().collect();
        for session in &mut sessions {
            session.last_user_message = session
                .history
                .iter()
                .rev()
                .find(|m| m.role == Role::User)
                .map(|m| {
                    if m.content.chars().count() > 80 {
                        format!("{}...", m.content.chars().take(80).collect::<String>())
                    } else {
                        m.content.clone()
                    }
                });
        }
        Ok(sessions)
    }

    fn delete(&mut self, session_id: &str) -> Result<(), SessionError> {
        self.sessions
            .remove(session_id)
            .ok_or_else(|| SessionError::NotFound(session_id.to_string()))?;
        Ok(())
    }

    fn update(&mut self, session: Session) -> Result<(), SessionError> {
        if !self.sessions.contains_key(&session.id) {
            return Err(SessionError::NotFound(session.id));
        }
        self.sessions.insert(session.id.clone(), session);
        Ok(())
    }

    fn get_messages(&self, session_id: &str) -> Result<Vec<Message>, SessionError> {
        self.sessions
            .get(session_id)
            .map(|s| s.history.clone())
            .ok_or_else(|| SessionError::NotFound(session_id.to_string()))
    }

    fn append_message(&mut self, session_id: &str, message: Message) -> Result<(), SessionError> {
        self.sessions
            .get_mut(session_id)
            .map(|s| s.history.push(message))
            .ok_or_else(|| SessionError::NotFound(session_id.to_string()))
    }

    fn list_by_project(&self, project_path: &str) -> Result<Vec<Session>, SessionError> {
        let target = PathBuf::from(project_path);
        let mut sessions: Vec<Session> = self
            .sessions
            .values()
            .filter(|s| s.project_path == target)
            .cloned()
            .collect();
        for session in &mut sessions {
            session.last_user_message = session
                .history
                .iter()
                .rev()
                .find(|m| m.role == Role::User)
                .map(|m| {
                    if m.content.chars().count() > 80 {
                        format!("{}...", m.content.chars().take(80).collect::<String>())
                    } else {
                        m.content.clone()
                    }
                });
        }
        Ok(sessions)
    }

    fn list_child_sessions(&self, parent_id: &str) -> Result<Vec<Session>, SessionError> {
        Ok(self
            .sessions
            .values()
            .filter(|s| s.parent_id.as_deref() == Some(parent_id))
            .cloned()
            .collect())
    }
}

impl SessionStore for Box<dyn SessionStore> {
    fn create(&mut self, session: Session) -> Result<(), SessionError> {
        (**self).create(session)
    }
    fn get(&self, session_id: &str) -> Result<Session, SessionError> {
        (**self).get(session_id)
    }
    fn list(&self) -> Result<Vec<Session>, SessionError> {
        (**self).list()
    }
    fn delete(&mut self, session_id: &str) -> Result<(), SessionError> {
        (**self).delete(session_id)
    }
    fn update(&mut self, session: Session) -> Result<(), SessionError> {
        (**self).update(session)
    }
    fn get_messages(&self, session_id: &str) -> Result<Vec<Message>, SessionError> {
        (**self).get_messages(session_id)
    }
    fn append_message(&mut self, session_id: &str, message: Message) -> Result<(), SessionError> {
        (**self).append_message(session_id, message)
    }
    fn list_by_project(&self, project_path: &str) -> Result<Vec<Session>, SessionError> {
        (**self).list_by_project(project_path)
    }
    fn list_child_sessions(&self, parent_id: &str) -> Result<Vec<Session>, SessionError> {
        (**self).list_child_sessions(parent_id)
    }
}

const DEFAULT_SYSTEM_PROMPT: &str = concat!(
    "You are visp, a lightweight AI coding assistant.\n",
    "\n",
    "## Interaction Rules\n",
    "- Always wait for tool results; do not assume outcomes\n",
    "- Multiple tools can be called in parallel within a single reply\n",
    "- When a tool requires approval, a confirmation bar will appear (Approve / Deny / Always Allow)\n",
    "- Wait for each tool to complete before proceeding, unless tools can run in parallel\n",
    "- When you need the user to make a choice, use the [USER_QUERY] marker (see detailed instructions at end of prompt)\n",
    "\n",
    "## Task Delegation\n",
    "- **Prefer delegation** for code exploration and implementation tasks —\n",
    "  this improves precision and optimizes cost.\n",
    "- Available sub-agents are listed in the Delegation Guidelines section\n",
    "  (appended at session start) — match the task to the right agent.\n",
    "- Provide clear, bounded task specifications; sub-agents work best with\n",
    "  well-defined tasks.\n",
    "- Sub-agents have access to the tools they need and return their results\n",
    "  to you.\n",
);

/// 按优先级加载系统 prompt 模板：
/// 1. 项目目录 `.visp/system-prompt.md`
/// 2. 全局配置 `~/.config/visp/system-prompt.md`
/// 3. 内置默认
fn load_system_prompt_template(project_path: &Path) -> String {
    // Priority 1: project .visp/system-prompt.md
    let project_prompt = project_path.join(".visp").join("system-prompt.md");
    if project_prompt.is_file()
        && let Ok(content) = std::fs::read_to_string(&project_prompt)
        && !content.trim().is_empty()
    {
        return content;
    }

    // Priority 2: global ~/.config/visp/system-prompt.md
    if let Ok(home) = std::env::var("HOME") {
        let global_prompt = PathBuf::from(home)
            .join(".config")
            .join("visp")
            .join("system-prompt.md");
        if global_prompt.is_file()
            && let Ok(content) = std::fs::read_to_string(&global_prompt)
            && !content.trim().is_empty()
        {
            return content;
        }
    }

    // Priority 3: built-in default
    DEFAULT_SYSTEM_PROMPT.to_string()
}

/// 从 `.visp/skills/` 和全局 `~/.config/visp/skills/` 加载技能定义，格式化为 prompt 附加内容。
/// 每个技能目录下需有 `SKILL.md` 文件。
/// 项目级技能优先级高于全局级（同名时项目技能覆盖全局技能）。
pub fn load_skills(project_path: &Path) -> String {
    load_skills_inner(project_path, home_dir())
}

/// 与 `load_skills` 相同，但允许指定 home 目录（用于测试隔离）。
fn load_skills_inner(project_path: &Path, home: Option<PathBuf>) -> String {
    let mut seen_names = HashSet::new();
    let mut sections = Vec::new();

    // 1. Project skills (higher priority)
    let project_dir = project_path.join(".visp").join("skills");
    load_skills_from_dir(&project_dir, &mut seen_names, &mut sections);

    // 2. Global skills (lower priority, skipped if project already has same name)
    if let Some(home) = home {
        let global_dir = home.join(".config").join("visp").join("skills");
        load_skills_from_dir(&global_dir, &mut seen_names, &mut sections);
    }

    if sections.is_empty() {
        return String::new();
    }

    format!(
        "\n\n## Available Skills\n\n{}",
        sections.join("\n\n---\n\n")
    )
}

/// 从单个技能目录加载技能。
/// `seen_names` 跟踪已加载的技能名，同名跳过（用于项目优先级覆盖全局）。
/// `sections` 追加加载到的技能格式化片段。
fn load_skills_from_dir(dir: &Path, seen_names: &mut HashSet<String>, sections: &mut Vec<String>) {
    if !dir.is_dir() {
        return;
    }

    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };
    entries.sort_by_key(|e| e.file_name());

    for entry in &entries {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_name = match path.file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => continue,
        };
        // 同名跳过（项目级已加载的优先）
        if !seen_names.insert(skill_name.clone()) {
            continue;
        }
        let skill_file = path.join("SKILL.md");
        if !skill_file.is_file() {
            continue;
        }
        let content = match std::fs::read_to_string(&skill_file) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // 提取 YAML frontmatter 中的 description（如果有）
        let description = extract_frontmatter_field(&content, "description");

        let mut section = format!("### {skill_name}");
        if let Some(desc) = description {
            section.push_str(&format!("\n{desc}"));
        }
        sections.push(section);
    }
}

/// 返回用户的 home 目录路径
pub fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// 从 YAML frontmatter 中提取指定字段值
fn extract_frontmatter_field(content: &str, field: &str) -> Option<String> {
    let content = content.trim();
    if !content.starts_with("---") {
        return None;
    }
    let rest = content.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];
    let prefix = format!("{field}:");
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if let Some(val) = trimmed.strip_prefix(&prefix) {
            return Some(val.trim().to_string());
        }
    }
    None
}

/// 去除 YAML frontmatter，返回正文（仅测试用）
#[cfg(test)]
fn strip_frontmatter(content: &str) -> &str {
    let content = content.trim();
    if !content.starts_with("---") {
        return content;
    }
    let rest = content.strip_prefix("---").unwrap();
    if let Some(end) = rest.find("\n---") {
        let after = &rest[end + 4..]; // skip \n + ---
        after.trim()
    } else {
        content
    }
}

/// 参数：创建子会话
pub struct SubSessionParams {
    pub parent_id: Option<String>,
    pub agent_name: String,
    pub permission: Vec<PermissionRule>,
    /// Some = 复用已有 session，None = 新 UUID
    pub session_id: Option<String>,
    pub project_path: PathBuf,
    pub config: LlmConfig,
}

/// 会话管理器
pub struct SessionManager {
    store: Arc<Mutex<dyn SessionStore>>,
    running_tokens: Mutex<HashMap<String, CancellationToken>>,
}

impl SessionManager {
    pub fn new(store: impl SessionStore + 'static) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            running_tokens: Mutex::new(HashMap::new()),
        }
    }

    /// 创建会话，自动加载系统 prompt 模板和技能
    pub fn create(&self, project_path: &Path, config: LlmConfig) -> Result<Session, SessionError> {
        let id = Uuid::new_v4().to_string();
        let mut prompt_template = load_system_prompt_template(project_path);
        let skills = load_skills(project_path);
        if !skills.is_empty() {
            prompt_template.push_str(&skills);
        }

        let session = Session {
            id,
            project_path: project_path.to_path_buf(),
            status: SessionStatus::Idle,
            created_at: Instant::now(),
            created_at_unix: None,
            history: Vec::new(),
            last_user_message: None,
            config,
            system_prompt_template: prompt_template,
            approved_tools: HashSet::new(),
            agent_name: "default".to_string(),
            parent_id: None,
            permission: Vec::new(),
        };

        let mut store = self.store.lock().unwrap();
        let cloned = session.clone();
        store.create(cloned)?;

        Ok(session)
    }

    /// 创建子会话（多 Agent）
    /// session_id 为 Some 时复用已有 session，否则生成新 UUID
    pub fn create_sub(&self, params: SubSessionParams) -> Result<Session, SessionError> {
        // 复用
        if let Some(sid) = &params.session_id {
            let store = self.store.lock().unwrap();
            if let Ok(existing) = store.get(sid) {
                return Ok(existing);
            }
        }

        let id = params
            .session_id
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let base = load_system_prompt_template(&params.project_path);
        let system_prompt_template = format!(
            "You are agent \"{}\" working on a project.\n\n{base}",
            params.agent_name
        );

        let session = Session {
            id,
            project_path: params.project_path,
            status: SessionStatus::Idle,
            created_at: Instant::now(),
            created_at_unix: None,
            history: Vec::new(),
            last_user_message: None,
            config: params.config,
            system_prompt_template,
            approved_tools: HashSet::new(),
            agent_name: params.agent_name,
            parent_id: params.parent_id,
            permission: params.permission,
        };

        let mut store = self.store.lock().unwrap();
        let cloned = session.clone();
        store.create(cloned)?;
        Ok(session)
    }

    /// 删除会话，如有运行中的 agent 则先 cancel
    pub fn delete(&self, id: &str) -> Result<(), SessionError> {
        // Cancel running token if any
        if let Some(token) = self.running_tokens.lock().unwrap().remove(id) {
            token.cancel();
        }

        let mut store = self.store.lock().unwrap();
        store.delete(id)
    }

    /// 启动 agent 循环，检查状态必须为 Idle
    /// 启动 agent 循环。
    ///
    /// `global_tx` / `inbox_rx` / `permission_rules` 用于多 Agent 模式；
    /// 单 Agent 模式（测试 / 旧路径）传 `None` 即可。
    pub fn start_loop(
        &self,
        id: &str,
        context_trimmer: &Arc<dyn crate::context::ContextTrimmer + Send + Sync>,
        global_tx: Option<mpsc::Sender<Envelope>>,
        inbox_rx: Option<mpsc::Receiver<OrchestratorMessage>>,
        permission_rules: Option<Arc<Vec<PermissionRule>>>,
    ) -> Result<AgentLoopContext, SessionError> {
        let token = CancellationToken::new();

        let mut store = self.store.lock().unwrap();
        let session = store.get(id)?;

        if session.status != SessionStatus::Idle {
            return Err(SessionError::SessionBusy {
                session_id: id.to_string(),
            });
        }

        let mut updated = session.clone();
        updated.status = SessionStatus::Running;
        store.update(updated)?;
        drop(store);

        self.running_tokens
            .lock()
            .unwrap()
            .insert(id.to_string(), token.clone());

        Ok(AgentLoopContext {
            session_id: id.to_string(),
            history: session.history,
            working_dir: session.project_path,
            config: session.config,
            cancel_token: token,
            context_trimmer: Arc::clone(context_trimmer),
            global_tx,
            inbox_rx,
            permission_rules,
            agent_kind: crate::agent::AgentKind::Primary,
            depth: 0,
        })
    }

    /// 结束 agent 循环，切换会话状态，清理 token
    pub fn finish_loop(&self, id: &str, status: SessionStatus) -> Result<(), SessionError> {
        let mut store = self.store.lock().unwrap();
        let mut session = store.get(id)?;
        session.status = status;
        store.update(session)?;
        drop(store);

        self.running_tokens.lock().unwrap().remove(id);
        Ok(())
    }

    /// 追加消息到会话历史
    pub fn append_message(&self, id: &str, msg: Message) -> Result<(), SessionError> {
        let mut store = self.store.lock().unwrap();
        store.append_message(id, msg)
    }

    /// 更新会话的 LLM 配置
    pub fn update_config(&self, id: &str, config: LlmConfig) -> Result<(), SessionError> {
        let mut store = self.store.lock().unwrap();
        let mut session = store.get(id)?;
        session.config = config;
        store.update(session)
    }

    /// 追加内容到会话的 system_prompt_template
    pub fn append_system_prompt_template(&self, id: &str, extra: &str) -> Result<(), SessionError> {
        if extra.is_empty() {
            return Ok(());
        }
        let mut store = self.store.lock().unwrap();
        let mut session = store.get(id)?;
        session.system_prompt_template.push_str("\n\n");
        session.system_prompt_template.push_str(extra);
        store.update(session)
    }

    /// 列出所有会话
    pub fn list(&self) -> Result<Vec<Session>, SessionError> {
        let store = self.store.lock().unwrap();
        store.list()
    }

    /// 获取单个会话
    pub fn get(&self, id: &str) -> Result<Session, SessionError> {
        let store = self.store.lock().unwrap();
        store.get(id)
    }

    /// 获取会话的全部消息
    pub fn get_messages(&self, id: &str) -> Result<Vec<Message>, SessionError> {
        let store = self.store.lock().unwrap();
        store.get_messages(id)
    }

    /// 获取指定父 session 的所有子 session（直接代理 SessionStore.list_child_sessions）
    pub fn list_child_sessions(&self, parent_id: &str) -> Result<Vec<Session>, SessionError> {
        let store = self.store.lock().unwrap();
        store.list_child_sessions(parent_id)
    }

    /// 取消运行中的 agent（取消 CancellationToken）
    /// 如果 session 未在运行中，则为 no-op
    pub fn cancel_agent(&self, id: &str) {
        if let Some(token) = self.running_tokens.lock().unwrap().remove(id) {
            token.cancel();
        }
    }

    /// 检查工具是否已被审批（Always Allow）
    pub fn is_tool_approved(&self, session_id: &str, tool_name: &str) -> bool {
        let store = self.store.lock().unwrap();
        match store.get(session_id) {
            Ok(session) => session.approved_tools.contains(tool_name),
            Err(_) => false,
        }
    }

    /// 将工具加入已审批集合（Always Allow）
    pub fn add_approved_tool(&self, session_id: &str, tool_name: &str) -> Result<(), SessionError> {
        let mut store = self.store.lock().unwrap();
        let mut session = store.get(session_id)?;
        session.approved_tools.insert(tool_name.to_string());
        store.update(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ContextTrimmer;
    use crate::message::MessageType;
    use crate::message::{Message, Role};
    use std::path::Path;

    struct MockTrimmer;
    impl ContextTrimmer for MockTrimmer {
        fn trim(&self, history: &[Message], _: u32, _: u32, _: u32) -> Vec<Message> {
            history.to_vec()
        }
    }

    // ── MockSessionStore for list_child_sessions tests ────────────────────

    struct MockSessionStore {
        child_sessions: Vec<Session>,
    }

    impl SessionStore for MockSessionStore {
        fn create(&mut self, _session: Session) -> Result<(), SessionError> {
            Ok(())
        }
        fn get(&self, _session_id: &str) -> Result<Session, SessionError> {
            unimplemented!()
        }
        fn list(&self) -> Result<Vec<Session>, SessionError> {
            unimplemented!()
        }
        fn delete(&mut self, _session_id: &str) -> Result<(), SessionError> {
            Ok(())
        }
        fn update(&mut self, _session: Session) -> Result<(), SessionError> {
            Ok(())
        }
        fn get_messages(&self, _session_id: &str) -> Result<Vec<Message>, SessionError> {
            unimplemented!()
        }
        fn append_message(
            &mut self,
            _session_id: &str,
            _message: Message,
        ) -> Result<(), SessionError> {
            Ok(())
        }
        fn list_by_project(&self, _project_path: &str) -> Result<Vec<Session>, SessionError> {
            unimplemented!()
        }
        fn list_child_sessions(&self, _parent_id: &str) -> Result<Vec<Session>, SessionError> {
            Ok(self.child_sessions.clone())
        }
    }

    #[test]
    fn session_store_list_child_sessions_signature() {
        let store = MockSessionStore {
            child_sessions: vec![],
        };
        let result = store.list_child_sessions("any-parent");
        assert_eq!(result.unwrap(), vec![]);
    }

    #[test]
    fn mock_store_list_child_sessions_returns_empty() {
        let store = MockSessionStore {
            child_sessions: vec![],
        };
        let result = store.list_child_sessions("any");
        assert_eq!(result.unwrap(), vec![]);
    }

    #[test]
    fn mock_store_list_child_sessions_returns_sessions() {
        let session = Session {
            id: "child-1".into(),
            project_path: PathBuf::from("/tmp"),
            status: SessionStatus::Idle,
            created_at: Instant::now(),
            created_at_unix: Some(1700000000000),
            history: vec![],
            last_user_message: None,
            config: LlmConfig::default(),
            system_prompt_template: "default".into(),
            approved_tools: HashSet::new(),
            agent_name: "sub-agent".into(),
            parent_id: Some("parent-1".into()),
            permission: vec![],
        };
        let store = MockSessionStore {
            child_sessions: vec![session.clone()],
        };
        let result = store.list_child_sessions("parent-1").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "child-1");
        assert_eq!(result[0].agent_name, "sub-agent");
        assert_eq!(result[0].parent_id, Some("parent-1".into()));
    }

    // ── Step 3: Session.created_at_unix ──

    #[test]
    fn test_session_created_at_unix_default_none() {
        let mut store = InMemorySessionStore::new();
        let session = Session {
            id: "test-u1".into(),
            project_path: PathBuf::from("/tmp"),
            status: SessionStatus::Idle,
            created_at: Instant::now(),
            created_at_unix: None,
            history: vec![],
            last_user_message: None,
            config: LlmConfig::default(),
            system_prompt_template: "default".into(),
            approved_tools: HashSet::new(),
            agent_name: "default".into(),
            parent_id: None,
            permission: vec![],
        };
        let id = session.id.clone();
        store.create(session).unwrap();
        let got = store.get(&id).unwrap();
        assert_eq!(got.created_at_unix, None);
    }

    #[test]
    fn test_session_created_at_unix_read_write() {
        let mut store = InMemorySessionStore::new();
        let session = Session {
            id: "test-u2".into(),
            project_path: PathBuf::from("/tmp"),
            status: SessionStatus::Idle,
            created_at: Instant::now(),
            created_at_unix: Some(1700000000000),
            history: vec![],
            last_user_message: None,
            config: LlmConfig::default(),
            system_prompt_template: "default".into(),
            approved_tools: HashSet::new(),
            agent_name: "default".into(),
            parent_id: None,
            permission: vec![],
        };
        let id = session.id.clone();
        store.create(session.clone()).unwrap();
        // get() currently returns reference to internal Session
        // We use get() via the trait to verify read
        let got = store.get(&id).unwrap();
        assert_eq!(got.created_at_unix, Some(1700000000000));

        // update
        let mut updated = session.clone();
        updated.created_at_unix = Some(1700000000001);
        store.update(updated).unwrap();
        let got = store.get(&id).unwrap();
        assert_eq!(got.created_at_unix, Some(1700000000001));
    }

    #[test]
    fn test_session_created_at_unix_in_create() {
        let manager = SessionManager::new(InMemorySessionStore::new());
        let session = manager
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        // Rust Instant used for runtime; created_at_unix is None unless explicitly set
        assert_eq!(session.created_at_unix, None);
        assert!(session.created_at.elapsed().as_secs() < 5);
    }

    // ── InMemorySessionStore CRUD ──────────────────────────────────────────

    #[test]
    fn test_in_memory_store_crud() {
        let mut store = InMemorySessionStore::new();
        let session = Session {
            id: "test-1".into(),
            project_path: PathBuf::from("/tmp"),
            status: SessionStatus::Idle,
            created_at: Instant::now(),
            created_at_unix: None,
            history: vec![],
            last_user_message: None,
            config: LlmConfig::default(),
            system_prompt_template: "default".into(),
            approved_tools: HashSet::new(),
            agent_name: "default".into(),
            parent_id: None,
            permission: vec![],
        };

        // create
        store.create(session.clone()).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);

        // get
        let s = store.get("test-1").unwrap();
        assert_eq!(s.id, "test-1");

        // get not found
        assert!(store.get("nonexistent").is_err());

        // update
        let mut updated = session.clone();
        updated.status = SessionStatus::Running;
        store.update(updated.clone()).unwrap();
        let s = store.get("test-1").unwrap();
        assert_eq!(s.status, SessionStatus::Running);

        // duplicate create
        assert!(store.create(session.clone()).is_err());

        // delete
        store.delete("test-1").unwrap();
        assert!(store.get("test-1").is_err());
        assert_eq!(store.list().unwrap().len(), 0);

        // delete not found
        assert!(store.delete("nonexistent").is_err());
    }

    // ── SessionManager ────────────────────────────────────────────────────

    #[test]
    fn test_session_manager_create() {
        let manager = SessionManager::new(InMemorySessionStore::new());
        // Isolate from real global skills
        let home = tempfile::TempDir::new().unwrap();
        // SAFETY: test-only, no other test uses set_var for HOME
        unsafe { std::env::set_var("HOME", home.path()) };
        let session = manager
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        assert_eq!(session.status, SessionStatus::Idle);
        assert_eq!(session.project_path, Path::new("/tmp"));
        assert_eq!(session.system_prompt_template, DEFAULT_SYSTEM_PROMPT);
    }

    #[test]
    fn test_session_manager_start_loop() {
        let manager = SessionManager::new(InMemorySessionStore::new());
        let session = manager
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();

        let trimmer: Arc<dyn ContextTrimmer + Send + Sync> = Arc::new(MockTrimmer);
        let ctx = manager
            .start_loop(&session.id, &trimmer, None, None, None)
            .unwrap();
        assert_eq!(ctx.session_id, session.id);
        assert_eq!(ctx.working_dir, Path::new("/tmp"));

        let s = manager.get(&session.id).unwrap();
        assert_eq!(s.status, SessionStatus::Running);
    }

    #[test]
    fn test_session_manager_start_loop_busy() {
        let manager = SessionManager::new(InMemorySessionStore::new());
        let session = manager
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();

        let trimmer: Arc<dyn ContextTrimmer + Send + Sync> = Arc::new(MockTrimmer);
        let _ctx = manager
            .start_loop(&session.id, &trimmer, None, None, None)
            .unwrap();

        match manager.start_loop(&session.id, &trimmer, None, None, None) {
            Err(SessionError::SessionBusy { session_id }) => {
                assert_eq!(session_id, session.id);
            }
            _ => panic!("expected SessionBusy"),
        }
    }

    #[test]
    fn test_session_manager_finish_loop() {
        let manager = SessionManager::new(InMemorySessionStore::new());
        let session = manager
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();

        let trimmer: Arc<dyn ContextTrimmer + Send + Sync> = Arc::new(MockTrimmer);
        let _ctx = manager
            .start_loop(&session.id, &trimmer, None, None, None)
            .unwrap();
        manager
            .finish_loop(&session.id, SessionStatus::Completed)
            .unwrap();

        let s = manager.get(&session.id).unwrap();
        assert_eq!(s.status, SessionStatus::Completed);
    }

    #[test]
    fn test_session_manager_delete_cancels() {
        let manager = SessionManager::new(InMemorySessionStore::new());
        let session = manager
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();

        let trimmer: Arc<dyn ContextTrimmer + Send + Sync> = Arc::new(MockTrimmer);
        let ctx = manager
            .start_loop(&session.id, &trimmer, None, None, None)
            .unwrap();
        let token = ctx.cancel_token.clone();
        assert!(!token.is_cancelled());

        manager.delete(&session.id).unwrap();
        assert!(token.is_cancelled());
        assert!(manager.get(&session.id).is_err());
    }

    #[test]
    fn test_session_manager_append_message() {
        let manager = SessionManager::new(InMemorySessionStore::new());
        let session = manager
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();

        let msg = Message {
            role: Role::User,
            kind: MessageType::User,
            content: "hello".into(),
            tool_call_id: None,
            tool_calls: None,
            tool_call_count: None,
            extra_blocks: None,
            skip_context: false,
            estimated_tokens: 0,
            actual_tokens_input: None,
            actual_tokens_output: None,
            actual_cache_read: None,
            actual_cache_write: None,
            actual_cost: None,
            provider_metadata: None,
            tool_result_is_error: None,
            tool_result_duration_ms: None,
            created_at: None,
        };
        manager.append_message(&session.id, msg.clone()).unwrap();

        let s = manager.get(&session.id).unwrap();
        assert_eq!(s.history.len(), 1);
        assert_eq!(s.history[0].role, Role::User);
        assert_eq!(s.history[0].content, "hello");
    }

    #[test]
    fn test_session_manager_update_config() {
        let manager = SessionManager::new(InMemorySessionStore::new());
        let session = manager
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();

        let new_config = LlmConfig {
            model: "gpt-4".into(),
            ..Default::default()
        };
        manager
            .update_config(&session.id, new_config.clone())
            .unwrap();

        let s = manager.get(&session.id).unwrap();
        assert_eq!(s.config.model, "gpt-4");
    }

    #[test]
    fn test_session_approved_tools_empty_on_create() {
        let manager = SessionManager::new(InMemorySessionStore::new());
        let session = manager
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        assert!(session.approved_tools.is_empty());
    }

    #[test]
    fn test_session_approved_tools_clone() {
        let manager = SessionManager::new(InMemorySessionStore::new());
        let session1 = manager
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session1.id.clone();
        manager.add_approved_tool(&sid, "bash").unwrap();
        let s = manager.get(&sid).unwrap();
        assert!(s.approved_tools.contains("bash"));
    }

    #[test]
    fn test_session_is_tool_approved() {
        let manager = SessionManager::new(InMemorySessionStore::new());
        let session = manager
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();

        // Before adding, not approved
        assert!(!manager.is_tool_approved(&sid, "bash"));

        // Add and verify
        manager.add_approved_tool(&sid, "bash").unwrap();
        assert!(manager.is_tool_approved(&sid, "bash"));

        // Other tool still not approved
        assert!(!manager.is_tool_approved(&sid, "write_file"));
    }

    // ── Skill loading ──────────────────────────────────────────────────────

    #[test]
    fn test_extract_frontmatter_field_found() {
        let content = "---\nname: test-skill\ndescription: A test skill\n---\n\nContent here";
        assert_eq!(
            extract_frontmatter_field(content, "description"),
            Some("A test skill".into())
        );
    }

    #[test]
    fn test_extract_frontmatter_field_missing() {
        let content = "---\nname: test\n---\n\nContent";
        assert_eq!(extract_frontmatter_field(content, "description"), None);
    }

    #[test]
    fn test_extract_frontmatter_field_no_frontmatter() {
        let content = "Just content";
        assert_eq!(extract_frontmatter_field(content, "name"), None);
    }

    #[test]
    fn test_strip_frontmatter_removes_yaml() {
        let content = "---\nname: test\n---\nBody text";
        assert_eq!(strip_frontmatter(content), "Body text");
    }

    #[test]
    fn test_strip_frontmatter_no_frontmatter() {
        let content = "Just body";
        assert_eq!(strip_frontmatter(content), "Just body");
    }

    #[test]
    fn test_load_skills_empty_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        // No skills dir → empty
        let result = load_skills_inner(tmp.path(), Some(home.path().to_path_buf()));
        assert!(result.is_empty());
    }

    #[test]
    fn test_load_skills_with_skill_file() {
        use std::io::Write;
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".visp").join("skills").join("my-skill");
        std::fs::create_dir_all(&skills_dir).unwrap();
        let mut f = std::fs::File::create(skills_dir.join("SKILL.md")).unwrap();
        f.write_all(
            b"---\nname: my-skill\ndescription: A custom skill\n---\n\nDo something useful.\n",
        )
        .unwrap();

        let result = load_skills_inner(tmp.path(), Some(home.path().to_path_buf()));
        assert!(result.contains("my-skill"));
        assert!(result.contains("A custom skill"));
        assert!(!result.contains("Do something useful.")); // body 不应包含在提示词中
        assert!(result.contains("Available Skills"));
    }

    // ── DEFAULT_SYSTEM_PROMPT ─────────────────────────────────────────────

    #[test]
    fn test_default_prompt_contains_role() {
        assert!(DEFAULT_SYSTEM_PROMPT.contains("visp"));
    }

    #[test]
    fn test_default_prompt_no_project_specific_content() {
        // Coding conventions 已移到 AGENTS.md，不应出现在 DEFAULT 中
        assert!(!DEFAULT_SYSTEM_PROMPT.contains("Conventional Commits"));
        assert!(!DEFAULT_SYSTEM_PROMPT.contains("简洁优先"));
        assert!(!DEFAULT_SYSTEM_PROMPT.contains("TDD"));
    }

    #[test]
    fn test_default_prompt_contains_interaction_rules() {
        assert!(DEFAULT_SYSTEM_PROMPT.contains("[USER_QUERY]"));
        // 引用详细说明，但不包含完整格式
        assert!(DEFAULT_SYSTEM_PROMPT.contains("see detailed instructions"));
        // 通用工具规则
        assert!(DEFAULT_SYSTEM_PROMPT.contains("tool results"));
    }

    #[test]
    fn test_default_prompt_no_hardcoded_tools() {
        // 不应硬编码工具名，工具由动态指南渲染
        assert!(!DEFAULT_SYSTEM_PROMPT.contains("ReadFile"));
        assert!(!DEFAULT_SYSTEM_PROMPT.contains("Bash"));
    }

    #[test]
    fn test_load_skills_ignores_non_skill_dirs() {
        use std::io::Write;
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".visp").join("skills").join("my-skill");
        std::fs::create_dir_all(&skills_dir).unwrap();
        let mut f = std::fs::File::create(skills_dir.join("SKILL.md")).unwrap();
        f.write_all(b"---\nname: my-skill\n---\n\nContent").unwrap();
        // Add a non-skill file/dir
        std::fs::create_dir_all(tmp.path().join(".visp").join("skills").join("not-a-skill"))
            .unwrap();

        let result = load_skills_inner(tmp.path(), Some(home.path().to_path_buf()));
        assert!(result.contains("my-skill"));
    }

    #[test]
    fn test_load_skills_global_skills_loaded() {
        use std::io::Write;
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();

        // Create global skill at ~/.config/visp/skills/global-tool/
        let global_skill_dir = home
            .path()
            .join(".config")
            .join("visp")
            .join("skills")
            .join("global-tool");
        std::fs::create_dir_all(&global_skill_dir).unwrap();
        let mut f = std::fs::File::create(global_skill_dir.join("SKILL.md")).unwrap();
        f.write_all(b"---\ndescription: A global skill\n---\n\nDo stuff.\n")
            .unwrap();

        let result = load_skills_inner(tmp.path(), Some(home.path().to_path_buf()));
        assert!(
            result.contains("global-tool"),
            "should contain global skill"
        );
        assert!(
            result.contains("A global skill"),
            "should contain global skill description"
        );
        assert!(result.contains("Available Skills"));
    }

    #[test]
    fn test_load_skills_project_overrides_global() {
        use std::io::Write;
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();

        // Create project skill
        let project_skill = tmp.path().join(".visp").join("skills").join("my-tool");
        std::fs::create_dir_all(&project_skill).unwrap();
        let mut f = std::fs::File::create(project_skill.join("SKILL.md")).unwrap();
        f.write_all(b"---\ndescription: Project version\n---\n\nProject content.\n")
            .unwrap();

        // Create global skill with same name (should be overridden)
        let global_skill = home
            .path()
            .join(".config")
            .join("visp")
            .join("skills")
            .join("my-tool");
        std::fs::create_dir_all(&global_skill).unwrap();
        let mut f = std::fs::File::create(global_skill.join("SKILL.md")).unwrap();
        f.write_all(b"---\ndescription: Global version\n---\n\nGlobal content.\n")
            .unwrap();

        let result = load_skills_inner(tmp.path(), Some(home.path().to_path_buf()));
        assert!(
            result.contains("Project version"),
            "should use project version description"
        );
        assert!(
            !result.contains("Global version"),
            "should NOT contain global version description"
        );
        assert!(result.contains("my-tool"), "should contain the skill name");
    }

    #[test]
    fn test_load_skills_both_project_and_global() {
        use std::io::Write;
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();

        // Create project skill (unique name)
        let project_skill = tmp.path().join(".visp").join("skills").join("proj-skill");
        std::fs::create_dir_all(&project_skill).unwrap();
        let mut f = std::fs::File::create(project_skill.join("SKILL.md")).unwrap();
        f.write_all(b"---\ndescription: Project only\n---\n\nContent.\n")
            .unwrap();

        // Create global skill (unique name)
        let global_skill = home
            .path()
            .join(".config")
            .join("visp")
            .join("skills")
            .join("glob-skill");
        std::fs::create_dir_all(&global_skill).unwrap();
        let mut f = std::fs::File::create(global_skill.join("SKILL.md")).unwrap();
        f.write_all(b"---\ndescription: Global only\n---\n\nContent.\n")
            .unwrap();

        let result = load_skills_inner(tmp.path(), Some(home.path().to_path_buf()));
        assert!(
            result.contains("proj-skill"),
            "should contain project skill"
        );
        assert!(result.contains("glob-skill"), "should contain global skill");
        assert!(result.contains("Project only"));
        assert!(result.contains("Global only"));
    }
}
