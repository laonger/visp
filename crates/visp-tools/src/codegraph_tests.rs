    use super::*;

    #[test]
    fn test_context_name() {
        let tool = CodeGraphContext;
        assert_eq!(tool.name(), "codegraph_context");
    }

    #[test]
    fn test_context_category() {
        let tool = CodeGraphContext;
        assert_eq!(tool.category(), "analyze");
    }

    #[test]
    fn test_context_description_not_empty() {
        let tool = CodeGraphContext;
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn test_context_parameters_schema() {
        let tool = CodeGraphContext;
        let params = tool.parameters();
        let props = params["properties"].as_object().unwrap();
        assert!(props.contains_key("task"), "context needs task param");
        assert!(props.contains_key("detail"), "context needs detail param");
        assert!(
            props.contains_key("max_nodes"),
            "context needs max_nodes param"
        );
        let required = params["required"].as_array().unwrap();
        assert!(
            required.iter().any(|v| v == "task"),
            "task should be required"
        );
    }

    #[test]
    fn test_trace_name() {
        let tool = CodeGraphTrace;
        assert_eq!(tool.name(), "codegraph_trace");
    }

    #[test]
    fn test_trace_category() {
        let tool = CodeGraphTrace;
        assert_eq!(tool.category(), "analyze");
    }

    #[test]
    fn test_trace_description_not_empty() {
        let tool = CodeGraphTrace;
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn test_trace_parameters_schema() {
        let tool = CodeGraphTrace;
        let params = tool.parameters();
        let props = params["properties"].as_object().unwrap();
        assert!(props.contains_key("from"), "trace needs from param");
        assert!(props.contains_key("to"), "trace needs to param");
        let required = params["required"].as_array().unwrap();
        assert!(
            required.iter().any(|v| v == "from"),
            "from should be required"
        );
        assert!(required.iter().any(|v| v == "to"), "to should be required");
    }

    #[test]
    fn test_impact_name() {
        let tool = CodeGraphImpact;
        assert_eq!(tool.name(), "codegraph_impact");
    }

    #[test]
    fn test_impact_category() {
        let tool = CodeGraphImpact;
        assert_eq!(tool.category(), "analyze");
    }

    #[test]
    fn test_impact_description_not_empty() {
        let tool = CodeGraphImpact;
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn test_impact_parameters_schema() {
        let tool = CodeGraphImpact;
        let params = tool.parameters();
        let props = params["properties"].as_object().unwrap();
        assert!(props.contains_key("symbol"), "impact needs symbol param");
        assert!(props.contains_key("depth"), "impact needs depth param");
        let required = params["required"].as_array().unwrap();
        assert!(
            required.iter().any(|v| v == "symbol"),
            "symbol should be required"
        );
        assert!(
            !required.iter().any(|v| v == "depth"),
            "depth should not be required"
        );
    }

    #[test]
    fn test_impact_default_depth() {
        let tool = CodeGraphImpact;
        let params = tool.parameters();
        let depth = &params["properties"]["depth"];
        assert_eq!(depth["default"], 1);
    }

    #[test]
    fn test_codegraph_search_updated_description() {
        let tool = CodeGraphSearch;
        let desc = tool.description();
        assert!(
            !desc.contains("Slower than Grep"),
            "description should not mention Grep speed comparison"
        );
    }
