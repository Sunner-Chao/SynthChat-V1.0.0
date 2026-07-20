//! Bounded, terminal-safe process output capture.
//!
//! The capture is deliberately independent from the process manager. Callers
//! may clone it into concurrent stdout and stderr drain tasks, then take a
//! sanitized snapshot once the process reaches a terminal state.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Maximum rendered foreground output, including its truncation marker.
pub const FOREGROUND_OUTPUT_LIMIT_BYTES: usize = 50_000;
/// Target size of the foreground prefix (40% of the total budget).
pub const FOREGROUND_HEAD_BYTES: usize = 20_000;
/// Number of sanitized bytes retained for a background process tail.
pub const BACKGROUND_OUTPUT_LIMIT_BYTES: usize = 200_000;
/// Hard upper bound for output passed back to a provider.
pub const PROVIDER_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;

const MAX_ANSI_SEQUENCE_CHARS: usize = 4_096;
// Profile secrets are limited to 2,560 bytes. Keep additional context on both
// sides of every retention boundary so exact-value and token redactors see a
// complete match before any bytes become visible.
const REDACTION_GUARD_BYTES: usize = 4 * 1024;
const FOREGROUND_TAIL_BYTES: usize = FOREGROUND_OUTPUT_LIMIT_BYTES - FOREGROUND_HEAD_BYTES;
const SHORT_TRUNCATION_MARKER: &str = "...[truncated]...";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureMode {
    Foreground,
    Background,
    /// Retain only a bounded prefix. The final truncation marker is included
    /// in `maximum_bytes`; an additional private guard is retained solely for
    /// redaction across the visible boundary.
    HeadOnly {
        maximum_bytes: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessStream {
    Stdout,
    Stderr,
}

/// Applies the application's secret policy to terminal-safe process output.
///
/// Capture retention supplies at least [`REDACTION_GUARD_BYTES`] of context at
/// every visible boundary before invoking this policy. Redactors must keep
/// individual exact-match secrets within that bound.
pub trait OutputRedactor {
    fn redact(&self, value: &str) -> String;
}

impl<F> OutputRedactor for F
where
    F: Fn(&str) -> String,
{
    fn redact(&self, value: &str) -> String {
        self(value)
    }
}

#[derive(Clone, Copy, Debug, Default)]
#[cfg(test)]
pub struct NoopRedactor;

#[cfg(test)]
impl OutputRedactor for NoopRedactor {
    fn redact(&self, value: &str) -> String {
        value.to_owned()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedOutput {
    /// Sanitized and redacted output ready for a caller or provider.
    pub text: String,
    /// Sanitized UTF-8 bytes observed before retention and redaction.
    pub observed_bytes: u64,
    /// True when either capture retention or the final provider bound omitted data.
    pub truncated: bool,
}

/// A synchronized capture shared by the stdout and stderr drain tasks.
#[derive(Clone)]
pub struct ProcessOutputCapture {
    state: Arc<Mutex<CaptureState>>,
}

impl ProcessOutputCapture {
    pub fn new(mode: CaptureMode) -> Self {
        Self {
            state: Arc::new(Mutex::new(CaptureState::new(mode))),
        }
    }

    #[cfg(test)]
    pub fn append_stdout(&self, bytes: impl AsRef<[u8]>) {
        self.append(ProcessStream::Stdout, bytes);
    }

    #[cfg(test)]
    pub fn append_stderr(&self, bytes: impl AsRef<[u8]>) {
        self.append(ProcessStream::Stderr, bytes);
    }

    pub fn append(&self, stream: ProcessStream, bytes: impl AsRef<[u8]>) {
        self.lock_state().append(stream, bytes.as_ref());
    }

    /// Takes a non-destructive snapshot and applies the supplied secret policy.
    ///
    /// Pending partial UTF-8 is represented by U+FFFD in the snapshot without
    /// consuming it from the live capture, so a later chunk can still complete
    /// the original scalar value.
    pub fn finish<R>(&self, redactor: &R) -> CapturedOutput
    where
        R: OutputRedactor + ?Sized,
    {
        self.finish_inner(redactor, None)
    }

    /// Finishes like [`Self::finish`], then applies a UTF-8-safe head/tail bound.
    ///
    /// `max_bytes` is clamped to the provider hard limit of 64 KiB.
    pub fn finish_bounded<R>(&self, redactor: &R, max_bytes: usize) -> CapturedOutput
    where
        R: OutputRedactor + ?Sized,
    {
        self.finish_inner(redactor, Some(max_bytes.min(PROVIDER_OUTPUT_LIMIT_BYTES)))
    }

    fn finish_inner<R>(&self, redactor: &R, maximum: Option<usize>) -> CapturedOutput
    where
        R: OutputRedactor + ?Sized,
    {
        // Drain tasks own capture clones. While any clone remains, withhold the
        // trailing redaction guard so a secret arriving over multiple reads
        // cannot expose a prefix through a live poll/log snapshot.
        let live = Arc::strong_count(&self.state) > 1;
        let rendered = {
            let mut snapshot = self.lock_state().clone();
            snapshot.flush_pending();
            snapshot.retention.render(redactor, live)
        };

        // Sanitize again after releasing the capture lock. The retention layer
        // already redacted with boundary context, but application redactors may
        // emit terminal controls in their replacement text.
        let sanitized = sanitize_complete_text(&rendered.text);
        let (text, bounded) = match maximum {
            Some(maximum) => bound_utf8_text(&sanitized, maximum),
            None => (sanitized, false),
        };

        CapturedOutput {
            text,
            observed_bytes: rendered.observed_bytes,
            truncated: rendered.truncated || bounded,
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, CaptureState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Clone)]
struct CaptureState {
    stdout: StreamFilter,
    stderr: StreamFilter,
    retention: Retention,
}

impl CaptureState {
    fn new(mode: CaptureMode) -> Self {
        Self {
            stdout: StreamFilter::default(),
            stderr: StreamFilter::default(),
            retention: Retention::new(mode),
        }
    }

    fn append(&mut self, stream: ProcessStream, bytes: &[u8]) {
        let filtered = match stream {
            ProcessStream::Stdout => self.stdout.push(bytes),
            ProcessStream::Stderr => self.stderr.push(bytes),
        };
        self.retention.append(filtered.as_bytes());
    }

    fn flush_pending(&mut self) {
        let stdout = self.stdout.finish();
        self.retention.append(stdout.as_bytes());

        let stderr = self.stderr.finish();
        self.retention.append(stderr.as_bytes());
    }
}

#[derive(Clone, Default)]
struct StreamFilter {
    decoder: Utf8Decoder,
    sanitizer: TerminalSanitizer,
}

impl StreamFilter {
    fn push(&mut self, bytes: &[u8]) -> String {
        let decoded = self.decoder.push(bytes);
        self.sanitizer.push(&decoded)
    }

    fn finish(&mut self) -> String {
        let decoded = self.decoder.finish();
        let mut output = self.sanitizer.push(&decoded);
        output.push_str(&self.sanitizer.finish());
        output
    }
}

#[derive(Clone, Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        let mut input = Vec::with_capacity(self.pending.len().saturating_add(bytes.len()));
        input.append(&mut self.pending);
        input.extend_from_slice(bytes);

        let mut output = String::with_capacity(input.len());
        let mut offset = 0;
        while offset < input.len() {
            match std::str::from_utf8(&input[offset..]) {
                Ok(valid) => {
                    output.push_str(valid);
                    offset = input.len();
                }
                Err(error) => {
                    let valid_end = offset + error.valid_up_to();
                    if valid_end > offset {
                        let valid = std::str::from_utf8(&input[offset..valid_end])
                            .expect("from_utf8 reported a valid prefix");
                        output.push_str(valid);
                    }

                    match error.error_len() {
                        Some(invalid_length) => {
                            output.push(char::REPLACEMENT_CHARACTER);
                            offset = valid_end.saturating_add(invalid_length);
                        }
                        None => {
                            self.pending.extend_from_slice(&input[valid_end..]);
                            debug_assert!(self.pending.len() <= 3);
                            offset = input.len();
                        }
                    }
                }
            }
        }
        output
    }

    fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            String::new()
        } else {
            self.pending.clear();
            char::REPLACEMENT_CHARACTER.to_string()
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
enum EscapeState {
    #[default]
    Ground,
    Escape {
        length: usize,
    },
    Csi {
        length: usize,
    },
    Osc {
        length: usize,
        saw_escape: bool,
    },
    ControlString {
        length: usize,
        saw_escape: bool,
    },
}

#[derive(Clone, Default)]
struct TerminalSanitizer {
    escape: EscapeState,
    pending_cr: bool,
}

impl TerminalSanitizer {
    fn push(&mut self, value: &str) -> String {
        let mut output = String::with_capacity(value.len());
        for character in value.chars() {
            self.push_character(character, &mut output);
        }
        output
    }

    fn finish(&mut self) -> String {
        // Incomplete terminal sequences are intentionally discarded.
        self.escape = EscapeState::Ground;
        if self.pending_cr {
            self.pending_cr = false;
            "\n".to_owned()
        } else {
            String::new()
        }
    }

    fn push_character(&mut self, character: char, output: &mut String) {
        let mut reprocess = true;
        while reprocess {
            reprocess = false;
            match self.escape {
                EscapeState::Ground => self.push_ground(character, output),
                EscapeState::Escape { length } => {
                    if length == 1 {
                        match character {
                            '[' => {
                                self.escape = EscapeState::Csi { length: 2 };
                                continue;
                            }
                            ']' => {
                                self.escape = EscapeState::Osc {
                                    length: 2,
                                    saw_escape: false,
                                };
                                continue;
                            }
                            'P' | 'X' | '^' | '_' => {
                                self.escape = EscapeState::ControlString {
                                    length: 2,
                                    saw_escape: false,
                                };
                                continue;
                            }
                            _ => {}
                        }
                    }

                    if is_escape_intermediate(character) {
                        if length < MAX_ANSI_SEQUENCE_CHARS {
                            self.escape = EscapeState::Escape { length: length + 1 };
                        } else {
                            self.escape = EscapeState::Ground;
                            reprocess = true;
                        }
                    } else if is_escape_final(character) {
                        self.escape = EscapeState::Ground;
                    } else {
                        self.escape = EscapeState::Ground;
                        reprocess = true;
                    }
                }
                EscapeState::Csi { length } => {
                    if character == '\u{1b}' {
                        self.escape = EscapeState::Escape { length: 1 };
                    } else if character == '\u{9c}' || is_csi_final(character) {
                        self.escape = EscapeState::Ground;
                    } else if is_csi_body(character) {
                        if length < MAX_ANSI_SEQUENCE_CHARS {
                            self.escape = EscapeState::Csi { length: length + 1 };
                        } else {
                            self.escape = EscapeState::Ground;
                            reprocess = true;
                        }
                    } else {
                        self.escape = EscapeState::Ground;
                        reprocess = true;
                    }
                }
                EscapeState::Osc { length, saw_escape } => {
                    if character == '\u{9c}'
                        || character == '\u{7}'
                        || (saw_escape && character == '\\')
                    {
                        self.escape = EscapeState::Ground;
                    } else if length < MAX_ANSI_SEQUENCE_CHARS {
                        self.escape = EscapeState::Osc {
                            length: length + 1,
                            saw_escape: character == '\u{1b}',
                        };
                    } else {
                        self.escape = EscapeState::Ground;
                        reprocess = true;
                    }
                }
                EscapeState::ControlString { length, saw_escape } => {
                    if character == '\u{9c}' || (saw_escape && character == '\\') {
                        self.escape = EscapeState::Ground;
                    } else if length < MAX_ANSI_SEQUENCE_CHARS {
                        self.escape = EscapeState::ControlString {
                            length: length + 1,
                            saw_escape: character == '\u{1b}',
                        };
                    } else {
                        self.escape = EscapeState::Ground;
                        reprocess = true;
                    }
                }
            }
        }
    }

    fn push_ground(&mut self, character: char, output: &mut String) {
        match character {
            '\u{1b}' => self.escape = EscapeState::Escape { length: 1 },
            '\u{9b}' => self.escape = EscapeState::Csi { length: 1 },
            '\u{9d}' => {
                self.escape = EscapeState::Osc {
                    length: 1,
                    saw_escape: false,
                }
            }
            '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => {
                self.escape = EscapeState::ControlString {
                    length: 1,
                    saw_escape: false,
                }
            }
            '\r' => {
                if self.pending_cr {
                    output.push('\n');
                }
                self.pending_cr = true;
            }
            '\n' => {
                self.pending_cr = false;
                output.push('\n');
            }
            '\t' => {
                self.flush_pending_cr(output);
                output.push('\t');
            }
            _ if is_dangerous_control(character) => {}
            _ => {
                self.flush_pending_cr(output);
                output.push(character);
            }
        }
    }

    fn flush_pending_cr(&mut self, output: &mut String) {
        if self.pending_cr {
            self.pending_cr = false;
            output.push('\n');
        }
    }
}

fn is_escape_intermediate(character: char) -> bool {
    matches!(character, '\u{20}'..='\u{2f}')
}

fn is_escape_final(character: char) -> bool {
    matches!(character, '\u{30}'..='\u{7e}')
}

fn is_csi_body(character: char) -> bool {
    matches!(character, '\u{20}'..='\u{3f}')
}

fn is_csi_final(character: char) -> bool {
    matches!(character, '\u{40}'..='\u{7e}')
}

fn is_dangerous_control(character: char) -> bool {
    matches!(
        character,
        '\u{0}'..='\u{8}'
            | '\u{b}'..='\u{1f}'
            | '\u{7f}'..='\u{9f}'
            | '\u{ad}'
            | '\u{34f}'
            | '\u{61c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
    )
}

fn sanitize_complete_text(value: &str) -> String {
    let mut sanitizer = TerminalSanitizer::default();
    let mut output = sanitizer.push(value);
    output.push_str(&sanitizer.finish());
    output
}

#[derive(Clone)]
enum Retention {
    Foreground(ForegroundRetention),
    Background(BackgroundRetention),
    HeadOnly(HeadOnlyRetention),
}

impl Retention {
    fn new(mode: CaptureMode) -> Self {
        match mode {
            CaptureMode::Foreground => Self::Foreground(ForegroundRetention::default()),
            CaptureMode::Background => Self::Background(BackgroundRetention::default()),
            CaptureMode::HeadOnly { maximum_bytes } => Self::HeadOnly(HeadOnlyRetention::new(
                maximum_bytes.min(PROVIDER_OUTPUT_LIMIT_BYTES),
            )),
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        match self {
            Self::Foreground(retention) => retention.append(bytes),
            Self::Background(retention) => retention.append(bytes),
            Self::HeadOnly(retention) => retention.append(bytes),
        }
    }

    fn render<R>(&self, redactor: &R, live: bool) -> RenderedOutput
    where
        R: OutputRedactor + ?Sized,
    {
        match self {
            Self::Foreground(retention) => retention.render(redactor, live),
            Self::Background(retention) => retention.render(redactor, live),
            Self::HeadOnly(retention) => retention.render(redactor, live),
        }
    }
}

#[derive(Clone)]
struct HeadOnlyRetention {
    head: Vec<u8>,
    maximum_bytes: usize,
    head_closed: bool,
    observed_bytes: u64,
}

impl HeadOnlyRetention {
    fn new(maximum_bytes: usize) -> Self {
        Self {
            head: Vec::new(),
            maximum_bytes,
            head_closed: false,
            observed_bytes: 0,
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        self.observed_bytes = self
            .observed_bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if self.head_closed {
            return;
        }

        let capacity = self.maximum_bytes.saturating_add(REDACTION_GUARD_BYTES);
        let room = capacity.saturating_sub(self.head.len());
        let prefix_length = utf8_prefix_length(bytes, room);
        self.head.extend_from_slice(&bytes[..prefix_length]);
        if self.head.len() == capacity || prefix_length != bytes.len() {
            self.head_closed = true;
        }
    }

    fn render<R>(&self, redactor: &R, live: bool) -> RenderedOutput
    where
        R: OutputRedactor + ?Sized,
    {
        let head = std::str::from_utf8(&self.head).expect("head-only retention is valid UTF-8");
        let redacted = redact_and_sanitize(redactor, head);
        // Once the private guard is full, future bytes cannot affect anything
        // inside the visible prefix. Until then, preserve the normal live
        // snapshot guarantee for a secret split across reads.
        let (redacted, live_withheld) = withhold_live_tail(redacted, live && !self.head_closed);
        let raw_truncated = self.observed_bytes > self.maximum_bytes as u64;
        let bounded = redacted.len() > self.maximum_bytes;
        let text = if raw_truncated || bounded {
            compose_bounded_head(
                &redacted,
                self.observed_bytes.max(redacted.len() as u64),
                self.maximum_bytes,
            )
        } else {
            redacted
        };

        debug_assert!(text.len() <= self.maximum_bytes);
        RenderedOutput {
            text,
            observed_bytes: self.observed_bytes,
            truncated: raw_truncated || bounded || live_withheld,
        }
    }
}

#[derive(Clone, Default)]
struct ForegroundRetention {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    head_closed: bool,
    observed_bytes: u64,
}

impl ForegroundRetention {
    fn append(&mut self, bytes: &[u8]) {
        self.observed_bytes = self
            .observed_bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));

        let mut remaining = bytes;
        if !self.head_closed {
            let room = FOREGROUND_HEAD_BYTES
                .saturating_add(REDACTION_GUARD_BYTES)
                .saturating_sub(self.head.len());
            let prefix_length = utf8_prefix_length(remaining, room);
            self.head.extend_from_slice(&remaining[..prefix_length]);
            remaining = &remaining[prefix_length..];
            if self.head.len() == FOREGROUND_HEAD_BYTES.saturating_add(REDACTION_GUARD_BYTES)
                || !remaining.is_empty()
            {
                self.head_closed = true;
            }
        }

        let tail_capacity =
            FOREGROUND_TAIL_BYTES.saturating_add(REDACTION_GUARD_BYTES.saturating_mul(2));
        append_ring(&mut self.tail, bytes, tail_capacity);
    }

    fn render<R>(&self, redactor: &R, live: bool) -> RenderedOutput
    where
        R: OutputRedactor + ?Sized,
    {
        let head = std::str::from_utf8(&self.head).expect("foreground head is valid UTF-8");
        let (tail_start, tail) = retained_tail_text(&self.tail, self.observed_bytes);
        let complete = merge_retained_parts(head, tail_start, &tail);

        let (text, live_withheld, bounded) = if let Some(complete) = complete {
            let redacted = redact_and_sanitize(redactor, &complete);
            let (redacted, live_withheld) = withhold_live_tail(redacted, live);
            let (text, bounded) = bound_utf8_text(&redacted, FOREGROUND_OUTPUT_LIMIT_BYTES);
            (text, live_withheld, bounded)
        } else {
            let mut redacted_head = redact_and_sanitize(redactor, head);
            remove_suffix_bytes(&mut redacted_head, REDACTION_GUARD_BYTES);
            let head_end = previous_char_boundary(
                &redacted_head,
                redacted_head.len().min(FOREGROUND_HEAD_BYTES),
            );
            redacted_head.truncate(head_end);

            let mut redacted_tail = redact_and_sanitize(redactor, &tail);
            remove_prefix_bytes(&mut redacted_tail, REDACTION_GUARD_BYTES);
            let (redacted_tail, live_withheld) = withhold_live_tail(redacted_tail, live);
            let text = compose_bounded_parts(
                &redacted_head,
                &redacted_tail,
                self.observed_bytes,
                FOREGROUND_OUTPUT_LIMIT_BYTES,
            );
            (text, live_withheld, true)
        };

        debug_assert!(text.len() <= FOREGROUND_OUTPUT_LIMIT_BYTES);
        RenderedOutput {
            text,
            observed_bytes: self.observed_bytes,
            truncated: self.observed_bytes > FOREGROUND_OUTPUT_LIMIT_BYTES as u64
                || live_withheld
                || bounded,
        }
    }
}

#[derive(Clone, Default)]
struct BackgroundRetention {
    tail: VecDeque<u8>,
    observed_bytes: u64,
}

impl BackgroundRetention {
    fn append(&mut self, bytes: &[u8]) {
        self.observed_bytes = self
            .observed_bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        append_ring(
            &mut self.tail,
            bytes,
            BACKGROUND_OUTPUT_LIMIT_BYTES.saturating_add(REDACTION_GUARD_BYTES.saturating_mul(2)),
        );
    }

    fn render<R>(&self, redactor: &R, live: bool) -> RenderedOutput
    where
        R: OutputRedactor + ?Sized,
    {
        let (tail_start, tail) = retained_tail_text(&self.tail, self.observed_bytes);
        let raw_truncated = tail_start != 0;
        let mut redacted = redact_and_sanitize(redactor, &tail);
        if raw_truncated {
            remove_prefix_bytes(&mut redacted, REDACTION_GUARD_BYTES);
        }
        let (redacted, live_withheld) = withhold_live_tail(redacted, live);
        let must_bound = self.observed_bytes > BACKGROUND_OUTPUT_LIMIT_BYTES as u64
            || redacted.len() > BACKGROUND_OUTPUT_LIMIT_BYTES;
        let text = if must_bound {
            let start = suffix_start(&redacted, BACKGROUND_OUTPUT_LIMIT_BYTES);
            let visible = &redacted[start..];
            let omitted = self
                .observed_bytes
                .saturating_sub(u64::try_from(visible.len()).unwrap_or(u64::MAX));
            let marker = truncation_marker(omitted);
            let mut text = String::with_capacity(marker.len().saturating_add(visible.len()));
            text.push_str(&marker);
            text.push_str(visible);
            text
        } else {
            redacted
        };

        RenderedOutput {
            text,
            observed_bytes: self.observed_bytes,
            truncated: must_bound || live_withheld,
        }
    }
}

struct RenderedOutput {
    text: String,
    observed_bytes: u64,
    truncated: bool,
}

fn retained_tail_text(tail: &VecDeque<u8>, observed_bytes: u64) -> (u64, String) {
    let bytes: Vec<u8> = tail.iter().copied().collect();
    let text = retained_utf8_tail(&bytes);
    let skipped = bytes.len().saturating_sub(text.len());
    let raw_start = observed_bytes.saturating_sub(bytes.len() as u64);
    (raw_start.saturating_add(skipped as u64), text.to_owned())
}

fn merge_retained_parts(head: &str, tail_start: u64, tail: &str) -> Option<String> {
    let head_end = head.len() as u64;
    if tail_start > head_end {
        return None;
    }
    let overlap = usize::try_from(head_end - tail_start).ok()?;
    if overlap > tail.len() || !tail.is_char_boundary(overlap) {
        return None;
    }
    let mut complete = String::with_capacity(head.len().saturating_add(tail.len() - overlap));
    complete.push_str(head);
    complete.push_str(&tail[overlap..]);
    Some(complete)
}

fn redact_and_sanitize<R>(redactor: &R, value: &str) -> String
where
    R: OutputRedactor + ?Sized,
{
    sanitize_complete_text(&redactor.redact(value))
}

fn remove_suffix_bytes(value: &mut String, bytes: usize) -> usize {
    let original = value.len();
    let keep = previous_char_boundary(value, original.saturating_sub(bytes));
    value.truncate(keep);
    original - keep
}

fn remove_prefix_bytes(value: &mut String, bytes: usize) -> usize {
    let mut remove = bytes.min(value.len());
    while remove < value.len() && !value.is_char_boundary(remove) {
        remove += 1;
    }
    value.drain(..remove);
    remove
}

fn withhold_live_tail(mut value: String, live: bool) -> (String, bool) {
    if !live || value.is_empty() {
        return (value, false);
    }
    let withheld = remove_suffix_bytes(&mut value, REDACTION_GUARD_BYTES);
    if withheld != 0 {
        value.push_str(&truncation_marker(withheld as u64));
    }
    (value, withheld != 0)
}

fn append_ring(ring: &mut VecDeque<u8>, bytes: &[u8], capacity: usize) {
    if capacity == 0 {
        ring.clear();
        return;
    }

    if bytes.len() >= capacity {
        ring.clear();
        ring.extend(&bytes[bytes.len() - capacity..]);
        return;
    }

    let overflow = ring
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(capacity);
    if overflow != 0 {
        ring.drain(..overflow);
    }
    ring.extend(bytes);
}

fn utf8_prefix_length(bytes: &[u8], maximum: usize) -> usize {
    let candidate = bytes.len().min(maximum);
    if candidate == bytes.len() {
        return candidate;
    }

    let text = std::str::from_utf8(bytes).expect("retention receives valid UTF-8");
    previous_char_boundary(text, candidate)
}

fn retained_utf8_tail(bytes: &[u8]) -> &str {
    let mut start = 0;
    while start < bytes.len() && bytes[start] & 0b1100_0000 == 0b1000_0000 {
        start += 1;
    }
    std::str::from_utf8(&bytes[start..]).expect("retained tail is a suffix of valid UTF-8")
}

fn previous_char_boundary(value: &str, maximum: usize) -> usize {
    let mut boundary = maximum.min(value.len());
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

fn suffix_start(value: &str, maximum_length: usize) -> usize {
    if value.len() <= maximum_length {
        return 0;
    }

    let mut start = value.len() - maximum_length;
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    start
}

fn truncation_marker(omitted_bytes: u64) -> String {
    format!("...[output truncated: {omitted_bytes} bytes omitted]...")
}

fn compose_bounded_parts(
    head: &str,
    available_tail: &str,
    total_bytes: u64,
    maximum: usize,
) -> String {
    debug_assert!(head.len() <= maximum);
    let marker_room = maximum.saturating_sub(head.len());
    let largest_marker = truncation_marker(total_bytes);
    if largest_marker.len() > marker_room {
        return short_marker(maximum);
    }

    let mut tail_budget = marker_room;
    loop {
        let tail_start = suffix_start(available_tail, tail_budget);
        let tail = &available_tail[tail_start..];
        let displayed = (head.len() as u64).saturating_add(tail.len() as u64);
        let marker = truncation_marker(total_bytes.saturating_sub(displayed));
        let next_budget = maximum
            .saturating_sub(head.len())
            .saturating_sub(marker.len());

        if next_budget == tail_budget {
            let mut output = String::with_capacity(
                head.len()
                    .saturating_add(marker.len())
                    .saturating_add(tail.len()),
            );
            output.push_str(head);
            output.push_str(&marker);
            output.push_str(tail);
            debug_assert!(output.len() <= maximum);
            return output;
        }
        tail_budget = next_budget;
    }
}

fn compose_bounded_head(available_head: &str, total_bytes: u64, maximum: usize) -> String {
    if maximum == 0 {
        return String::new();
    }
    let largest_marker = truncation_marker(total_bytes);
    if largest_marker.len() > maximum {
        return short_marker(maximum);
    }

    let mut head_budget = maximum.saturating_sub(largest_marker.len());
    loop {
        let head_end = previous_char_boundary(available_head, head_budget);
        let head = &available_head[..head_end];
        let marker = truncation_marker(total_bytes.saturating_sub(head.len() as u64));
        let next_budget = maximum.saturating_sub(marker.len());
        if next_budget == head_budget {
            let mut output = String::with_capacity(head.len().saturating_add(marker.len()));
            output.push_str(head);
            output.push_str(&marker);
            debug_assert!(output.len() <= maximum);
            return output;
        }
        head_budget = next_budget;
    }
}

fn bound_utf8_text(value: &str, maximum: usize) -> (String, bool) {
    if value.len() <= maximum {
        return (value.to_owned(), false);
    }
    if maximum == 0 {
        return (String::new(), true);
    }

    let largest_marker = truncation_marker(value.len() as u64);
    if largest_marker.len() > maximum {
        return (short_marker(maximum), true);
    }

    let preferred_head = maximum.saturating_mul(2) / 5;
    let head_limit = preferred_head.min(maximum.saturating_sub(largest_marker.len()));
    let head_end = previous_char_boundary(value, head_limit);
    let text = compose_bounded_parts(&value[..head_end], value, value.len() as u64, maximum);
    (text, true)
}

fn short_marker(maximum: usize) -> String {
    let marker = if maximum >= SHORT_TRUNCATION_MARKER.len() {
        SHORT_TRUNCATION_MARKER
    } else {
        "..."
    };
    marker[..marker.len().min(maximum)].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::thread;

    #[test]
    fn foreground_is_bounded_and_preserves_head_and_tail() {
        let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
        let mut input = b"HEAD:".to_vec();
        input.extend(std::iter::repeat_n(b'x', 80_000));
        input.extend_from_slice(b":TAIL");
        capture.append_stdout(&input);

        let output = capture.finish(&NoopRedactor);
        assert!(output.truncated);
        assert_eq!(output.observed_bytes, input.len() as u64);
        assert!(output.text.len() <= FOREGROUND_OUTPUT_LIMIT_BYTES);
        assert!(output.text.starts_with("HEAD:"));
        assert!(output.text.ends_with(":TAIL"));
        assert!(output.text.contains("...[output truncated:"));
        assert!(output.text.contains("bytes omitted]..."));
    }

    #[test]
    fn foreground_keeps_exactly_in_budget_output() {
        let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
        let input = vec![b'x'; FOREGROUND_OUTPUT_LIMIT_BYTES];
        capture.append_stdout(&input);

        let output = capture.finish(&NoopRedactor);
        assert!(!output.truncated);
        assert_eq!(output.text.as_bytes(), input);
    }

    #[test]
    fn head_only_retains_prefix_and_never_reintroduces_the_tail() {
        const LIMIT: usize = 10_000;
        let capture = ProcessOutputCapture::new(CaptureMode::HeadOnly {
            maximum_bytes: LIMIT,
        });
        let mut input = b"HEAD:".to_vec();
        input.extend(std::iter::repeat_n(b'x', LIMIT * 2));
        input.extend_from_slice(b":TAIL-MUST-NOT-BE-RETAINED");
        capture.append_stderr(&input);

        let output = capture.finish(&NoopRedactor);
        assert!(output.truncated);
        assert_eq!(output.observed_bytes, input.len() as u64);
        assert!(output.text.len() <= LIMIT);
        assert!(output.text.starts_with("HEAD:"));
        assert!(output.text.contains("...[output truncated:"));
        assert!(!output.text.contains("TAIL-MUST-NOT-BE-RETAINED"));
    }

    #[test]
    fn head_only_keeps_exactly_in_budget_output() {
        const LIMIT: usize = 10_000;
        let capture = ProcessOutputCapture::new(CaptureMode::HeadOnly {
            maximum_bytes: LIMIT,
        });
        let input = vec![b'x'; LIMIT];
        capture.append_stderr(&input);

        let output = capture.finish(&NoopRedactor);
        assert!(!output.truncated);
        assert_eq!(output.observed_bytes, LIMIT as u64);
        assert_eq!(output.text.as_bytes(), input);
    }

    #[test]
    fn head_only_redacts_a_secret_crossing_the_visible_boundary() {
        const LIMIT: usize = 10_000;
        let capture = ProcessOutputCapture::new(CaptureMode::HeadOnly {
            maximum_bytes: LIMIT,
        });
        let secret = format!("secret-start-{}-secret-end", "s".repeat(2_500));
        let mut input = vec![b'a'; LIMIT - 12];
        input.extend_from_slice(secret.as_bytes());
        input.extend(std::iter::repeat_n(b'b', LIMIT));
        input.extend_from_slice(b":TAIL");
        capture.append_stderr(input);

        let redact = |value: &str| value.replace(&secret, "[REDACTED]");
        let output = capture.finish(&redact);
        assert!(output.truncated);
        assert!(output.text.len() <= LIMIT);
        assert!(!output.text.contains("secret-start-"));
        assert!(!output.text.contains("-secret-end"));
        assert!(!output.text.contains(":TAIL"));
    }

    #[test]
    fn head_only_truncation_is_utf8_safe() {
        const LIMIT: usize = 10_000;
        let capture = ProcessOutputCapture::new(CaptureMode::HeadOnly {
            maximum_bytes: LIMIT,
        });
        capture.append_stderr("\u{4f60}".repeat(LIMIT).as_bytes());

        let output = capture.finish(&NoopRedactor);
        assert!(output.truncated);
        assert!(output.text.len() <= LIMIT);
        assert!(output.text.starts_with('\u{4f60}'));
        assert!(!output.text.contains(char::REPLACEMENT_CHARACTER));
    }

    #[test]
    fn background_retains_only_the_rolling_tail() {
        let capture = ProcessOutputCapture::new(CaptureMode::Background);
        capture.append_stdout(vec![b'a'; 10_000]);
        capture.append_stdout(vec![b'b'; BACKGROUND_OUTPUT_LIMIT_BYTES]);

        let output = capture.finish(&NoopRedactor);
        assert!(output.truncated);
        assert_eq!(output.observed_bytes, 210_000);
        let marker = truncation_marker(10_000);
        assert!(output.text.starts_with(&marker));
        assert_eq!(
            output.text.len(),
            marker.len() + BACKGROUND_OUTPUT_LIMIT_BYTES
        );
        assert!(output.text[marker.len()..].bytes().all(|byte| byte == b'b'));
    }

    #[test]
    fn utf8_scalars_survive_arbitrary_chunk_boundaries() {
        let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
        let expected = "A\u{4f60}\u{597d}\u{1f642}Z";
        for byte in expected.as_bytes() {
            capture.append_stdout([*byte]);
        }

        assert_eq!(capture.finish(&NoopRedactor).text, expected);
    }

    #[test]
    fn malformed_and_incomplete_utf8_are_replaced() {
        let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
        capture.append_stdout(b"ok\xffend\xe2\x82");

        let replacement = char::REPLACEMENT_CHARACTER;
        assert_eq!(
            capture.finish(&NoopRedactor).text,
            format!("ok{replacement}end{replacement}")
        );
    }

    #[test]
    fn split_csi_osc_and_control_strings_are_removed() {
        let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
        capture.append_stdout(b"plain\x1b[");
        capture.append_stdout(b"31mred\x1b]window ti");
        capture.append_stdout(b"tle\x1b");
        capture.append_stdout(b"\\after\x1bPprivate");
        capture.append_stdout(b"\x1b\\done");

        assert_eq!(capture.finish(&NoopRedactor).text, "plainredafterdone");
    }

    #[test]
    fn overlong_terminal_sequence_cannot_swallow_unbounded_output() {
        let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
        let mut input = b"before\x1b]".to_vec();
        input.extend(std::iter::repeat_n(b'x', MAX_ANSI_SEQUENCE_CHARS + 100));
        input.extend_from_slice(b"after");
        capture.append_stdout(input);

        let output = capture.finish(&NoopRedactor);
        assert!(output.text.starts_with("before"));
        assert!(output.text.ends_with("after"));
        assert!(output.text.len() >= 100);
    }

    #[test]
    fn crlf_and_lone_cr_are_normalized_across_chunks() {
        let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
        capture.append_stdout(b"a\r");
        capture.append_stdout(b"\nb\r");
        capture.append_stdout(b"c\r");

        assert_eq!(capture.finish(&NoopRedactor).text, "a\nb\nc\n");
    }

    #[test]
    fn dangerous_controls_and_invisible_directionality_are_removed() {
        let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
        let input = "a\0\u{7}\u{7f}\u{61c}\u{200b}\u{202e}\u{2067}\u{feff}b\tc\n";
        capture.append_stdout(input.as_bytes());

        assert_eq!(capture.finish(&NoopRedactor).text, "ab\tc\n");
    }

    #[test]
    fn concurrent_stream_appends_are_complete_and_race_free() {
        let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
        let mut handles = Vec::new();
        for worker in 0..4 {
            let capture = capture.clone();
            handles.push(thread::spawn(move || {
                for item in 0..100 {
                    let line = format!("worker-{worker}-item-{item}\n");
                    if worker % 2 == 0 {
                        capture.append_stdout(line.as_bytes());
                    } else {
                        capture.append_stderr(line.as_bytes());
                    }
                }
            }));
        }
        for handle in handles {
            handle.join().expect("capture worker panicked");
        }

        let output = capture.finish(&NoopRedactor);
        assert!(!output.truncated);
        let actual: HashSet<&str> = output.text.lines().collect();
        let expected: HashSet<String> = (0..4)
            .flat_map(|worker| (0..100).map(move |item| format!("worker-{worker}-item-{item}")))
            .collect();
        assert_eq!(actual.len(), expected.len());
        assert!(expected.iter().all(|line| actual.contains(line.as_str())));
    }

    #[test]
    fn injected_redactor_removes_secrets_and_cannot_add_terminal_controls() {
        let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
        capture.append_stdout(b"token=super-secret");
        let redact = |value: &str| value.replace("super-secret", "\x1b[31m[REDACTED]\x1b[0m\0");

        let output = capture.finish(&redact);
        assert_eq!(output.text, "token=[REDACTED]");
        assert!(!output.text.contains("super-secret"));
        assert!(!output.text.contains('\u{1b}'));
    }

    #[test]
    fn foreground_redacts_a_secret_crossing_the_head_retention_boundary() {
        let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
        let secret = format!("secret-start-{}-secret-end", "x".repeat(2_500));
        let prefix_length = FOREGROUND_HEAD_BYTES - 12;
        let mut input = vec![b'a'; prefix_length];
        input.extend_from_slice(secret.as_bytes());
        input.extend(std::iter::repeat_n(b'b', FOREGROUND_OUTPUT_LIMIT_BYTES));
        input.extend_from_slice(b":TAIL");
        capture.append_stdout(input);

        let redact = |value: &str| value.replace(&secret, "[REDACTED]");
        let output = capture.finish(&redact);

        assert!(output.truncated);
        assert!(!output.text.contains("secret-start-"));
        assert!(!output.text.contains("-secret-end"));
        assert!(output.text.ends_with(":TAIL"));
    }

    #[test]
    fn background_redacts_a_secret_crossing_the_rolling_tail_boundary() {
        let capture = ProcessOutputCapture::new(CaptureMode::Background);
        let secret = format!("secret-start-{}-secret-end", "y".repeat(2_500));
        let cut = secret.len() / 2;
        let prefix_length = 20_000;
        let suffix_length = BACKGROUND_OUTPUT_LIMIT_BYTES - secret.len() + cut;
        capture.append_stdout(vec![b'a'; prefix_length]);
        capture.append_stdout(secret.as_bytes());
        capture.append_stdout(vec![b'b'; suffix_length]);

        let redact = |value: &str| value.replace(&secret, "[REDACTED]");
        let output = capture.finish(&redact);

        assert!(output.truncated);
        assert!(!output.text.contains("secret-start-"));
        assert!(!output.text.contains("-secret-end"));
        assert!(output.text.len() <= BACKGROUND_OUTPUT_LIMIT_BYTES + 64);
    }

    #[test]
    fn live_snapshot_withholds_a_secret_prefix_until_all_writers_finish() {
        let capture = ProcessOutputCapture::new(CaptureMode::Background);
        let writer = capture.clone();
        let safe_prefix = "safe-output\n".repeat(500);
        let secret = format!("cross-chunk-secret-{}-value", "z".repeat(2_500));
        let split = secret.len() / 2;
        writer.append_stdout(safe_prefix.as_bytes());
        writer.append_stdout(&secret.as_bytes()[..split]);
        let redact = |value: &str| value.replace(&secret, "[REDACTED]");

        let partial = capture.finish(&redact);
        assert!(partial.text.contains("safe-output"));
        assert!(!partial.text.contains(&secret[..split]));

        writer.append_stdout(&secret.as_bytes()[split..]);
        let complete_but_live = capture.finish(&redact);
        assert!(!complete_but_live.text.contains(&secret));
        drop(writer);

        let final_output = capture.finish(&redact);
        assert!(final_output.text.contains("[REDACTED]"));
        assert!(!final_output.text.contains(&secret));
    }

    #[test]
    fn expanded_redaction_is_clamped_to_the_provider_limit() {
        let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
        capture.append_stdout(b"seed");
        let expand = |_value: &str| "x".repeat(PROVIDER_OUTPUT_LIMIT_BYTES * 2);

        let output = capture.finish_bounded(&expand, usize::MAX);
        assert!(output.truncated);
        assert_eq!(output.observed_bytes, 4);
        assert!(output.text.len() <= PROVIDER_OUTPUT_LIMIT_BYTES);
        assert!(output.text.contains("...[output truncated:"));
    }

    #[test]
    fn multibyte_truncation_always_returns_valid_utf8() {
        let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
        let input = "\u{4f60}".repeat(30_000);
        capture.append_stdout(input.as_bytes());

        let output = capture.finish(&NoopRedactor);
        assert!(output.truncated);
        assert!(output.text.len() <= FOREGROUND_OUTPUT_LIMIT_BYTES);
        assert!(output.text.starts_with('\u{4f60}'));
        assert!(output.text.ends_with('\u{4f60}'));
        assert!(!output.text.contains(char::REPLACEMENT_CHARACTER));
    }

    #[test]
    fn pending_snapshot_does_not_consume_a_partial_scalar() {
        let capture = ProcessOutputCapture::new(CaptureMode::Foreground);
        capture.append_stdout([0xe4, 0xbd]);
        assert_eq!(
            capture.finish(&NoopRedactor).text,
            char::REPLACEMENT_CHARACTER.to_string()
        );

        capture.append_stdout([0xa0]);
        assert_eq!(capture.finish(&NoopRedactor).text, "\u{4f60}");
    }
}
