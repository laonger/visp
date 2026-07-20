    use super::*;
    use tempfile::TempDir;

    // ---- ReadFile ----

    #[test]
    fn test_read_file_success() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let tool = ReadFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({"path": "test.txt"});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );
        assert_eq!(result.content, "hello world");
    }

    #[test]
    fn test_read_file_not_found() {
        let tmp = TempDir::new().unwrap();
        let tool = ReadFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({"path": "nonexistent.txt"});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(result.is_error);
    }

    #[test]
    fn test_read_file_too_large() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("large.txt");
        let content = vec![b'a'; (MAX_FILE_SIZE + 1) as usize];
        std::fs::write(&file_path, &content).unwrap();

        let tool = ReadFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({"path": "large.txt"});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(result.is_error);
        assert!(result.content.contains("too large"));
    }

    #[test]
    fn test_read_file_binary() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("binary.bin");
        let mut content = vec![0u8; 2000]; // 2000 null bytes > 10% of 8000
        content[0] = b'h';
        content[1] = b'i';
        std::fs::write(&file_path, &content).unwrap();

        let tool = ReadFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({"path": "binary.bin"});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(result.is_error);
        assert!(result.content.contains("binary"));
    }

    #[test]
    fn test_read_file_line_range_start_only() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("ranges.txt");
        let lines: Vec<String> = (1..=20).map(|i| format!("line {}", i)).collect();
        std::fs::write(&file_path, lines.join("\n")).unwrap();

        let tool = ReadFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({
            "path": "ranges.txt",
            "start_line": 5
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );
        assert!(result.content.contains("lines 5-20, 16 of 20 lines"));
        assert!(result.content.contains("line 5"));
        assert!(result.content.contains("line 20"));
        assert!(!result.content.contains("line 4"));
    }

    #[test]
    fn test_read_file_line_range_both() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("ranges.txt");
        let lines: Vec<String> = (1..=20).map(|i| format!("line {}", i)).collect();
        std::fs::write(&file_path, lines.join("\n")).unwrap();

        let tool = ReadFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({
            "path": "ranges.txt",
            "start_line": 5,
            "end_line": 8
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );
        assert!(result.content.contains("lines 5-8, 4 of 20 lines"));
        assert!(result.content.contains("line 5"));
        assert!(result.content.contains("line 6"));
        assert!(result.content.contains("line 7"));
        assert!(result.content.contains("line 8"));
        assert!(!result.content.contains("line 4"));
        assert!(!result.content.contains("line 9"));
    }

    #[test]
    fn test_read_file_line_range_single_line() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("single.txt");
        let lines: Vec<String> = (1..=5).map(|i| format!("line {}", i)).collect();
        std::fs::write(&file_path, lines.join("\n")).unwrap();

        let tool = ReadFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({
            "path": "single.txt",
            "start_line": 3,
            "end_line": 3
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );
        assert!(result.content.contains("(lines 3-3, 1 of 5 lines)"));
        assert!(result.content.contains("line 3"));
    }

    #[test]
    fn test_read_file_line_range_start_exceeds() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("small.txt");
        std::fs::write(&file_path, "a\nb\nc\n").unwrap();

        let tool = ReadFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({
            "path": "small.txt",
            "start_line": 100
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(result.is_error);
        assert!(result.content.contains("exceeds file length"));
    }

    #[test]
    fn test_read_file_line_range_end_exceeds() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("small.txt");
        std::fs::write(&file_path, "a\nb\nc\n").unwrap();

        let tool = ReadFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({
            "path": "small.txt",
            "start_line": 1,
            "end_line": 100
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(result.is_error);
        assert!(result.content.contains("exceeds file length"));
    }

    #[test]
    fn test_read_file_line_range_start_after_end() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("small.txt");
        std::fs::write(&file_path, "a\nb\nc\nd\ne\n").unwrap();

        let tool = ReadFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({
            "path": "small.txt",
            "start_line": 4,
            "end_line": 2
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(result.is_error);
        assert!(result.content.contains("start_line"));
        assert!(result.content.contains("after end_line"));
    }

    #[test]
    fn test_read_file_whole_file_still_works() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("whole.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let tool = ReadFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({"path": "whole.txt"});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );
        assert_eq!(result.content, "hello world");
    }

    #[test]
    fn test_read_file_paths() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn a() {}").unwrap();
        std::fs::write(tmp.path().join("b.rs"), "fn b() {}").unwrap();

        let tool = ReadFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({"paths": ["a.rs", "b.rs"]});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );
        assert!(result.content.contains("=== a.rs ==="));
        assert!(result.content.contains("=== b.rs ==="));
        assert!(result.content.contains("fn a() {}"));
        assert!(result.content.contains("fn b() {}"));
    }

    #[test]
    fn test_read_file_paths_partial_fail() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn a() {}").unwrap();

        let tool = ReadFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({"paths": ["a.rs", "nonexistent.rs"]});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        // 至少一个成功 => 整体成功
        assert!(
            !result.is_error,
            "expected success (best effort), got error: {}",
            result.content
        );
        assert!(result.content.contains("=== a.rs ==="));
        assert!(result.content.contains("fn a() {}"));
        assert!(result.content.contains("=== nonexistent.rs ==="));
        assert!(result.content.contains("Error:"));
    }

    #[test]
    fn test_read_file_path_and_paths_conflict() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("primary.txt"), "primary").unwrap();
        std::fs::write(tmp.path().join("secondary.txt"), "secondary").unwrap();

        let tool = ReadFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        // 同时传 path 和 paths，path 优先
        let args = serde_json::json!({
            "path": "primary.txt",
            "paths": ["secondary.txt"]
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );
        assert_eq!(result.content, "primary");
    }

    // ---- WriteFile ----

    #[test]
    fn test_write_file_success() {
        let tmp = TempDir::new().unwrap();
        let tool = WriteFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({"path": "written.txt", "content": "hello write"});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );

        let read_back = std::fs::read_to_string(tmp.path().join("written.txt")).unwrap();
        assert_eq!(read_back, "hello write");
    }

    #[test]
    fn test_write_file_auto_create_parent() {
        let tmp = TempDir::new().unwrap();
        let tool = WriteFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({"path": "subdir/nested/file.txt", "content": "nested"});

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );

        let target = tmp.path().join("subdir/nested/file.txt");
        assert!(target.exists(), "file should exist");
        let read_back = std::fs::read_to_string(&target).unwrap();
        assert_eq!(read_back, "nested");
    }

    // ---- EditFile ----

    #[test]
    fn test_edit_file_success() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("edit.txt");
        std::fs::write(&file_path, "hello world foo").unwrap();

        let tool = EditFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({
            "path": "edit.txt",
            "old_string": "world",
            "new_string": "there"
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );

        let read_back = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(read_back, "hello there foo");
    }

    #[test]
    fn test_edit_file_no_match() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("edit.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let tool = EditFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({
            "path": "edit.txt",
            "old_string": "xyz",
            "new_string": "abc"
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(result.is_error);
        assert!(result.content.contains("No matches"));
    }

    #[test]
    fn test_edit_file_multiple_matches() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("edit.txt");
        std::fs::write(&file_path, "foo bar foo baz foo").unwrap();

        let tool = EditFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({
            "path": "edit.txt",
            "old_string": "foo",
            "new_string": "xyz"
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(result.is_error);
        assert!(result.content.contains("matches"));
        // Should mention line/column positions
        assert!(result.content.contains("line"));
        assert!(result.content.contains("column"));
    }

    #[test]
    fn test_edit_file_atomic_write() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("edit.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let tool = EditFile::default();
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({
            "path": "edit.txt",
            "old_string": "world",
            "new_string": "there"
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(
            !result.is_error,
            "expected success, got error: {}",
            result.content
        );

        // 验证内容
        let read_back = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(read_back, "hello there");

        // 验证临时文件已清理
        let temp_name = format!(
            ".{}.visp-tmp",
            file_path.file_name().unwrap().to_string_lossy()
        );
        let temp_path = file_path.with_file_name(&temp_name);
        assert!(!temp_path.exists(), "temp file should be cleaned up");
    }

    // ── from_toml ─────────────────────────────────────────────────────────

    #[test]
    fn test_read_file_from_toml_default() {
        let tool = ReadFile::from_toml(None);
        assert_eq!(tool.max_file_size, MAX_FILE_SIZE);
    }

    #[test]
    fn test_read_file_from_toml_max_file_size() {
        let toml_str = "max_file_size = 500";
        let value: toml::Value = toml::from_str(toml_str).unwrap();
        let tool = ReadFile::from_toml(Some(&value));
        assert_eq!(tool.max_file_size, 500);
    }

    #[test]
    fn test_write_file_from_toml_default() {
        let tool = WriteFile::from_toml(None);
        assert_eq!(tool.max_file_size, MAX_FILE_SIZE);
    }

    #[test]
    fn test_edit_file_from_toml_default() {
        let tool = EditFile::from_toml(None);
        assert_eq!(tool.max_file_size, MAX_FILE_SIZE);
    }

    #[test]
    fn test_edit_file_too_large() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("edit_too_large.txt");
        // Write 600 bytes
        let content = vec![b'a'; 600];
        std::fs::write(&file_path, &content).unwrap();

        let tool = EditFile {
            max_file_size: 500,
            require_approval: false,
        };
        let ctx = ToolContext {
            working_dir: tmp.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let args = serde_json::json!({
            "path": "edit_too_large.txt",
            "old_string": "aaa",
            "new_string": "bbb"
        });

        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args, &ctx));
        assert!(result.is_error);
        assert!(result.content.contains("too large"));
    }
