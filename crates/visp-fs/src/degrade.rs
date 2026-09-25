//! 缺目录降级链解析（设计 §4.2 G5 / §4.3 细则 5）。
//!
//! 纯函数：存在性由调用方以谓词注入（运行时传 `Path::exists`，测试传内存集合），
//! 本模块**不做 IO**。据此可驱动「最近存在祖先 + 前缀过滤」的降级监听，
//! 并输出祖先链供复用。

use std::path::{Path, PathBuf};

/// 降级链解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DegradePlan {
    /// 祖先链：从目标根逐级向上，止于**最近存在的祖先**（含）。
    /// 目标根存在时为 `[root]`；无任何可挂载祖先时为空。
    pub ancestors: Vec<PathBuf>,
    /// 挂载方案。
    pub mount: MountTarget,
}

/// 挂载方案（设计 §4.3 细则 5：区分「根不存在」与「不可挂载」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountTarget {
    /// 目标根存在：直接监听根。
    Direct(PathBuf),
    /// 根不存在：监听最近存在祖先，并对目标根做前缀过滤，待其出现后补挂。
    Fallback {
        /// 最近存在的祖先目录。
        ancestor: PathBuf,
        /// 从该祖先到目标根的缺失前缀（相对祖先）。
        pending: PathBuf,
    },
    /// 无任何可挂载祖先：终止并显式上报「自动监听不可用」。
    Unmountable,
}

/// 解析缺目录降级链（纯函数）。
///
/// `exists` 判定路径是否存在（含目录）；调用方注入。
pub fn resolve_degradation(root: &Path, exists: impl Fn(&Path) -> bool) -> DegradePlan {
    if exists(root) {
        return DegradePlan {
            ancestors: vec![root.to_path_buf()],
            mount: MountTarget::Direct(root.to_path_buf()),
        };
    }

    // 从目标根逐级向上找最近存在的祖先。
    let chain: Vec<PathBuf> = root.ancestors().map(Path::to_path_buf).collect();
    for (idx, ancestor) in chain.iter().enumerate() {
        // 相对路径在 `ancestors()` 末尾可能产出空路径，跳过。
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        if exists(ancestor) {
            let pending = root
                .strip_prefix(ancestor)
                .unwrap_or(Path::new(""))
                .to_path_buf();
            return DegradePlan {
                ancestors: chain[..=idx].to_vec(),
                mount: MountTarget::Fallback {
                    ancestor: ancestor.clone(),
                    pending,
                },
            };
        }
    }

    DegradePlan {
        ancestors: Vec::new(),
        mount: MountTarget::Unmountable,
    }
}

#[cfg(test)]
#[path = "degrade_tests.rs"]
mod tests;
