//! Provider-agnostic guards for outbound user content blocks.
//!
//! Anthropic rejects any text content block whose text is empty
//! (`400 invalid_request_error — messages: text content blocks must be
//! non-empty`). An image-only user message must therefore carry ONLY the image
//! block, never an empty `{"type":"text","text":""}` beside it. Because the
//! Claude CLI replays its persisted transcript on every `--resume`, a single
//! empty text block permanently bricks a thread: it is re-sent on every turn.
//!
//! Every provider serializer routes its outbound user content through
//! [`sanitize_user_content`], the single choke point that:
//!
//! - drops empty / whitespace-only text blocks when other valid blocks remain,
//! - preserves image, tool_use, tool_result and non-empty text blocks,
//! - rejects the message locally when no valid block is left,
//!
//! This prevents any code path from emitting an empty text block again, and a
//! fully empty message fails fast in-process instead of at the provider.

use serde_json::Value;

/// Context for a sanitization pass, used only for safe structural logging.
/// Never carries prompt text, image bytes, keys, or auth headers.
pub struct SanitizeCtx<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    /// Number of attachments on the originating user message.
    pub attachment_count: usize,
}

/// A user message had no content any provider will accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyContentError {
    pub provider: String,
}

impl std::fmt::Display for EmptyContentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "message has no sendable content (no non-empty text and no valid attachment); not sent to {}",
            self.provider
        )
    }
}

impl std::error::Error for EmptyContentError {}

/// True for a `text` block whose text is missing, null, non-string, or
/// whitespace-only — i.e. exactly the blocks Anthropic rejects.
pub fn is_empty_text_block(block: &Value) -> bool {
    if block.get("type").and_then(Value::as_str) != Some("text") {
        return false;
    }
    match block.get("text").and_then(Value::as_str) {
        Some(text) => text.trim().is_empty(),
        // text is null / missing / not a string: not sendable.
        None => true,
    }
}

/// Drop empty/whitespace-only text blocks while preserving every other block
/// (image, tool_use, tool_result, non-empty text). Errors when nothing valid
/// remains, so an all-empty message is rejected locally before the provider
/// call. Idempotent: running it on already-clean content returns it unchanged.
pub fn sanitize_user_content(
    ctx: &SanitizeCtx,
    blocks: Vec<Value>,
) -> Result<Vec<Value>, EmptyContentError> {
    let mut kept = Vec::with_capacity(blocks.len());
    for (block_index, block) in blocks.into_iter().enumerate() {
        if is_empty_text_block(&block) {
            log_dropped_block(ctx, block_index, &block);
            continue;
        }
        kept.push(block);
    }
    if kept.is_empty() {
        tracing::warn!(
            provider = ctx.provider,
            model = ctx.model,
            message_index = 0,
            attachment_count = ctx.attachment_count,
            "rejecting user message locally: no valid content blocks after sanitization"
        );
        return Err(EmptyContentError {
            provider: ctx.provider.to_string(),
        });
    }
    Ok(kept)
}

fn log_dropped_block(ctx: &SanitizeCtx, block_index: usize, block: &Value) {
    let text = block.get("text").and_then(Value::as_str);
    let text_len = text.map(str::len).unwrap_or(0);
    let trimmed_len = text.map(|t| t.trim().len()).unwrap_or(0);
    let block_type = block.get("type").and_then(Value::as_str).unwrap_or("unknown");
    tracing::warn!(
        provider = ctx.provider,
        model = ctx.model,
        message_index = 0,
        block_index,
        block_type,
        text_len,
        trimmed_len,
        attachment_count = ctx.attachment_count,
        "dropping empty text content block before provider request"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx(attachment_count: usize) -> SanitizeCtx<'static> {
        SanitizeCtx {
            provider: "test",
            model: "test-model",
            attachment_count,
        }
    }

    fn text(t: &str) -> Value {
        json!({ "type": "text", "text": t })
    }

    fn image() -> Value {
        json!({ "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } })
    }

    #[test]
    fn empty_text_block_detected() {
        assert!(is_empty_text_block(&text("")));
        assert!(is_empty_text_block(&text("   ")));
        assert!(is_empty_text_block(&text("\n\t ")));
        assert!(is_empty_text_block(&json!({ "type": "text", "text": null })));
        assert!(is_empty_text_block(&json!({ "type": "text" })));
        assert!(!is_empty_text_block(&text("hello")));
        assert!(!is_empty_text_block(&image()));
    }

    #[test]
    fn text_only_message_kept() {
        let out = sanitize_user_content(&ctx(0), vec![text("hello")]).unwrap();
        assert_eq!(out, vec![text("hello")]);
    }

    #[test]
    fn text_plus_image_kept() {
        let out = sanitize_user_content(&ctx(1), vec![text("look"), image()]).unwrap();
        assert_eq!(out, vec![text("look"), image()]);
    }

    #[test]
    fn image_only_message_has_no_empty_text_block() {
        // The bug: an empty text block sits beside the image. Sanitize removes it.
        let out = sanitize_user_content(&ctx(1), vec![text(""), image()]).unwrap();
        assert_eq!(out, vec![image()]);
        assert!(!out.iter().any(is_empty_text_block));
    }

    #[test]
    fn whitespace_text_beside_image_dropped() {
        let out = sanitize_user_content(&ctx(1), vec![text("   \n"), image()]).unwrap();
        assert_eq!(out, vec![image()]);
    }

    #[test]
    fn multiple_images_no_text_kept() {
        let out = sanitize_user_content(&ctx(2), vec![text(""), image(), image()]).unwrap();
        assert_eq!(out, vec![image(), image()]);
    }

    #[test]
    fn tool_blocks_preserved() {
        let tool_use = json!({ "type": "tool_use", "id": "t1", "name": "x", "input": {} });
        let tool_result = json!({ "type": "tool_result", "tool_use_id": "t1", "content": "ok" });
        let out = sanitize_user_content(
            &ctx(0),
            vec![text(""), tool_use.clone(), tool_result.clone()],
        )
        .unwrap();
        assert_eq!(out, vec![tool_use, tool_result]);
    }

    #[test]
    fn empty_text_no_attachment_rejected() {
        let err = sanitize_user_content(&ctx(0), vec![text("")]).unwrap_err();
        assert_eq!(err.provider, "test");
    }

    #[test]
    fn whitespace_only_no_attachment_rejected() {
        assert!(sanitize_user_content(&ctx(0), vec![text("   ")]).is_err());
    }

    #[test]
    fn empty_content_array_rejected() {
        assert!(sanitize_user_content(&ctx(0), vec![]).is_err());
    }

    #[test]
    fn sanitize_is_idempotent() {
        let once = sanitize_user_content(&ctx(1), vec![text(""), image()]).unwrap();
        let twice = sanitize_user_content(&ctx(1), once.clone()).unwrap();
        assert_eq!(once, twice);
        assert_eq!(twice, vec![image()]);
    }
}
