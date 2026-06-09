use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::AgentLoopContext;
use crate::error::SessionError;
use crate::message::Message;
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
    pub history: Vec<Message>,
    pub config: LlmConfig,
    pub system_prompt_template: String,
    /// 已审批的工具名称集合（Always Allow）
    pub approved_tools: HashSet<String>,
}

/// 会话存储抽象 trait
pub trait SessionStore: Send {
    fn create(&mut self, session: Session) -> Result<(), SessionError>;
    fn get(&self, session_id: &str) -> Result<&Session, SessionError>;
    fn list(&self) -> Result<Vec<&Session>, SessionError>;
    fn delete(&mut self, session_id: &str) -> Result<(), SessionError>;
    fn update(&mut self, session: Session) -> Result<(), SessionError>;
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

    fn get(&self, session_id: &str) -> Result<&Session, SessionError> {
        self.sessions
            .get(session_id)
            .ok_or_else(|| SessionError::NotFound(session_id.to_string()))
    }

    fn list(&self) -> Result<Vec<&Session>, SessionError> {
        Ok(self.sessions.values().collect())
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
}

const DEFAULT_SYSTEM_PROMPT: &str = concat!(
    "You are vibewisp, a lightweight AI coding assistant.\n",
    "\n",
    "## Interaction Rules\n",
    "- Always wait for tool results; do not assume outcomes\n",
    "- Multiple tools can be called in parallel within a single reply\n",
    "- When a tool requires approval, a confirmation bar will appear (Approve / Deny / Always Allow)\n",
    "- Wait for each tool to complete before proceeding, unless tools can run in parallel\n",
    "- When you need the user to make a choice, use the [USER_QUERY] marker (see detailed instructions at end of prompt)\n",
);

/// 按优先级加载系统 prompt 模板：
/// 1. 项目目录 `.vibewisp/system-prompt.md`
/// 2. 全局配置 `~/.config/vibewisp/system-prompt.md`
/// 3. 内置默认
fn load_system_prompt_template(project_path: &Path) -> String {
    // Priority 1: project .vibewisp/system-prompt.md
    let project_prompt = project_path.join(".vibewisp").join("system-prompt.md");
    if project_prompt.is_file()
        && let Ok(content) = std::fs::read_to_string(&project_prompt)
        && !content.trim().is_empty()
    {
        return content;
    }

    // Priority 2: global ~/.config/vibewisp/system-prompt.md
    if let Ok(home) = std::env::var("HOME") {
        let global_prompt = PathBuf::from(home)
            .join(".config")
            .join("vibewisp")
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

/// 从 `.vibewisp/skills/` 加载技能定义，格式化为 prompt 附加内容。
/// 每个技能目录下需有 `SKILL.md` 文件。
fn load_skills(project_path: &Path) -> String {
    let skills_dir = project_path.join(".vibewisp").join("skills");
    if !skills_dir.is_dir() {
        return String::new();
    }

    let mut sections = Vec::new();

    let mut entries: Vec<_> = match std::fs::read_dir(&skills_dir) {
        Ok(entries) => entries.filter_map(|e| e.ok()).collect(),
        Err(_) => return String::new(),
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

    if sections.is_empty() {
        return String::new();
    }

    format!(
        "\n\n## Available Skills\n\n{}",
        sections.join("\n\n---\n\n")
    )
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
            history: Vec::new(),
            config,
            system_prompt_template: prompt_template,
            approved_tools: HashSet::new(),
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
    pub fn start_loop(&self, id: &str) -> Result<AgentLoopContext, SessionError> {
        let token = CancellationToken::new();

        let mut store = self.store.lock().unwrap();
        let session = store.get(id)?.clone();

        if session.status != SessionStatus::Idle {
            return Err(SessionError::SessionBusy {
                session_id: id.to_string(),
            });
        }

        let mut updated = session.clone();
        updated.status = SessionStatus::Running;
        store.update(updated)?;
        // store lock released here

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
        })
    }

    /// 结束 agent 循环，切换会话状态，清理 token
    pub fn finish_loop(&self, id: &str, _status: SessionStatus) -> Result<(), SessionError> {
        let mut store = self.store.lock().unwrap();
        let mut session = store.get(id)?.clone();
        session.status = SessionStatus::Idle;
        store.update(session)?;
        drop(store);

        self.running_tokens.lock().unwrap().remove(id);
        Ok(())
    }

    /// 追加消息到会话历史
    pub fn append_message(&self, id: &str, msg: Message) -> Result<(), SessionError> {
        let mut store = self.store.lock().unwrap();
        let mut session = store.get(id)?.clone();
        session.history.push(msg);
        store.update(session)
    }

    /// 更新会话的 LLM 配置
    pub fn update_config(&self, id: &str, config: LlmConfig) -> Result<(), SessionError> {
        let mut store = self.store.lock().unwrap();
        let mut session = store.get(id)?.clone();
        session.config = config;
        store.update(session)
    }

    /// 列出所有会话
    pub fn list(&self) -> Result<Vec<Session>, SessionError> {
        let store = self.store.lock().unwrap();
        let sessions = store.list()?;
        Ok(sessions.into_iter().cloned().collect())
    }

    /// 获取单个会话
    pub fn get(&self, id: &str) -> Result<Session, SessionError> {
        let store = self.store.lock().unwrap();
        let session = store.get(id)?;
        Ok(session.clone())
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
        let mut session = store.get(session_id)?.clone();
        session.approved_tools.insert(tool_name.to_string());
        store.update(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;
    use std::path::Path;

    // ── InMemorySessionStore CRUD ──────────────────────────────────────────

    #[test]
    fn test_in_memory_store_crud() {
        let mut store = InMemorySessionStore::new();
        let session = Session {
            id: "test-1".into(),
            project_path: PathBuf::from("/tmp"),
            status: SessionStatus::Idle,
            created_at: Instant::now(),
            history: vec![],
            config: LlmConfig::default(),
            system_prompt_template: "default".into(),
            approved_tools: HashSet::new(),
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

        let ctx = manager.start_loop(&session.id).unwrap();
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
        let _ctx = manager.start_loop(&session.id).unwrap();

        match manager.start_loop(&session.id) {
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
        let _ctx = manager.start_loop(&session.id).unwrap();
        manager
            .finish_loop(&session.id, SessionStatus::Completed)
            .unwrap();

        let s = manager.get(&session.id).unwrap();
        assert_eq!(s.status, SessionStatus::Idle);
    }

    #[test]
    fn test_session_manager_delete_cancels() {
        let manager = SessionManager::new(InMemorySessionStore::new());
        let session = manager
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let ctx = manager.start_loop(&session.id).unwrap();
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
            content: "hello".into(),
            tool_call_id: None,
            tool_calls: None,
            extra_blocks: None,
            skip_context: false,
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

        let mut new_config = LlmConfig::default();
        new_config.model = "gpt-4".into();
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
        // No skills dir → empty
        let result = load_skills(tmp.path());
        assert!(result.is_empty());
    }

    #[test]
    fn test_load_skills_with_skill_file() {
        use std::io::Write;
        let tmp = tempfile::TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".vibewisp").join("skills").join("my-skill");
        std::fs::create_dir_all(&skills_dir).unwrap();
        let mut f = std::fs::File::create(skills_dir.join("SKILL.md")).unwrap();
        f.write_all(
            b"---\nname: my-skill\ndescription: A custom skill\n---\n\nDo something useful.\n",
        )
        .unwrap();

        let result = load_skills(tmp.path());
        assert!(result.contains("my-skill"));
        assert!(result.contains("A custom skill"));
        assert!(!result.contains("Do something useful.")); // body 不应包含在提示词中
        assert!(result.contains("Available Skills"));
    }

    // ── DEFAULT_SYSTEM_PROMPT ─────────────────────────────────────────────

    #[test]
    fn test_default_prompt_contains_role() {
        assert!(DEFAULT_SYSTEM_PROMPT.contains("vibewisp"));
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
        let skills_dir = tmp.path().join(".vibewisp").join("skills").join("my-skill");
        std::fs::create_dir_all(&skills_dir).unwrap();
        let mut f = std::fs::File::create(skills_dir.join("SKILL.md")).unwrap();
        f.write_all(b"---\nname: my-skill\n---\n\nContent").unwrap();
        // Add a non-skill file/dir
        std::fs::create_dir_all(
            tmp.path()
                .join(".vibewisp")
                .join("skills")
                .join("not-a-skill"),
        )
        .unwrap();

        let result = load_skills(tmp.path());
        assert!(result.contains("my-skill"));
    }
}
