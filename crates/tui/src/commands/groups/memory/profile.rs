use crate::commands::CommandResult;
use std::fs;
use std::path::PathBuf;

const USAGE: &str = "/me [show|set <field>=<value>|path|clear|help]";

fn profile_path() -> Option<PathBuf> {
    crate::profile::default_profile_path()
}

fn profile(_app: &mut crate::tui::app::App, arg: Option<&str>) -> CommandResult {
    let input = match arg {
        Some(c) => c.trim(),
        None => "",
    };

    let path = match profile_path() {
        Some(p) => p,
        None => {
            return CommandResult::error("could not determine home directory for profile");
        }
    };

    if input.is_empty() || input == "show" {
        return show_profile(&path);
    }

    let (command, rest) = split_command(input);

    match command.to_ascii_lowercase().as_str() {
        "show" => show_profile(&path),
        "set" => set_field_command(&path, rest),
        "path" => CommandResult::message(format!("Profile path: {}", path.display())),
        "clear" => clear_profile(&path),
        "help" => CommandResult::message(format!("Usage: {USAGE}")),
        _ => CommandResult::error(format!(
            "unknown subcommand `{command}`. Try `/me help`.\n\nUsage: {USAGE}"
        )),
    }
}

fn show_profile(path: &PathBuf) -> CommandResult {
    let profile = crate::profile::load(path);
    match profile {
        Some(p) if !p.is_empty() => {
            let block = crate::profile::render_block(&p)
                .unwrap_or_else(|| "(empty profile)".to_string());
            CommandResult::message(format!("{}\n\n{}", path.display(), block))
        }
        _ => CommandResult::message(format!(
            "{}\n(no profile set yet — use `/me set <field>=<value>` to add fields)",
            path.display()
        )),
    }
}

fn set_field_command(path: &PathBuf, rest: Option<&str>) -> CommandResult {
    let input = match rest {
        Some(s) => s.trim(),
        None => return CommandResult::error("Usage: /me set <field>=<value>"),
    };
    if input.is_empty() {
        return CommandResult::error("Usage: /me set <field>=<value>");
    }

    // Parse key=value (support = in value)
    let eq_pos = input.find('=').ok_or_else(|| {
        CommandResult::error(format!(
            "expected `<field>=<value>`, got `{input}`.\nUsage: /me set <field>=<value>"
        ))
    });
    let eq_pos = match eq_pos {
        Ok(p) => p,
        Err(e) => return e,
    };

    let key = input[..eq_pos].trim();
    let value = input[eq_pos + 1..].trim();

    if key.is_empty() || value.is_empty() {
        return CommandResult::error("field and value cannot be empty");
    }

    // Load or create profile
    let mut profile = crate::profile::load(path).unwrap_or_default();

    if let Err(err) = crate::profile::set_field(&mut profile, key, value) {
        return CommandResult::error(err);
    }

    if let Err(err) = crate::profile::save(path, &profile) {
        return CommandResult::error(err);
    }

    // Show the updated profile
    let block = crate::profile::render_block(&profile)
        .unwrap_or_else(|| "(empty profile)".to_string());
    CommandResult::message(format!("Profile updated:\n\n{}", block))
}

fn clear_profile(path: &PathBuf) -> CommandResult {
    // Write empty
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::write(path, "") {
        Ok(()) => CommandResult::message(format!("Profile cleared: {}", path.display())),
        Err(err) => CommandResult::error(format!("failed to clear profile: {err}")),
    }
}

fn split_command(input: &str) -> (&str, Option<&str>) {
    match input.find(char::is_whitespace) {
        Some(index) => (&input[..index], Some(input[index..].trim())),
        None => (input, None),
    }
}

pub(in crate::commands) const COMMAND_INFO: crate::commands::traits::CommandInfo =
    crate::commands::traits::CommandInfo {
        name: "me",
        aliases: &[],
        usage: USAGE,
        description_id: crate::localization::MessageId::CmdProfileDescription,
    };

pub(in crate::commands) struct ProfileCmd;

impl crate::commands::traits::RegisterCommand for ProfileCmd {
    fn info() -> &'static crate::commands::traits::CommandInfo {
        &COMMAND_INFO
    }

    fn execute(
        app: &mut crate::tui::app::App,
        arg: Option<&str>,
    ) -> crate::commands::CommandResult {
        profile(app, arg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_missing_profile_returns_hint() {
        let tmp = std::env::temp_dir().join("profile_test_show_missing");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("profile.toml");

        let result = show_profile(&path);
        let msg = result.message.expect("should return message");
        assert!(msg.contains("no profile set yet"));
        assert!(msg.contains("/me set"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn set_field_creates_and_shows_profile() {
        let tmp = std::env::temp_dir().join("profile_test_set_field");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("profile.toml");

        // Set name
        let result = set_field_command(&path, Some("name=老潘"));
        assert!(result.message.expect("should succeed").contains("Profile updated"));

        // Verify file written
        assert!(path.exists());
        let loaded = crate::profile::load(&path).expect("should load");
        assert_eq!(loaded.name.as_deref(), Some("老潘"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn clear_removes_profile() {
        let tmp = std::env::temp_dir().join("profile_test_clear");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("profile.toml");

        crate::profile::save(&path, &crate::profile::Profile {
            name: Some("test".to_string()),
            ..Default::default()
        }).expect("save should work");
        assert!(path.exists());

        clear_profile(&path);
        let loaded = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(loaded.trim().is_empty());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn set_invalid_field_returns_error() {
        let tmp = std::env::temp_dir().join("profile_test_invalid");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("profile.toml");

        let result = set_field_command(&path, Some("preferred_style=extreme"));
        assert!(result.message.expect("should return error").contains("invalid preferred_style"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
