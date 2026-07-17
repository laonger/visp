    use super::*;

    #[test]
    fn test_parse_valid_event() {
        let input = "event: message\ndata: {\"hello\":\"world\"}\n\n";
        let events = parse_sse_events(input);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("message"));
        assert_eq!(events[0].data.as_deref(), Some(r#"{"hello":"world"}"#));
    }

    #[test]
    fn test_parse_missing_event() {
        let input = "data: {\"key\":\"val\"}\n\n";
        let events = parse_sse_events(input);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, None);
        assert_eq!(events[0].data.as_deref(), Some(r#"{"key":"val"}"#));
    }

    #[test]
    fn test_parse_empty_input() {
        let input = "\n\n";
        let events = parse_sse_events(input);
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_parse_data_across_lines() {
        let input = "data: line1\ndata: line2\ndata: line3\n\n";
        let events = parse_sse_events(input);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, None);
        assert_eq!(events[0].data.as_deref(), Some("line1\nline2\nline3"));
    }
