//! Groups a streaming assistant reply into speakable chunks.
//!
//! Deltas arrive at token granularity; one TTS request per token would sound
//! shredded and burn request overhead, one per turn would forfeit streaming.
//! This sits between: accumulate deltas, flush at clause/sentence boundaries,
//! and sanitize markdown into something a voice can say.
//!
//! Dedup matters: every driver emits `assistant_delta` fragments and then a
//! full `assistant_message` carrying the same text block (see the frontend's
//! `replaceLastStreaming`). A naive consumer would speak everything twice.
//!
//! Pure — no I/O, no clocks. The session loop owns timing and passes elapsed
//! time in for the stall flush.

/// Don't flush at a sentence boundary until at least this much is pending —
/// tiny fragments sound choppy even on a fast TTS model.
const MIN_FLUSH_CHARS: usize = 30;
/// Force a cut once this much is pending, at the best boundary available.
const HARD_MAX_CHARS: usize = 350;
/// A stalled stream flushes what it has after this long...
pub const STALL_FLUSH_MS: u64 = 1500;
/// ...provided there is at least a phrase worth saying.
pub const STALL_MIN_CHARS: usize = 20;

/// Common abbreviations a period does not end a sentence after.
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "etc", "e.g", "i.e", "no", "approx",
];

#[derive(Default)]
pub struct Chunker {
    /// Delta text of the current assistant block, for `assistant_message` dedup.
    block_buf: String,
    /// Raw text not yet flushed to TTS.
    pending: String,
    /// Are we inside a ``` fence at the end of `pending`'s consumed prefix?
    /// Tracked across flushes so a cut never lands mid-code.
    open_fence: bool,
    /// "code omitted" is spoken at most once per turn.
    code_notice_emitted: bool,
}

impl Chunker {
    /// Feed an `assistant_delta`; returns any chunks now ready to speak.
    pub fn push_delta(&mut self, text: &str) -> Vec<String> {
        self.block_buf.push_str(text);
        self.pending.push_str(text);
        self.drain_ready()
    }

    /// Feed the full `assistant_message` that closes a text block. Speaks only
    /// what the deltas did not already cover, then ends the block: a block
    /// boundary is a paragraph boundary, so the tail flushes.
    pub fn push_message(&mut self, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        if self.block_buf.is_empty() {
            // A driver path that skipped deltas — the message is all we get.
            self.pending.push_str(text);
        } else if let Some(rest) = text.strip_prefix(self.block_buf.as_str()) {
            self.pending.push_str(rest);
        }
        // else: deltas already spoke this block and the final message's
        // wording drifted — trust what was streamed, discard the rest.
        self.block_buf.clear();
        out.extend(self.drain_ready());
        out.extend(self.flush_tail());
        out
    }

    /// The stream has stalled for `waited_ms` — flush a phrase if one is worth
    /// saying, so slow token streams don't hold audio hostage.
    pub fn stall_flush(&mut self, waited_ms: u64) -> Vec<String> {
        if waited_ms < STALL_FLUSH_MS || self.pending.trim().len() < STALL_MIN_CHARS {
            return Vec::new();
        }
        // Never cut inside a fence; the stall ends when the fence does.
        if self.fence_open_at(self.pending.len()) {
            return Vec::new();
        }
        self.flush_tail()
    }

    /// Turn completed: flush whatever remains.
    pub fn finish(&mut self) -> Vec<String> {
        self.block_buf.clear();
        self.flush_tail()
    }

    /// Turn aborted or failed: nothing further should be spoken.
    pub fn abort(&mut self) {
        self.block_buf.clear();
        self.pending.clear();
        self.open_fence = false;
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.trim().is_empty()
    }

    fn drain_ready(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(cut) = self.flush_point() {
            let chunk: String = self.pending.drain(..cut).collect();
            self.consume_fences(&chunk);
            if let Some(text) = self.sanitize(&chunk) {
                out.push(text);
            }
        }
        out
    }

    fn flush_tail(&mut self) -> Vec<String> {
        let mut out = self.drain_ready();
        let rest: String = std::mem::take(&mut self.pending);
        self.consume_fences(&rest);
        if let Some(text) = self.sanitize(&rest) {
            out.push(text);
        }
        out
    }

    /// Byte index to cut `pending` at, or `None` to keep accumulating.
    fn flush_point(&self) -> Option<usize> {
        let bytes = self.pending.as_bytes();
        if self.pending.len() >= MIN_FLUSH_CHARS {
            let mut best = None;
            for (i, &b) in bytes.iter().enumerate() {
                if !matches!(b, b'.' | b'!' | b'?' | b'\n') {
                    continue;
                }
                // A terminator counts only before whitespace/EOL — "3.5" and
                // "e.g." keep flowing.
                let ends_here = bytes
                    .get(i + 1)
                    .map(|&n| n.is_ascii_whitespace())
                    .unwrap_or(false);
                if !ends_here {
                    continue;
                }
                if b == b'.' && self.is_abbreviation(i) {
                    continue;
                }
                if i + 1 >= MIN_FLUSH_CHARS && !self.fence_open_at(i + 1) {
                    best = Some(i + 1);
                    break;
                }
            }
            if best.is_some() {
                return best;
            }
        }
        if self.pending.len() >= HARD_MAX_CHARS {
            // Best clause boundary, else whitespace, scanning back from the cap.
            let cap = floor_char_boundary(&self.pending, HARD_MAX_CHARS);
            if !self.fence_open_at(cap) {
                let window = &self.pending[..cap];
                let cut = window
                    .rfind([',', ';', ':', ')'])
                    .map(|i| i + 1)
                    .or_else(|| window.rfind(char::is_whitespace).map(|i| i + 1))
                    .unwrap_or(cap);
                if cut > 0 && !self.fence_open_at(cut) {
                    return Some(cut);
                }
            }
        }
        None
    }

    fn is_abbreviation(&self, dot: usize) -> bool {
        let before = &self.pending[..dot];
        let word_start = before
            .rfind(|c: char| c.is_whitespace() || c == '(')
            .map(|i| i + 1)
            .unwrap_or(0);
        let word = before[word_start..].to_lowercase();
        ABBREVIATIONS.contains(&word.as_str())
            // A single letter with a dot ("J.") is an initial, not an end.
            || word.len() == 1
    }

    /// Would a cut at byte `at` land inside a code fence?
    fn fence_open_at(&self, at: usize) -> bool {
        let mut open = self.open_fence;
        for _ in self.pending[..floor_char_boundary(&self.pending, at)].matches("```") {
            open = !open;
        }
        open
    }

    /// Update the persistent fence state for consumed text.
    fn consume_fences(&mut self, consumed: &str) {
        for _ in consumed.matches("```") {
            self.open_fence = !self.open_fence;
        }
    }

    /// Markdown → speech. Returns `None` when nothing sayable remains.
    ///
    /// Fences may open mid-line ("Here's the patch. ```rust"), so the chunk is
    /// split on the ``` marker itself: odd segments are code. A chunk never
    /// STARTS inside a fence — `flush_point` refuses to cut there — so segment
    /// zero is always prose (an unterminated trailing fence only happens on
    /// the final flush of an aborted/completed turn, and is still code).
    fn sanitize(&mut self, raw: &str) -> Option<String> {
        let mut text = String::with_capacity(raw.len());
        for (i, segment) in raw.split("```").enumerate() {
            if i % 2 == 1 {
                if !self.code_notice_emitted {
                    text.push_str(" Code omitted — it's in the thread. ");
                    self.code_notice_emitted = true;
                }
                continue;
            }
            for line in segment.lines() {
                let trimmed = line.trim();
                // A table row reads as noise; say so once per contiguous table.
                if trimmed.starts_with('|') && trimmed.ends_with('|') {
                    if !text.ends_with("Table omitted — it's in the thread. ") {
                        text.push_str(" Table omitted — it's in the thread. ");
                    }
                    continue;
                }
                let line = strip_inline_markdown(trimmed);
                if !line.is_empty() {
                    text.push_str(&line);
                    text.push(' ');
                }
            }
        }
        let cleaned: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let has_words = cleaned.chars().any(|c| c.is_alphanumeric());
        has_words.then_some(cleaned)
    }
}

/// Largest byte index `<= at` that lands on a char boundary.
fn floor_char_boundary(s: &str, at: usize) -> usize {
    if at >= s.len() {
        return s.len();
    }
    let mut i = at;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Strip the markdown that reads as symbols: emphasis, headings, quotes,
/// inline code ticks, list markers; links keep their text.
fn strip_inline_markdown(line: &str) -> String {
    let line = line
        .trim_start_matches(|c: char| c == '#' || c == '>')
        .trim_start();
    // "- item" / "* item" / "1. item" → "item"
    let line = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| {
            let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
            (digits > 0)
                .then(|| line[digits..].strip_prefix(". "))
                .flatten()
        })
        .unwrap_or(line);

    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' | '_' | '`' => {}
            // [text](url) → text
            '[' => {
                let mut label = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == ']' {
                        closed = true;
                        break;
                    }
                    label.push(c);
                }
                out.push_str(&label);
                if closed && chars.peek() == Some(&'(') {
                    for c in chars.by_ref() {
                        if c == ')' {
                            break;
                        }
                    }
                }
            }
            c if is_speech_noise(c) => {}
            c => out.push(c),
        }
    }
    out.trim().to_string()
}

/// Symbols with no spoken value: emoji ranges, box drawing, arrows.
fn is_speech_noise(c: char) -> bool {
    matches!(c as u32,
        0x1F300..=0x1FAFF // emoji blocks
        | 0x2190..=0x21FF // arrows
        | 0x2500..=0x257F // box drawing
        | 0x2700..=0x27BF // dingbats
        | 0xFE0F // variation selector
        | 0x200D // zero-width joiner
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(chunks: Vec<String>) -> String {
        chunks.join(" | ")
    }

    #[test]
    fn accumulates_until_a_sentence_boundary_past_the_minimum() {
        let mut c = Chunker::default();
        assert!(c.push_delta("Sure. ").is_empty()); // boundary, but < 30 chars
        let out = c.push_delta("The auth bug lives in the session layer. Next we");
        assert_eq!(all(out), "Sure. The auth bug lives in the session layer.");
        assert!(c.has_pending());
    }

    #[test]
    fn decimals_and_abbreviations_do_not_end_sentences() {
        let mut c = Chunker::default();
        let out =
            c.push_delta("Version 3.5 shipped with Dr. Smith's fix for e.g. the cache. And then ");
        assert_eq!(
            all(out),
            "Version 3.5 shipped with Dr. Smith's fix for e.g. the cache."
        );
    }

    #[test]
    fn dedups_the_full_message_after_deltas() {
        let mut c = Chunker::default();
        let mut spoken = Vec::new();
        spoken.extend(c.push_delta("The fix is in. "));
        spoken.extend(c.push_delta("Run the tests to confirm."));
        // Claude then sends the whole block again as assistant_message.
        spoken.extend(c.push_message("The fix is in. Run the tests to confirm."));
        assert_eq!(
            all(spoken),
            "The fix is in. Run the tests to confirm."
        );
    }

    #[test]
    fn message_without_deltas_is_spoken_whole() {
        let mut c = Chunker::default();
        let out = c.push_message("Short answer: yes.");
        assert_eq!(all(out), "Short answer: yes.");
    }

    #[test]
    fn drifted_message_is_not_double_spoken() {
        let mut c = Chunker::default();
        let mut spoken = Vec::new();
        spoken.extend(c.push_delta("It works now, give it a try today okay."));
        spoken.extend(c.push_message("It works now — give it a try."));
        assert_eq!(all(spoken), "It works now, give it a try today okay.");
    }

    #[test]
    fn code_fences_are_never_cut_and_speak_one_notice() {
        let mut c = Chunker::default();
        let mut spoken = Vec::new();
        spoken.extend(c.push_delta("Here is the patch. ```rust\nfn x() {}\n"));
        // The fence is open: even a long stall must not flush inside it.
        assert!(c.stall_flush(10_000).is_empty());
        spoken.extend(c.push_delta("let y = 1. Do it.\n``` Apply it with care. "));
        spoken.extend(c.finish());
        let text = all(spoken);
        assert!(text.contains("Here is the patch."));
        assert!(text.contains("Code omitted"));
        assert!(!text.contains("fn x"));
        assert!(!text.contains("let y"));
        assert!(text.contains("Apply it with care."));

        // Second fence in the same turn: no second notice.
        let more = all(c.push_message("```js\nz\n``` Done now, truly."));
        assert!(!more.contains("Code omitted"));
        assert!(more.contains("Done now, truly."));
    }

    #[test]
    fn tables_and_markdown_noise_are_stripped() {
        let mut c = Chunker::default();
        let out = all(c.push_message(
            "## Results\n| a | b |\n| - | - |\n| 1 | 2 |\nSee **bold** and `code` and [the docs](https://x). 🚀",
        ));
        assert!(out.contains("Table omitted"));
        assert_eq!(out.matches("Table omitted").count(), 1);
        assert!(out.contains("Results"));
        assert!(out.contains("See bold and code and the docs."));
        assert!(!out.contains('🚀'));
        assert!(!out.contains('|'));
        assert!(!out.contains('*'));
    }

    #[test]
    fn hard_cap_cuts_at_a_clause_boundary() {
        let mut c = Chunker::default();
        let long = "word ".repeat(100); // 500 chars, no sentence end
        let out = c.push_delta(&long);
        assert!(!out.is_empty());
        assert!(out[0].len() <= HARD_MAX_CHARS);
    }

    #[test]
    fn stall_flush_needs_time_and_content() {
        let mut c = Chunker::default();
        c.push_delta("Thinking about it");
        assert!(c.stall_flush(500).is_empty()); // too soon
        assert!(c.stall_flush(2000).is_empty()); // too short
        c.push_delta(" some more words here");
        let out = c.stall_flush(2000);
        assert_eq!(all(out), "Thinking about it some more words here");
    }

    #[test]
    fn abort_silences_the_tail() {
        let mut c = Chunker::default();
        c.push_delta("This will never be spoken");
        c.abort();
        assert!(c.finish().is_empty());
        assert!(!c.has_pending());
    }

    #[test]
    fn empty_after_sanitizing_is_skipped_entirely() {
        let mut c = Chunker::default();
        assert!(c.push_message("*** --- 🚀🚀 ***").is_empty());
    }
}
