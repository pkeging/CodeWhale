//! `remember` tool — model-callable bullet-add into the user memory file.
//!
//! Lets the model itself notice a durable preference, convention, or fact
//! worth keeping across sessions and write it to the user's `memory.md`.
//! The tool is auto-approved and side-effecting only on the user-owned
//! memory file (`~/.deepseek/memory.md` by default), so it doesn't get
//! gated behind the same approval flow as shell or arbitrary file writes.
//!
//! Only registered when `[memory] enabled = true` (or
//! `DEEPSEEK_MEMORY=on`). When disabled, the tool isn't surfaced to the
//! model at all, so prompts that mention `remember` simply fall through.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec, required_str,
};

/// Tool that appends one bullet to the user memory file.
pub struct RememberTool;

#[async_trait]
impl ToolSpec for RememberTool {
    fn name(&self) -> &'static str {
        "remember"
    }

    fn description(&self) -> &'static str {
        "Append a durable note to the user memory file so it surfaces in \
         future sessions. Use this when the user states a preference, a \
         convention they want enforced, or a fact about themselves or \
         their workflow that you should not have to relearn next time. \
         Keep notes terse (one sentence). Don't store secrets, transient \
         tasks, or reasoning scratch — those belong in a checklist or in \
         the conversation."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "note": {
                    "type": "string",
                    "description": "The single-sentence durable note to remember."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string", "description": "A hashtag (with or without leading #)" },
                    "description": "Optional tags to attach to this entry for future retrieval. Use tags like \"project:codewhale\", \"type:preference\", or \"scope:config\"."
                }
            },
            "required": ["note"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        // Memory writes are scoped to the user's own memory file; gating
        // them behind the standard shell/write approval would defeat the
        // point of automatic memory.
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let note = required_str(&input, "note")?;
        let path = context.memory_path.as_ref().ok_or_else(|| {
            ToolError::execution_failed(
                "user memory is disabled — set `[memory] enabled = true` in config.toml or \
                 `DEEPSEEK_MEMORY=on` in the environment to enable",
            )
        })?;

        // Extract optional tags, normalizing leading #
        let tags: Vec<String> = input
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|t| {
                        let trimmed = t.trim().trim_start_matches('#');
                        if trimmed.is_empty() { String::new() } else { trimmed.to_string() }
                    })
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        // Auto-tag when the model didn't provide explicit tags
        let final_tags: Vec<String> = if tags.is_empty() {
            crate::memory::auto_tag(note, 5)
        } else {
            tags
        };
        let tag_refs: Vec<&str> = final_tags.iter().map(String::as_str).collect();
        crate::memory::append_entry(path, note, &tag_refs).map_err(|err| {
            ToolError::execution_failed(format!("failed to append to {}: {err}", path.display()))
        })?;

        let tag_msg = if final_tags.is_empty() {
            String::new()
        } else {
            format!(
                " [{}]",
                final_tags
                    .iter()
                    .map(|t| format!("#{t}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        Ok(ToolResult::success(format!(
            "remembered: {}{}",
            note.trim_start_matches('#').trim(),
            tag_msg
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn ctx_with_memory(path: PathBuf) -> ToolContext {
        let mut ctx = ToolContext::new(path.parent().unwrap_or_else(|| std::path::Path::new(".")));
        ctx.memory_path = Some(path);
        ctx
    }

    #[tokio::test]
    async fn returns_error_when_memory_disabled() {
        let tmp = tempdir().unwrap();
        let mut ctx = ToolContext::new(tmp.path());
        ctx.memory_path = None; // explicitly disabled

        let tool = RememberTool;
        let err = tool
            .execute(json!({"note": "use 4 spaces for indentation"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("memory is disabled"), "{err}");
    }

    #[tokio::test]
    async fn appends_bullet_to_memory_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        let ctx = ctx_with_memory(path.clone());

        let tool = RememberTool;
        let result = tool
            .execute(json!({"note": "use 4 spaces for indentation"}), &ctx)
            .await
            .expect("ok");
        assert!(result.success);
        assert!(result.content.contains("4 spaces"));

        let body = std::fs::read_to_string(&path).expect("read");
        assert!(body.contains("4 spaces"));
        assert!(body.starts_with("- ("), "{body}");
    }

    #[tokio::test]
    async fn rejects_missing_note_field() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        let ctx = ctx_with_memory(path);

        let tool = RememberTool;
        let err = tool.execute(json!({}), &ctx).await.unwrap_err();
        assert!(err.to_string().to_lowercase().contains("note"), "{err}");
    }

    #[tokio::test]
    async fn appends_with_tags() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        let ctx = ctx_with_memory(path.clone());

        let tool = RememberTool;
        let result = tool
            .execute(json!({"note": "use 4 spaces", "tags": ["indentation", "rust"]}), &ctx)
            .await
            .expect("ok");
        assert!(result.success);

        let body = std::fs::read_to_string(&path).expect("read");
        assert!(body.contains("use 4 spaces"), "{body}");
        assert!(body.contains("#indentation"), "{body}");
        assert!(body.contains("#rust"), "{body}");
    }

    #[tokio::test]
    async fn appends_with_tags_normalizes_leading_hash() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        let ctx = ctx_with_memory(path.clone());

        let tool = RememberTool;
        let result = tool
            .execute(json!({"note": "prefer tabs", "tags": ["#indentation", " #spacing"]}), &ctx)
            .await
            .expect("ok");
        assert!(result.success);

        let body = std::fs::read_to_string(&path).expect("read");
        assert!(body.contains("#indentation"), "{body}");
        assert!(body.contains("#spacing"), "{body}");
    }

    #[tokio::test]
    async fn appends_with_empty_tags_skips() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        let ctx = ctx_with_memory(path.clone());

        let tool = RememberTool;
        let result = tool
            .execute(json!({"note": "bare note", "tags": []}), &ctx)
            .await
            .expect("ok");
        assert!(result.success);

        let body = std::fs::read_to_string(&path).expect("read");
        assert!(body.contains("bare note"), "{body}");
        assert!(!body.contains('#'), "no tag char expected: {body}");
    }

    #[tokio::test]
    async fn auto_tags_when_no_tags_provided() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        let ctx = ctx_with_memory(path.clone());

        let tool = RememberTool;
        let result = tool
            .execute(json!({"note": "Use CodeWhale with DeepSeek V4"}), &ctx)
            .await
            .expect("ok");
        assert!(result.success);
        // auto_tag should extract "codewhale" and "deepseek" from capitalized words
        assert!(result.content.contains("#codewhale"), "result: {}", result.content);
        assert!(result.content.contains("#deepseek"), "result: {}", result.content);

        let body = std::fs::read_to_string(&path).expect("read");
        assert!(body.contains("Use CodeWhale with DeepSeek V4"), "{body}");
        assert!(body.contains("#codewhale"), "{body}");
        assert!(body.contains("#deepseek"), "{body}");
    }

    #[tokio::test]
    async fn explicit_tags_override_auto_tag() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("memory.md");
        let ctx = ctx_with_memory(path.clone());

        let tool = RememberTool;
        let result = tool
            .execute(json!({"note": "Use CodeWhale", "tags": ["manual"]}), &ctx)
            .await
            .expect("ok");
        assert!(result.success);
        // Should NOT auto-tag since explicit tags were provided
        assert!(result.content.contains("#manual"), "result: {}", result.content);
        assert!(!result.content.contains("#codewhale"), "should not auto-tag: {}", result.content);

        let body = std::fs::read_to_string(&path).expect("read");
        assert!(body.contains("#manual"), "{body}");
    }
}
