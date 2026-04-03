//! Policy suggestion module.
//!
//! Analyzes tool calls and generates fine-grained policy suggestions.

use crate::config::{MatchMode, Policy};

/// Given a tool call, generate a Policy that would match it.
///
/// Returns None if we can't generate a safe, specific policy suggestion
/// (e.g., for complex piped commands that are too risky to auto-suggest).
pub fn suggest_policy(tool_name: &str, tool_input: &str) -> Option<Policy> {
    match tool_name {
        "Bash" => suggest_bash_policy(tool_input),
        "Read" | "Edit" | "Write" => suggest_file_policy(tool_name, tool_input),
        "WebFetch" => suggest_webfetch_policy(tool_input),
        "Glob" | "Grep" => suggest_path_policy(tool_name, tool_input),
        _ => None,
    }
}

/// Suggest a policy for a Bash command.
///
/// For simple, recognizable commands, suggests a specific pattern.
/// For complex commands (pipes, chains, subshells), returns None.
fn suggest_bash_policy(tool_input: &str) -> Option<Policy> {
    let command = extract_json_field(tool_input, "command")?;

    // Reject complex commands that are too risky to auto-suggest
    if is_complex_command(&command) {
        return None;
    }

    let pattern = suggest_bash_pattern(&command)?;

    Some(Policy {
        tool: "Bash".into(),
        action: "allow".into(),
        pattern: Some(pattern),
        match_mode: MatchMode::Regex,
    })
}

/// Check if a command is too complex to safely suggest a policy for.
fn is_complex_command(command: &str) -> bool {
    // Pipe chains
    if command.contains('|') {
        return true;
    }

    // Command chaining with && or ||
    if command.contains("&&") || command.contains("||") {
        return true;
    }

    // Semicolon chaining (but not inside quotes)
    if contains_unquoted(command, ';') {
        return true;
    }

    // Subshells
    if command.contains("$(") || command.contains('`') {
        return true;
    }

    // Redirections to files (could overwrite important files)
    if command.contains('>') {
        return true;
    }

    false
}

/// Check if a character appears outside of quotes.
fn contains_unquoted(s: &str, target: char) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_char = ' ';

    for c in s.chars() {
        if c == '\'' && !in_double && prev_char != '\\' {
            in_single = !in_single;
        } else if c == '"' && !in_single && prev_char != '\\' {
            in_double = !in_double;
        } else if c == target && !in_single && !in_double {
            return true;
        }
        prev_char = c;
    }
    false
}

/// Generate a regex pattern for a Bash command.
///
/// Strategy: match the command name and key arguments exactly,
/// but be appropriately specific (not overly broad).
fn suggest_bash_pattern(command: &str) -> Option<String> {
    let command = command.trim();

    // Parse the command into executable and arguments
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }

    let executable = parts[0];

    // Known safe commands with specific patterns
    match executable {
        // Version control
        "git" => suggest_git_pattern(&parts),

        // Build tools
        "cargo" => suggest_cargo_pattern(&parts),
        "npm" | "npx" | "yarn" | "pnpm" => suggest_npm_pattern(&parts),
        "make" => suggest_make_pattern(&parts),

        // Read-only commands
        "ls" | "pwd" | "cat" | "head" | "tail" | "wc" | "which" | "whoami" | "date" | "echo" => {
            Some(format!("^{}( |$)", regex::escape(executable)))
        }

        // File inspection
        "file" | "stat" | "du" | "df" => Some(format!("^{}( |$)", regex::escape(executable))),

        // Process inspection
        "ps" | "top" | "htop" => Some(format!("^{}( |$)", regex::escape(executable))),

        // Network inspection (read-only)
        "ping" | "curl" | "wget" => Some(format!("^{}( |$)", regex::escape(executable))),

        // For unknown commands, match the exact command
        _ => Some(format!("^{}$", regex::escape(command))),
    }
}

/// Suggest pattern for git commands.
fn suggest_git_pattern(parts: &[&str]) -> Option<String> {
    if parts.len() < 2 {
        return Some("^git$".into());
    }

    let subcommand = parts[1];

    // Safe read-only git commands - match more broadly
    let read_only = [
        "status", "log", "diff", "show", "branch", "tag", "remote", "config", "stash", "blame",
        "ls-files", "ls-tree", "rev-parse", "describe", "shortlog",
    ];

    if read_only.contains(&subcommand) {
        return Some(format!("^git {}( |$)", regex::escape(subcommand)));
    }

    // Safe write commands - match exactly
    match subcommand {
        "add" | "commit" | "checkout" | "switch" | "merge" | "rebase" | "pull" | "fetch"
        | "clone" | "init" => {
            // Match git subcommand with any arguments
            Some(format!("^git {}( |$)", regex::escape(subcommand)))
        }

        // Dangerous commands - match exact command only
        "push" | "reset" | "clean" | "rm" => {
            let exact = parts.join(" ");
            Some(format!("^{}$", regex::escape(&exact)))
        }

        _ => {
            // Unknown subcommand - match exactly
            let exact = parts.join(" ");
            Some(format!("^{}$", regex::escape(&exact)))
        }
    }
}

/// Suggest pattern for cargo commands.
fn suggest_cargo_pattern(parts: &[&str]) -> Option<String> {
    if parts.len() < 2 {
        return Some("^cargo$".into());
    }

    let subcommand = parts[1];

    // Safe cargo commands - allow with any flags
    let safe_commands = [
        "build", "check", "test", "run", "bench", "doc", "clippy", "fmt", "tree", "metadata",
        "version", "search", "update", "fetch", "verify-project",
    ];

    if safe_commands.contains(&subcommand) {
        return Some(format!("^cargo {}( |$)", regex::escape(subcommand)));
    }

    // Match exact command for others
    let exact = parts.join(" ");
    Some(format!("^{}$", regex::escape(&exact)))
}

/// Suggest pattern for npm/yarn/pnpm commands.
fn suggest_npm_pattern(parts: &[&str]) -> Option<String> {
    if parts.len() < 2 {
        let cmd = parts[0];
        return Some(format!("^{}$", regex::escape(cmd)));
    }

    let cmd = parts[0];
    let subcommand = parts[1];

    // Safe npm commands
    let safe_commands = [
        "install", "ci", "test", "run", "build", "start", "list", "ls", "outdated", "audit",
        "version", "info", "search", "view",
    ];

    if safe_commands.contains(&subcommand) {
        return Some(format!("^{} {}( |$)", regex::escape(cmd), regex::escape(subcommand)));
    }

    // Match exact for others (publish, unpublish, etc.)
    let exact = parts.join(" ");
    Some(format!("^{}$", regex::escape(&exact)))
}

/// Suggest pattern for make commands.
fn suggest_make_pattern(parts: &[&str]) -> Option<String> {
    // For make, match the exact target(s)
    let exact = parts.join(" ");
    Some(format!("^{}$", regex::escape(&exact)))
}

/// Suggest a policy for file operations (Read/Edit/Write).
fn suggest_file_policy(tool_name: &str, tool_input: &str) -> Option<Policy> {
    let file_path = extract_json_field(tool_input, "file_path")?;

    let pattern = suggest_file_pattern(&file_path);

    Some(Policy {
        tool: tool_name.into(),
        action: "allow".into(),
        pattern: Some(pattern),
        match_mode: MatchMode::Regex,
    })
}

/// Generate a regex pattern for a file path.
///
/// Strategy: match the directory prefix to allow access to related files.
fn suggest_file_pattern(file_path: &str) -> String {
    // Get the directory containing the file
    if let Some(parent) = std::path::Path::new(file_path).parent() {
        let parent_str = parent.to_string_lossy();
        if !parent_str.is_empty() {
            // Match anything in this directory (but not subdirectories)
            return format!("^{}{}[^/]+$", regex::escape(&parent_str), regex::escape("/"));
        }
    }

    // Fallback: match exact file
    format!("^{}$", regex::escape(file_path))
}

/// Suggest a policy for WebFetch.
fn suggest_webfetch_policy(tool_input: &str) -> Option<Policy> {
    let url = extract_json_field(tool_input, "url")?;

    let domain = extract_domain(&url)?;

    Some(Policy {
        tool: "WebFetch".into(),
        action: "allow".into(),
        pattern: Some(domain),
        match_mode: MatchMode::Domain,
    })
}

/// Extract the domain from a URL.
fn extract_domain(url: &str) -> Option<String> {
    // Skip scheme
    let after_scheme = url.find("://").map(|i| &url[i + 3..]).unwrap_or(url);

    // Get host (up to first /, :, ?, or #)
    let host = after_scheme.split(['/', ':', '?', '#']).next()?;

    if host.is_empty() {
        return None;
    }

    Some(host.to_string())
}

/// Suggest a policy for path-based tools (Glob, Grep).
fn suggest_path_policy(tool_name: &str, tool_input: &str) -> Option<Policy> {
    let path = extract_json_field(tool_input, "path")?;

    let pattern = suggest_path_pattern(&path);

    Some(Policy {
        tool: tool_name.into(),
        action: "allow".into(),
        pattern: Some(pattern),
        match_mode: MatchMode::Regex,
    })
}

/// Generate a regex pattern for a path.
fn suggest_path_pattern(path: &str) -> String {
    // For directories, match the directory and its contents
    if path.ends_with('/') || std::path::Path::new(path).is_dir() {
        format!("^{}", regex::escape(path))
    } else {
        // Match exact path or as a prefix for directories
        format!("^{}($|/)", regex::escape(path))
    }
}

/// Extract a string field from JSON.
fn extract_json_field(json: &str, field: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get(field).and_then(|c| c.as_str()).map(String::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    // Helper to get the pattern from a suggested policy
    fn get_pattern(tool_name: &str, tool_input: &str) -> Option<String> {
        suggest_policy(tool_name, tool_input).and_then(|p| p.pattern)
    }

    // Helper to check if pattern matches a command
    fn pattern_matches(pattern: &str, text: &str) -> bool {
        Regex::new(pattern).map(|re| re.is_match(text)).unwrap_or(false)
    }

    // ==================== Bash command tests ====================

    #[test]
    fn test_bash_cargo_build() {
        let input = r#"{"command": "cargo build"}"#;
        let policy = suggest_policy("Bash", input).unwrap();

        assert_eq!(policy.tool, "Bash");
        assert_eq!(policy.action, "allow");

        let pattern = policy.pattern.unwrap();
        assert!(pattern_matches(&pattern, "cargo build"));
        assert!(pattern_matches(&pattern, "cargo build --release"));
        assert!(!pattern_matches(&pattern, "cargo test"));
    }

    #[test]
    fn test_bash_cargo_test() {
        let input = r#"{"command": "cargo test"}"#;
        let pattern = get_pattern("Bash", input).unwrap();

        assert!(pattern_matches(&pattern, "cargo test"));
        assert!(pattern_matches(&pattern, "cargo test --lib"));
        assert!(pattern_matches(&pattern, "cargo test some_test"));
    }

    #[test]
    fn test_bash_git_status() {
        let input = r#"{"command": "git status"}"#;
        let pattern = get_pattern("Bash", input).unwrap();

        assert!(pattern_matches(&pattern, "git status"));
        assert!(pattern_matches(&pattern, "git status -s"));
        assert!(!pattern_matches(&pattern, "git push"));
    }

    #[test]
    fn test_bash_git_push_exact() {
        // Dangerous commands should match exactly
        let input = r#"{"command": "git push origin main"}"#;
        let pattern = get_pattern("Bash", input).unwrap();

        assert!(pattern_matches(&pattern, "git push origin main"));
        assert!(!pattern_matches(&pattern, "git push origin develop"));
        assert!(!pattern_matches(&pattern, "git push --force"));
    }

    #[test]
    fn test_bash_npm_install() {
        let input = r#"{"command": "npm install"}"#;
        let pattern = get_pattern("Bash", input).unwrap();

        assert!(pattern_matches(&pattern, "npm install"));
        assert!(pattern_matches(&pattern, "npm install lodash"));
    }

    #[test]
    fn test_bash_npm_test() {
        let input = r#"{"command": "npm test"}"#;
        let pattern = get_pattern("Bash", input).unwrap();

        assert!(pattern_matches(&pattern, "npm test"));
        assert!(pattern_matches(&pattern, "npm test -- --watch"));
    }

    #[test]
    fn test_bash_ls() {
        let input = r#"{"command": "ls -la"}"#;
        let pattern = get_pattern("Bash", input).unwrap();

        assert!(pattern_matches(&pattern, "ls"));
        assert!(pattern_matches(&pattern, "ls -la"));
        assert!(pattern_matches(&pattern, "ls /tmp"));
    }

    #[test]
    fn test_bash_unknown_command_exact() {
        let input = r#"{"command": "my-custom-tool --flag"}"#;
        let pattern = get_pattern("Bash", input).unwrap();

        // Unknown commands match exactly
        assert!(pattern_matches(&pattern, "my-custom-tool --flag"));
        assert!(!pattern_matches(&pattern, "my-custom-tool --other"));
    }

    // ==================== Complex command rejection tests ====================

    #[test]
    fn test_bash_pipe_returns_none() {
        let input = r#"{"command": "cat file.txt | grep foo"}"#;
        assert!(suggest_policy("Bash", input).is_none());
    }

    #[test]
    fn test_bash_chain_and_returns_none() {
        let input = r#"{"command": "cd /tmp && rm -rf *"}"#;
        assert!(suggest_policy("Bash", input).is_none());
    }

    #[test]
    fn test_bash_chain_or_returns_none() {
        let input = r#"{"command": "test -f foo || touch foo"}"#;
        assert!(suggest_policy("Bash", input).is_none());
    }

    #[test]
    fn test_bash_semicolon_returns_none() {
        let input = r#"{"command": "echo a; echo b"}"#;
        assert!(suggest_policy("Bash", input).is_none());
    }

    #[test]
    fn test_bash_subshell_returns_none() {
        let input = r#"{"command": "echo $(whoami)"}"#;
        assert!(suggest_policy("Bash", input).is_none());
    }

    #[test]
    fn test_bash_backtick_returns_none() {
        let input = r#"{"command": "echo `whoami`"}"#;
        assert!(suggest_policy("Bash", input).is_none());
    }

    #[test]
    fn test_bash_redirect_returns_none() {
        let input = r#"{"command": "echo foo > bar.txt"}"#;
        assert!(suggest_policy("Bash", input).is_none());
    }

    // ==================== File operation tests ====================

    #[test]
    fn test_read_file() {
        let input = r#"{"file_path": "/Users/test/project/src/main.rs"}"#;
        let policy = suggest_policy("Read", input).unwrap();

        assert_eq!(policy.tool, "Read");
        assert_eq!(policy.action, "allow");

        let pattern = policy.pattern.unwrap();
        // Should match files in the same directory
        assert!(pattern_matches(&pattern, "/Users/test/project/src/main.rs"));
        assert!(pattern_matches(&pattern, "/Users/test/project/src/lib.rs"));
        // Should not match subdirectories
        assert!(!pattern_matches(&pattern, "/Users/test/project/src/util/helpers.rs"));
    }

    #[test]
    fn test_edit_file() {
        let input = r#"{"file_path": "/app/config.toml", "old_string": "x", "new_string": "y"}"#;
        let policy = suggest_policy("Edit", input).unwrap();

        assert_eq!(policy.tool, "Edit");
        let pattern = policy.pattern.unwrap();
        assert!(pattern_matches(&pattern, "/app/config.toml"));
        assert!(pattern_matches(&pattern, "/app/other.toml"));
    }

    #[test]
    fn test_write_file() {
        let input = r#"{"file_path": "/tmp/output.txt", "content": "hello"}"#;
        let policy = suggest_policy("Write", input).unwrap();

        assert_eq!(policy.tool, "Write");
        let pattern = policy.pattern.unwrap();
        assert!(pattern_matches(&pattern, "/tmp/output.txt"));
    }

    // ==================== WebFetch tests ====================

    #[test]
    fn test_webfetch_domain() {
        let input = r#"{"url": "https://docs.rs/serde/latest/serde/"}"#;
        let policy = suggest_policy("WebFetch", input).unwrap();

        assert_eq!(policy.tool, "WebFetch");
        assert_eq!(policy.action, "allow");
        assert_eq!(policy.match_mode, MatchMode::Domain);
        assert_eq!(policy.pattern, Some("docs.rs".into()));
    }

    #[test]
    fn test_webfetch_with_port() {
        let input = r#"{"url": "http://localhost:8080/api/test"}"#;
        let policy = suggest_policy("WebFetch", input).unwrap();

        assert_eq!(policy.pattern, Some("localhost".into()));
    }

    #[test]
    fn test_webfetch_subdomain() {
        let input = r#"{"url": "https://api.github.com/users/test"}"#;
        let policy = suggest_policy("WebFetch", input).unwrap();

        assert_eq!(policy.pattern, Some("api.github.com".into()));
    }

    // ==================== Path-based tools tests ====================

    #[test]
    fn test_grep_path() {
        let input = r#"{"pattern": "TODO", "path": "/Users/test/project"}"#;
        let policy = suggest_policy("Grep", input).unwrap();

        assert_eq!(policy.tool, "Grep");
        let pattern = policy.pattern.unwrap();
        assert!(pattern_matches(&pattern, "/Users/test/project"));
        assert!(pattern_matches(&pattern, "/Users/test/project/src"));
    }

    #[test]
    fn test_glob_path() {
        let input = r#"{"pattern": "*.rs", "path": "src"}"#;
        let policy = suggest_policy("Glob", input).unwrap();

        assert_eq!(policy.tool, "Glob");
        let pattern = policy.pattern.unwrap();
        assert!(pattern_matches(&pattern, "src"));
        assert!(pattern_matches(&pattern, "src/lib"));
    }

    // ==================== Edge cases ====================

    #[test]
    fn test_unknown_tool() {
        let input = r#"{"foo": "bar"}"#;
        assert!(suggest_policy("UnknownTool", input).is_none());
    }

    #[test]
    fn test_invalid_json() {
        let input = "not valid json";
        assert!(suggest_policy("Bash", input).is_none());
    }

    #[test]
    fn test_missing_field() {
        let input = r#"{"other_field": "value"}"#;
        assert!(suggest_policy("Bash", input).is_none());
    }

    #[test]
    fn test_contains_unquoted() {
        assert!(contains_unquoted("a;b", ';'));
        assert!(!contains_unquoted("'a;b'", ';'));
        assert!(!contains_unquoted("\"a;b\"", ';'));
        assert!(contains_unquoted("'a';b", ';'));
        assert!(contains_unquoted("a;'b'", ';'));
    }
}
