    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_grep_skips_binary() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"hello world\n").unwrap();
        fs::write(dir.path().join("b.bin"), b"hello\0binary\n").unwrap();

        let grep = Grep::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let result = grep
            .execute(serde_json::json!({"pattern": "hello"}), &ctx)
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(
            result.content.contains("a.txt"),
            "should find a.txt:\n{}",
            result.content
        );
        assert!(
            !result.content.contains("b.bin"),
            "should NOT find b.bin:\n{}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_grep_missing_pattern() {
        let dir = tempdir().unwrap();
        let grep = Grep::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let result = grep.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_error);
        assert!(result.content.contains("pattern"));
    }

    #[tokio::test]
    async fn test_glob_basic() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "").unwrap();
        fs::write(dir.path().join("b.rs"), "").unwrap();
        fs::write(dir.path().join("c.txt"), "").unwrap();

        let glob = Glob::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let result = glob
            .execute(serde_json::json!({"pattern": "*.rs"}), &ctx)
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(
            result.content.contains("a.rs"),
            "should find a.rs:\n{}",
            result.content
        );
        assert!(
            result.content.contains("b.rs"),
            "should find b.rs:\n{}",
            result.content
        );
        assert!(
            !result.content.contains("c.txt"),
            "should not find c.txt:\n{}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_glob_nested() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("nested.rs"), "").unwrap();
        fs::write(dir.path().join("root.rs"), "").unwrap();

        let glob = Glob::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let result = glob
            .execute(serde_json::json!({"pattern": "*.rs"}), &ctx)
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(
            result.content.contains("root.rs"),
            "should find root.rs:\n{}",
            result.content
        );
        assert!(
            result.content.contains("nested.rs"),
            "should find nested.rs:\n{}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_glob_missing_pattern() {
        let dir = tempdir().unwrap();
        let glob = Glob::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let result = glob.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_error);
        assert!(result.content.contains("pattern"));
    }

    // ── from_toml 测试 ──────────────────────────────────────────────────────

    #[test]
    fn test_grep_from_toml_default() {
        let grep = Grep::from_toml(None);
        assert_eq!(grep.timeout_secs, GREP_TIMEOUT_SECS);
        assert!(grep.exclude_dirs.is_empty());
    }

    #[test]
    fn test_grep_from_toml_exclude_dirs() {
        let value: toml::Value = toml::from_str(
            r#"
timeout_secs = 60
exclude_dirs = ["vendor", ".build"]
"#,
        )
        .unwrap();
        let grep = Grep::from_toml(Some(&value));
        assert_eq!(grep.timeout_secs, 60);
        assert_eq!(grep.exclude_dirs, vec!["vendor", ".build"]);
    }

    #[test]
    fn test_grep_from_toml_zero_timeout_ignored() {
        let value: toml::Value = toml::from_str(
            r#"
timeout_secs = 0
"#,
        )
        .unwrap();
        let grep = Grep::from_toml(Some(&value));
        assert_eq!(grep.timeout_secs, GREP_TIMEOUT_SECS);
    }

    #[test]
    fn test_glob_from_toml_default() {
        let glob = Glob::from_toml(None);
        assert_eq!(glob.timeout_secs, GREP_TIMEOUT_SECS);
    }

    #[test]
    fn test_glob_from_toml_timeout() {
        let value: toml::Value = toml::from_str(
            r#"
timeout_secs = 45
"#,
        )
        .unwrap();
        let glob = Glob::from_toml(Some(&value));
        assert_eq!(glob.timeout_secs, 45);
    }

    #[test]
    fn test_glob_from_toml_zero_timeout_ignored() {
        let value: toml::Value = toml::from_str(
            r#"
timeout_secs = 0
"#,
        )
        .unwrap();
        let glob = Glob::from_toml(Some(&value));
        assert_eq!(glob.timeout_secs, GREP_TIMEOUT_SECS);
    }

    // ── grep 新参数测试 ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_grep_with_context() {
        let dir = tempdir().unwrap();
        // Create a file with context around the match
        fs::write(
            dir.path().join("main.rs"),
            "line1\nline2\nline3\nMATCH\nline4\nline5\nline6\n",
        )
        .unwrap();

        let grep = Grep::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let result = grep
            .execute(serde_json::json!({"pattern": "MATCH", "context": 2}), &ctx)
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        // With context=2, we should see 2 lines before and 2 lines after
        // File layout: line1(3-before), line2(2-before), line3(1-before), MATCH, line4(1-after), line5(2-after), line6(3-after)
        assert!(
            result.content.contains("line2"),
            "should contain line2 (2 lines before MATCH):\n{}",
            result.content
        );
        assert!(
            result.content.contains("line3"),
            "should contain line3 (1 line before MATCH):\n{}",
            result.content
        );
        assert!(
            result.content.contains("line4"),
            "should contain line4 (1 line after MATCH):\n{}",
            result.content
        );
        assert!(
            result.content.contains("line5"),
            "should contain line5 (2 lines after MATCH):\n{}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_grep_with_include() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "hello\n").unwrap();
        fs::write(dir.path().join("b.txt"), "hello\n").unwrap();

        let grep = Grep::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let result = grep
            .execute(
                serde_json::json!({"pattern": "hello", "include": "*.rs"}),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        if has_rg() {
            // With rg, include restricts to *.rs files
            assert!(
                result.content.contains("a.rs"),
                "should find a.rs:\n{}",
                result.content
            );
            assert!(
                !result.content.contains("b.txt"),
                "should NOT find b.txt:\n{}",
                result.content
            );
        } else {
            // Without rg, include is ignored; grep finds both
            assert!(
                result.content.contains("hello"),
                "should find matches:\n{}",
                result.content
            );
        }
    }

    #[tokio::test]
    async fn test_grep_with_max_matches() {
        let dir = tempdir().unwrap();
        // Create a file with 10 matching lines
        let content: String = (0..10).map(|i| format!("match line {}\n", i)).collect();
        fs::write(dir.path().join("data.txt"), content).unwrap();

        let grep = Grep::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        let result = grep
            .execute(
                serde_json::json!({"pattern": "match", "max_matches": 3}),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        // Should find at most 3 matches
        let match_count = result.content.matches("match line").count();
        assert!(
            match_count <= 3,
            "expected ≤3 matches, got {}:\n{}",
            match_count,
            result.content
        );
    }

    #[tokio::test]
    async fn test_grep_context_clamped() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("main.rs"),
            "a\nb\nc\nd\ne\nMATCH\nf\ng\nh\ni\nj\n",
        )
        .unwrap();

        let grep = Grep::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        // context=100 should be clamped to 50; at minimum no crash
        let result = grep
            .execute(
                serde_json::json!({"pattern": "MATCH", "context": 100}),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        // Should still find the match
        assert!(
            result.content.contains("MATCH"),
            "should find MATCH:\n{}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_grep_max_matches_clamped() {
        let dir = tempdir().unwrap();
        let content: String = (0..5).map(|i| format!("match line {}\n", i)).collect();
        fs::write(dir.path().join("data.txt"), content).unwrap();

        let grep = Grep::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };
        // max_matches=0 should be clamped to 1
        let result = grep
            .execute(
                serde_json::json!({"pattern": "match", "max_matches": 0}),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        // Should find at least 1 match
        assert!(
            result.content.contains("match line"),
            "should find at least one match:\n{}",
            result.content
        );
    }
