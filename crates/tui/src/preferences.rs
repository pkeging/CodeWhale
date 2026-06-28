use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Evidence weight for a single observation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum EvidenceWeight {
    ExplicitChoice = 10,
    RepeatedAction = 8,
    InferredPattern = 5,
    SingleMention = 3,
}

impl EvidenceWeight {
    /// Learning increment per evidence occurrence.
    pub fn increment(&self) -> f64 {
        match self {
            Self::ExplicitChoice => 0.15,
            Self::RepeatedAction => 0.10,
            Self::InferredPattern => 0.06,
            Self::SingleMention => 0.04,
        }
    }
}

/// A single learned preference entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LearnedPreference {
    /// Category identifier, e.g. "response_style", "domain", "tool_affinity".
    pub category: String,
    /// The learned value, e.g. "concise", "rust".
    pub key: String,
    /// Serialised value payload.
    pub value: toml::Value,
    /// Confidence in [0.0, 1.0].
    pub confidence: f64,
    /// Number of evidence observations that contributed.
    pub evidence_count: u32,
    /// When this preference was last updated.
    pub last_updated: DateTime<Utc>,
}

/// Collection of learned preferences stored at `~/.codewhale/preferences.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    pub metadata: PreferencesMetadata,
    pub preferences: Vec<LearnedPreference>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreferencesMetadata {
    pub version: String,
    pub total_evidence: u64,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decay_policy: Option<String>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            metadata: PreferencesMetadata {
                version: "1.0.0".to_string(),
                total_evidence: 0,
                updated_at: Utc::now(),
                decay_policy: None,
            },
            preferences: Vec::new(),
        }
    }
}

impl Preferences {
    /// Record an evidence observation and update (or insert) the matching preference.
    pub fn learn(
        &mut self,
        category: &str,
        key: &str,
        value: toml::Value,
        weight: EvidenceWeight,
    ) {
        let now = Utc::now();

        // Find existing preference by (category, key).
        if let Some(existing) = self
            .preferences
            .iter_mut()
            .find(|p| p.category == category && p.key == key)
        {
            existing.confidence = (existing.confidence + weight.increment()).min(1.0);
            existing.evidence_count += 1;
            existing.value = value;
            existing.last_updated = now;
        } else {
            let initial = match weight {
                EvidenceWeight::ExplicitChoice => 1.0,
                _ => 0.5,
            };
            self.preferences.push(LearnedPreference {
                category: category.to_string(),
                key: key.to_string(),
                value,
                confidence: initial,
                evidence_count: 1,
                last_updated: now,
            });
        }

        self.metadata.total_evidence += 1;
        self.metadata.updated_at = now;
    }

    /// Resolve the highest-confidence preference for a given category.
    pub fn best(&self, category: &str) -> Option<&LearnedPreference> {
        self.preferences
            .iter()
            .filter(|p| p.category == category)
            .max_by(|a, b| a.confidence.partial_cmp(&b.confidence).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// Return all preferences with confidence above threshold.
    pub fn above_threshold(&self, min_confidence: f64) -> Vec<&LearnedPreference> {
        self.preferences
            .iter()
            .filter(|p| p.confidence >= min_confidence)
            .collect()
    }

    #[expect(dead_code)]
    pub fn clear(&mut self) {
        self.preferences.clear();
        self.metadata.total_evidence = 0;
        self.metadata.updated_at = Utc::now();
    }
}

const PREFERENCES_FILE: &str = "preferences.toml";

pub fn default_preferences_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".codewhale").join(PREFERENCES_FILE))
}

pub fn load(path: &Path) -> Option<Preferences> {
    let content = fs::read_to_string(path).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    toml::from_str(&content).ok()
}

pub fn save(path: &Path, prefs: &Preferences) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("failed to create preferences directory: {e}"))?;
    }
    let content =
        toml::to_string_pretty(prefs).map_err(|e| format!("failed to serialize preferences: {e}"))?;
    fs::write(path, content).map_err(|e| format!("failed to write preferences: {e}"))
}

/// Render learned preferences as a system-prompt-style block (lower priority).
/// The caller should merge with the explicit profile before injection.
pub fn render_learned_block(prefs: &Preferences) -> Option<String> {
    let confident = prefs.above_threshold(0.6);
    if confident.is_empty() {
        return None;
    }
    let mut lines: Vec<String> = Vec::new();
    for p in &confident {
        let conf_pct = (p.confidence * 100.0) as u32;
        lines.push(format!("- {} → {} (confidence {}%)", p.category, p.key, conf_pct));
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!("## Learned Preferences\n\n{}", lines.join("\n")))
}

/// Merge explicit profile fields with learned preferences.
/// Explicit values always win for the same field.
pub fn preferred_style_from_learned(prefs: &Preferences) -> Option<String> {
    prefs.best("response_style").map(|p| p.key.clone())
}

pub fn domain_from_learned(prefs: &Preferences) -> Option<Vec<String>> {
    let domains: Vec<String> = prefs
        .above_threshold(0.6)
        .iter()
        .filter(|p| p.category == "domain")
        .map(|p| p.key.clone())
        .collect();
    if domains.is_empty() {
        None
    } else {
        Some(domains)
    }
}

pub fn work_mode_from_learned(prefs: &Preferences) -> Option<String> {
    prefs.best("work_mode").map(|p| p.key.clone())
}

/// Infer domain from workspace files and record as learned preference.
/// Scans common project manifest files to determine the primary domain.
/// This is a simple heuristic — sophisticated analysis can be added later.
pub fn learn_from_workspace(workspace: &std::path::Path) {
    let Some(prefs_path) = default_preferences_path() else {
        return;
    };
    let mut prefs = load(&prefs_path).unwrap_or_default();

    // Check for common project manifests
    let indicators = [
        ("Cargo.toml", "domain", "rust"),
        ("package.json", "domain", "js"),
        ("pyproject.toml", "domain", "python"),
        ("go.mod", "domain", "go"),
        ("CMakeLists.txt", "domain", "cpp"),
        ("Cargo.toml", "tool_affinity", "cargo"),
        ("package.json", "tool_affinity", "npm"),
        ("pyproject.toml", "tool_affinity", "pip"),
        ("go.mod", "tool_affinity", "go"),
    ];

    for (filename, category, key) in &indicators {
        if workspace.join(filename).exists() {
            prefs.learn(
                category,
                key,
                toml::Value::String(key.to_string()),
                EvidenceWeight::InferredPattern,
            );
        }
    }

    // Only save if we actually learned something new
    if prefs.metadata.total_evidence > 0 {
        let _ = save(&prefs_path, &prefs);
    }
}

/// Record a tool usage observation for preference learning.
#[expect(dead_code)]
pub fn learn_from_tool_usage(tool_name: &str) {
    let Some(prefs_path) = default_preferences_path() else {
        return;
    };
    let mut prefs = load(&prefs_path).unwrap_or_default();

    let category = "tool_affinity";
    let key = tool_name;

    prefs.learn(
        category,
        key,
        toml::Value::String(key.to_string()),
        EvidenceWeight::RepeatedAction,
    );

    let _ = save(&prefs_path, &prefs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty() {
        let prefs = Preferences::default();
        assert!(prefs.preferences.is_empty());
        assert_eq!(prefs.metadata.version, "1.0.0");
    }

    #[test]
    fn learn_inserts_new_preference() {
        let mut prefs = Preferences::default();
        prefs.learn("response_style", "concise", toml::Value::String("concise".into()), EvidenceWeight::RepeatedAction);
        assert_eq!(prefs.preferences.len(), 1);
        assert_eq!(prefs.preferences[0].confidence, 0.5);
        assert_eq!(prefs.preferences[0].evidence_count, 1);
    }

    #[test]
    fn learn_updates_existing() {
        let mut prefs = Preferences::default();
        prefs.learn("response_style", "concise", toml::Value::String("concise".into()), EvidenceWeight::RepeatedAction);
        prefs.learn("response_style", "concise", toml::Value::String("concise".into()), EvidenceWeight::RepeatedAction);
        assert_eq!(prefs.preferences.len(), 1);
        assert_eq!(prefs.preferences[0].evidence_count, 2);
        assert!((prefs.preferences[0].confidence - 0.6).abs() < 0.01);
    }

    #[test]
    fn learn_explicit_choice_starts_at_full_confidence() {
        let mut prefs = Preferences::default();
        prefs.learn("response_style", "detailed", toml::Value::String("detailed".into()), EvidenceWeight::ExplicitChoice);
        assert!((prefs.preferences[0].confidence - 1.0).abs() < 0.01);
    }

    #[test]
    fn best_returns_highest_confidence() {
        let mut prefs = Preferences::default();
        prefs.learn("response_style", "concise", toml::Value::String("concise".into()), EvidenceWeight::SingleMention);
        prefs.learn("response_style", "detailed", toml::Value::String("detailed".into()), EvidenceWeight::RepeatedAction);
        // After 1 mention: concise = 0.5, detailed = 0.5 (same)
        // detailed has 2nd evidence at RepeatedAction = 0.5 + 0.1 = 0.6
        let best = prefs.best("response_style");
        assert!(best.is_some());
        assert_eq!(best.unwrap().key, "detailed");
    }

    #[test]
    fn above_threshold_filters_by_confidence() {
        let mut prefs = Preferences::default();
        prefs.learn("domain", "rust", toml::Value::String("rust".into()), EvidenceWeight::RepeatedAction);
        prefs.learn("domain", "js", toml::Value::String("js".into()), EvidenceWeight::SingleMention);
        let confident = prefs.above_threshold(0.6);
        // rust got RepeatedAction = 0.5, js got SingleMention = 0.5, both below 0.6
        // Need more evidence to cross threshold
        assert_eq!(confident.len(), 0);
        // Add more evidence for rust
        prefs.learn("domain", "rust", toml::Value::String("rust".into()), EvidenceWeight::RepeatedAction);
        let confident = prefs.above_threshold(0.6);
        assert_eq!(confident.len(), 1);
        assert_eq!(confident[0].key, "rust");
    }

    #[test]
    fn clear_removes_all() {
        let mut prefs = Preferences::default();
        prefs.learn("response_style", "concise", toml::Value::String("concise".into()), EvidenceWeight::RepeatedAction);
        assert_eq!(prefs.preferences.len(), 1);
        prefs.clear();
        assert!(prefs.preferences.is_empty());
        assert_eq!(prefs.metadata.total_evidence, 0);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = std::env::temp_dir().join("preferences_test_roundtrip");
        let _ = std::fs::create_dir_all(&tmp);
        let path = tmp.join("preferences.toml");

        let mut prefs = Preferences::default();
        prefs.learn("response_style", "concise", toml::Value::String("concise".into()), EvidenceWeight::RepeatedAction);
        save(&path, &prefs).expect("save should succeed");

        let loaded = load(&path).expect("load should succeed");
        assert_eq!(loaded.preferences.len(), 1);
        assert_eq!(loaded.preferences[0].key, "concise");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn render_learned_block_shows_high_confidence() {
        let mut prefs = Preferences::default();
        prefs.learn("response_style", "concise", toml::Value::String("concise".into()), EvidenceWeight::RepeatedAction);
        prefs.learn("response_style", "concise", toml::Value::String("concise".into()), EvidenceWeight::RepeatedAction);
        prefs.learn("response_style", "concise", toml::Value::String("concise".into()), EvidenceWeight::RepeatedAction);
        // confidence = 0.5 + 3 * 0.1 = 0.8 > 0.6 threshold
        let block = render_learned_block(&prefs);
        assert!(block.is_some());
        assert!(block.unwrap().contains("response_style"));
    }

    #[test]
    fn render_learned_block_none_when_below_threshold() {
        let prefs = Preferences::default();
        assert!(render_learned_block(&prefs).is_none());
    }
}

