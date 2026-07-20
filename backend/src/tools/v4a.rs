use std::fmt;

const MAX_PATCH_BYTES: usize = 64 * 1024;
const MAX_PATCH_LINES: usize = 8 * 1024;
const MAX_LINE_BYTES: usize = 16 * 1024;
const MAX_PATH_BYTES: usize = 1_024;
const MAX_PATH_COMPONENT_BYTES: usize = 255;
const MAX_OPERATIONS: usize = 64;
const MAX_HUNKS: usize = 256;
const MAX_HUNK_LINES: usize = 4 * 1024;
const MAX_TOTAL_HUNK_LINES: usize = 8 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct V4aPatch {
    pub(super) operations: Vec<V4aOperation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum V4aOperation {
    Add {
        path: V4aPath,
        content: String,
    },
    Update {
        path: V4aPath,
        hunks: Vec<V4aHunk>,
    },
    Delete {
        path: V4aPath,
    },
    Move {
        source: V4aPath,
        destination: V4aPath,
    },
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct V4aPath(String);

impl V4aPath {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct V4aHunk {
    pub(super) context_hint: Option<String>,
    pub(super) lines: Vec<V4aHunkLine>,
}

impl V4aHunk {
    pub(super) fn changes_content(&self) -> bool {
        let mut before = Vec::new();
        let mut after = Vec::new();
        for line in &self.lines {
            match line {
                V4aHunkLine::Context(content) => {
                    before.push(content.as_str());
                    after.push(content.as_str());
                }
                V4aHunkLine::Remove(content) => before.push(content.as_str()),
                V4aHunkLine::Add(content) => after.push(content.as_str()),
            }
        }
        before != after
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum V4aHunkLine {
    Context(String),
    Remove(String),
    Add(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum V4aParseError {
    InputTooLarge,
    InvalidInput,
    TooManyLines,
    LineTooLong { line: usize },
    TooManyOperations,
    TooManyHunks,
    TooManyHunkLines,
    UnexpectedBoundary { line: usize },
    UnknownHeader { line: usize },
    MalformedHeader { line: usize },
    UnexpectedContent { line: usize },
    MalformedHunk { line: usize },
    InvalidPath { line: usize },
    UpdateWithoutHunks { line: usize },
    MoveMissingEndpoint { line: usize },
    NoChanges,
}

impl fmt::Display for V4aParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge => formatter.write_str("V4A patch exceeds the input limit"),
            Self::InvalidInput => formatter.write_str("V4A patch contains invalid input"),
            Self::TooManyLines => formatter.write_str("V4A patch contains too many lines"),
            Self::LineTooLong { line } => write!(formatter, "V4A line {line} is too long"),
            Self::TooManyOperations => {
                formatter.write_str("V4A patch contains too many operations")
            }
            Self::TooManyHunks => formatter.write_str("V4A patch contains too many hunks"),
            Self::TooManyHunkLines => formatter.write_str("V4A patch contains too many hunk lines"),
            Self::UnexpectedBoundary { line } => {
                write!(formatter, "unexpected V4A boundary at line {line}")
            }
            Self::UnknownHeader { line } => write!(formatter, "unknown V4A header at line {line}"),
            Self::MalformedHeader { line } => {
                write!(formatter, "malformed V4A header at line {line}")
            }
            Self::UnexpectedContent { line } => {
                write!(formatter, "unexpected V4A content at line {line}")
            }
            Self::MalformedHunk { line } => write!(formatter, "malformed V4A hunk at line {line}"),
            Self::InvalidPath { line } => write!(formatter, "invalid V4A path at line {line}"),
            Self::UpdateWithoutHunks { line } => {
                write!(formatter, "V4A update at line {line} has no hunks")
            }
            Self::MoveMissingEndpoint { line } => {
                write!(formatter, "V4A move at line {line} is missing an endpoint")
            }
            Self::NoChanges => formatter.write_str("V4A patch contains no changes"),
        }
    }
}

impl std::error::Error for V4aParseError {}

enum Directive<'a> {
    Begin,
    End,
    Add(&'a str),
    Update(&'a str),
    Delete(&'a str),
    Move(&'a str, &'a str),
}

enum PendingOperation {
    Add {
        path: V4aPath,
        content_lines: Vec<String>,
    },
    Update {
        path: V4aPath,
        header_line: usize,
        hunks: Vec<V4aHunk>,
        current_hunk: Option<V4aHunk>,
    },
}

struct Parser {
    operations: Vec<V4aOperation>,
    pending: Option<PendingOperation>,
    operation_headers: usize,
    hunk_count: usize,
    hunk_line_count: usize,
    saw_begin: bool,
    saw_end: bool,
    started: bool,
}

impl Parser {
    fn new() -> Self {
        Self {
            operations: Vec::new(),
            pending: None,
            operation_headers: 0,
            hunk_count: 0,
            hunk_line_count: 0,
            saw_begin: false,
            saw_end: false,
            started: false,
        }
    }

    fn consume_directive(
        &mut self,
        directive: Directive<'_>,
        line: usize,
    ) -> Result<(), V4aParseError> {
        match directive {
            Directive::Begin => {
                if self.saw_begin
                    || self.saw_end
                    || self.started
                    || self.pending.is_some()
                    || !self.operations.is_empty()
                {
                    return Err(V4aParseError::UnexpectedBoundary { line });
                }
                self.saw_begin = true;
                self.started = true;
            }
            Directive::End => {
                if self.saw_end {
                    return Err(V4aParseError::UnexpectedBoundary { line });
                }
                self.finish_pending()?;
                self.saw_end = true;
                self.started = true;
            }
            Directive::Add(raw_path) => {
                self.begin_operation(line)?;
                let path = parse_path(raw_path, line)?;
                self.pending = Some(PendingOperation::Add {
                    path,
                    content_lines: Vec::new(),
                });
            }
            Directive::Update(raw_path) => {
                self.begin_operation(line)?;
                let path = parse_path(raw_path, line)?;
                self.pending = Some(PendingOperation::Update {
                    path,
                    header_line: line,
                    hunks: Vec::new(),
                    current_hunk: None,
                });
            }
            Directive::Delete(raw_path) => {
                self.begin_operation(line)?;
                let path = parse_path(raw_path, line)?;
                self.operations.push(V4aOperation::Delete { path });
            }
            Directive::Move(raw_source, raw_destination) => {
                self.begin_operation(line)?;
                let source = parse_path(raw_source.trim(), line)?;
                let destination = parse_path(raw_destination.trim(), line)?;
                if source == destination {
                    return Err(V4aParseError::NoChanges);
                }
                self.operations.push(V4aOperation::Move {
                    source,
                    destination,
                });
            }
        }
        Ok(())
    }

    fn begin_operation(&mut self, line: usize) -> Result<(), V4aParseError> {
        if self.saw_end {
            return Err(V4aParseError::UnexpectedContent { line });
        }
        self.finish_pending()?;
        self.started = true;
        self.operation_headers = self
            .operation_headers
            .checked_add(1)
            .ok_or(V4aParseError::TooManyOperations)?;
        if self.operation_headers > MAX_OPERATIONS {
            return Err(V4aParseError::TooManyOperations);
        }
        Ok(())
    }

    fn consume_body_line(&mut self, raw: &str, line: usize) -> Result<(), V4aParseError> {
        if self.saw_end {
            return Err(V4aParseError::UnexpectedContent { line });
        }
        match self.pending.as_mut() {
            Some(PendingOperation::Add { content_lines, .. }) => {
                if raw == "\\ No newline at end of file" {
                    return Ok(());
                }
                let Some(content) = raw.strip_prefix('+') else {
                    return Err(V4aParseError::UnexpectedContent { line });
                };
                content_lines.push(content.to_owned());
                Ok(())
            }
            Some(PendingOperation::Update { .. }) if raw.starts_with("@@") => {
                let context_hint = parse_hunk_marker(raw, line)?;
                self.start_hunk(context_hint)
            }
            Some(PendingOperation::Update { .. }) => {
                if raw == "\\ No newline at end of file" {
                    return Ok(());
                }
                let hunk_line = if let Some(content) = raw.strip_prefix('+') {
                    V4aHunkLine::Add(content.to_owned())
                } else if let Some(content) = raw.strip_prefix('-') {
                    V4aHunkLine::Remove(content.to_owned())
                } else if let Some(content) = raw.strip_prefix(' ') {
                    V4aHunkLine::Context(content.to_owned())
                } else {
                    V4aHunkLine::Context(raw.to_owned())
                };
                self.push_hunk_line(hunk_line)
            }
            None => Err(V4aParseError::UnexpectedContent { line }),
        }
    }

    fn start_hunk(&mut self, context_hint: Option<String>) -> Result<(), V4aParseError> {
        self.hunk_count = self
            .hunk_count
            .checked_add(1)
            .ok_or(V4aParseError::TooManyHunks)?;
        if self.hunk_count > MAX_HUNKS {
            return Err(V4aParseError::TooManyHunks);
        }
        let Some(PendingOperation::Update {
            hunks,
            current_hunk,
            ..
        }) = self.pending.as_mut()
        else {
            return Err(V4aParseError::InvalidInput);
        };
        if let Some(previous) = current_hunk.take()
            && !previous.lines.is_empty()
        {
            hunks.push(previous);
        }
        *current_hunk = Some(V4aHunk {
            context_hint,
            lines: Vec::new(),
        });
        Ok(())
    }

    fn push_hunk_line(&mut self, hunk_line: V4aHunkLine) -> Result<(), V4aParseError> {
        let needs_implicit_hunk = matches!(
            self.pending,
            Some(PendingOperation::Update {
                current_hunk: None,
                ..
            })
        );
        if needs_implicit_hunk {
            self.start_hunk(None)?;
        }
        self.hunk_line_count = self
            .hunk_line_count
            .checked_add(1)
            .ok_or(V4aParseError::TooManyHunkLines)?;
        if self.hunk_line_count > MAX_TOTAL_HUNK_LINES {
            return Err(V4aParseError::TooManyHunkLines);
        }
        let Some(PendingOperation::Update {
            current_hunk: Some(hunk),
            ..
        }) = self.pending.as_mut()
        else {
            return Err(V4aParseError::InvalidInput);
        };
        if hunk.lines.len() >= MAX_HUNK_LINES {
            return Err(V4aParseError::TooManyHunkLines);
        }
        hunk.lines.push(hunk_line);
        Ok(())
    }

    fn finish_pending(&mut self) -> Result<(), V4aParseError> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        match pending {
            PendingOperation::Add {
                path,
                content_lines,
            } => self.operations.push(V4aOperation::Add {
                path,
                content: content_lines.join("\n"),
            }),
            PendingOperation::Update {
                path,
                header_line,
                mut hunks,
                current_hunk,
            } => {
                if let Some(hunk) = current_hunk
                    && !hunk.lines.is_empty()
                {
                    hunks.push(hunk);
                }
                if hunks.is_empty() {
                    return Err(V4aParseError::UpdateWithoutHunks { line: header_line });
                }
                self.operations.push(V4aOperation::Update { path, hunks });
            }
        }
        Ok(())
    }

    fn finish(mut self) -> Result<V4aPatch, V4aParseError> {
        self.finish_pending()?;
        let has_changes = self.operations.iter().any(|operation| match operation {
            V4aOperation::Update { hunks, .. } => hunks.iter().any(V4aHunk::changes_content),
            V4aOperation::Move {
                source,
                destination,
            } => source != destination,
            V4aOperation::Add { .. } | V4aOperation::Delete { .. } => true,
        });
        if self.operations.is_empty() || !has_changes {
            return Err(V4aParseError::NoChanges);
        }
        Ok(V4aPatch {
            operations: self.operations,
        })
    }
}

pub(super) fn parse_v4a_patch(patch: &str) -> Result<V4aPatch, V4aParseError> {
    if patch.len() > MAX_PATCH_BYTES {
        return Err(V4aParseError::InputTooLarge);
    }
    if patch.contains('\0') {
        return Err(V4aParseError::InvalidInput);
    }

    let mut parser = Parser::new();
    for (index, raw_line) in patch.split('\n').enumerate() {
        if index >= MAX_PATCH_LINES {
            return Err(V4aParseError::TooManyLines);
        }
        let line_number = index + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.len() > MAX_LINE_BYTES {
            return Err(V4aParseError::LineTooLong { line: line_number });
        }
        if let Some(directive) = parse_directive(line, line_number)? {
            parser.consume_directive(directive, line_number)?;
        } else if !line.is_empty() {
            parser.consume_body_line(line, line_number)?;
        }
    }
    parser.finish()
}

fn parse_directive(line: &str, line_number: usize) -> Result<Option<Directive<'_>>, V4aParseError> {
    let Some(rest) = line.strip_prefix("***") else {
        return Ok(None);
    };
    let rest = rest.trim_start_matches([' ', '\t']);
    if boundary_matches(rest, "Begin") {
        return Ok(Some(Directive::Begin));
    }
    if boundary_matches(rest, "End") {
        return Ok(Some(Directive::End));
    }
    if let Some(path) = parse_file_header(rest, "Add", line_number)? {
        return Ok(Some(Directive::Add(path)));
    }
    if let Some(path) = parse_file_header(rest, "Update", line_number)? {
        return Ok(Some(Directive::Update(path)));
    }
    if let Some(path) = parse_file_header(rest, "Delete", line_number)? {
        return Ok(Some(Directive::Delete(path)));
    }
    if let Some(move_body) = parse_file_header(rest, "Move", line_number)? {
        let Some((source, destination)) = move_body.split_once("->") else {
            return Err(V4aParseError::MoveMissingEndpoint { line: line_number });
        };
        if source.trim().is_empty() || destination.trim().is_empty() || destination.contains("->") {
            return Err(V4aParseError::MoveMissingEndpoint { line: line_number });
        }
        return Ok(Some(Directive::Move(source, destination)));
    }
    Err(V4aParseError::UnknownHeader { line: line_number })
}

fn boundary_matches(rest: &str, boundary: &str) -> bool {
    let mut words = rest.split_ascii_whitespace();
    words.next() == Some(boundary) && words.next() == Some("Patch") && words.next().is_none()
}

fn parse_file_header<'a>(
    rest: &'a str,
    operation: &str,
    line: usize,
) -> Result<Option<&'a str>, V4aParseError> {
    let Some(after_operation) = rest.strip_prefix(operation) else {
        return Ok(None);
    };
    if after_operation
        .chars()
        .next()
        .is_some_and(|character| !character.is_ascii_whitespace())
    {
        return Ok(None);
    }
    let after_operation = after_operation.trim_start_matches([' ', '\t']);
    let Some(path) = after_operation.strip_prefix("File:") else {
        return Err(V4aParseError::MalformedHeader { line });
    };
    Ok(Some(path))
}

fn parse_hunk_marker(marker: &str, line: usize) -> Result<Option<String>, V4aParseError> {
    if marker == "@@" {
        return Ok(None);
    }
    let Some(inner) = marker
        .strip_prefix("@@")
        .and_then(|rest| rest.strip_suffix("@@"))
    else {
        return Err(V4aParseError::MalformedHunk { line });
    };
    let hint = inner.trim();
    Ok((!hint.is_empty()).then(|| hint.to_owned()))
}

fn parse_path(raw: &str, line: usize) -> Result<V4aPath, V4aParseError> {
    let raw = raw.trim_start_matches([' ', '\t']);
    if raw.is_empty()
        || raw.len() > MAX_PATH_BYTES
        || raw.ends_with([' ', '\t'])
        || raw.starts_with('/')
        || raw.starts_with('\\')
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
    {
        return Err(V4aParseError::InvalidPath { line });
    }

    let mut components = Vec::new();
    for component in raw.split('/') {
        if component.is_empty() || component == ".." {
            return Err(V4aParseError::InvalidPath { line });
        }
        if component == "." {
            continue;
        }
        if !portable_component(component) {
            return Err(V4aParseError::InvalidPath { line });
        }
        components.push(component);
    }
    if components.is_empty() {
        return Err(V4aParseError::InvalidPath { line });
    }
    Ok(V4aPath(components.join("/")))
}

fn portable_component(component: &str) -> bool {
    if component.len() > MAX_PATH_COMPONENT_BYTES
        || component.ends_with(' ')
        || component.ends_with('.')
        || component
            .chars()
            .any(|character| matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return false;
    }

    let stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .to_ascii_lowercase();
    if matches!(
        stem.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
    ) {
        return false;
    }
    let stem_bytes = stem.as_bytes();
    if stem_bytes.len() == 4 {
        let prefix = &stem_bytes[..3];
        let suffix = stem_bytes[3];
        if matches!(prefix, b"com" | b"lpt") && matches!(suffix, b'1'..=b'9') {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_operation_types() {
        let patch = "*** Begin Patch\n\
*** Update File: src/lib.rs\n\
@@ function @@\n\
 keep\n\
-old\n\
+new\n\
*** Add File: src/new.rs\n\
+fn new() {}\n\
+\n\
*** Delete File: src/old.rs\n\
*** Move File: src/from.rs -> src/to.rs\n\
*** End Patch";

        let parsed = parse_v4a_patch(patch).unwrap();
        assert_eq!(parsed.operations.len(), 4);
        assert!(matches!(
            &parsed.operations[0],
            V4aOperation::Update { path, hunks }
                if path.as_str() == "src/lib.rs"
                    && hunks.len() == 1
                    && hunks[0].context_hint.as_deref() == Some("function")
                    && hunks[0].changes_content()
        ));
        assert!(matches!(
            &parsed.operations[1],
            V4aOperation::Add { path, content }
                if path.as_str() == "src/new.rs" && content == "fn new() {}\n"
        ));
        assert!(matches!(
            &parsed.operations[2],
            V4aOperation::Delete { path } if path.as_str() == "src/old.rs"
        ));
        assert!(matches!(
            &parsed.operations[3],
            V4aOperation::Move { source, destination }
                if source.as_str() == "src/from.rs" && destination.as_str() == "src/to.rs"
        ));
    }

    #[test]
    fn move_trims_surrounding_whitespace_and_paths_support_utf8() {
        let patch = "*** Move File:\t\u{76ee}\u{5f55}/\u{65e7}.rs \t -> \t \u{76ee}\u{5f55}/\u{65b0}.rs \t ";
        let parsed = parse_v4a_patch(patch).unwrap();
        assert!(matches!(
            &parsed.operations[0],
            V4aOperation::Move { source, destination }
                if source.as_str() == "\u{76ee}\u{5f55}/\u{65e7}.rs"
                    && destination.as_str() == "\u{76ee}\u{5f55}/\u{65b0}.rs"
        ));

        let added =
            parse_v4a_patch("*** Add File: \u{8d44}\u{6599}/\u{914d}\u{7f6e}.toml\n+\u{503c} = 1")
                .unwrap();
        assert!(matches!(
            &added.operations[0],
            V4aOperation::Add { path, content }
                if path.as_str() == "\u{8d44}\u{6599}/\u{914d}\u{7f6e}.toml"
                    && content == "\u{503c} = 1"
        ));
    }

    #[test]
    fn normalizes_crlf_and_skips_exact_no_newline_markers() {
        let patch = concat!(
            "*** Begin Patch\r\n",
            "*** Update File: src/lib.rs\r\n",
            "@@ function @@\r\n",
            " keep\r\n",
            "-old\r\n",
            "+new\r\n",
            "\\ No newline at end of file\r\n",
            "*** End Patch\r\n",
        );
        let parsed = parse_v4a_patch(patch).unwrap();
        let V4aOperation::Update { hunks, .. } = &parsed.operations[0] else {
            panic!("expected update operation");
        };
        assert_eq!(hunks[0].context_hint.as_deref(), Some("function"));
        assert_eq!(
            hunks[0].lines,
            vec![
                V4aHunkLine::Context("keep".to_owned()),
                V4aHunkLine::Remove("old".to_owned()),
                V4aHunkLine::Add("new".to_owned()),
            ]
        );
    }

    #[test]
    fn parses_without_boundaries_or_hunk_marker_and_keeps_implicit_context() {
        let patch = concat!(
            "*** Update File: src/lib.rs\n",
            "implicit context\n",
            "\\literal context\n",
            "-old\n",
            "+new",
        );
        let parsed = parse_v4a_patch(patch).unwrap();
        let V4aOperation::Update { hunks, .. } = &parsed.operations[0] else {
            panic!("expected update operation");
        };
        assert!(hunks[0].context_hint.is_none());
        assert_eq!(
            hunks[0].lines,
            vec![
                V4aHunkLine::Context("implicit context".to_owned()),
                V4aHunkLine::Context("\\literal context".to_owned()),
                V4aHunkLine::Remove("old".to_owned()),
                V4aHunkLine::Add("new".to_owned()),
            ]
        );
    }

    #[test]
    fn consecutive_operations_preserve_prefixed_empty_lines() {
        let patch = concat!(
            "*** Add File: first.txt\n",
            "+alpha\n",
            "+\n",
            "+omega\n",
            "\\ No newline at end of file\n",
            "\n",
            "*** Update File: second.txt\n",
            " context\n",
            " \n",
            "-\n",
            "+replacement\n",
            "*** Delete File: third.txt",
        );
        let parsed = parse_v4a_patch(patch).unwrap();
        assert_eq!(parsed.operations.len(), 3);
        assert!(matches!(
            &parsed.operations[0],
            V4aOperation::Add { content, .. } if content == "alpha\n\nomega"
        ));
        let V4aOperation::Update { hunks, .. } = &parsed.operations[1] else {
            panic!("expected update operation");
        };
        assert_eq!(
            hunks[0].lines,
            vec![
                V4aHunkLine::Context("context".to_owned()),
                V4aHunkLine::Context(String::new()),
                V4aHunkLine::Remove(String::new()),
                V4aHunkLine::Add("replacement".to_owned()),
            ]
        );
        assert!(matches!(
            &parsed.operations[2],
            V4aOperation::Delete { path } if path.as_str() == "third.txt"
        ));
    }

    #[test]
    fn accepts_lenient_markers_headers_and_missing_boundaries() {
        let with_markers = "***Begin Patch\n***Update File: ./src/lib.rs\n-old\n+new\n***End Patch";
        let parsed = parse_v4a_patch(with_markers).unwrap();
        assert!(matches!(
            &parsed.operations[0],
            V4aOperation::Update { path, .. } if path.as_str() == "src/lib.rs"
        ));

        let without_markers = "***Add File: src/new.rs\n+content";
        assert!(matches!(
            &parse_v4a_patch(without_markers).unwrap().operations[0],
            V4aOperation::Add { content, .. } if content == "content"
        ));
    }

    #[test]
    fn boundary_markers_must_be_whole_directives_and_cannot_repeat() {
        let content =
            parse_v4a_patch("*** Add File: note.txt\n+literal *** End Patch value").unwrap();
        assert!(matches!(
            &content.operations[0],
            V4aOperation::Add { content, .. } if content == "literal *** End Patch value"
        ));

        assert_eq!(
            parse_v4a_patch("prefix *** Begin Patch\n*** Add File: x\n+x"),
            Err(V4aParseError::UnexpectedContent { line: 1 })
        );
        assert_eq!(
            parse_v4a_patch("*** Begin Patch suffix\n*** Add File: x\n+x"),
            Err(V4aParseError::UnknownHeader { line: 1 })
        );
        assert_eq!(
            parse_v4a_patch("*** Begin Patch\n*** Begin Patch\n*** Add File: x\n+x"),
            Err(V4aParseError::UnexpectedBoundary { line: 2 })
        );
        assert_eq!(
            parse_v4a_patch("*** Add File: x\n+x\n*** End Patch\n*** End Patch"),
            Err(V4aParseError::UnexpectedBoundary { line: 4 })
        );
    }

    #[test]
    fn enforces_resource_limits() {
        assert_eq!(
            parse_v4a_patch(&"x".repeat(MAX_PATCH_BYTES + 1)),
            Err(V4aParseError::InputTooLarge)
        );
        let too_many_lines = "\n".repeat(MAX_PATCH_LINES);
        assert_eq!(
            parse_v4a_patch(&too_many_lines),
            Err(V4aParseError::TooManyLines)
        );
        let long_line = format!("*** Add File: x\n+{}", "x".repeat(MAX_LINE_BYTES + 1));
        assert!(matches!(
            parse_v4a_patch(&long_line),
            Err(V4aParseError::LineTooLong { .. })
        ));

        let line_at_limit = format!("*** Add File: x\n+{}", "x".repeat(MAX_LINE_BYTES - 1));
        assert!(parse_v4a_patch(&line_at_limit).is_ok());

        let operations = (0..=MAX_OPERATIONS)
            .map(|index| format!("*** Delete File: f{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            parse_v4a_patch(&operations),
            Err(V4aParseError::TooManyOperations)
        );
        let operations_at_limit = (0..MAX_OPERATIONS)
            .map(|index| format!("*** Delete File: f{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            parse_v4a_patch(&operations_at_limit)
                .unwrap()
                .operations
                .len(),
            MAX_OPERATIONS
        );

        let hunks = (0..=MAX_HUNKS)
            .map(|index| format!("@@ h{index} @@\n+line{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            parse_v4a_patch(&format!("*** Update File: f\n{hunks}")),
            Err(V4aParseError::TooManyHunks)
        );

        let hunk_lines = (0..=MAX_HUNK_LINES)
            .map(|_| "+x")
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            parse_v4a_patch(&format!("*** Update File: f\n{hunk_lines}")),
            Err(V4aParseError::TooManyHunkLines)
        );

        assert_eq!(
            parse_v4a_patch("*** Add File: x\n+bad\0content"),
            Err(V4aParseError::InvalidInput)
        );
    }

    #[test]
    fn path_limits_are_measured_in_utf8_bytes_and_across_components() {
        let four_byte_component = parse_v4a_patch("*** Add File: \u{e9}\u{e9}\n+x").unwrap();
        assert!(matches!(
            &four_byte_component.operations[0],
            V4aOperation::Add { path, .. } if path.as_str() == "\u{e9}\u{e9}"
        ));

        let glyph = "\u{754c}";
        let component_at_limit = glyph.repeat(MAX_PATH_COMPONENT_BYTES / glyph.len());
        let accepted = format!("*** Add File: {component_at_limit}\n+x");
        assert!(parse_v4a_patch(&accepted).is_ok());

        let oversized_component = format!("{component_at_limit}a");
        assert!(matches!(
            parse_v4a_patch(&format!("*** Add File: {oversized_component}\n+x")),
            Err(V4aParseError::InvalidPath { .. })
        ));

        let oversized_path = (0..6)
            .map(|_| "a".repeat(200))
            .collect::<Vec<_>>()
            .join("/");
        assert!(oversized_path.len() > MAX_PATH_BYTES);
        assert!(matches!(
            parse_v4a_patch(&format!("*** Add File: {oversized_path}\n+x")),
            Err(V4aParseError::InvalidPath { .. })
        ));
    }

    #[test]
    fn rejects_unsafe_or_nonportable_paths() {
        for path in [
            "../escape",
            "src/../escape",
            "/absolute",
            "C:/absolute",
            r"src\file",
            "src/file:stream",
            "src/trailing.",
            "src/trailing ",
            "src/CON.txt",
            "src/LPT9",
            "src/bad?.txt",
            "src/control\u{0001}",
            ".",
            "src//file",
        ] {
            let patch = format!("*** Add File: {path}\n+x");
            assert!(
                matches!(
                    parse_v4a_patch(&patch),
                    Err(V4aParseError::InvalidPath { .. })
                ),
                "unsafe path accepted: {path:?}"
            );
        }

        let oversized = "a".repeat(MAX_PATH_BYTES + 1);
        assert!(matches!(
            parse_v4a_patch(&format!("*** Add File: {oversized}\n+x")),
            Err(V4aParseError::InvalidPath { .. })
        ));
    }

    #[test]
    fn rejects_unknown_and_malformed_headers() {
        assert!(matches!(
            parse_v4a_patch("*** Copy File: a -> b"),
            Err(V4aParseError::UnknownHeader { .. })
        ));
        assert!(matches!(
            parse_v4a_patch("*** Update File:"),
            Err(V4aParseError::InvalidPath { .. })
        ));
        for malformed in [
            "*** Move File: source",
            "*** Move File: -> destination",
            "*** Move File: source ->",
            "*** Move File: a -> b -> c",
        ] {
            assert!(matches!(
                parse_v4a_patch(malformed),
                Err(V4aParseError::MoveMissingEndpoint { .. })
            ));
        }
        assert!(matches!(
            parse_v4a_patch("*** Update File src/lib.rs\n-old\n+new"),
            Err(V4aParseError::MalformedHeader { .. })
        ));
    }

    #[test]
    fn rejects_update_without_hunks_and_patches_without_changes() {
        assert!(matches!(
            parse_v4a_patch("*** Update File: src/lib.rs"),
            Err(V4aParseError::UpdateWithoutHunks { .. })
        ));
        for patch in [
            "",
            "*** Begin Patch\n*** End Patch",
            "*** Update File: src/lib.rs\n context only",
            "*** Update File: src/lib.rs\n-same\n+same",
            "*** Move File: same -> same",
        ] {
            assert_eq!(parse_v4a_patch(patch), Err(V4aParseError::NoChanges));
        }
    }

    #[test]
    fn rejects_content_outside_operations_and_after_end() {
        assert!(matches!(
            parse_v4a_patch("prose\n*** Add File: x\n+x"),
            Err(V4aParseError::UnexpectedContent { line: 1 })
        ));
        assert!(matches!(
            parse_v4a_patch("*** Delete File: x\nbody"),
            Err(V4aParseError::UnexpectedContent { line: 2 })
        ));
        assert!(matches!(
            parse_v4a_patch("*** Add File: x\n+x\n*** End Patch\ntrailing"),
            Err(V4aParseError::UnexpectedContent { line: 4 })
        ));
    }
}
