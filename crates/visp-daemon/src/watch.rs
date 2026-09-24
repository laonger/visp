//! daemon 文件监听模块（设计 §5.6）。
//!
//! 分两层：
//! - [`WatchPlan`]：启动时一次性构建的监听计划（纯构建 + 纯匹配，步骤 6a）；
//! - 运行时（步骤 6b）：基于 `notify` 多路径挂载、按领域 debounce、缺目录补挂
//!   与已监听路径去重。
//!
//! 计划构建与事件匹配不触碰任何全局状态，测试用 tempdir 构造存在性组合即可覆盖。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::reload::{ReloadCore, ReloadDomain, ReloadItem};

/// 自动重载的默认 debounce 窗口（设计 §7 决策 9）。
///
/// 足以合并编辑器原子写的多事件与「Save All」多文件风暴，同时低于人类的
/// 感知阈值。测试注入更短的窗口，不睡真实 200ms 的整数倍。
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// 监听计划（设计 §5.6 九行表）：路径全集 + 每路径监听模式 + 过滤规则 + 领域映射。
///
/// `entries` 是全部**希望**监听的目录，含启动时尚不存在、运行中补挂的目标；
/// 运行时按存在性决定实际挂载（缺目录降级链的执行层落实）。
#[derive(Debug, Clone, Default)]
pub struct WatchPlan {
    pub entries: Vec<WatchEntry>,
}

/// 单个监听条目。
#[derive(Debug, Clone)]
pub struct WatchEntry {
    /// 被监听目录（绝对路径）。
    pub dir: PathBuf,
    /// 是否递归监听。
    pub recursive: bool,
    /// 该目录下事件 → 领域的过滤规则。
    pub rules: Vec<WatchRule>,
    /// 该目录被动态补挂时需立即重扫的领域。
    ///
    /// 补挂后立即重扫是 kqueue 递归监听竞态窗口的兜底（设计 §5.6 缓解一）。
    /// 祖先目录 / 全局根 / 项目 `.visp` 等结构性目录无领域语义，故为空。
    pub rescan_domains: Vec<ReloadDomain>,
}

/// 路径过滤规则（设计 §5.6 表格「路径过滤」列）。
#[derive(Debug, Clone)]
pub struct WatchRule {
    pub matcher: Matcher,
    /// 命中时归属的领域；[`Matcher::VispSubtree`] 由子目录名动态决定，此处恒空。
    pub domains: Vec<ReloadDomain>,
}

/// 匹配器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matcher {
    /// 直接子项文件名/目录名精确等于给定串（非递归条目）。
    Child(String),
    /// 递归子树内扩展名等于给定串（不含点）。
    Extension(String),
    /// 递归子树内任意事件（skills 目标形态 `skills/<name>/SKILL.md`）。
    Any,
    /// project 根下的 `.visp` 子树，按第二层子目录名分流 rules/agents/skills。
    VispSubtree,
}

/// 一次事件分类结果。`domains` 为空表示「事件相关但暂不直接命中领域」，
/// 例如 `.visp` 目录本身创建——仍需触发补挂校准（设计 §5.6 缺目录降级链）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub domains: Vec<ReloadDomain>,
}

impl WatchPlan {
    /// 构建监听计划。
    ///
    /// `global_config` 为解析后的全局配置根（`~/.config/visp` 或 `VISP_CONFIG_DIR`）；
    /// 为 `None` 或不存在时跳过全部全局条目。
    pub fn build(project: &Path, global_config: Option<&Path>) -> Self {
        let mut plan = WatchPlan::default();

        // #1 project 向上每个祖先目录（非递归）：命中 AGENTS.md；
        // project 根这一层另加 `.visp` 子树规则，服务 #6 的降级补挂链。
        for ancestor in ancestors_inclusive(project) {
            let mut rules = vec![child_rule("AGENTS.md", ReloadDomain::Rules)];
            if ancestor == project {
                rules.push(WatchRule {
                    matcher: Matcher::VispSubtree,
                    domains: Vec::new(),
                });
            }
            plan.push_entry(ancestor, false, rules, Vec::new());
        }

        // #2 全局配置根（非递归）：顶层 AGENTS.md + rules/agents/skills 子目录事件分流。
        if let Some(global) = global_config.filter(|dir| dir.exists()) {
            let rules = vec![
                child_rule("AGENTS.md", ReloadDomain::Rules),
                child_rule("rules", ReloadDomain::Rules),
                child_rule("agents", ReloadDomain::Agents),
                child_rule("skills", ReloadDomain::Skills),
            ];
            plan.push_entry(global.to_path_buf(), false, rules, Vec::new());

            // #3/#4/#5 全局 rules/agents/skills（递归）。
            plan.push_recursive(global.join("rules"), ReloadDomain::Rules, false);
            plan.push_recursive(global.join("agents"), ReloadDomain::Agents, false);
            plan.push_recursive(global.join("skills"), ReloadDomain::Skills, true);
        }

        // #6 项目 `.visp`（非递归）：rules/agents/skills 子目录事件分流。
        let visp = project.join(".visp");
        let visp_rules = vec![
            child_rule("rules", ReloadDomain::Rules),
            child_rule("agents", ReloadDomain::Agents),
            child_rule("skills", ReloadDomain::Skills),
        ];
        plan.push_entry(visp.clone(), false, visp_rules, Vec::new());

        // #7/#8/#9 项目 rules/agents/skills（递归）。
        plan.push_recursive(visp.join("rules"), ReloadDomain::Rules, false);
        plan.push_recursive(visp.join("agents"), ReloadDomain::Agents, false);
        plan.push_recursive(visp.join("skills"), ReloadDomain::Skills, true);

        plan
    }

    /// 将单个事件路径分类为领域集合（纯函数，不做任何 I/O）。
    ///
    /// 返回 `None` 表示事件与计划完全无关（如排除清单内的文件），应当丢弃；
    /// 返回 `Some` 且 `domains` 为空表示事件相关但仅需触发补挂校准。
    pub fn classify(&self, path: &Path) -> Option<Classification> {
        let mut domains: Vec<ReloadDomain> = Vec::new();
        let mut matched = false;
        for entry in &self.entries {
            let Ok(rel) = path.strip_prefix(&entry.dir) else {
                continue;
            };
            if rel.as_os_str().is_empty() {
                continue;
            }
            for rule in &entry.rules {
                if let Some(rule_domains) = rule.match_domains(rel) {
                    matched = true;
                    for domain in rule_domains {
                        if !domains.contains(&domain) {
                            domains.push(domain);
                        }
                    }
                }
            }
        }
        matched.then_some(Classification { domains })
    }

    /// 合并写入一个监听条目（同目录去重，递归模式向上取或）。
    fn push_entry(
        &mut self,
        dir: PathBuf,
        recursive: bool,
        rules: Vec<WatchRule>,
        rescan_domains: Vec<ReloadDomain>,
    ) {
        if let Some(existing) = self.entries.iter_mut().find(|entry| entry.dir == dir) {
            existing.recursive |= recursive;
            existing.rules.extend(rules);
            existing.rescan_domains.extend(rescan_domains);
        } else {
            self.entries.push(WatchEntry {
                dir,
                recursive,
                rules,
                rescan_domains,
            });
        }
    }

    /// 加入一个递归监听的领域目录（rules/agents 用 `.md` 过滤，skills 用任意事件）。
    fn push_recursive(&mut self, dir: PathBuf, domain: ReloadDomain, any: bool) {
        let matcher = if any {
            Matcher::Any
        } else {
            Matcher::Extension("md".to_string())
        };
        self.push_entry(
            dir,
            true,
            vec![WatchRule {
                matcher,
                domains: vec![domain],
            }],
            vec![domain],
        );
    }
}

impl WatchRule {
    /// 判断相对路径 `rel`（相对所属监听目录）是否命中本规则，命中则返回领域集合。
    fn match_domains(&self, rel: &Path) -> Option<Vec<ReloadDomain>> {
        match &self.matcher {
            Matcher::Child(name) => (rel == Path::new(name)).then(|| self.domains.clone()),
            Matcher::Extension(ext) => (rel.extension().and_then(|value| value.to_str())
                == Some(ext.as_str()))
            .then(|| self.domains.clone()),
            Matcher::Any => Some(self.domains.clone()),
            Matcher::VispSubtree => visp_subtree_domains(rel),
        }
    }
}

/// `.visp` 子树分流：第二层子目录名决定领域；`.visp` 本身无领域（仅补挂）。
///
/// 排除清单（system-prompt.md、daemon.toml、webfetch.toml、logs、codegraph.db
/// 及其余 `.visp` 内容）因不在 rules/agents/skills 之内，自然零命中。
fn visp_subtree_domains(rel: &Path) -> Option<Vec<ReloadDomain>> {
    let mut components = rel.components();
    match components.next() {
        Some(std::path::Component::Normal(name)) if name == ".visp" => {}
        _ => return None,
    }
    match components.next() {
        None => Some(Vec::new()),
        Some(std::path::Component::Normal(name)) => match name.to_str()? {
            "rules" => Some(vec![ReloadDomain::Rules]),
            "agents" => Some(vec![ReloadDomain::Agents]),
            "skills" => Some(vec![ReloadDomain::Skills]),
            _ => None,
        },
        Some(_) => None,
    }
}

/// 构造「直接子项名精确匹配」规则。
fn child_rule(name: &str, domain: ReloadDomain) -> WatchRule {
    WatchRule {
        matcher: Matcher::Child(name.to_string()),
        domains: vec![domain],
    }
}

/// `path` 自身及全部祖先目录（近先远后）。
fn ancestors_inclusive(path: &Path) -> Vec<PathBuf> {
    let mut ancestors = Vec::new();
    let mut current = Some(path);
    while let Some(dir) = current {
        ancestors.push(dir.to_path_buf());
        current = dir.parent();
    }
    ancestors
}

/// 领域重载执行器抽象（设计 §5.6 事件处理管线第 3 步）。
///
/// 真实实现 [`CoreReloadExecutor`] 委托 [`ReloadCore::reload_domains`]；
/// 测试注入假体记录调用，避免依赖真实资产。
#[async_trait::async_trait]
pub trait ReloadExecutor: Send + Sync + 'static {
    /// 重载给定领域子集，返回逐项结果。
    async fn reload_domains(&self, domains: &[ReloadDomain]) -> Vec<ReloadItem>;
}

/// 委托共享 reload 核心的真实执行器。
pub struct CoreReloadExecutor {
    core: Arc<ReloadCore>,
}

impl CoreReloadExecutor {
    pub fn new(core: Arc<ReloadCore>) -> Self {
        Self { core }
    }
}

#[async_trait::async_trait]
impl ReloadExecutor for CoreReloadExecutor {
    async fn reload_domains(&self, domains: &[ReloadDomain]) -> Vec<ReloadItem> {
        self.core.reload_domains(domains).await
    }
}

/// 文件监听运行时（步骤 6b）。
///
/// 多路径挂载 [`WatchPlan`] → 回调线程过滤 → 后台任务按领域 debounce →
/// 补挂缺目录 → 调用 [`ReloadExecutor`]。`stop` 中止后台任务并 drop watcher。
pub struct FileWatcher {
    watcher: Arc<StdMutex<RecommendedWatcher>>,
    task: tokio::task::JoinHandle<()>,
}

impl FileWatcher {
    /// 启动监听。
    ///
    /// 初始挂载计划中已存在的目录；个别挂载失败（如祖先层权限不足）降级为
    /// 跳过 + warn，不阻断启动（设计 §5.6）。缺目录由运行中补挂链覆盖。
    pub async fn start(
        plan: Arc<WatchPlan>,
        executor: Arc<dyn ReloadExecutor>,
        debounce: Duration,
    ) -> Result<Self, notify::Error> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<ReloadDomain>>();

        let callback_plan = plan.clone();
        let watcher =
            notify::recommended_watcher(move |result: Result<notify::Event, notify::Error>| {
                let Ok(event) = result else {
                    return;
                };
                for path in event.paths {
                    if let Some(classification) = callback_plan.classify(&path) {
                        let _ = tx.send(classification.domains);
                    }
                }
            })?;
        let watcher = Arc::new(StdMutex::new(watcher));

        let mut mounted: HashSet<PathBuf> = HashSet::new();
        for entry in &plan.entries {
            if !entry.dir.exists() {
                continue;
            }
            if mount(&watcher, &entry.dir, entry.recursive).is_ok() {
                mounted.insert(entry.dir.clone());
            } else {
                tracing::warn!(dir = %entry.dir.display(), "文件监听挂载失败，跳过该目录");
            }
        }

        let task_watcher = watcher.clone();
        let task_plan = plan.clone();
        let task = tokio::spawn(async move {
            run_loop(rx, task_watcher, task_plan, executor, debounce, mounted).await;
        });

        Ok(Self { watcher, task })
    }

    /// 停止监听：中止后台任务并释放 notify watcher。
    pub fn stop(self) {
        self.task.abort();
        drop(self.watcher);
    }
}

/// 后台任务：窗口重置式 debounce + 补挂校准 + 领域重载。
async fn run_loop(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Vec<ReloadDomain>>,
    watcher: Arc<StdMutex<RecommendedWatcher>>,
    plan: Arc<WatchPlan>,
    executor: Arc<dyn ReloadExecutor>,
    debounce: Duration,
    mut mounted: HashSet<PathBuf>,
) {
    while let Some(first) = rx.recv().await {
        let mut domains = first;

        // 开窗：首个事件起算，窗口内每来事件重置计时、合并领域（按领域聚合）。
        let sleep = tokio::time::sleep(debounce);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                biased;
                Some(more) = rx.recv() => {
                    for domain in more {
                        if !domains.contains(&domain) {
                            domains.push(domain);
                        }
                    }
                    sleep.as_mut().reset(tokio::time::Instant::now() + debounce);
                }
                _ = &mut sleep => break,
            }
        }

        // 补挂校准：新出现的目录立刻挂载，并补该领域一次重扫标记
        // （kqueue 递归监听竞态兜底，设计 §5.6 缓解一）；已删除目录移出集合，
        // 使重建时可再次补挂且不重复挂载（缓解二）。
        for entry in &plan.entries {
            if entry.dir.exists() {
                if !mounted.contains(&entry.dir)
                    && mount(&watcher, &entry.dir, entry.recursive).is_ok()
                {
                    mounted.insert(entry.dir.clone());
                    for domain in &entry.rescan_domains {
                        if !domains.contains(domain) {
                            domains.push(*domain);
                        }
                    }
                }
            } else {
                mounted.remove(&entry.dir);
            }
        }

        if !domains.is_empty() {
            executor.reload_domains(&domains).await;
        }
    }
}

/// 以指定模式挂载单个目录。
fn mount(
    watcher: &StdMutex<RecommendedWatcher>,
    dir: &Path,
    recursive: bool,
) -> Result<(), notify::Error> {
    let mode = if recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    watcher.lock().unwrap().watch(dir, mode)
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
