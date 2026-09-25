#![cfg(test)]
use super::*;
use crate::store::Store;
use tokio::sync::mpsc;
use visp_fs::normalize::EventType;
use visp_fs::runtime::{FileEvent as FsFileEvent, WatchMessage};

// ------------------------------------------------------------------
//  Helpers
// ------------------------------------------------------------------

fn setup() -> (
    tempfile::TempDir,
    PathBuf,
    Arc<Store>,
    Arc<Indexer>,
    CodeGraphConfig,
) {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let db_path = tmp.path().join("test.db");
    let store = Arc::new(Store::open(&db_path).unwrap());
    let indexer = Arc::new(Indexer::new(store.clone()));
    let config = CodeGraphConfig::default();

    (tmp, project, store, indexer, config)
}

fn valid_ts(name: &str) -> String {
    format!("export function {}() {{}}\n", name)
}

/// 在有限时间内轮询，直到谓词成立（最终收敛断言，后端无关）。
async fn wait_until(mut pred: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if pred() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return pred();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn symbol_names(store: &Store) -> Vec<String> {
    store
        .search_symbols("", 100)
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect()
}

// ------------------------------------------------------------------
//  1. 子树内既有源文件原地覆写 → 索引更新
// ------------------------------------------------------------------

#[tokio::test]
async fn test_watcher_in_place_overwrite_updates_index() {
    let (_tmp, project, store, indexer, config) = setup();

    // 监听启动前已存在（覆盖 kqueue 目录 diff 的盲区）。
    let file_path = project.join("a.ts");
    std::fs::write(&file_path, valid_ts("original_fn")).unwrap();

    let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await; // let watcher settle

    // 原地覆写
    std::fs::write(&file_path, valid_ts("overwritten_fn")).unwrap();

    assert!(
        wait_until(|| symbol_names(&store).contains(&"overwritten_fn".to_string())).await,
        "原地覆写后应索引新符号，实际：{:?}",
        symbol_names(&store)
    );
    assert!(
        !symbol_names(&store).contains(&"original_fn".to_string()),
        "旧符号应被替换"
    );
}

// ------------------------------------------------------------------
//  2. 原子替换（tmp + rename）→ 索引更新
// ------------------------------------------------------------------

#[tokio::test]
async fn test_watcher_atomic_replace_updates_index() {
    let (_tmp, project, store, indexer, config) = setup();

    let file_path = project.join("a.ts");
    std::fs::write(&file_path, valid_ts("before_replace")).unwrap();

    let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // tmp + rename 覆盖同名路径
    let tmp_path = project.join("a.ts.tmp");
    std::fs::write(&tmp_path, valid_ts("after_replace")).unwrap();
    std::fs::rename(&tmp_path, &file_path).unwrap();

    assert!(
        wait_until(|| symbol_names(&store).contains(&"after_replace".to_string())).await,
        "原子替换后应索引新符号，实际：{:?}",
        symbol_names(&store)
    );
}

// ------------------------------------------------------------------
//  3. 重命名语义：旧路径残留行清理 + 新路径重插
// ------------------------------------------------------------------

#[tokio::test]
async fn test_watcher_rename_cleans_old_rows_and_indexes_new() {
    let (_tmp, project, store, indexer, config) = setup();

    // a.ts 自身有 import（制造 imports 行）并被 main.ts 引用（制造跨文件目标边，
    // 重命名旧路径后该边不得悬空）。
    std::fs::write(project.join("helper.ts"), "export function helper() {}\n").unwrap();
    std::fs::write(
        project.join("a.ts"),
        "import { helper } from \"./helper\";\nexport function foo() { helper(); }\n",
    )
    .unwrap();
    std::fs::write(
        project.join("main.ts"),
        "import { foo } from \"./a\";\nfoo();\n",
    )
    .unwrap();

    // 先全量索引，确保旧路径的 imports/exports/files/edges 行确实存在。
    indexer.build_full(&project, &config).unwrap();
    let before = store.count_file_rows("a.ts");
    assert_eq!(before.symbols, 1, "前提：全量索引后 a.ts 应有符号行");
    assert!(
        before.imports >= 1 && before.exports >= 1 && before.files >= 1,
        "前提：全量索引后 a.ts 应有 imports/exports/files 行，实际：{before:?}"
    );

    let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 重命名 a.ts → renamed.ts
    std::fs::rename(project.join("a.ts"), project.join("renamed.ts")).unwrap();

    assert!(
        wait_until(|| {
            let rows = store.count_file_rows("renamed.ts");
            rows.symbols == 1 && rows.files == 1
        })
        .await,
        "新路径应被重新索引，实际 renamed.ts={:?}",
        store.count_file_rows("renamed.ts")
    );

    let old = store.count_file_rows("a.ts");
    assert_eq!(old.symbols, 0, "旧路径符号行应被清理");
    assert_eq!(old.imports, 0, "旧路径 imports 行应被清理");
    assert_eq!(old.exports, 0, "旧路径 exports 行应被清理");
    assert_eq!(old.files, 0, "旧路径 files 行应被清理");
    assert_eq!(
        store.count_dangling_edges(),
        0,
        "不得遗留 target_id/target_name 均为 NULL 的悬空边"
    );
    assert!(
        store.get_unresolved_edges().unwrap().is_empty(),
        "重命名后跨文件引用应重新解析，实际未解析：{:?}",
        store.get_unresolved_edges().unwrap()
    );
}

// ------------------------------------------------------------------
//  4. exclude 目录内变更零触发
// ------------------------------------------------------------------

#[tokio::test]
async fn test_watcher_excluded_dir_produces_no_events() {
    let (_tmp, project, store, indexer, config) = setup();

    let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // node_modules 内变更应被排除。分段进行，避免依赖「同一窗口多文件创建」
    // 这一设计明确不承诺的 kqueue 行为。
    std::fs::create_dir_all(project.join("node_modules")).unwrap();
    std::fs::write(
        project.join("node_modules/ignored.ts"),
        valid_ts("ignored_fn"),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(
        !symbol_names(&store).contains(&"ignored_fn".to_string()),
        "exclude 目录内变更不得触发索引"
    );

    // 对照文件在独立窗口创建，证明监听确实在工作。
    std::fs::create_dir_all(project.join("src")).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    std::fs::write(project.join("src/ok.ts"), valid_ts("ok_fn")).unwrap();
    assert!(
        wait_until(|| symbol_names(&store).contains(&"ok_fn".to_string())).await,
        "对照文件应被索引，证明监听确实在工作"
    );
}

// ------------------------------------------------------------------
//  5. 无关扩展名零触发
// ------------------------------------------------------------------

#[tokio::test]
async fn test_watcher_unsupported_extension_produces_no_events() {
    let (_tmp, project, store, indexer, config) = setup();

    let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // .json 不在包含规则内。分段进行，避免依赖「同一窗口多文件创建」
    // 这一设计明确不承诺的 kqueue 行为。
    std::fs::write(project.join("a.json"), "{}").unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(symbol_names(&store).is_empty(), "无关扩展名不得触发索引");

    // 对照文件在独立窗口创建，证明监听确实在工作。
    std::fs::write(project.join("a.ts"), valid_ts("canary_fn")).unwrap();
    assert!(
        wait_until(|| symbol_names(&store).contains(&"canary_fn".to_string())).await,
        "对照文件应被索引，证明监听确实在工作"
    );
    assert_eq!(
        symbol_names(&store),
        vec!["canary_fn".to_string()],
        "无关扩展名不得产生任何索引行"
    );
}

// ------------------------------------------------------------------
//  6. 忽略重扫信号（设计 §4.7 决策）
// ------------------------------------------------------------------

#[tokio::test]
async fn test_watcher_ignores_rescan_signal() {
    let (_tmp, project, store, indexer, _config) = setup();

    let (tx, rx) = mpsc::unbounded_channel::<WatchMessage>();
    let handle = tokio::spawn(consume_events(rx, indexer, project.clone()));

    // 重扫信号不得驱动任何索引行为（codegraph 为纯增量消费者）。
    tx.send(WatchMessage::Rescan).unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        store.search_symbols("", 100).unwrap().is_empty(),
        "codegraph 应忽略 visp-fs 的重扫信号"
    );

    // 信号之后消费循环仍正常处理文件事件。
    let file = project.join("a.ts");
    std::fs::write(&file, valid_ts("after_rescan")).unwrap();
    tx.send(WatchMessage::Event(FsFileEvent {
        path: file,
        kind: EventType::Created,
    }))
    .unwrap();
    assert!(
        wait_until(|| symbol_names(&store).contains(&"after_rescan".to_string())).await,
        "重扫信号不得破坏后续事件消费"
    );

    drop(tx);
    let _ = handle.await;
}

// ------------------------------------------------------------------
//  7. 删除文件 → 索引条目移除
// ------------------------------------------------------------------

#[tokio::test]
async fn test_watcher_file_delete_removes_from_index() {
    let (_tmp, project, store, indexer, config) = setup();

    std::fs::write(project.join("a.ts"), valid_ts("will_be_deleted")).unwrap();
    indexer.build_full(&project, &config).unwrap();
    assert_eq!(symbol_names(&store), vec!["will_be_deleted".to_string()]);

    let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    std::fs::remove_file(project.join("a.ts")).unwrap();

    assert!(
        wait_until(|| symbol_names(&store).is_empty()).await,
        "删除后索引条目应被移除，实际：{:?}",
        symbol_names(&store)
    );
}

// ------------------------------------------------------------------
//  8. 快速连续写入 → debounce 收敛到最终状态
// ------------------------------------------------------------------

#[tokio::test]
async fn test_watcher_rapid_writes_converge_to_last() {
    let (_tmp, project, store, indexer, config) = setup();

    let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let file_path = project.join("a.ts");
    std::fs::write(&file_path, valid_ts("first")).unwrap();
    std::fs::write(&file_path, valid_ts("second")).unwrap();
    std::fs::write(&file_path, valid_ts("third")).unwrap();

    assert!(
        wait_until(|| symbol_names(&store) == vec!["third".to_string()]).await,
        "debounce 应收敛到最终状态，实际：{:?}",
        symbol_names(&store)
    );
}

// ------------------------------------------------------------------
//  9. Watcher.stop() 停止处理
// ------------------------------------------------------------------

#[tokio::test]
async fn test_watcher_stop() {
    let (_tmp, project, store, indexer, config) = setup();

    let watcher = Watcher::start(&project, indexer, config).await.unwrap();
    watcher.stop(); // Should not panic

    // Write after stop should not be indexed
    let file_path = project.join("a.ts");
    std::fs::write(&file_path, valid_ts("after_stop")).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;

    assert!(
        store.search_symbols("", 100).unwrap().is_empty(),
        "no symbols should appear after watcher stopped"
    );
}
