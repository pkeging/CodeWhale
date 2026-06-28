use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// User profile stored at `~/.codewhale/profile.toml`.
///
/// The profile gives the AI model a lightweight summary of who the user is,
/// how they prefer to work, and what domains they care about. Unlike
/// `/memory` which is session-scoped free text, the profile is structured
/// and changes infrequently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: Option<String>,
    pub locale: Option<String>,
    pub preferred_style: Option<String>,
    pub domain: Option<Vec<String>>,
    pub work_mode: Option<String>,
}

impl Profile {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.locale.is_none()
            && self.preferred_style.is_none()
            && self.domain.is_none()
            && self.work_mode.is_none()
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: None,
            locale: None,
            preferred_style: None,
            domain: None,
            work_mode: None,
        }
    }
}

const PROFILE_FILE: &str = "profile.toml";

/// Resolve the profile path in the CodeWhale config directory.
pub fn default_profile_path() -> Option<PathBuf> {
    effective_home_dir().map(|home| home.join(".codewhale").join(PROFILE_FILE))
}

/// Load the profile from `path`, returning `None` when the file doesn't exist
/// or is malformed.
#[must_use]
pub fn load(path: &Path) -> Option<Profile> {
    let content = fs::read_to_string(path).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    toml::from_str(&content).ok()
}

/// Save the profile to `path`, creating parent directories if needed.
pub fn save(path: &Path, profile: &Profile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("failed to create profile directory: {e}"))?;
    }
    let content = toml::to_string_pretty(profile)
        .map_err(|e| format!("failed to serialize profile: {e}"))?;
    fs::write(path, content).map_err(|e| format!("failed to write profile: {e}"))
}

/// Render a user profile as a system prompt block.
///
/// Returns `None` when every field is empty so the caller can skip injection.
#[must_use]
pub fn render_block(profile: &Profile) -> Option<String> {
    if profile.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    if let Some(name) = &profile.name {
        lines.push(format!("- Name: {name}"));
    }
    if let Some(locale) = &profile.locale {
        lines.push(format!("- Locale: {locale}"));
    }
    if let Some(style) = &profile.preferred_style {
        lines.push(format!("- Preferred style: {style}"));
    }
    if let Some(domains) = &profile.domain {
        if !domains.is_empty() {
            lines.push(format!("- Domains: {}", domains.join(", ")));
        }
    }
    if let Some(mode) = &profile.work_mode {
        lines.push(format!("- Work mode: {mode}"));
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!("## User Profile\n\n{}", lines.join("\n")))
}

/// Set a profile field from a `"key=value"` string.
///
/// Supported keys: `name`, `locale`, `preferred_style`, `domain`, `work_mode`.
/// The `domain` key accepts comma-separated values.
pub fn set_field(profile: &mut Profile, key: &str, value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("value cannot be empty".to_string());
    }
    match key {
        "name" => profile.name = Some(value.to_string()),
        "locale" => profile.locale = Some(value.to_string()),
        "preferred_style" => {
            let valid = ["concise", "detailed", "balanced"];
            if !valid.contains(&value) {
                return Err(format!(
                    "invalid preferred_style `{value}`. Choose from: {}",
                    valid.join(", ")
                ));
            }
            profile.preferred_style = Some(value.to_string());
        }
        "domain" => {
            let domains: Vec<String> =
                value.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            if domains.is_empty() {
                return Err("domain list cannot be empty".to_string());
            }
            profile.domain = Some(domains);
        }
        "work_mode" => {
            let valid = ["solo", "team", "hybrid"];
            if !valid.contains(&value) {
                return Err(format!(
                    "invalid work_mode `{value}`. Choose from: {}",
                    valid.join(", ")
                ));
            }
            profile.work_mode = Some(value.to_string());
        }
        _ => {
            return Err(format!(
                "unknown field `{key}`. Supported: name, locale, preferred_style, domain, work_mode"
            ));
        }
    }
    Ok(())
}

/// Load both profile and preferences, returning the merged profile.
/// Explicit profile fields always override learned preferences.
#[must_use]
pub fn load_merged(profile_path: &Path, prefs: &crate::preferences::Preferences) -> Profile {
    let mut merged = load(profile_path).unwrap_or_default();

    // Only fill fields not explicitly set by the user.
    if merged.preferred_style.is_none() {
        if let Some(style) = crate::preferences::preferred_style_from_learned(prefs) {
            merged.preferred_style = Some(style);
        }
    }
    if merged.domain.is_none() {
        if let Some(domains) = crate::preferences::domain_from_learned(prefs) {
            merged.domain = Some(domains);
        }
    }
    if merged.work_mode.is_none() {
        if let Some(mode) = crate::preferences::work_mode_from_learned(prefs) {
            merged.work_mode = Some(mode);
        }
    }

    merged
}

/// Render the merged profile as a system prompt block, including learned preferences.
#[must_use]
pub fn render_merged_block(merged: &Profile, prefs: &crate::preferences::Preferences) -> Option<String> {
    let profile_block = render_block(merged);
    let learned_block = crate::preferences::render_learned_block(prefs);

    match (profile_block, learned_block) {
        (Some(p), Some(l)) => Some(format!("{p}\n\n{l}")),
        (Some(p), None) => Some(p),
        (None, Some(l)) => Some(l),
        (None, None) => None,
    }
}

fn effective_home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}
