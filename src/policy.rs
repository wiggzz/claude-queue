use crate::config::Policy;
use regex::Regex;

/// Check a tool name against the policy list. First match wins.
/// `tool_input` is the JSON string of tool input (used for pattern matching, e.g. Bash commands).
/// Returns Some("allow"), Some("deny"), or None (meaning "ask" / no match).
pub fn check(tool_name: &str, tool_input: &str, policies: &[Policy]) -> Option<String> {
    for policy in policies {
        if matches_tool(&policy.tool, tool_name) && matches_input_pattern(policy, tool_input) {
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

/// Check if the tool input matches the policy's optional pattern (regex).
/// If no pattern is specified, matches everything (backwards compatible).
fn matches_input_pattern(policy: &Policy, tool_input: &str) -> bool {
    let pattern = match &policy.pattern {
        Some(p) => p,
        None => return true, // no pattern = match all
    };

    // Extract the "command" field from JSON tool_input for Bash-like matching
    let command = extract_command(tool_input);
    let text = command.as_deref().unwrap_or(tool_input);

    match Regex::new(pattern) {
        Ok(re) => re.is_match(text),
        Err(_) => false, // invalid regex = no match
    }
}

/// Extract the "command" field from a JSON tool_input string.
fn extract_command(tool_input: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(tool_input)
        .ok()
        .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(String::from))
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

    #[test]
    fn test_exact_match() {
        let policies = vec![Policy {
            tool: "Read".into(),
            action: "allow".into(),
            pattern: None,
        }];
        assert_eq!(check("Read", "", &policies), Some("allow".into()));
        assert_eq!(check("Write", "", &policies), None);
    }

    #[test]
    fn test_wildcard() {
        let policies = vec![Policy {
            tool: "*".into(),
            action: "deny".into(),
            pattern: None,
        }];
        assert_eq!(check("Anything", "", &policies), Some("deny".into()));
    }

    #[test]
    fn test_first_match_wins() {
        let policies = vec![
            Policy {
                tool: "Bash".into(),
                action: "deny".into(),
                pattern: None,
            },
            Policy {
                tool: "*".into(),
                action: "allow".into(),
                pattern: None,
            },
        ];
        assert_eq!(check("Bash", "", &policies), Some("deny".into()));
        assert_eq!(check("Read", "", &policies), Some("allow".into()));
    }

    #[test]
    fn test_ask_returns_none() {
        let policies = vec![Policy {
            tool: "Bash".into(),
            action: "ask".into(),
            pattern: None,
        }];
        assert_eq!(check("Bash", "", &policies), None);
    }

    #[test]
    fn test_glob_pattern() {
        let policies = vec![Policy {
            tool: "mcp__*".into(),
            action: "deny".into(),
            pattern: None,
        }];
        assert_eq!(check("mcp__chrome", "", &policies), Some("deny".into()));
        assert_eq!(check("Read", "", &policies), None);
    }

    #[test]
    fn test_bash_pattern_allow_safe_commands() {
        let policies = vec![
            Policy {
                tool: "Bash".into(),
                action: "allow".into(),
                pattern: Some(r"^(ls|git status|git log|git diff|cargo build|cargo test)".into()),
            },
            Policy {
                tool: "Bash".into(),
                action: "deny".into(),
                pattern: None,
            },
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
        let policies = vec![
            Policy {
                tool: "Bash".into(),
                action: "allow".into(),
                pattern: Some(r"^ls$".into()),
            },
            // No catch-all — unmatched commands get None (ask)
        ];

        let ls_input = r#"{"command": "ls"}"#;
        assert_eq!(check("Bash", ls_input, &policies), Some("allow".into()));

        let other_input = r#"{"command": "rm -rf /"}"#;
        assert_eq!(check("Bash", other_input, &policies), None);
    }

    #[test]
    fn test_pattern_with_non_json_input() {
        let policies = vec![Policy {
            tool: "Bash".into(),
            action: "allow".into(),
            pattern: Some(r"ls".into()),
        }];

        // Non-JSON input: regex matches against raw string
        assert_eq!(check("Bash", "ls -la", &policies), Some("allow".into()));
        assert_eq!(check("Bash", "rm -rf", &policies), None);
    }

    #[test]
    fn test_pattern_does_not_affect_other_tools() {
        let policies = vec![
            Policy {
                tool: "Bash".into(),
                action: "allow".into(),
                pattern: Some(r"^ls$".into()),
            },
            Policy {
                tool: "Read".into(),
                action: "allow".into(),
                pattern: None,
            },
        ];

        // Read has no pattern, so it matches regardless of input
        assert_eq!(check("Read", "", &policies), Some("allow".into()));
    }

    #[test]
    fn test_invalid_regex_no_match() {
        let policies = vec![Policy {
            tool: "Bash".into(),
            action: "allow".into(),
            pattern: Some(r"[invalid".into()),
        }];

        assert_eq!(check("Bash", r#"{"command": "ls"}"#, &policies), None);
    }
}
