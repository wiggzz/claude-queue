use crate::config::Policy;

/// Check a tool name against the policy list. First match wins.
/// Returns Some("allow"), Some("deny"), or None (meaning "ask" / no match).
pub fn check(tool_name: &str, policies: &[Policy]) -> Option<String> {
    for policy in policies {
        if matches_pattern(&policy.tool, tool_name) {
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

/// Simple glob-style pattern matching.
/// Supports: "*" (match all), exact match, and basic wildcards.
fn matches_pattern(pattern: &str, tool_name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') && !pattern.contains('?') {
        return pattern == tool_name;
    }
    // Simple wildcard matching
    glob_match(pattern, tool_name)
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
        let policies = vec![
            Policy { tool: "Read".into(), action: "allow".into() },
        ];
        assert_eq!(check("Read", &policies), Some("allow".into()));
        assert_eq!(check("Write", &policies), None);
    }

    #[test]
    fn test_wildcard() {
        let policies = vec![
            Policy { tool: "*".into(), action: "deny".into() },
        ];
        assert_eq!(check("Anything", &policies), Some("deny".into()));
    }

    #[test]
    fn test_first_match_wins() {
        let policies = vec![
            Policy { tool: "Bash".into(), action: "deny".into() },
            Policy { tool: "*".into(), action: "allow".into() },
        ];
        assert_eq!(check("Bash", &policies), Some("deny".into()));
        assert_eq!(check("Read", &policies), Some("allow".into()));
    }

    #[test]
    fn test_ask_returns_none() {
        let policies = vec![
            Policy { tool: "Bash".into(), action: "ask".into() },
        ];
        assert_eq!(check("Bash", &policies), None);
    }

    #[test]
    fn test_glob_pattern() {
        let policies = vec![
            Policy { tool: "mcp__*".into(), action: "deny".into() },
        ];
        assert_eq!(check("mcp__chrome", &policies), Some("deny".into()));
        assert_eq!(check("Read", &policies), None);
    }
}
