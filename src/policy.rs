use crate::config::{MatchMode, Policy};
use regex::Regex;

/// Check a tool name against the policy list. First match wins.
/// `tool_input` is the JSON string of tool input (used for pattern matching, e.g. Bash commands).
/// Returns Some("allow"), Some("deny"), or None (meaning "ask" / no match).
pub fn check(tool_name: &str, tool_input: &str, policies: &[Policy]) -> Option<String> {
    for policy in policies {
        if matches_tool(&policy.tool, tool_name)
            && matches_input_pattern(policy, tool_name, tool_input)
        {
            return match policy.action.as_str() {
                "allow" => Some("allow".into()),
                "deny" => Some("deny".into()),
                "ask" => None, // explicit ask = treat as no auto-decision
                _ => None,
            };
        }
    }
    None
}

/// Check if the tool name matches the policy's tool field (glob-style).
fn matches_tool(pattern: &str, tool_name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') && !pattern.contains('?') {
        return pattern == tool_name;
    }
    glob_match(pattern, tool_name)
}

/// Check if the tool input matches the policy's optional pattern.
/// If no pattern is specified, matches everything (backwards compatible).
fn matches_input_pattern(policy: &Policy, tool_name: &str, tool_input: &str) -> bool {
    let pattern = match &policy.pattern {
        Some(p) => p,
        None => return true, // no pattern = match all
    };

    match policy.match_mode {
        MatchMode::Domain => {
            // Domain matching: extract URL from tool_input, check host against pattern
            let url = extract_json_field(tool_input, "url");
            match url {
                Some(url_str) => matches_domain(&url_str, pattern),
                None => false,
            }
        }
        MatchMode::Regex => {
            // Extract the relevant field based on tool type
            let extracted = extract_match_text(tool_name, tool_input);
            let text = extracted.as_deref().unwrap_or(tool_input);

            match Regex::new(pattern) {
                Ok(re) => re.is_match(text),
                Err(_) => false, // invalid regex = no match
            }
        }
    }
}

/// Extract the text to match against based on the tool type.
/// Each tool stores its matchable content in a different JSON field.
fn extract_match_text(tool_name: &str, tool_input: &str) -> Option<String> {
    match tool_name {
        "Bash" => extract_json_field(tool_input, "command"),
        "Read" | "Edit" | "Write" => extract_json_field(tool_input, "file_path"),
        "Glob" | "Grep" | "LS" => extract_json_field(tool_input, "path"),
        "WebFetch" => extract_json_field(tool_input, "url"),
        _ => None,
    }
}

/// Extract a string field from a JSON tool_input string.
fn extract_json_field(tool_input: &str, field: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(tool_input)
        .ok()
        .and_then(|v| v.get(field).and_then(|c| c.as_str()).map(String::from))
}

/// Check if a URL's host matches the given domain pattern.
/// Matches if the host equals the domain or ends with ".{domain}".
fn matches_domain(url: &str, domain: &str) -> bool {
    // Extract host from URL: skip scheme, take up to next / or :
    let after_scheme = url.find("://").map(|i| &url[i + 3..]).unwrap_or(url);
    let host = after_scheme
        .split(['/', ':', '?', '#'])
        .next()
        .unwrap_or(after_scheme);

    host == domain || host.ends_with(&format!(".{domain}"))
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let mut dp = vec![vec![false; txt.len() + 1]; pat.len() + 1];
    dp[0][0] = true;

    for i in 1..=pat.len() {
        if pat[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }

    for i in 1..=pat.len() {
        for j in 1..=txt.len() {
            if pat[i - 1] == '*' {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else if pat[i - 1] == '?' || pat[i - 1] == txt[j - 1] {
                dp[i][j] = dp[i - 1][j - 1];
            }
        }
    }

    dp[pat.len()][txt.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MatchMode;

    fn policy(tool: &str, action: &str, pattern: Option<&str>) -> Policy {
        Policy {
            tool: tool.into(),
            action: action.into(),
            pattern: pattern.map(String::from),
            match_mode: MatchMode::default(),
        }
    }

    fn domain_policy(tool: &str, action: &str, domain: &str) -> Policy {
        Policy {
            tool: tool.into(),
            action: action.into(),
            pattern: Some(domain.into()),
            match_mode: MatchMode::Domain,
        }
    }

    #[test]
    fn test_exact_match() {
        let policies = vec![policy("Read", "allow", None)];
        assert_eq!(check("Read", "", &policies), Some("allow".into()));
        assert_eq!(check("Write", "", &policies), None);
    }

    #[test]
    fn test_wildcard() {
        let policies = vec![policy("*", "deny", None)];
        assert_eq!(check("Anything", "", &policies), Some("deny".into()));
    }

    #[test]
    fn test_first_match_wins() {
        let policies = vec![policy("Bash", "deny", None), policy("*", "allow", None)];
        assert_eq!(check("Bash", "", &policies), Some("deny".into()));
        assert_eq!(check("Read", "", &policies), Some("allow".into()));
    }

    #[test]
    fn test_ask_returns_none() {
        let policies = vec![policy("Bash", "ask", None)];
        assert_eq!(check("Bash", "", &policies), None);
    }

    #[test]
    fn test_glob_pattern() {
        let policies = vec![policy("mcp__*", "deny", None)];
        assert_eq!(check("mcp__chrome", "", &policies), Some("deny".into()));
        assert_eq!(check("Read", "", &policies), None);
    }

    #[test]
    fn test_bash_pattern_allow_safe_commands() {
        let policies = vec![
            policy(
                "Bash",
                "allow",
                Some(r"^(ls|git status|git log|git diff|cargo build|cargo test)"),
            ),
            policy("Bash", "deny", None),
        ];

        let safe_input = r#"{"command": "git status"}"#;
        assert_eq!(check("Bash", safe_input, &policies), Some("allow".into()));

        let safe_input2 = r#"{"command": "cargo test"}"#;
        assert_eq!(check("Bash", safe_input2, &policies), Some("allow".into()));

        // Unsafe command falls through to the deny-all Bash policy
        let unsafe_input = r#"{"command": "rm -rf /"}"#;
        assert_eq!(check("Bash", unsafe_input, &policies), Some("deny".into()));
    }

    #[test]
    fn test_bash_pattern_no_match_falls_through() {
        let policies = vec![policy("Bash", "allow", Some(r"^ls$"))];

        let ls_input = r#"{"command": "ls"}"#;
        assert_eq!(check("Bash", ls_input, &policies), Some("allow".into()));

        let other_input = r#"{"command": "rm -rf /"}"#;
        assert_eq!(check("Bash", other_input, &policies), None);
    }

    #[test]
    fn test_pattern_with_non_json_input() {
        let policies = vec![policy("Bash", "allow", Some(r"ls"))];

        // Non-JSON input: regex matches against raw string
        assert_eq!(check("Bash", "ls -la", &policies), Some("allow".into()));
        assert_eq!(check("Bash", "rm -rf", &policies), None);
    }

    #[test]
    fn test_pattern_does_not_affect_other_tools() {
        let policies = vec![
            policy("Bash", "allow", Some(r"^ls$")),
            policy("Read", "allow", None),
        ];

        // Read has no pattern, so it matches regardless of input
        assert_eq!(check("Read", "", &policies), Some("allow".into()));
    }

    #[test]
    fn test_invalid_regex_no_match() {
        let policies = vec![policy("Bash", "allow", Some(r"[invalid"))];
        assert_eq!(check("Bash", r#"{"command": "ls"}"#, &policies), None);
    }

    // --- New tests for tool-aware field extraction ---

    #[test]
    fn test_read_matches_file_path() {
        let policies = vec![policy("Read", "allow", Some(r"^/Users/wtj/src/"))];

        let input = r#"{"file_path": "/Users/wtj/src/foo.rs"}"#;
        assert_eq!(check("Read", input, &policies), Some("allow".into()));

        let input2 = r#"{"file_path": "/etc/passwd"}"#;
        assert_eq!(check("Read", input2, &policies), None);
    }

    #[test]
    fn test_edit_matches_file_path() {
        let policies = vec![policy("Edit", "allow", Some(r"^/Users/wtj/"))];

        let input =
            r#"{"file_path": "/Users/wtj/code/main.rs", "old_string": "x", "new_string": "y"}"#;
        assert_eq!(check("Edit", input, &policies), Some("allow".into()));

        let input2 = r#"{"file_path": "/tmp/evil.sh", "old_string": "x", "new_string": "y"}"#;
        assert_eq!(check("Edit", input2, &policies), None);
    }

    #[test]
    fn test_write_matches_file_path() {
        let policies = vec![policy("Write", "deny", Some(r"\.env$"))];

        let input = r#"{"file_path": "/app/.env", "content": "SECRET=x"}"#;
        assert_eq!(check("Write", input, &policies), Some("deny".into()));

        let input2 = r#"{"file_path": "/app/config.toml", "content": "x"}"#;
        assert_eq!(check("Write", input2, &policies), None);
    }

    #[test]
    fn test_webfetch_matches_url() {
        let policies = vec![policy("WebFetch", "allow", Some(r"^https://docs\.rs/"))];

        let input = r#"{"url": "https://docs.rs/serde/latest"}"#;
        assert_eq!(check("WebFetch", input, &policies), Some("allow".into()));

        let input2 = r#"{"url": "https://evil.com/payload"}"#;
        assert_eq!(check("WebFetch", input2, &policies), None);
    }

    #[test]
    fn test_webfetch_domain_matching() {
        let policies = vec![domain_policy("WebFetch", "allow", "example.com")];

        // Exact match
        let input = r#"{"url": "https://example.com/path"}"#;
        assert_eq!(check("WebFetch", input, &policies), Some("allow".into()));

        // Subdomain match
        let input2 = r#"{"url": "https://api.example.com/v1"}"#;
        assert_eq!(check("WebFetch", input2, &policies), Some("allow".into()));

        // Different domain — no match
        let input3 = r#"{"url": "https://evil.com/path"}"#;
        assert_eq!(check("WebFetch", input3, &policies), None);

        // Suffix attack — notexample.com should NOT match
        let input4 = r#"{"url": "https://notexample.com/path"}"#;
        assert_eq!(check("WebFetch", input4, &policies), None);
    }

    #[test]
    fn test_domain_matching_helper() {
        assert!(matches_domain("https://example.com/path", "example.com"));
        assert!(matches_domain("https://api.example.com/v1", "example.com"));
        assert!(matches_domain("http://example.com", "example.com"));
        assert!(matches_domain(
            "https://example.com:8080/path",
            "example.com"
        ));
        assert!(!matches_domain(
            "https://notexample.com/path",
            "example.com"
        ));
        assert!(!matches_domain("https://evil.com/path", "example.com"));
    }

    #[test]
    fn test_grep_matches_path() {
        let policies = vec![policy("Grep", "allow", Some(r"^/Users/wtj/"))];

        let input = r#"{"pattern": "TODO", "path": "/Users/wtj/project"}"#;
        assert_eq!(check("Grep", input, &policies), Some("allow".into()));

        let input2 = r#"{"pattern": "TODO", "path": "/etc"}"#;
        assert_eq!(check("Grep", input2, &policies), None);
    }

    #[test]
    fn test_ls_matches_path() {
        let policies = vec![policy("LS", "allow", Some(r"^src"))];
        let input = r#"{"path": "src"}"#;
        assert_eq!(check("LS", input, &policies), Some("allow".into()));
    }
}
