    use super::*;

    #[test]
    fn test_short_content_not_truncated() {
        let result = truncate_output("hello world", DEFAULT_MAX_OUTPUT_BYTES);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_long_content_truncated() {
        let long = "a".repeat(DEFAULT_MAX_OUTPUT_BYTES + 1);
        let result = truncate_output(&long, DEFAULT_MAX_OUTPUT_BYTES);
        assert_ne!(result, long, "result should differ from original");
        assert!(
            result.contains("truncated at"),
            "result should contain truncation hint"
        );
        assert!(
            result.ends_with("]"),
            "result should end with truncation message"
        );
    }

    #[test]
    fn test_exact_boundary_not_truncated() {
        let exact = "a".repeat(DEFAULT_MAX_OUTPUT_BYTES);
        let result = truncate_output(&exact, DEFAULT_MAX_OUTPUT_BYTES);
        assert_eq!(result.len(), DEFAULT_MAX_OUTPUT_BYTES);
    }
