use std::collections::HashMap;

use thiserror::Error;

use super::{ToolExecutionControl, ToolExecutionControlError};

const MAX_CONTENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_EDIT_BYTES: usize = 64 * 1024;
const MAX_LINES: usize = 100_000;
const MAX_MATCHES: usize = 100_000;
const MAX_SIMILARITY_STEPS: usize = 20_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FuzzyStrategy {
    Exact,
    LineTrimmed,
    WhitespaceNormalized,
    IndentationFlexible,
    EscapeNormalized,
    TrimmedBoundary,
    UnicodeNormalized,
    BlockAnchor,
    ContextAware,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FuzzyReplaceResult {
    pub content: String,
    pub match_count: usize,
    pub strategy: FuzzyStrategy,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(super) enum FuzzyError {
    #[error("old_string cannot be empty")]
    EmptyOldString,
    #[error("old_string and new_string are identical")]
    IdenticalStrings,
    #[error("fuzzy input exceeds its bounded limit")]
    InputTooLarge,
    #[error("fuzzy replacement exceeds its bounded result limit")]
    ResultTooLarge,
    #[error("found {matches} matches but a unique match is required")]
    Ambiguous { matches: usize },
    #[error("could not find a match for old_string")]
    NoMatch,
    #[error("escape drift would introduce a spurious backslash")]
    EscapeDrift,
    #[error("fuzzy matches overlap")]
    OverlappingMatches,
    #[error("fuzzy similarity work exceeded its bounded budget")]
    ComplexityLimit,
    #[error("fuzzy operation was cancelled")]
    Cancelled,
    #[error("fuzzy operation exceeded its deadline")]
    DeadlineExceeded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
}

type StrategyFn = fn(&str, &str, &ToolExecutionControl) -> Result<Vec<Span>, FuzzyError>;

pub(super) fn find_and_replace(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    control: &ToolExecutionControl,
) -> Result<FuzzyReplaceResult, FuzzyError> {
    check_active(control)?;
    validate_inputs(content, old_string, new_string)?;
    let strategies: [(FuzzyStrategy, StrategyFn); 9] = [
        (FuzzyStrategy::Exact, strategy_exact),
        (FuzzyStrategy::LineTrimmed, strategy_line_trimmed),
        (
            FuzzyStrategy::WhitespaceNormalized,
            strategy_whitespace_normalized,
        ),
        (
            FuzzyStrategy::IndentationFlexible,
            strategy_indentation_flexible,
        ),
        (FuzzyStrategy::EscapeNormalized, strategy_escape_normalized),
        (FuzzyStrategy::TrimmedBoundary, strategy_trimmed_boundary),
        (
            FuzzyStrategy::UnicodeNormalized,
            strategy_unicode_normalized,
        ),
        (FuzzyStrategy::BlockAnchor, strategy_block_anchor),
        (FuzzyStrategy::ContextAware, strategy_context_aware),
    ];

    for (strategy, matcher) in strategies {
        check_active(control)?;
        let spans = matcher(content, old_string, control)?;
        if spans.is_empty() {
            continue;
        }
        if spans.len() > MAX_MATCHES {
            return Err(FuzzyError::InputTooLarge);
        }
        if spans.len() > 1 && !replace_all {
            return Err(FuzzyError::Ambiguous {
                matches: spans.len(),
            });
        }
        if strategy != FuzzyStrategy::Exact
            && has_escape_drift(content, &spans, old_string, new_string)
        {
            return Err(FuzzyError::EscapeDrift);
        }
        let mut effective_new = guarded_unescape(new_string, content, &spans);
        if strategy == FuzzyStrategy::UnicodeNormalized {
            effective_new = preserve_unicode(content, &spans, old_string, &effective_new, control)?;
        }
        let candidate = apply_replacements(
            content,
            &spans,
            &effective_new,
            (strategy != FuzzyStrategy::Exact).then_some(old_string),
            control,
        )?;
        return Ok(FuzzyReplaceResult {
            content: candidate,
            match_count: spans.len(),
            strategy,
        });
    }
    Err(FuzzyError::NoMatch)
}

fn validate_inputs(content: &str, old_string: &str, new_string: &str) -> Result<(), FuzzyError> {
    if old_string.is_empty() {
        return Err(FuzzyError::EmptyOldString);
    }
    if old_string == new_string {
        return Err(FuzzyError::IdenticalStrings);
    }
    if content.len() > MAX_CONTENT_BYTES
        || old_string.len() > MAX_EDIT_BYTES
        || new_string.len() > MAX_EDIT_BYTES
        || content.matches('\n').count() >= MAX_LINES
        || old_string.matches('\n').count() >= MAX_LINES
        || new_string.matches('\n').count() >= MAX_LINES
    {
        return Err(FuzzyError::InputTooLarge);
    }
    Ok(())
}

fn check_active(control: &ToolExecutionControl) -> Result<(), FuzzyError> {
    control.check().map_err(|error| match error {
        ToolExecutionControlError::Cancelled => FuzzyError::Cancelled,
        ToolExecutionControlError::DeadlineExceeded => FuzzyError::DeadlineExceeded,
    })
}

fn strategy_exact(
    content: &str,
    pattern: &str,
    control: &ToolExecutionControl,
) -> Result<Vec<Span>, FuzzyError> {
    exact_matches(content, pattern, control)
}

fn exact_matches(
    content: &str,
    pattern: &str,
    control: &ToolExecutionControl,
) -> Result<Vec<Span>, FuzzyError> {
    if pattern.is_empty() {
        return Ok(Vec::new());
    }
    let mut spans = Vec::new();
    let mut offset = 0usize;
    while offset <= content.len() {
        check_active(control)?;
        let Some(relative) = content[offset..].find(pattern) else {
            break;
        };
        let start = offset
            .checked_add(relative)
            .ok_or(FuzzyError::InputTooLarge)?;
        let end = start
            .checked_add(pattern.len())
            .ok_or(FuzzyError::InputTooLarge)?;
        spans.push(Span { start, end });
        if spans.len() > MAX_MATCHES {
            return Err(FuzzyError::InputTooLarge);
        }
        offset = end;
    }
    Ok(spans)
}

struct Lines<'a> {
    values: Vec<&'a str>,
    starts: Vec<usize>,
    content_len: usize,
}

impl<'a> Lines<'a> {
    fn new(content: &'a str) -> Result<Self, FuzzyError> {
        let values = content.split('\n').collect::<Vec<_>>();
        if values.len() > MAX_LINES {
            return Err(FuzzyError::InputTooLarge);
        }
        let mut starts = Vec::with_capacity(values.len());
        let mut offset = 0usize;
        for value in &values {
            starts.push(offset);
            offset = offset
                .checked_add(value.len())
                .and_then(|value| value.checked_add(1))
                .ok_or(FuzzyError::InputTooLarge)?;
        }
        Ok(Self {
            values,
            starts,
            content_len: content.len(),
        })
    }

    fn span(&self, start_line: usize, end_line: usize) -> Span {
        let start = self.starts[start_line];
        let end = if end_line >= self.values.len() {
            self.content_len
        } else {
            self.starts[end_line].saturating_sub(1)
        };
        Span { start, end }
    }
}

fn normalized_line_matches(
    content: &str,
    pattern: &str,
    normalize: fn(&str) -> String,
    control: &ToolExecutionControl,
) -> Result<Vec<Span>, FuzzyError> {
    let content_lines = Lines::new(content)?;
    let pattern_lines = pattern.split('\n').map(normalize).collect::<Vec<_>>();
    if pattern_lines.len() > content_lines.values.len() {
        return Ok(Vec::new());
    }
    let normalized_content = content_lines
        .values
        .iter()
        .map(|line| normalize(line))
        .collect::<Vec<_>>();
    let mut spans = Vec::new();
    for index in 0..=normalized_content.len() - pattern_lines.len() {
        check_active(control)?;
        if normalized_content[index..index + pattern_lines.len()] == pattern_lines {
            spans.push(content_lines.span(index, index + pattern_lines.len()));
        }
    }
    Ok(spans)
}

fn trim_line(value: &str) -> String {
    value.trim().to_owned()
}

fn trim_start_line(value: &str) -> String {
    value.trim_start().to_owned()
}

fn strategy_line_trimmed(
    content: &str,
    pattern: &str,
    control: &ToolExecutionControl,
) -> Result<Vec<Span>, FuzzyError> {
    normalized_line_matches(content, pattern, trim_line, control)
}

#[derive(Clone, Copy)]
struct NormalizedToken {
    normalized_start: usize,
    normalized_end: usize,
    original_start: usize,
    original_end: usize,
}

struct NormalizedText {
    text: String,
    tokens: Vec<NormalizedToken>,
}

fn whitespace_normalize(value: &str) -> NormalizedText {
    let mut text = String::with_capacity(value.len());
    let mut tokens = Vec::new();
    let mut characters = value.char_indices().peekable();
    while let Some((original_start, character)) = characters.next() {
        if matches!(character, ' ' | '\t') {
            let mut original_end = original_start + character.len_utf8();
            while let Some((next_start, next)) = characters.peek().copied() {
                if !matches!(next, ' ' | '\t') {
                    break;
                }
                characters.next();
                original_end = next_start + next.len_utf8();
            }
            let normalized_start = text.len();
            text.push(' ');
            tokens.push(NormalizedToken {
                normalized_start,
                normalized_end: text.len(),
                original_start,
                original_end,
            });
        } else {
            let normalized_start = text.len();
            text.push(character);
            tokens.push(NormalizedToken {
                normalized_start,
                normalized_end: text.len(),
                original_start,
                original_end: original_start + character.len_utf8(),
            });
        }
    }
    NormalizedText { text, tokens }
}

fn map_normalized_spans(normalized: &NormalizedText, spans: Vec<Span>) -> Vec<Span> {
    spans
        .into_iter()
        .filter_map(|span| {
            let first = normalized
                .tokens
                .partition_point(|token| token.normalized_end <= span.start);
            let after_last = normalized
                .tokens
                .partition_point(|token| token.normalized_start < span.end);
            (first < after_last).then(|| Span {
                start: normalized.tokens[first].original_start,
                end: normalized.tokens[after_last - 1].original_end,
            })
        })
        .collect()
}

fn strategy_whitespace_normalized(
    content: &str,
    pattern: &str,
    control: &ToolExecutionControl,
) -> Result<Vec<Span>, FuzzyError> {
    let normalized_content = whitespace_normalize(content);
    let normalized_pattern = whitespace_normalize(pattern);
    let spans = exact_matches(&normalized_content.text, &normalized_pattern.text, control)?;
    Ok(map_normalized_spans(&normalized_content, spans))
}

fn strategy_indentation_flexible(
    content: &str,
    pattern: &str,
    control: &ToolExecutionControl,
) -> Result<Vec<Span>, FuzzyError> {
    normalized_line_matches(content, pattern, trim_start_line, control)
}

fn strategy_escape_normalized(
    content: &str,
    pattern: &str,
    control: &ToolExecutionControl,
) -> Result<Vec<Span>, FuzzyError> {
    let unescaped = pattern
        .replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\r", "\r");
    if unescaped == pattern {
        return Ok(Vec::new());
    }
    exact_matches(content, &unescaped, control)
}

fn strategy_trimmed_boundary(
    content: &str,
    pattern: &str,
    control: &ToolExecutionControl,
) -> Result<Vec<Span>, FuzzyError> {
    let content_lines = Lines::new(content)?;
    let mut pattern_lines = pattern.split('\n').map(str::to_owned).collect::<Vec<_>>();
    if pattern_lines.is_empty() || pattern_lines.len() > content_lines.values.len() {
        return Ok(Vec::new());
    }
    pattern_lines[0] = pattern_lines[0].trim().to_owned();
    let last = pattern_lines.len() - 1;
    pattern_lines[last] = pattern_lines[last].trim().to_owned();
    let mut spans = Vec::new();
    for index in 0..=content_lines.values.len() - pattern_lines.len() {
        check_active(control)?;
        let mut matches = true;
        for (offset, expected) in pattern_lines.iter().enumerate() {
            let actual = content_lines.values[index + offset];
            let actual = if offset == 0 || offset == last {
                actual.trim()
            } else {
                actual
            };
            if actual != expected {
                matches = false;
                break;
            }
        }
        if matches {
            spans.push(content_lines.span(index, index + pattern_lines.len()));
        }
    }
    Ok(spans)
}

fn unicode_replacement(character: char) -> Option<&'static str> {
    match character {
        '\u{201c}' | '\u{201d}' => Some("\""),
        '\u{2018}' | '\u{2019}' => Some("'"),
        '\u{2014}' => Some("--"),
        '\u{2013}' => Some("-"),
        '\u{2026}' => Some("..."),
        '\u{00a0}' => Some(" "),
        _ => None,
    }
}

fn unicode_normalize(value: &str) -> NormalizedText {
    let mut text = String::with_capacity(value.len());
    let mut tokens = Vec::new();
    for (original_start, character) in value.char_indices() {
        let replacement = unicode_replacement(character);
        let normalized = replacement.unwrap_or_else(|| {
            let end = original_start + character.len_utf8();
            &value[original_start..end]
        });
        for normalized_character in normalized.chars() {
            let normalized_start = text.len();
            text.push(normalized_character);
            tokens.push(NormalizedToken {
                normalized_start,
                normalized_end: text.len(),
                original_start,
                original_end: original_start + character.len_utf8(),
            });
        }
    }
    NormalizedText { text, tokens }
}

fn strategy_unicode_normalized(
    content: &str,
    pattern: &str,
    control: &ToolExecutionControl,
) -> Result<Vec<Span>, FuzzyError> {
    let normalized_content = unicode_normalize(content);
    let normalized_pattern = unicode_normalize(pattern);
    if normalized_content.text == content && normalized_pattern.text == pattern {
        return Ok(Vec::new());
    }
    let mut spans = exact_matches(&normalized_content.text, &normalized_pattern.text, control)?;
    if spans.is_empty() {
        spans = strategy_line_trimmed(&normalized_content.text, &normalized_pattern.text, control)?;
    }
    Ok(map_normalized_spans(&normalized_content, spans))
}

fn strategy_block_anchor(
    content: &str,
    pattern: &str,
    control: &ToolExecutionControl,
) -> Result<Vec<Span>, FuzzyError> {
    let normalized_content = unicode_normalize(content).text;
    let normalized_pattern = unicode_normalize(pattern).text;
    let normalized_lines = Lines::new(&normalized_content)?;
    let original_lines = Lines::new(content)?;
    let pattern_lines = normalized_pattern.split('\n').collect::<Vec<_>>();
    if pattern_lines.len() < 2 || pattern_lines.len() > normalized_lines.values.len() {
        return Ok(Vec::new());
    }
    let first = pattern_lines[0].trim();
    let last = pattern_lines[pattern_lines.len() - 1].trim();
    let mut candidates = Vec::new();
    for index in 0..=normalized_lines.values.len() - pattern_lines.len() {
        check_active(control)?;
        if normalized_lines.values[index].trim() == first
            && normalized_lines.values[index + pattern_lines.len() - 1].trim() == last
        {
            candidates.push(index);
        }
    }
    let threshold = if candidates.len() == 1 { 0.50 } else { 0.70 };
    let pattern_middle = if pattern_lines.len() <= 2 {
        String::new()
    } else {
        pattern_lines[1..pattern_lines.len() - 1].join("\n")
    };
    let mut budget = SimilarityBudget::new();
    let mut spans = Vec::new();
    for index in candidates {
        check_active(control)?;
        let similarity = if pattern_lines.len() <= 2 {
            1.0
        } else {
            let content_middle =
                normalized_lines.values[index + 1..index + pattern_lines.len() - 1].join("\n");
            sequence_ratio(&content_middle, &pattern_middle, control, &mut budget)?
        };
        if similarity >= threshold {
            spans.push(original_lines.span(index, index + pattern_lines.len()));
        }
    }
    Ok(spans)
}

fn strategy_context_aware(
    content: &str,
    pattern: &str,
    control: &ToolExecutionControl,
) -> Result<Vec<Span>, FuzzyError> {
    let content_lines = Lines::new(content)?;
    let pattern_lines = pattern.split('\n').collect::<Vec<_>>();
    if pattern_lines.len() > content_lines.values.len() {
        return Ok(Vec::new());
    }
    let mut budget = SimilarityBudget::new();
    let mut spans = Vec::new();
    for index in 0..=content_lines.values.len() - pattern_lines.len() {
        check_active(control)?;
        let mut high_similarity = 0usize;
        for (offset, expected) in pattern_lines.iter().enumerate() {
            let ratio = sequence_ratio(
                expected.trim(),
                content_lines.values[index + offset].trim(),
                control,
                &mut budget,
            )?;
            if ratio >= 0.80 {
                high_similarity += 1;
            }
        }
        if high_similarity.saturating_mul(2) >= pattern_lines.len() {
            spans.push(content_lines.span(index, index + pattern_lines.len()));
        }
    }
    Ok(spans)
}

fn has_escape_drift(content: &str, spans: &[Span], old_string: &str, new_string: &str) -> bool {
    ["\\'", "\\\""].into_iter().any(|suspect| {
        new_string.contains(suspect)
            && old_string.contains(suspect)
            && !spans
                .iter()
                .any(|span| content[span.start..span.end].contains(suspect))
    })
}

fn guarded_unescape(new_string: &str, content: &str, spans: &[Span]) -> String {
    let has_tab = spans
        .iter()
        .any(|span| content[span.start..span.end].contains('\t'));
    let has_carriage_return = spans
        .iter()
        .any(|span| content[span.start..span.end].contains('\r'));
    let mut output = new_string.to_owned();
    if has_tab && output.contains("\\t") {
        output = output.replace("\\t", "\t");
    }
    if has_carriage_return && output.contains("\\r") {
        output = output.replace("\\r", "\r");
    }
    output
}

fn first_meaningful_line(value: &str) -> Option<&str> {
    value.split('\n').find(|line| !line.trim().is_empty())
}

fn leading_whitespace(value: &str) -> &str {
    value
        .trim_start_matches([' ', '\t'])
        .pipe(|without| &value[..value.len() - without.len()])
}

trait Pipe: Sized {
    fn pipe<T>(self, operation: impl FnOnce(Self) -> T) -> T {
        operation(self)
    }
}
impl<T> Pipe for T {}

fn reindent_replacement(file_region: &str, old_string: &str, new_string: &str) -> String {
    if new_string.is_empty() {
        return String::new();
    }
    let Some(old_first) = first_meaningful_line(old_string) else {
        return new_string.to_owned();
    };
    let Some(file_first) = first_meaningful_line(file_region) else {
        return new_string.to_owned();
    };
    let old_indent = leading_whitespace(old_first);
    let file_indent = leading_whitespace(file_first);
    if old_indent == file_indent {
        return new_string.to_owned();
    }
    new_string
        .split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                line.to_owned()
            } else if let Some(remainder) = line.strip_prefix(old_indent) {
                format!("{file_indent}{remainder}")
            } else {
                format!("{file_indent}{}", line.trim_start_matches([' ', '\t']))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn apply_replacements(
    content: &str,
    spans: &[Span],
    new_string: &str,
    old_string: Option<&str>,
    control: &ToolExecutionControl,
) -> Result<String, FuzzyError> {
    let mut ordered = spans.to_vec();
    ordered.sort_by_key(|span| span.start);
    for pair in ordered.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(FuzzyError::OverlappingMatches);
        }
    }
    let mut output = String::with_capacity(content.len().min(MAX_CONTENT_BYTES));
    let mut cursor = 0usize;
    for span in ordered {
        check_active(control)?;
        output.push_str(&content[cursor..span.start]);
        let adjusted = old_string.map_or_else(
            || new_string.to_owned(),
            |old| reindent_replacement(&content[span.start..span.end], old, new_string),
        );
        output.push_str(&adjusted);
        if output.len() > MAX_CONTENT_BYTES {
            return Err(FuzzyError::ResultTooLarge);
        }
        cursor = span.end;
    }
    output.push_str(&content[cursor..]);
    if output.len() > MAX_CONTENT_BYTES {
        return Err(FuzzyError::ResultTooLarge);
    }
    Ok(output)
}

#[derive(Clone, Copy)]
enum OpcodeTag {
    Equal,
    Replace,
    Delete,
    Insert,
}

struct Opcode {
    tag: OpcodeTag,
    i1: usize,
    i2: usize,
    j1: usize,
    j2: usize,
}

struct SimilarityBudget {
    remaining: usize,
}

impl SimilarityBudget {
    fn new() -> Self {
        Self {
            remaining: MAX_SIMILARITY_STEPS,
        }
    }

    fn consume(&mut self, amount: usize) -> Result<(), FuzzyError> {
        self.remaining = self
            .remaining
            .checked_sub(amount)
            .ok_or(FuzzyError::ComplexityLimit)?;
        Ok(())
    }
}

fn sequence_ratio(
    a: &str,
    b: &str,
    control: &ToolExecutionControl,
    budget: &mut SimilarityBudget,
) -> Result<f64, FuzzyError> {
    let a = a.chars().collect::<Vec<_>>();
    let b = b.chars().collect::<Vec<_>>();
    if a.is_empty() && b.is_empty() {
        return Ok(1.0);
    }
    let blocks = matching_blocks(&a, &b, control, budget)?;
    let matched = blocks.iter().map(|(_, _, size)| size).sum::<usize>();
    Ok(2.0 * matched as f64 / (a.len() + b.len()) as f64)
}

fn matching_blocks(
    a: &[char],
    b: &[char],
    control: &ToolExecutionControl,
    budget: &mut SimilarityBudget,
) -> Result<Vec<(usize, usize, usize)>, FuzzyError> {
    let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
    for (index, character) in b.iter().copied().enumerate() {
        b2j.entry(character).or_default().push(index);
    }
    if b.len() >= 200 {
        let popular = b.len() / 100 + 1;
        b2j.retain(|_, positions| positions.len() <= popular);
    }
    let mut pending = vec![(0usize, a.len(), 0usize, b.len())];
    let mut blocks = Vec::new();
    while let Some((alo, ahi, blo, bhi)) = pending.pop() {
        check_active(control)?;
        let (i, j, size) = find_longest_match(a, b, &b2j, alo, ahi, blo, bhi, control, budget)?;
        if size == 0 {
            continue;
        }
        blocks.push((i, j, size));
        if alo < i && blo < j {
            pending.push((alo, i, blo, j));
        }
        if i + size < ahi && j + size < bhi {
            pending.push((i + size, ahi, j + size, bhi));
        }
    }
    blocks.sort_unstable();
    let mut collapsed: Vec<(usize, usize, usize)> = Vec::new();
    for (i, j, size) in blocks {
        if let Some(last) = collapsed.last_mut()
            && last.0 + last.2 == i
            && last.1 + last.2 == j
        {
            last.2 += size;
        } else {
            collapsed.push((i, j, size));
        }
    }
    Ok(collapsed)
}

#[allow(clippy::too_many_arguments)]
fn find_longest_match(
    a: &[char],
    b: &[char],
    b2j: &HashMap<char, Vec<usize>>,
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
    control: &ToolExecutionControl,
    budget: &mut SimilarityBudget,
) -> Result<(usize, usize, usize), FuzzyError> {
    let (mut best_i, mut best_j, mut best_size) = (alo, blo, 0usize);
    let mut previous: HashMap<usize, usize> = HashMap::new();
    for (iteration, i) in (alo..ahi).enumerate() {
        if iteration % 256 == 0 {
            check_active(control)?;
        }
        let mut current = HashMap::new();
        if let Some(positions) = b2j.get(&a[i]) {
            for &j in positions {
                if j < blo {
                    continue;
                }
                if j >= bhi {
                    break;
                }
                budget.consume(1)?;
                let size = previous.get(&j.wrapping_sub(1)).copied().unwrap_or(0) + 1;
                current.insert(j, size);
                if size > best_size {
                    best_i = i + 1 - size;
                    best_j = j + 1 - size;
                    best_size = size;
                }
            }
        }
        previous = current;
    }
    while best_i > alo && best_j > blo && a[best_i - 1] == b[best_j - 1] {
        budget.consume(1)?;
        best_i -= 1;
        best_j -= 1;
        best_size += 1;
    }
    while best_i + best_size < ahi
        && best_j + best_size < bhi
        && a[best_i + best_size] == b[best_j + best_size]
    {
        budget.consume(1)?;
        best_size += 1;
    }
    Ok((best_i, best_j, best_size))
}

fn sequence_opcodes(
    a: &str,
    b: &str,
    control: &ToolExecutionControl,
) -> Result<Vec<Opcode>, FuzzyError> {
    let a = a.chars().collect::<Vec<_>>();
    let b = b.chars().collect::<Vec<_>>();
    let mut budget = SimilarityBudget::new();
    let mut blocks = matching_blocks(&a, &b, control, &mut budget)?;
    blocks.push((a.len(), b.len(), 0));
    let (mut i, mut j) = (0usize, 0usize);
    let mut opcodes = Vec::new();
    for (ai, bj, size) in blocks {
        let tag = match (i < ai, j < bj) {
            (true, true) => Some(OpcodeTag::Replace),
            (true, false) => Some(OpcodeTag::Delete),
            (false, true) => Some(OpcodeTag::Insert),
            (false, false) => None,
        };
        if let Some(tag) = tag {
            opcodes.push(Opcode {
                tag,
                i1: i,
                i2: ai,
                j1: j,
                j2: bj,
            });
        }
        if size > 0 {
            opcodes.push(Opcode {
                tag: OpcodeTag::Equal,
                i1: ai,
                i2: ai + size,
                j1: bj,
                j2: bj + size,
            });
        }
        i = ai + size;
        j = bj + size;
    }
    Ok(opcodes)
}

fn char_offsets(value: &str) -> Vec<usize> {
    value
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(value.len()))
        .collect()
}

fn char_slice<'a>(value: &'a str, offsets: &[usize], start: usize, end: usize) -> &'a str {
    &value[offsets[start]..offsets[end]]
}

fn preserve_unicode(
    content: &str,
    spans: &[Span],
    old_string: &str,
    new_string: &str,
    control: &ToolExecutionControl,
) -> Result<String, FuzzyError> {
    let file_region = spans
        .iter()
        .map(|span| &content[span.start..span.end])
        .collect::<String>();
    let normalized_old = unicode_normalize(old_string).text;
    let normalized_file = unicode_normalize(&file_region).text;
    if normalized_old != normalized_file {
        return Ok(new_string.to_owned());
    }
    let original_chars = file_region.chars().collect::<Vec<_>>();
    let mut original_to_normalized = Vec::with_capacity(original_chars.len() + 1);
    let mut normalized_position = 0usize;
    for character in &original_chars {
        original_to_normalized.push(normalized_position);
        normalized_position +=
            unicode_replacement(*character).map_or(1, |replacement| replacement.chars().count());
    }
    original_to_normalized.push(normalized_position);
    let mut normalized_to_original = HashMap::new();
    for (original, normalized) in original_to_normalized[..original_chars.len()]
        .iter()
        .copied()
        .enumerate()
    {
        normalized_to_original.entry(normalized).or_insert(original);
    }
    let opcodes = sequence_opcodes(&normalized_old, new_string, control)?;
    let file_offsets = char_offsets(&file_region);
    let new_offsets = char_offsets(new_string);
    let mut output = String::new();
    for opcode in opcodes {
        check_active(control)?;
        match opcode.tag {
            OpcodeTag::Equal => {
                let original_start = normalized_to_original.get(&opcode.i1).copied().unwrap_or(0);
                let mut original_end = original_start;
                while original_end < original_chars.len()
                    && original_to_normalized[original_end] < opcode.i2
                {
                    original_end += 1;
                }
                output.push_str(char_slice(
                    &file_region,
                    &file_offsets,
                    original_start,
                    original_end,
                ));
            }
            OpcodeTag::Replace | OpcodeTag::Insert => {
                output.push_str(char_slice(new_string, &new_offsets, opcode.j1, opcode.j2));
            }
            OpcodeTag::Delete => {}
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn control() -> ToolExecutionControl {
        ToolExecutionControl::new(Instant::now() + Duration::from_secs(30))
    }

    fn replace(content: &str, old: &str, new: &str, all: bool) -> FuzzyReplaceResult {
        find_and_replace(content, old, new, all, &control()).unwrap()
    }

    #[test]
    fn exact_is_first_unique_and_non_overlapping() {
        let result = replace("prefix hello suffix", "hello", "hi", false);
        assert_eq!(result.strategy, FuzzyStrategy::Exact);
        assert_eq!(result.content, "prefix hi suffix");
        let result = replace("aaaa", "aa", "b", true);
        assert_eq!(result.content, "bb");
        assert_eq!(result.match_count, 2);
        assert_eq!(
            find_and_replace("aaa aaa", "aaa", "x", false, &control()),
            Err(FuzzyError::Ambiguous { matches: 2 })
        );
    }

    #[test]
    fn ordered_chain_selects_line_whitespace_escape_unicode_block_and_context() {
        let cases = [
            (
                "  alpha  \n  beta  ",
                "alpha\nbeta",
                "done",
                FuzzyStrategy::LineTrimmed,
            ),
            (
                "let  value\t=  1;",
                "let value = 1;",
                "let value = 2;",
                FuzzyStrategy::WhitespaceNormalized,
            ),
            (
                "head\nbody\ntail",
                "head\\nbody\\ntail",
                "changed",
                FuzzyStrategy::EscapeNormalized,
            ),
            (
                "She said \u{201c}hello\u{201d}.",
                "She said \"hello\".",
                "She said \"goodbye\".",
                FuzzyStrategy::UnicodeNormalized,
            ),
            (
                "start\nalpha beta gamma\nend",
                "start\nalpha beta delta\nend",
                "block",
                FuzzyStrategy::BlockAnchor,
            ),
            (
                "alpha\ncompletely different",
                "alpha\nexpected line",
                "context",
                FuzzyStrategy::ContextAware,
            ),
        ];
        for (content, old, new, expected) in cases {
            let result = replace(content, old, new, false);
            assert_eq!(result.strategy, expected, "case: {old:?}");
        }
    }

    #[test]
    fn indentation_and_boundary_strategies_match_the_pinned_helpers() {
        let indent = strategy_indentation_flexible(
            "    alpha  \n        beta  ",
            "alpha  \n  beta  ",
            &control(),
        )
        .unwrap();
        assert_eq!(indent.len(), 1);
        let boundary = strategy_trimmed_boundary(
            "  first\n  middle  \nlast  ",
            "first\n  middle  \nlast",
            &control(),
        )
        .unwrap();
        assert_eq!(boundary.len(), 1);
    }

    #[test]
    fn fuzzy_replacement_reindents_relative_nesting_and_preserves_blank_lines() {
        let content = "    def old():\n        value = 1\n";
        let old = "def old():\n  value = 1";
        let new = "def new():\n  if ready:\n    value = 2\n";
        let result = replace(content, old, new, false);
        assert_ne!(result.strategy, FuzzyStrategy::Exact);
        assert_eq!(
            result.content,
            "    def new():\n      if ready:\n        value = 2\n\n"
        );
    }

    #[test]
    fn fuzzy_replacement_anchors_lines_dedented_below_the_llm_base() {
        let result = replace(
            "    if ready:\n        old()\nnext()",
            "  if ready:\n    old()",
            "  if ready:\n    new()\nreturn done",
            false,
        );
        assert_eq!(result.strategy, FuzzyStrategy::LineTrimmed);
        assert_eq!(
            result.content,
            "    if ready:\n      new()\n    return done\nnext()"
        );
    }

    #[test]
    fn escape_drift_is_blocked_but_genuine_escapes_are_allowed() {
        let error = find_and_replace(
            "print('hello')\n  next()",
            "print(\\'hello\\')\nnext()",
            "print(\\'world\\')\nnext()",
            false,
            &control(),
        );
        assert_eq!(error, Err(FuzzyError::EscapeDrift));
        let genuine = replace(
            "print(\\'hello\\')\n  next()",
            "print(\\'hello\\')\nnext()",
            "print(\\'world\\')\nnext()",
            false,
        );
        assert!(genuine.content.contains("\\'world\\'"));

        let introduced = replace(
            "print(\"hello\")\n  next()",
            "print(\"hello\")\nnext()",
            "print(\\\"world\\\")\nnext()",
            false,
        );
        assert!(introduced.content.contains("\\\"world\\\""));

        let double_quote_drift = find_and_replace(
            "print(\"hello\")\n  next()",
            "print(\\\"hello\\\")\nnext()",
            "print(\\\"world\\\")\nnext()",
            false,
            &control(),
        );
        assert_eq!(double_quote_drift, Err(FuzzyError::EscapeDrift));
    }

    #[test]
    fn tabs_and_carriage_returns_unescape_only_when_present_in_matched_region() {
        let tabbed = replace(
            "\talpha\n\tbeta",
            "alpha\nbeta",
            "alpha\\tvalue\nbeta",
            false,
        );
        assert!(tabbed.content.contains("alpha\tvalue"));
        let literal = replace("alpha  beta", "alpha beta", "alpha\\tbeta", false);
        assert!(literal.content.contains("\\t"));

        let carriage = replace("alpha\rbeta", "alpha\\rbeta", "alpha\\rnext", false);
        assert!(carriage.content.contains('\r'));
    }

    #[test]
    fn exact_matches_use_the_same_guarded_tab_and_carriage_return_unescape() {
        let tabbed = replace("alpha\tbeta", "alpha\tbeta", "alpha\\tnext", false);
        assert_eq!(tabbed.strategy, FuzzyStrategy::Exact);
        assert_eq!(tabbed.content, "alpha\tnext");

        let carriage = replace("alpha\rbeta", "alpha\rbeta", "alpha\\rnext", false);
        assert_eq!(carriage.strategy, FuzzyStrategy::Exact);
        assert_eq!(carriage.content, "alpha\rnext");

        let literal = replace("alpha\\tbeta", "alpha\\tbeta", "alpha\\rnext", false);
        assert_eq!(literal.strategy, FuzzyStrategy::Exact);
        assert_eq!(literal.content, "alpha\\rnext");
    }

    #[test]
    fn unicode_strategy_preserves_unchanged_smart_punctuation() {
        let result = replace(
            "A \u{2014} \u{201c}quoted\u{201d} value\u{2026}",
            "A -- \"quoted\" value...",
            "B -- \"quoted\" value...",
            false,
        );
        assert_eq!(result.strategy, FuzzyStrategy::UnicodeNormalized);
        assert_eq!(
            result.content,
            "B \u{2014} \u{201c}quoted\u{201d} value\u{2026}"
        );
    }

    #[test]
    fn block_anchor_thresholds_and_context_half_line_rule_are_locked() {
        let single_at_half =
            strategy_block_anchor("top\nabcDEF\nbottom", "top\nabcXYZ\nbottom", &control())
                .unwrap();
        assert_eq!(single_at_half.len(), 1);

        let multiple_below_seventy = strategy_block_anchor(
            "top\nabcDEF\nbottom\ntop\nabcUVW\nbottom",
            "top\nabcXYZ\nbottom",
            &control(),
        )
        .unwrap();
        assert!(multiple_below_seventy.is_empty());

        let multiple_at_seventy = strategy_block_anchor(
            "top\nabcdefgXYZ\nbottom\ntop\nabcdefgUVW\nbottom",
            "top\nabcdefg123\nbottom",
            &control(),
        )
        .unwrap();
        assert_eq!(multiple_at_seventy.len(), 2);

        let context = strategy_context_aware(
            "same\nnope\nother\nwrong",
            "same\nexpected\nother\nmissing",
            &control(),
        )
        .unwrap();
        assert_eq!(context.len(), 1);

        let below_half = strategy_context_aware(
            "same\nnope\nother\nwrong",
            "same\nexpected\ndifferent\nmissing",
            &control(),
        )
        .unwrap();
        assert!(below_half.is_empty());
    }

    #[test]
    fn normalized_whitespace_does_not_consume_a_following_boundary_space() {
        let result = replace("foo   bar baz", "foo bar", "XY", false);
        assert_eq!(result.content, "XY baz");
    }

    #[test]
    fn invalid_bounded_and_controlled_inputs_fail_closed() {
        assert_eq!(
            find_and_replace("abc", "", "x", false, &control()),
            Err(FuzzyError::EmptyOldString)
        );
        assert_eq!(
            find_and_replace("abc", "abc", "abc", false, &control()),
            Err(FuzzyError::IdenticalStrings)
        );
        let cancelled = control();
        cancelled.cancel();
        assert_eq!(
            find_and_replace("abc", "a", "x", false, &cancelled),
            Err(FuzzyError::Cancelled)
        );
        assert_eq!(
            find_and_replace(
                "abc",
                "a",
                "x",
                false,
                &ToolExecutionControl::new(Instant::now()),
            ),
            Err(FuzzyError::DeadlineExceeded)
        );
    }

    #[test]
    fn byte_line_match_and_result_limits_fail_closed() {
        let oversized_content = "x".repeat(MAX_CONTENT_BYTES + 1);
        assert_eq!(
            find_and_replace(&oversized_content, "x", "y", true, &control()),
            Err(FuzzyError::InputTooLarge)
        );

        let oversized_edit = "x".repeat(MAX_EDIT_BYTES + 1);
        assert_eq!(
            find_and_replace("abc", &oversized_edit, "y", false, &control()),
            Err(FuzzyError::InputTooLarge)
        );
        assert_eq!(
            find_and_replace("abc", "a", &oversized_edit, false, &control()),
            Err(FuzzyError::InputTooLarge)
        );

        let too_many_lines = "\n".repeat(MAX_LINES);
        assert_eq!(
            find_and_replace(&too_many_lines, "x", "y", false, &control()),
            Err(FuzzyError::InputTooLarge)
        );

        let too_many_matches = "a".repeat(MAX_MATCHES + 1);
        assert_eq!(
            find_and_replace(&too_many_matches, "a", "b", true, &control()),
            Err(FuzzyError::InputTooLarge)
        );

        let content_at_limit = format!("X{}", "a".repeat(MAX_CONTENT_BYTES - 1));
        assert_eq!(
            find_and_replace(&content_at_limit, "X", "YZ", false, &control()),
            Err(FuzzyError::ResultTooLarge)
        );
    }
}
