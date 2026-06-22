//! `/memory` slash command — inspect and edit the user memory file.
//!
//! When the user-memory feature is opted-in (`[memory] enabled = true` in
//! config or `DEEPSEEK_MEMORY=on` in the environment), `/memory` shows
//! the current memory file path and contents inline. Subcommands let the
//! user clear or open the file:
//!
//! - `/memory` — show path + content
//! - `/memory show` — alias for the no-arg form
//! - `/memory clear` — replace the file contents with an empty marker
//! - `/memory path` — show only the resolved path
//! - `/memory help` — show command-specific help and the resolved path
//!
//! Editor integration (`/memory edit`) is intentionally minimal: the
//! command prints a copy-pasteable shell line to open the file in the
//! user's `$VISUAL` / `$EDITOR`, since the in-process external editor
//! plumbing requires terminal teardown that the slash-command handler
//! doesn't have access to.

use std::fs;
use std::path::Path;

use super::CommandResult;
use crate::tui::app::App;

const MEMORY_USAGE: &str = "/memory [show|path|clear|edit|tags|search <query>|search --tag <tag>|help]";

fn memory_help(path: &Path) -> String {
    format!(
        "Inspect or manage your persistent user-memory file.\n\n\
         Usage: {MEMORY_USAGE}\n\n\
         Current path: {}\n\n\
         Subcommands:\n\
           /memory                    Show the resolved path and current contents\n\
           /memory show               Alias for the no-arg form\n\
           /memory path               Print just the resolved path\n\
           /memory clear              Replace the file contents with an empty marker\n\
           /memory edit               Print the editor command for this file\n\
           /memory tags               List all tags with occurrence counts\n\
           /memory search <query>     Search memory by text (body + tags)\n\
           /memory search --tag <t>   Search memory by tag (exact match)\n\
           /memory help               Show this help\n\n\
         Quick capture: type `# foo` in the composer to append a timestamped\n\
         bullet without firing a turn.",
        path.display()
    )
}

/// Split the argument into subcommand and remaining args.
fn split_subcommand(arg: Option<&str>) -> (&str, Option<&str>) {
    match arg {
        Some(a) => {
            let trimmed = a.trim();
            match trimmed.find(char::is_whitespace) {
                Some(pos) => (&trimmed[..pos], Some(trimmed[pos + 1..].trim_start())),
                None => (trimmed, None),
            }
        }
        None => ("show", None),
    }
}

fn render_entries(entries: &[&crate::memory::MemoryEntry], prefix: &str) -> String {
    let mut lines = String::new();
    for entry in entries {
        let _ = std::fmt::Write::write_fmt(
            &mut lines,
            format_args!("\n{prefix}- ({}) {}", entry.timestamp, entry.body),
        );
        if !entry.tags.is_empty() {
            let _ = std::fmt::Write::write_fmt(
                &mut lines,
                format_args!(
                    " {}",
                    entry
                        .tags
                        .iter()
                        .map(|t| format!("#{t}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            );
        }
    }
    lines
}

pub fn memory(app: &mut App, arg: Option<&str>) -> CommandResult {
    if !app.use_memory {
        return CommandResult::error(
            "user memory is disabled. Enable with `[memory] enabled = true` in `~/.codewhale/config.toml` or `DEEPSEEK_MEMORY=on` in your environment, then restart the TUI.",
        );
    }

    let path = app.memory_path.clone();
    let (sub, rest) = split_subcommand(arg);

    match sub {
        "" | "show" => {
            let body = match fs::read_to_string(&path) {
                Ok(text) if text.trim().is_empty() => format!(
                    "{}\n(empty — add via `# foo` from the composer or have the model use the `remember` tool)",
                    path.display()
                ),
                Ok(text) => format!("{}\n\n{}", path.display(), text.trim_end()),
                Err(_) => format!(
                    "{}\n(file does not exist yet — add via `# foo` from the composer to create it)",
                    path.display()
                ),
            };
            CommandResult::message(body)
        }
        "path" => CommandResult::message(path.display().to_string()),
        "tags" => match fs::read_to_string(&path) {
            Ok(content) => {
                let tags = crate::memory::list_tags(&content);
                if tags.is_empty() {
                    CommandResult::message("no tags found in memory file")
                } else {
                    let mut lines = format!("Tags in {}:\n", path.display());
                    for (i, (tag, count)) in tags.iter().enumerate() {
                        let _ = std::fmt::Write::write_fmt(
                            &mut lines,
                            format_args!("\n  {}. #{}  ({})", i + 1, tag, count),
                        );
                    }
                    CommandResult::message(lines)
                }
            }
            Err(_) => CommandResult::message(format!(
                "{}\n(file does not exist yet)",
                path.display()
            )),
        },
        "search" => {
            let Some(query) = rest.filter(|r| !r.is_empty()) else {
                return CommandResult::error(
                    "Usage: /memory search <query>  or  /memory search --tag <tag>",
                );
            };
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => {
                    return CommandResult::message(format!(
                        "memory file does not exist yet at {}",
                        path.display()
                    ));
                }
            };
            let entries = crate::memory::parse_all(&content);

            // Check for --tag flag
            let results: Vec<&crate::memory::MemoryEntry> = if query.starts_with("--tag ") {
                let tag = query.trim_start_matches("--tag ").trim();
                crate::memory::search_by_tags(&entries, &[tag])
            } else {
                crate::memory::search_text(&entries, query)
            };

            if results.is_empty() {
                CommandResult::message(format!(
                    "no memory entries matching \"{query}\""
                ))
            } else {
                let body = render_entries(&results, "");
                CommandResult::message(format!(
                    "{} matching entry(ies) for \"{query}\":{}",
                    results.len(),
                    body
                ))
            }
        }
        "clear" => match fs::write(&path, "") {
            Ok(()) => CommandResult::message(format!("memory cleared: {}", path.display())),
            Err(err) => CommandResult::error(format!("failed to clear {}: {err}", path.display())),
        },
        "edit" => CommandResult::message(format!(
            "to edit your memory file, run:\n\n  ${{VISUAL:-${{EDITOR:-vi}}}} {}",
            path.display()
        )),
        "help" => CommandResult::message(memory_help(&path)),
        _ => CommandResult::error(format!(
            "unknown subcommand `{sub}`. Try `/memory help`.\n\n{}",
            memory_help(&path)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::tui::app::{App, TuiOptions};
    use tempfile::TempDir;

    fn create_test_app_with_memory(tmpdir: &TempDir, use_memory: bool) -> App {
        let options = TuiOptions {
            model: "deepseek-v4-pro".to_string(),
            workspace: tmpdir.path().to_path_buf(),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            use_alt_screen: true,
            use_mouse_capture: false,
            use_bracketed_paste: true,
            max_subagents: 1,
            skills_dir: tmpdir.path().join("skills"),
            memory_path: tmpdir.path().join("memory.md"),
            notes_path: tmpdir.path().join("notes.txt"),
            mcp_config_path: tmpdir.path().join("mcp.json"),
            use_memory,
            start_in_agent_mode: false,
            skip_onboarding: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        App::new(options, &Config::default())
    }

    #[test]
    fn memory_help_lists_subcommands_and_resolved_path() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, true);
        let result = memory(&mut app, Some("help"));
        let msg = result.message.expect("help should return text");
        assert!(msg.contains("Usage: /memory [show|path|clear|edit|tags|search <query>|search --tag <tag>|help]"));
        assert!(msg.contains("/memory edit"));
        assert!(msg.contains(app.memory_path.to_string_lossy().as_ref()));
    }

    #[test]
    fn memory_unknown_subcommand_points_to_help() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, true);
        let result = memory(&mut app, Some("wat"));
        let msg = result
            .message
            .expect("unknown subcommand should return text");
        assert!(msg.contains("Try `/memory help`"));
        assert!(msg.contains("/memory clear"));
    }

    #[test]
    fn memory_disabled_returns_enablement_hint() {
        let tmpdir = TempDir::new().expect("tempdir");
        let mut app = create_test_app_with_memory(&tmpdir, false);
        let result = memory(&mut app, None);
        let msg = result.message.expect("disabled memory should return text");
        assert!(msg.contains("user memory is disabled"));
        assert!(msg.contains("DEEPSEEK_MEMORY=on"));
    }
}
