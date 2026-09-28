//! 复合 Handler 路由测试（设计 D8；任务 1b-4b）。
//!
//! 断言内置绑定与脚本 Handler 经**同一 executor 接口**分发：`builtin:*` 走内置绑定，
//! 其余走脚本 Handler；未注册的内置 id 不误落脚本路径。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use visp_hooks::{
    CompositeHandler, DispatchInput, DispatchRule, Executor, HookEventName, RecordingHandler,
};

fn rule(id: &str, event: HookEventName) -> DispatchRule {
    DispatchRule {
        id: id.to_string(),
        event: vec![event],
        ..DispatchRule::default()
    }
}

/// `builtin:*` → 内置绑定；其余 → 脚本 Handler（同一 executor 接口分发）。
#[tokio::test]
async fn routes_builtin_to_binding_and_script_to_spawn_handler() {
    let scripts = Arc::new(RecordingHandler::new());
    let builtin = Arc::new(RecordingHandler::new());
    let composite = CompositeHandler::new(scripts.clone(), HashMap::new())
        .with_builtin("builtin:herdr", builtin.clone());
    let executor = Executor::new(
        vec![
            rule("script-rule", HookEventName::Stop),
            rule("builtin:herdr", HookEventName::Stop),
        ],
        Arc::new(composite),
    );

    executor.dispatch(DispatchInput::new(HookEventName::Stop));

    tokio::time::timeout(Duration::from_secs(1), scripts.wait_for(1))
        .await
        .expect("脚本规则应经脚本 Handler 执行");
    tokio::time::timeout(Duration::from_secs(1), builtin.wait_for(1))
        .await
        .expect("内置规则应经绑定执行");

    let script_records = scripts.records();
    assert_eq!(script_records.len(), 1);
    assert_eq!(script_records[0].rule_id, "script-rule");

    let builtin_records = builtin.records();
    assert_eq!(builtin_records.len(), 1);
    assert_eq!(builtin_records[0].rule_id, "builtin:herdr");
}

/// 未注册的 `builtin:*` id → 静默跳过，不误落脚本路径。
#[tokio::test]
async fn unknown_builtin_id_is_skipped_not_fallen_back_to_scripts() {
    let scripts = Arc::new(RecordingHandler::new());
    let composite = CompositeHandler::new(scripts.clone(), HashMap::new());
    let executor = Executor::new(
        vec![rule("builtin:missing", HookEventName::Stop)],
        Arc::new(composite),
    );

    executor.dispatch(DispatchInput::new(HookEventName::Stop));
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        scripts.records().is_empty(),
        "未注册的内置 id 不得落到脚本路径"
    );
}
