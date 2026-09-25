//! 监听目标声明与匹配/排除规则（设计 §4.2 / §4.4）。
//!
//! 目标 = `(根路径, 模式, 过滤规则)`；本模块为纯逻辑，判定不触碰文件系统。

use std::path::{Component, Path, PathBuf};

/// 监听模式（设计 §4.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    /// 递归：根目录及其整个子树。
    Recursive,
    /// 直接子级：根目录的**直接子项（文件与目录）**。
    ///
    /// 必须包含子目录：结构性事件中的「目录创建」是 G5 补挂链与
    /// daemon 目标 #6（`.visp/{rules,agents,skills}` 出现）的前提。
    DirectChildren,
}

/// 包含规则（设计 §4.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Include {
    /// 文件名精确匹配。
    FileName(String),
    /// 扩展名匹配；兼容含点（`.md`）与不含点（`md`）两种书写。
    Extension(String),
    /// 前缀 / 子树匹配：相对根路径以给定路径为组件级前缀。
    Prefix(PathBuf),
}

/// 过滤规则：包含 + 路径组件级排除。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterRules {
    /// 包含规则；为空表示「全部包含」（仅受排除约束）。
    pub includes: Vec<Include>,
    /// 排除的路径组件名（如 `node_modules`、`.git`）；大小写精确。
    pub excludes: Vec<String>,
}

/// 监听目标声明：`(根路径, 模式, 过滤规则)`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchTarget {
    /// 目标根路径。
    pub root: PathBuf,
    /// 监听模式。
    pub mode: WatchMode,
    /// 过滤规则。
    pub filter: FilterRules,
}

impl WatchTarget {
    /// 判定路径是否命中本目标（纯函数）。
    ///
    /// 顺序：**模式范围 → 排除优先 → 包含**。路径不在模式范围内、被排除、
    /// 或不满足任一包含规则时均不命中；包含规则为空表示全部包含。
    pub fn matches(&self, path: &Path) -> bool {
        let Some(rel) = self.scope_relative(path) else {
            return false;
        };
        if self.filter.is_excluded(rel) {
            return false;
        }
        self.filter.is_included(rel)
    }

    /// 取相对根路径；路径不在模式范围内时返回 `None`。
    fn scope_relative<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        let rel = path.strip_prefix(&self.root).ok()?;
        match self.mode {
            WatchMode::Recursive => Some(rel),
            // 直接子级：恰好一个路径组件（文件或目录）。
            WatchMode::DirectChildren => match (rel.components().next(), rel.components().nth(1)) {
                (Some(_), None) => Some(rel),
                _ => None,
            },
        }
    }
}

impl FilterRules {
    /// 相对根路径是否被排除（任一组件名命中排除项，大小写精确）。
    pub fn is_excluded(&self, rel: &Path) -> bool {
        rel.components().any(|component| match component {
            Component::Normal(name) => name
                .to_str()
                .is_some_and(|n| self.excludes.iter().any(|e| e == n)),
            _ => false,
        })
    }

    /// 相对根路径是否被包含；包含规则为空视为全部包含。
    pub fn is_included(&self, rel: &Path) -> bool {
        self.includes.is_empty() || self.includes.iter().any(|include| include.matches(rel))
    }
}

impl Include {
    /// 单条包含规则对相对根路径的判定。
    fn matches(&self, rel: &Path) -> bool {
        match self {
            Include::FileName(name) => {
                rel.file_name().and_then(|n| n.to_str()) == Some(name.as_str())
            }
            Include::Extension(ext) => {
                // 兼容含点与不含点两种书写。
                let want = ext.strip_prefix('.').unwrap_or(ext);
                rel.extension().and_then(|e| e.to_str()) == Some(want)
            }
            Include::Prefix(prefix) => rel.starts_with(prefix),
        }
    }
}

#[cfg(test)]
#[path = "target_tests.rs"]
mod tests;
