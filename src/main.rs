mod audit;
mod cli;
mod config;
mod db;
mod discover;
mod format;
mod hook;
mod policy;
mod session;
mod supervisor;
mod update;
mod watch;

use clap::Parser;
use cli::{Cli, Commands, ConfigCommands, PendingCommands, PolicyCommands, SessionsCommands};
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
        Commands::Update => {
            update::run()?;
        }

        Commands::Version => {
            println!("cq {}", env!("CARGO_PKG_VERSION"));
        }

        Commands::Hook => {
            hook::run()?;
        }

        Commands::Start {
            prompt,
            name,
            cwd,
            cancel,
        } => {
            if cancel {
                let name = name.as_deref().expect("--cancel requires --name");
                if session::cancel_queued(name)? {
                    println!("Cancelled queued message for session {name}.");
                } else {
                    println!("No queued message for session {name}.");
                }
            } else {
                let prompt = prompt.ok_or("prompt is required (unless using --cancel)")?;
                if let Some(ref name) = name {
                    match session::queue_or_start(&prompt, name, &cwd)? {
                        session::StartResult::Started(session_id) => {
                            println!("Started session: {name} ({session_id})");
                        }
                        session::StartResult::Queued => {
                            println!("Queued message for running session: {name}");
                        }
                        session::StartResult::Replaced => {
                            println!("Replaced queued message for running session: {name}");
                        }
                    }
                } else {
                    let session_id = session::start(&prompt, None, &cwd)?;
                    println!("Started session: {} ({session_id})", &session_id[..8]);
                }
            }
        }

        Commands::Resume {
            session_id,
            prompt,
            cwd,
        } => {
            let new_session_id = session::resume(&session_id, &prompt, &cwd)?;
            println!("Resumed session: {new_session_id}");
        }

        Commands::List { session, status } => {
            let db = open_db()?;
            let mut sessions = db.get_sessions()?;
            if let Some(ref filter) = session {
                sessions.retain(|s| {
                    s.name
                        .as_deref()
                        .is_some_and(|n| n.contains(filter.as_str()))
                        || s.session_id.starts_with(filter.as_str())
                });
            }
            // Resolve true status for each session, then apply status filter
            let rows: Vec<_> = sessions
                .iter()
                .map(|s| {
                    let alive = s.pid.map(session::is_pid_alive).unwrap_or(false);
                    let resolved_status = if s.status == "running" && !alive {
                        let resolved = session::resolve_dead_session(&db, &s.session_id);
                        if resolved == "completed" {
                            "completed"
                        } else {
                            "failed"
                        }
                    } else {
                        &s.status
                    };
                    (s, resolved_status)
                })
                .filter(|(_s, resolved_status)| {
                    status
                        .as_ref()
                        .is_none_or(|f| resolved_status.eq_ignore_ascii_case(f))
                })
                .collect();
            if rows.is_empty() {
                println!("No sessions.");
                return Ok(());
            }
            println!(
                "{:<14} {:<10} {:<20} PROMPT",
                "NAME/ID", "STATUS", "STARTED"
            );
            for (s, resolved_status) in &rows {
                let id_display = s.name.as_deref().unwrap_or(&s.session_id[..8]);
                let prompt_short = if s.prompt.len() > 50 {
                    format!("{}...", &s.prompt[..47])
                } else {
                    s.prompt.clone()
                };
                println!(
                    "{:<14} {:<10} {:<20} {}",
                    id_display, resolved_status, &s.started_at, prompt_short,
                );
            }
        }

        Commands::Pending {
            session,
            wait,
            full,
            json,
            command,
        } => {
            let db = open_db()?;

            match command {
                Some(PendingCommands::Show { id }) => {
                    let tc = db
                        .get_tool_call_by_id(id)?
                        .ok_or_else(|| format!("No tool call with ID {id}"))?;
                    println!("ID:        {}", tc.id);
                    println!("Session:   {}", tc.session_id);
                    println!("Tool:      {}", tc.tool_name);
                    println!("Status:    {}", tc.status);
                    if let Some(summary) = &tc.summary {
                        println!("Summary:   {summary}");
                    }
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
                        if json {
                            for tc in &existing {
                                println!("{}", tool_call_to_json(tc));
                            }
                        } else if full {
                            print_pending_full(&existing);
                        } else {
                            print_pending_table(&existing);
                        }
                        return Ok(());
                    }

                    // Queue is empty — poll for new calls
                    let mut known_ids: std::collections::HashSet<i64> =
                        std::collections::HashSet::new();

                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        let current = db.get_pending_tool_calls(session.as_deref())?;
                        let new_calls: Vec<_> = current
                            .into_iter()
                            .filter(|tc| !known_ids.contains(&tc.id))
                            .collect();
                        if !new_calls.is_empty() {
                            if json {
                                for tc in &new_calls {
                                    println!("{}", tool_call_to_json(tc));
                                }
                            } else {
                                print_pending_table(&new_calls);
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
                        if !json {
                            println!("No pending approvals.");
                        }
                        return Ok(());
                    }
                    if json {
                        for tc in &pending {
                            println!("{}", tool_call_to_json(tc));
                        }
                    } else if full {
                        print_pending_full(&pending);
                    } else {
                        print_pending_table(&pending);
                    }
                }
            }
        }

        Commands::Approve {
            id,
            session,
            tool,
            match_pattern,
        } => {
            let db = open_db()?;
            if id == "all" {
                if let Some(ref pattern) = match_pattern {
                    // --match mode: fetch pending calls, filter by regex, approve individually
                    let re = regex::Regex::new(pattern)
                        .map_err(|e| format!("Invalid regex '{}': {}", pattern, e))?;

                    // Resolve session filter to a session_id prefix
                    let session_id_prefix = match &session {
                        Some(session_filter) => {
                            let sess = db
                                .find_session(session_filter)?
                                .ok_or_else(|| format!("No session matching '{session_filter}'"))?;
                            Some(sess.session_id)
                        }
                        None => None,
                    };

                    let pending = db.get_pending_tool_calls(session_id_prefix.as_deref())?;
                    let mut approved_calls: Vec<&db::ToolCall> = Vec::new();
                    for tc in &pending {
                        // Filter by tool type if specified
                        if let Some(ref tool_name) = tool
                            && tc.tool_name != *tool_name
                        {
                            continue;
                        }
                        // Extract the text to match against: for Bash, use "command" field; otherwise raw tool_input
                        let match_text =
                            match serde_json::from_str::<serde_json::Value>(&tc.tool_input) {
                                Ok(val) => {
                                    if tc.tool_name == "Bash" {
                                        val.get("command")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or(&tc.tool_input)
                                            .to_string()
                                    } else {
                                        tc.tool_input.clone()
                                    }
                                }
                                Err(_) => tc.tool_input.clone(),
                            };
                        if re.is_match(&match_text) {
                            db.resolve_tool_call(tc.id, "approved", None)?;
                            audit::log(
                                &tc.session_id,
                                &tc.tool_name,
                                &tc.tool_input,
                                "approve",
                                &format!("Batch approve matching /{pattern}/"),
                                "human",
                            );
                            approved_calls.push(tc);
                        }
                    }
                    if !approved_calls.is_empty() {
                        let names = db.get_session_names().unwrap_or_default();
                        print_approved_details(&approved_calls, &names);
                    }
                    println!(
                        "Approved {} pending tool call(s) matching /{pattern}/.",
                        approved_calls.len()
                    );
                } else {
                    // Original behavior without --match
                    // Fetch pending calls before approving so we can audit-log each one
                    let names = db.get_session_names().unwrap_or_default();
                    match (session, tool) {
                        (Some(session_filter), Some(tool_name)) => {
                            let sess = db
                                .find_session(&session_filter)?
                                .ok_or_else(|| format!("No session matching '{session_filter}'"))?;
                            let display = sess.name.as_deref().unwrap_or(&sess.session_id[..8]);
                            let pending = db.get_pending_tool_calls(Some(&sess.session_id))?;
                            let count = db.approve_all_pending_for_session_and_tool(
                                &sess.session_id,
                                &tool_name,
                            )?;
                            let matched: Vec<_> = pending
                                .iter()
                                .filter(|tc| tc.tool_name == tool_name)
                                .collect();
                            for tc in &matched {
                                audit::log(
                                    &tc.session_id,
                                    &tc.tool_name,
                                    &tc.tool_input,
                                    "approve",
                                    "Batch approve all",
                                    "human",
                                );
                            }
                            print_approved_details(&matched, &names);
                            println!(
                                "Approved {count} pending {tool_name} call(s) for session {display}."
                            );
                        }
                        (Some(session_filter), None) => {
                            let sess = db
                                .find_session(&session_filter)?
                                .ok_or_else(|| format!("No session matching '{session_filter}'"))?;
                            let display = sess.name.as_deref().unwrap_or(&sess.session_id[..8]);
                            let pending = db.get_pending_tool_calls(Some(&sess.session_id))?;
                            let count = db.approve_all_pending_for_session(&sess.session_id)?;
                            for tc in &pending {
                                audit::log(
                                    &tc.session_id,
                                    &tc.tool_name,
                                    &tc.tool_input,
                                    "approve",
                                    "Batch approve all",
                                    "human",
                                );
                            }
                            print_approved_details(&pending, &names);
                            println!(
                                "Approved {count} pending tool call(s) for session {display}."
                            );
                        }
                        (None, Some(tool_name)) => {
                            let pending = db.get_pending_tool_calls(None)?;
                            let count = db.approve_all_pending_for_tool(&tool_name)?;
                            let matched: Vec<_> = pending
                                .iter()
                                .filter(|tc| tc.tool_name == tool_name)
                                .collect();
                            for tc in &matched {
                                audit::log(
                                    &tc.session_id,
                                    &tc.tool_name,
                                    &tc.tool_input,
                                    "approve",
                                    "Batch approve all",
                                    "human",
                                );
                            }
                            print_approved_details(&matched, &names);
                            println!("Approved {count} pending {tool_name} call(s).");
                        }
                        (None, None) => {
                            let pending = db.get_pending_tool_calls(None)?;
                            if !pending.is_empty() {
                                eprintln!(
                                    "Warning: approving all pending calls across all sessions. \
                                     Consider using --session to scope approvals."
                                );
                            }
                            let count = db.approve_all_pending()?;
                            for tc in &pending {
                                audit::log(
                                    &tc.session_id,
                                    &tc.tool_name,
                                    &tc.tool_input,
                                    "approve",
                                    "Batch approve all",
                                    "human",
                                );
                            }
                            print_approved_details(&pending, &names);
                            println!("Approved {count} pending tool call(s).");
                        }
                    }
                }
            } else {
                if session.is_some() || tool.is_some() || match_pattern.is_some() {
                    return Err(
                        "--session, --tool, and --match can only be used with 'cq approve all'"
                            .into(),
                    );
                }
                // Try numeric ID first
                if let Ok(numeric_id) = id.parse::<i64>() {
                    let tc = db.get_tool_call_by_id(numeric_id)?;
                    if db.resolve_tool_call(numeric_id, "approved", None)? {
                        if let Some(tc) = tc {
                            audit::log(
                                &tc.session_id,
                                &tc.tool_name,
                                &tc.tool_input,
                                "approve",
                                "Manual approve",
                                "human",
                            );
                        }
                        println!("Approved tool call {numeric_id}.");
                    } else {
                        eprintln!("Tool call {numeric_id} not found or not pending.");
                    }
                } else {
                    // Not a number — try summary match
                    let matches = db.find_pending_by_summary(&id)?;
                    match matches.len() {
                        0 => {
                            eprintln!("No pending tool call found matching summary \"{id}\".");
                            std::process::exit(1);
                        }
                        1 => {
                            let tc = &matches[0];
                            if db.resolve_tool_call(tc.id, "approved", None)? {
                                audit::log(
                                    &tc.session_id,
                                    &tc.tool_name,
                                    &tc.tool_input,
                                    "approve",
                                    &format!(
                                        "Approved by summary: {}",
                                        tc.summary.as_deref().unwrap_or(&id)
                                    ),
                                    "human",
                                );
                                println!(
                                    "Approved tool call {} ({}).",
                                    tc.id,
                                    tc.summary.as_deref().unwrap_or("")
                                );
                            } else {
                                eprintln!("Tool call {} is no longer pending.", tc.id);
                            }
                        }
                        n => {
                            eprintln!("Multiple pending tool calls ({n}) match \"{id}\":");
                            for tc in &matches {
                                eprintln!(
                                    "  [{}] {} — {}",
                                    tc.id,
                                    tc.tool_name,
                                    tc.summary.as_deref().unwrap_or("(no summary)")
                                );
                            }
                            eprintln!("Be more specific, or use the numeric ID.");
                            std::process::exit(1);
                        }
                    }
                }
            }
        }

        Commands::Deny { id, reason } => {
            let db = open_db()?;
            let reason_str = reason.as_deref().unwrap_or("Denied by operator");
            let tc = db.get_tool_call_by_id(id)?;
            if db.resolve_tool_call(id, "denied", Some(reason_str))? {
                if let Some(tc) = tc {
                    audit::log(
                        &tc.session_id,
                        &tc.tool_name,
                        &tc.tool_input,
                        "deny",
                        reason_str,
                        "human",
                    );
                }
                println!("Denied tool call {id}.");
            } else {
                eprintln!("Tool call {id} not found or not pending.");
            }
        }

        Commands::Result { session_id } => {
            let db = open_db()?;

            // If multiple sessions share a name, concatenate all their outputs
            let sessions_by_name = db.find_sessions_by_name(&session_id)?;
            let sessions = if sessions_by_name.len() > 1 {
                sessions_by_name
            } else {
                let sess = db
                    .find_session(&session_id)?
                    .ok_or_else(|| format!("No session matching '{session_id}'"))?;
                vec![sess]
            };

            let last_sess = sessions.last().unwrap();

            // Resolve status of the most recent session
            let status = if last_sess.status == "running" {
                let alive = last_sess.pid.map(session::is_pid_alive).unwrap_or(false);
                if !alive {
                    session::resolve_dead_session(&db, &last_sess.session_id)
                } else {
                    last_sess.status.clone()
                }
            } else {
                last_sess.status.clone()
            };

            let mut parts = Vec::new();
            for sess in &sessions {
                if let Ok(content) = session::get_output(&sess.session_id) {
                    let trimmed = content.trim();
                    if !trimmed.is_empty() {
                        parts.push(trimmed.to_string());
                    }
                }
            }
            let combined = parts.join("\n\n--- resumed ---\n\n");

            if combined.is_empty() {
                let stderr = session::get_stderr(&last_sess.session_id).unwrap_or_default();
                if !stderr.trim().is_empty() {
                    eprintln!(
                        "Session {} ({}):\n{}",
                        &last_sess.session_id[..8],
                        status,
                        stderr.trim()
                    );
                } else if status == "running" {
                    println!("(no output yet — session is still running)");
                } else {
                    println!("(no output — session {})", status);
                }
            } else {
                println!("{combined}");
            }
        }

        Commands::Output { session_id, follow } => {
            let db = open_db()?;

            // If multiple sessions share a name, concatenate all their outputs
            let sessions_by_name = db.find_sessions_by_name(&session_id)?;
            let sessions = if sessions_by_name.len() > 1 {
                sessions_by_name
            } else {
                let sess = db
                    .find_session(&session_id)?
                    .ok_or_else(|| format!("No session matching '{session_id}'"))?;
                vec![sess]
            };

            let last_sess = sessions.last().unwrap();

            // Resolve status of the most recent session
            let status = if last_sess.status == "running" {
                let alive = last_sess.pid.map(session::is_pid_alive).unwrap_or(false);
                if !alive {
                    session::resolve_dead_session(&db, &last_sess.session_id)
                } else {
                    last_sess.status.clone()
                }
            } else {
                last_sess.status.clone()
            };

            if follow {
                // For follow mode, only tail the most recent session
                let mut last_len = 0;
                loop {
                    let content = session::get_output(&last_sess.session_id)?;
                    if content.len() > last_len {
                        print!("{}", &content[last_len..]);
                        last_len = content.len();
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            } else {
                let mut parts = Vec::new();
                for sess in &sessions {
                    if let Ok(content) = session::get_output(&sess.session_id)
                        && !content.is_empty()
                    {
                        parts.push(content);
                    }
                }
                let combined = parts.join("\n--- resumed ---\n");

                if combined.is_empty() {
                    let stderr = session::get_stderr(&last_sess.session_id).unwrap_or_default();
                    if !stderr.trim().is_empty() {
                        eprintln!(
                            "Session {} ({}):\n{}",
                            &last_sess.session_id[..8],
                            status,
                            stderr.trim()
                        );
                    } else if status == "running" {
                        println!("(no output yet — session is still running)");
                    } else {
                        println!("(no output — session {})", status);
                    }
                } else {
                    print!("{combined}");
                }
            }
        }

        Commands::Wait { session_id } => {
            let db = open_db()?;
            let sess = db
                .find_session(&session_id)?
                .ok_or_else(|| format!("No session matching '{session_id}'"))?;
            let display = sess.name.as_deref().unwrap_or(&sess.session_id[..8]);

            // Check if already done
            let mut status = sess.status.clone();
            if status == "running" {
                let alive = sess.pid.map(session::is_pid_alive).unwrap_or(false);
                if !alive {
                    status = session::resolve_dead_session(&db, &sess.session_id);
                }
            }

            if status != "running" {
                // Already finished — print result and exit
                print_session_result(&sess.session_id, &status)?;
                if status != "completed" {
                    std::process::exit(1);
                }
                return Ok(());
            }

            // Poll until done
            eprintln!("Waiting for session {display}...");
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));

                // Re-fetch session to check status
                let current = db
                    .find_session(&sess.session_id)?
                    .ok_or("Session disappeared from database")?;

                let mut current_status = current.status.clone();
                if current_status == "running" {
                    let alive = current.pid.map(session::is_pid_alive).unwrap_or(false);
                    if !alive {
                        current_status = session::resolve_dead_session(&db, &current.session_id);
                    }
                }

                if current_status != "running" {
                    print_session_result(&sess.session_id, &current_status)?;
                    if current_status != "completed" {
                        std::process::exit(1);
                    }
                    return Ok(());
                }
            }
        }

        Commands::Kill { session_id } => {
            let db = open_db()?;
            let sess = db
                .find_session(&session_id)?
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
                    println!(
                        "{:<38} {:<12} {:<20} PROMPT",
                        "SESSION ID", "BRANCH", "LAST ACTIVITY"
                    );
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
                        println!(
                            "{:<38} {:<12} {:<20} {}",
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

        Commands::Audit {
            tail,
            json,
            follow,
            verbose,
        } => {
            if follow {
                audit::follow(tail, json, verbose);
            } else {
                let entries = audit::read_tail(tail);
                if entries.is_empty() {
                    if !json {
                        println!("No audit log entries.");
                    }
                    return Ok(());
                }
                if json {
                    for entry in &entries {
                        println!("{}", serde_json::to_string(entry).unwrap());
                    }
                } else {
                    let session_names = audit::load_session_names();
                    println!(
                        "{:<22} {:<10} {:<10} {:<15} {:<10} REASON",
                        "TIMESTAMP", "DECISION", "ACTOR", "TOOL", "SESSION"
                    );
                    for entry in &entries {
                        audit::print_entry(entry, false, &session_names, verbose);
                    }
                }
            }
        }

        Commands::Config { command } => {
            let cwd = std::env::current_dir()?;
            match command {
                ConfigCommands::Show => {
                    let defaults = Config::default();
                    let user_cfg = config::load_file(&config::user_config_path());
                    let project_path = config::project_config_path(&cwd);
                    let project_cfg = config::load_file(&project_path);
                    let merged = Config::load(&cwd);
                    let project_exists = project_path.exists();

                    // Determine source for scalar settings
                    let timeout_source =
                        if project_exists && project_cfg.timeout != defaults.timeout {
                            "project"
                        } else if user_cfg.timeout != defaults.timeout {
                            "user"
                        } else {
                            "default"
                        };
                    let poll_source =
                        if project_exists && project_cfg.poll_interval != defaults.poll_interval {
                            "project"
                        } else if user_cfg.poll_interval != defaults.poll_interval {
                            "user"
                        } else {
                            "default"
                        };

                    println!(
                        "timeout:                    {} ({timeout_source})",
                        merged.timeout
                    );
                    println!(
                        "poll_interval:              {} ({poll_source})",
                        merged.poll_interval
                    );

                    // Supervisor section
                    let sv_enabled_source = if project_exists && project_cfg.supervisor.enabled {
                        "project"
                    } else if user_cfg.supervisor.enabled {
                        "user"
                    } else {
                        "default"
                    };
                    let sv_model_source = if project_exists
                        && !project_cfg.supervisor.model.is_empty()
                        && project_cfg.supervisor.model != defaults.supervisor.model
                    {
                        "project"
                    } else if user_cfg.supervisor.model != defaults.supervisor.model {
                        "user"
                    } else {
                        "default"
                    };
                    let sv_context_source =
                        if project_exists && project_cfg.supervisor.include_session_context {
                            "project"
                        } else if user_cfg.supervisor.include_session_context {
                            "user"
                        } else {
                            "default"
                        };

                    println!();
                    println!("Supervisor:");
                    println!(
                        "  enabled:                  {} ({sv_enabled_source})",
                        merged.supervisor.enabled
                    );
                    println!(
                        "  model:                    {} ({sv_model_source})",
                        merged.supervisor.model
                    );
                    println!(
                        "  include_session_context:   {} ({sv_context_source})",
                        merged.supervisor.include_session_context
                    );
                    if merged.supervisor.rules.is_empty() {
                        println!("  rules:                    (none)");
                    } else {
                        println!("  rules:");
                        // Show project rules first, then user rules
                        for rule in &project_cfg.supervisor.rules {
                            println!("    [project] {rule}");
                        }
                        for rule in &user_cfg.supervisor.rules {
                            println!("    [user]    {rule}");
                        }
                    }

                    // Policies
                    println!();
                    if merged.policies.is_empty() {
                        println!("Policies: (none)");
                    } else {
                        println!("Policies (evaluation order):");
                        let project_policy_count = if project_exists {
                            project_cfg.policies.len()
                        } else {
                            0
                        };
                        for (i, p) in merged.policies.iter().enumerate() {
                            let source = if i < project_policy_count {
                                "project"
                            } else {
                                "user"
                            };
                            let pattern_str = p
                                .pattern
                                .as_ref()
                                .map(|pat| format!(" (pattern: {pat})"))
                                .unwrap_or_default();
                            println!(
                                "  [{source}] {tool} -> {action}{pattern_str}",
                                tool = p.tool,
                                action = p.action,
                            );
                        }
                    }

                    // Config file paths
                    let user_path = config::user_config_path();
                    let home = std::env::var("HOME").unwrap_or_default();
                    let user_display = user_path.to_string_lossy().replace(&home, "~");
                    println!();
                    println!("Config files:");
                    println!("  User:    {user_display}");
                    if project_exists {
                        println!("  Project: {}", project_path.display());
                    } else {
                        println!("  Project: {} (not found)", project_path.display());
                    }
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
                    println!("{:<20} {:<10} PATTERN", "TOOL", "ACTION");
                    for p in &config.policies {
                        let pattern_display = match (&p.pattern, &p.match_mode) {
                            (Some(pat), crate::config::MatchMode::Domain) => {
                                format!("domain:{pat}")
                            }
                            (Some(pat), _) => pat.clone(),
                            (None, _) => "-".to_string(),
                        };
                        println!("{:<20} {:<10} {}", p.tool, p.action, pattern_display);
                    }
                }
                PolicyCommands::Add {
                    tool,
                    action,
                    user,
                    pattern,
                } => {
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
                    cfg.policies.push(config::Policy {
                        tool: tool.clone(),
                        action: action.clone(),
                        pattern: pattern.clone(),
                        match_mode: config::MatchMode::default(),
                    });
                    cfg.save(&path)?;
                    let scope = if user { "user" } else { "project" };
                    let pattern_msg = pattern
                        .map(|p| format!(" (pattern: {p})"))
                        .unwrap_or_default();
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

fn print_session_result(session_id: &str, status: &str) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!(
        "Session {} {status}.",
        &session_id[..8.min(session_id.len())]
    );
    let content = session::get_output(session_id)?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        let stderr = session::get_stderr(session_id).unwrap_or_default();
        if !stderr.trim().is_empty() {
            eprintln!("{}", stderr.trim());
        }
    } else {
        println!("{trimmed}");
    }
    Ok(())
}

fn open_db() -> Result<db::Db, Box<dyn std::error::Error>> {
    Ok(db::Db::open(&config::db_path())?)
}

fn print_pending_full(calls: &[db::ToolCall]) {
    for (i, tc) in calls.iter().enumerate() {
        if i > 0 {
            println!("{}", "-".repeat(60));
        }
        println!("ID:        {}", tc.id);
        println!(
            "Session:   {}",
            &tc.session_id[..8.min(tc.session_id.len())]
        );
        println!("Tool:      {}", tc.tool_name);
        if let Some(ref summary) = tc.summary {
            println!("Summary:   {summary}");
        }
        println!("Since:     {}", tc.created_at);
        println!("\nInput:");
        match serde_json::from_str::<serde_json::Value>(&tc.tool_input) {
            Ok(val) => println!("{}", serde_json::to_string_pretty(&val).unwrap()),
            Err(_) => println!("{}", tc.tool_input),
        }
        println!();
    }
}

fn print_pending_table(calls: &[db::ToolCall]) {
    println!(
        "{:<6} {:<10} {:<15} {:<20} DESCRIPTION",
        "ID", "SESSION", "TOOL", "SINCE"
    );
    for tc in calls {
        let description = if let Some(ref summary) = tc.summary {
            format!("\"{}\"", truncate_str(summary, 58))
        } else {
            format::format_tool_input(&tc.tool_name, &tc.tool_input, 60)
        };
        println!(
            "{:<6} {:<10} {:<15} {:<20} {}",
            tc.id,
            &tc.session_id[..8.min(tc.session_id.len())],
            tc.tool_name,
            &tc.created_at,
            description,
        );
    }
}

fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}

/// Print details of each approved tool call to stderr so orchestrators surface what was approved.
fn print_approved_details(
    calls: &[impl std::borrow::Borrow<db::ToolCall>],
    session_names: &std::collections::HashMap<String, String>,
) {
    for item in calls {
        let tc = item.borrow();
        let session_display = session_names
            .get(&tc.session_id)
            .map(|s| s.as_str())
            .unwrap_or(&tc.session_id[..8.min(tc.session_id.len())]);
        let description = if let Some(ref summary) = tc.summary {
            format!("\"{}\"", truncate_str(summary, 70))
        } else {
            format::format_tool_input(&tc.tool_name, &tc.tool_input, 72)
        };
        eprintln!(
            "  ✓ [{}] {} — {}",
            session_display, tc.tool_name, description
        );
    }
}

fn tool_call_to_json(tc: &db::ToolCall) -> String {
    let tool_input = serde_json::from_str::<serde_json::Value>(&tc.tool_input)
        .unwrap_or_else(|_| serde_json::Value::String(tc.tool_input.clone()));
    let mut obj = serde_json::json!({
        "id": tc.id,
        "session_id": tc.session_id,
        "tool_name": tc.tool_name,
        "tool_input": tool_input,
        "created_at": tc.created_at,
    });
    if let Some(ref summary) = tc.summary {
        obj["summary"] = serde_json::Value::String(summary.clone());
    }
    serde_json::to_string(&obj).unwrap()
}
