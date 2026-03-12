mod cli;
mod config;
mod db;
mod discover;
mod format;
mod hook;
mod policy;
mod session;
mod supervisor;
mod watch;

use clap::Parser;
use cli::{Cli, Commands, PendingCommands, PolicyCommands, SessionsCommands};
use config::Config;

fn main() {
    config::ensure_user_config();

    let cli = Cli::parse();

    if let Err(e) = run(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Commands::Hook => {
            hook::run()?;
        }

        Commands::Start { prompt, name, cwd } => {
            let session_id = session::start(&prompt, name.as_deref(), &cwd)?;
            let display = name.as_deref().unwrap_or(&session_id[..8]);
            println!("Started session: {display} ({session_id})");
        }

        Commands::Resume { session_id, prompt, cwd } => {
            let new_session_id = session::resume(&session_id, &prompt, &cwd)?;
            println!("Resumed session: {new_session_id}");
        }

        Commands::List { session } => {
            let db = open_db()?;
            let mut sessions = db.get_sessions()?;
            if let Some(ref filter) = session {
                sessions.retain(|s| {
                    s.name.as_deref().map_or(false, |n| n.contains(filter.as_str()))
                        || s.session_id.starts_with(filter.as_str())
                });
            }
            if sessions.is_empty() {
                println!("No sessions.");
                return Ok(());
            }
            println!("{:<14} {:<10} {:<20} {}",
                "NAME/ID", "STATUS", "STARTED", "PROMPT");
            for s in &sessions {
                let alive = s.pid.map(session::is_pid_alive).unwrap_or(false);
                let status = if s.status == "running" && !alive {
                    let resolved = session::resolve_dead_session(&db, &s.session_id);
                    if resolved == "completed" { "completed" } else { "failed" }
                } else {
                    &s.status
                };
                let id_display = s.name.as_deref().unwrap_or(&s.session_id[..8]);
                let prompt_short = if s.prompt.len() > 50 {
                    format!("{}...", &s.prompt[..47])
                } else {
                    s.prompt.clone()
                };
                println!("{:<14} {:<10} {:<20} {}",
                    id_display,
                    status,
                    &s.started_at,
                    prompt_short,
                );
            }
        }

        Commands::Pending { session, wait, full, command } => {
            let db = open_db()?;

            match command {
                Some(PendingCommands::Show { id }) => {
                    let tc = db.get_tool_call_by_id(id)?
                        .ok_or_else(|| format!("No tool call with ID {id}"))?;
                    println!("ID:        {}", tc.id);
                    println!("Session:   {}", tc.session_id);
                    println!("Tool:      {}", tc.tool_name);
                    println!("Status:    {}", tc.status);
                    println!("Created:   {}", tc.created_at);
                    if let Some(resolved) = &tc.resolved_at {
                        println!("Resolved:  {resolved}");
                    }
                    if let Some(reason) = &tc.reason {
                        println!("Reason:    {reason}");
                    }
                    println!("\nInput:");
                    match serde_json::from_str::<serde_json::Value>(&tc.tool_input) {
                        Ok(val) => println!("{}", serde_json::to_string_pretty(&val).unwrap()),
                        Err(_) => println!("{}", tc.tool_input),
                    }
                }
                None if wait => {
                    // If there are already pending calls, print them and exit
                    // (same behavior as without --wait)
                    let existing = db.get_pending_tool_calls(session.as_deref())?;
                    if !existing.is_empty() {
                        if full {
                            for (i, tc) in existing.iter().enumerate() {
                                if i > 0 {
                                    println!("{}", "-".repeat(60));
                                }
                                println!("ID:        {}", tc.id);
                                println!("Session:   {}", &tc.session_id[..8.min(tc.session_id.len())]);
                                println!("Tool:      {}", tc.tool_name);
                                println!("Since:     {}", tc.created_at);
                                println!("\nInput:");
                                match serde_json::from_str::<serde_json::Value>(&tc.tool_input) {
                                    Ok(val) => println!("{}", serde_json::to_string_pretty(&val).unwrap()),
                                    Err(_) => println!("{}", tc.tool_input),
                                }
                                println!();
                            }
                        } else {
                            println!("{:<6} {:<10} {:<15} {:<20} {}",
                                "ID", "SESSION", "TOOL", "SINCE", "INPUT");
                            for tc in &existing {
                                let input_short = format::format_tool_input(&tc.tool_name, &tc.tool_input, 60);
                                println!("{:<6} {:<10} {:<15} {:<20} {}",
                                    tc.id,
                                    &tc.session_id[..8.min(tc.session_id.len())],
                                    tc.tool_name,
                                    &tc.created_at,
                                    input_short,
                                );
                            }
                        }
                        return Ok(());
                    }

                    // Queue is empty — poll for new calls
                    let mut known_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();

                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        let current = db.get_pending_tool_calls(session.as_deref())?;
                        let new_calls: Vec<_> = current
                            .into_iter()
                            .filter(|tc| !known_ids.contains(&tc.id))
                            .collect();
                        if !new_calls.is_empty() {
                            println!("{:<6} {:<10} {:<15} {:<20} {}",
                                "ID", "SESSION", "TOOL", "SINCE", "INPUT");
                            for tc in &new_calls {
                                let input_short = format::format_tool_input(&tc.tool_name, &tc.tool_input, 60);
                                println!("{:<6} {:<10} {:<15} {:<20} {}",
                                    tc.id,
                                    &tc.session_id[..8.min(tc.session_id.len())],
                                    tc.tool_name,
                                    &tc.created_at,
                                    input_short,
                                );
                            }
                            return Ok(());
                        }
                        // Accumulate known IDs (don't replace — avoids missing
                        // new calls that appear while resolved ones disappear)
                        for tc in db.get_pending_tool_calls(session.as_deref())? {
                            known_ids.insert(tc.id);
                        }
                    }
                }
                None => {
                    let pending = db.get_pending_tool_calls(session.as_deref())?;
                    if pending.is_empty() {
                        println!("No pending approvals.");
                        return Ok(());
                    }
                    if full {
                        for (i, tc) in pending.iter().enumerate() {
                            if i > 0 {
                                println!("{}", "-".repeat(60));
                            }
                            println!("ID:        {}", tc.id);
                            println!("Session:   {}", &tc.session_id[..8.min(tc.session_id.len())]);
                            println!("Tool:      {}", tc.tool_name);
                            println!("Since:     {}", tc.created_at);
                            println!("\nInput:");
                            match serde_json::from_str::<serde_json::Value>(&tc.tool_input) {
                                Ok(val) => println!("{}", serde_json::to_string_pretty(&val).unwrap()),
                                Err(_) => println!("{}", tc.tool_input),
                            }
                            println!();
                        }
                    } else {
                        println!("{:<6} {:<10} {:<15} {:<20} {}",
                            "ID", "SESSION", "TOOL", "SINCE", "INPUT");
                        for tc in &pending {
                            let input_short = format::format_tool_input(&tc.tool_name, &tc.tool_input, 60);
                            println!("{:<6} {:<10} {:<15} {:<20} {}",
                                tc.id,
                                &tc.session_id[..8.min(tc.session_id.len())],
                                tc.tool_name,
                                &tc.created_at,
                                input_short,
                            );
                        }
                    }
                }
            }
        }

        Commands::Approve { id, session, tool } => {
            let db = open_db()?;
            if id == "all" {
                match (session, tool) {
                    (Some(session_filter), Some(tool_name)) => {
                        let sess = db.find_session(&session_filter)?
                            .ok_or_else(|| format!("No session matching '{session_filter}'"))?;
                        let display = sess.name.as_deref().unwrap_or(&sess.session_id[..8]);
                        let count = db.approve_all_pending_for_session_and_tool(&sess.session_id, &tool_name)?;
                        println!("Approved {count} pending {tool_name} call(s) for session {display}.");
                    }
                    (Some(session_filter), None) => {
                        let sess = db.find_session(&session_filter)?
                            .ok_or_else(|| format!("No session matching '{session_filter}'"))?;
                        let display = sess.name.as_deref().unwrap_or(&sess.session_id[..8]);
                        let count = db.approve_all_pending_for_session(&sess.session_id)?;
                        println!("Approved {count} pending tool call(s) for session {display}.");
                    }
                    (None, Some(tool_name)) => {
                        let count = db.approve_all_pending_for_tool(&tool_name)?;
                        println!("Approved {count} pending {tool_name} call(s).");
                    }
                    (None, None) => {
                        let count = db.approve_all_pending()?;
                        println!("Approved {count} pending tool call(s).");
                    }
                }
            } else {
                if session.is_some() || tool.is_some() {
                    return Err("--session and --tool can only be used with 'cq approve all'".into());
                }
                let id: i64 = id.parse().map_err(|_| "Invalid ID. Use a number or 'all'.")?;
                if db.resolve_tool_call(id, "approved", None)? {
                    println!("Approved tool call {id}.");
                } else {
                    eprintln!("Tool call {id} not found or not pending.");
                }
            }
        }

        Commands::Deny { id, reason } => {
            let db = open_db()?;
            let reason_str = reason.as_deref().unwrap_or("Denied by operator");
            if db.resolve_tool_call(id, "denied", Some(reason_str))? {
                println!("Denied tool call {id}.");
            } else {
                eprintln!("Tool call {id} not found or not pending.");
            }
        }

        Commands::Result { session_id } => {
            let db = open_db()?;
            let sess = db.find_session(&session_id)?
                .ok_or_else(|| format!("No session matching '{session_id}'"))?;

            // Resolve status if the process died without updating DB
            let status = if sess.status == "running" {
                let alive = sess.pid.map(session::is_pid_alive).unwrap_or(false);
                if !alive {
                    session::resolve_dead_session(&db, &sess.session_id)
                } else {
                    sess.status.clone()
                }
            } else {
                sess.status.clone()
            };

            let content = session::get_output(&sess.session_id)?;
            let trimmed = content.trim();
            if trimmed.is_empty() {
                let stderr = session::get_stderr(&sess.session_id).unwrap_or_default();
                if !stderr.trim().is_empty() {
                    eprintln!("Session {} ({}):\n{}", sess.session_id[..8].to_string(), status, stderr.trim());
                } else if status == "running" {
                    println!("(no output yet — session is still running)");
                } else {
                    println!("(no output — session {})", status);
                }
            } else {
                println!("{trimmed}");
            }
        }

        Commands::Output { session_id, follow } => {
            let db = open_db()?;
            let sess = db.find_session(&session_id)?
                .ok_or_else(|| format!("No session matching '{session_id}'"))?;

            // Resolve status if the process died without updating DB
            let status = if sess.status == "running" {
                let alive = sess.pid.map(session::is_pid_alive).unwrap_or(false);
                if !alive {
                    session::resolve_dead_session(&db, &sess.session_id)
                } else {
                    sess.status.clone()
                }
            } else {
                sess.status.clone()
            };

            if follow {
                let mut last_len = 0;
                loop {
                    let content = session::get_output(&sess.session_id)?;
                    if content.len() > last_len {
                        print!("{}", &content[last_len..]);
                        last_len = content.len();
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            } else {
                let content = session::get_output(&sess.session_id)?;
                if content.is_empty() {
                    let stderr = session::get_stderr(&sess.session_id).unwrap_or_default();
                    if !stderr.trim().is_empty() {
                        eprintln!("Session {} ({}):\n{}", &sess.session_id[..8], status, stderr.trim());
                    } else if status == "running" {
                        println!("(no output yet — session is still running)");
                    } else {
                        println!("(no output — session {})", status);
                    }
                } else {
                    print!("{content}");
                }
            }
        }

        Commands::Kill { session_id } => {
            let db = open_db()?;
            let sess = db.find_session(&session_id)?
                .ok_or_else(|| format!("No session matching '{session_id}'"))?;
            if let Some(pid) = sess.pid {
                session::kill_session(pid)?;
                db.update_session_status(&sess.session_id, "killed", None)?;
                println!("Killed session {}.", &sess.session_id[..8]);
            } else {
                eprintln!("Session has no PID.");
            }
        }

        Commands::Watch => {
            watch::run()?;
        }

        Commands::Sessions { command } => {
            match command {
                SessionsCommands::List { limit } => {
                    let sessions = discover::scan_sessions();
                    if sessions.is_empty() {
                        println!("No external Claude Code sessions found.");
                        return Ok(());
                    }
                    let count = sessions.len().min(limit);
                    println!("{:<38} {:<12} {:<20} {}",
                        "SESSION ID", "BRANCH", "LAST ACTIVITY", "PROMPT");
                    for s in sessions.into_iter().take(count) {
                        let branch = s.git_branch.as_deref().unwrap_or("-");
                        let branch_short = if branch.len() > 10 {
                            format!("{}...", &branch[..7])
                        } else {
                            branch.to_string()
                        };
                        let activity = s.last_activity.as_deref().unwrap_or("-");
                        let activity_short = if activity.len() > 19 {
                            &activity[..19]
                        } else {
                            activity
                        };
                        let prompt = match &s.first_prompt {
                            Some(p) if p.len() > 50 => format!("{}...", &p[..47]),
                            Some(p) => p.clone(),
                            None => "(no prompt)".to_string(),
                        };
                        println!("{:<38} {:<12} {:<20} {}",
                            &s.session_id[..38.min(s.session_id.len())],
                            branch_short,
                            activity_short,
                            prompt,
                        );
                    }
                    println!("\nResume with: claude --resume <session-id>");
                }
                SessionsCommands::Search { query } => {
                    let results = discover::search_sessions(&query);
                    if results.is_empty() {
                        println!("No sessions found matching \"{query}\".");
                        return Ok(());
                    }
                    println!("Found {} session(s) matching \"{query}\":\n", results.len());
                    for s in &results {
                        let prompt = match &s.first_prompt {
                            Some(p) if p.len() > 80 => format!("{}...", &p[..77]),
                            Some(p) => p.clone(),
                            None => "(no prompt)".to_string(),
                        };
                        println!("  {} {}", &s.session_id, s.project_dir);
                        if let Some(branch) = &s.git_branch {
                            print!("    branch: {branch}");
                        }
                        if let Some(activity) = &s.last_activity {
                            print!("  last active: {activity}");
                        }
                        println!();
                        println!("    prompt: {prompt}");
                        println!("    resume: claude --resume {}", s.session_id);
                        println!();
                    }
                }
                SessionsCommands::Show { session_id } => {
                    let session = discover::find_session(&session_id)
                        .ok_or_else(|| format!("No session found matching '{session_id}'"))?;

                    println!("Session:      {}", session.session_id);
                    println!("Project:      {}", session.project_dir);
                    if let Some(cwd) = &session.cwd {
                        println!("Working dir:  {cwd}");
                    }
                    if let Some(branch) = &session.git_branch {
                        println!("Branch:       {branch}");
                    }
                    if let Some(activity) = &session.last_activity {
                        println!("Last active:  {activity}");
                    }
                    println!("Messages:     {}", session.message_count);
                    if let Some(prompt) = &session.first_prompt {
                        println!("First prompt: {prompt}");
                    }
                    println!("File:         {}", session.jsonl_path.display());
                    println!();

                    // Show recent conversation summary
                    let summary = discover::get_session_summary(&session.jsonl_path, 10);
                    if !summary.is_empty() {
                        println!("Recent activity:");
                        for msg in &summary {
                            println!("  {msg}");
                        }
                        println!();
                    }

                    println!("Resume with: claude --resume {}", session.session_id);
                }
            }
        }

        Commands::Policy { command } => {
            let cwd = std::env::current_dir()?;
            match command {
                PolicyCommands::List => {
                    let config = Config::load(&cwd);
                    if config.policies.is_empty() {
                        println!("No policies configured.");
                        return Ok(());
                    }
                    println!("{:<20} {:<10}",
                        "TOOL", "ACTION");
                    for p in &config.policies {
                        println!("{:<20} {:<10}",
                            p.tool, p.action);
                    }
                }
                PolicyCommands::Add { tool, action, user, pattern } => {
                    if !["allow", "deny", "ask"].contains(&action.as_str()) {
                        return Err("Action must be 'allow', 'deny', or 'ask'".into());
                    }
                    let path = if user {
                        config::user_config_path()
                    } else {
                        config::project_config_path(&cwd)
                    };
                    let mut cfg = config::load_file(&path);
                    cfg.policies.retain(|p| p.tool != tool);
                    cfg.policies.push(config::Policy { tool: tool.clone(), action: action.clone(), pattern: pattern.clone() });
                    cfg.save(&path)?;
                    let scope = if user { "user" } else { "project" };
                    let pattern_msg = pattern.map(|p| format!(" (pattern: {p})")).unwrap_or_default();
                    println!("Added policy: {tool} -> {action}{pattern_msg} ({scope})");
                }
                PolicyCommands::Remove { tool, user } => {
                    let path = if user {
                        config::user_config_path()
                    } else {
                        config::project_config_path(&cwd)
                    };
                    let mut cfg = config::load_file(&path);
                    let before = cfg.policies.len();
                    cfg.policies.retain(|p| p.tool != tool);
                    if cfg.policies.len() < before {
                        cfg.save(&path)?;
                        println!("Removed policy for tool: {tool}");
                    } else {
                        eprintln!("No policy found for tool: {tool}");
                    }
                }
            }
        }
    }
    Ok(())
}

fn open_db() -> Result<db::Db, Box<dyn std::error::Error>> {
    Ok(db::Db::open(&config::db_path())?)
}

