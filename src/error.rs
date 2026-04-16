use std::fmt;

/// A source location (line and column, 1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceLoc {
    pub line: u32,
    pub col: u32,
}

/// A compile error with an optional source location.
#[derive(Debug, Clone)]
pub struct CompileError {
    pub kind: ErrorKind,
    pub message: String,
    pub loc: Option<SourceLoc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Parse,
    Type,
    Codegen,
    Unsupported,
}

impl CompileError {
    pub fn parse(msg: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Parse, message: msg.into(), loc: None }
    }

    pub fn type_err(msg: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Type, message: msg.into(), loc: None }
    }

    pub fn codegen(msg: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Codegen, message: msg.into(), loc: None }
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self { kind: ErrorKind::Unsupported, message: msg.into(), loc: None }
    }

    /// Attach a source location to this error.
    pub fn with_loc(mut self, loc: SourceLoc) -> Self {
        self.loc = Some(loc);
        self
    }

    /// Attach a source location from a byte offset.
    pub fn with_offset(mut self, offset: u32, source: &str) -> Self {
        self.loc = Some(offset_to_loc(source, offset));
        self
    }

    /// Return the kind label for display.
    fn kind_label(&self) -> &'static str {
        match self.kind {
            ErrorKind::Parse => "parse error",
            ErrorKind::Type => "type error",
            ErrorKind::Codegen => "codegen error",
            ErrorKind::Unsupported => "unsupported",
        }
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(loc) = self.loc {
            write!(f, "{}:{}:{}: {}", loc.line, loc.col, self.kind_label(), self.message)
        } else {
            write!(f, "{}: {}", self.kind_label(), self.message)
        }
    }
}

impl std::error::Error for CompileError {}

/// Convert a byte offset in source to a 1-based line:column.
pub fn offset_to_loc(source: &str, offset: u32) -> SourceLoc {
    let offset = offset as usize;
    let bytes = source.as_bytes();
    let mut line = 1u32;
    let mut col = 1u32;
    for &byte in &bytes[..offset.min(bytes.len())] {
        if byte == b'\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    SourceLoc { line, col }
}

/// Format an error with a source context snippet.
pub fn format_error_with_context(error: &CompileError, source: &str) -> String {
    let Some(loc) = error.loc else {
        return error.to_string();
    };

    let lines: Vec<&str> = source.lines().collect();
    let line_idx = (loc.line as usize).saturating_sub(1);

    let mut out = format!("{}\n", error);

    if line_idx < lines.len() {
        let line_text = lines[line_idx];
        let line_num = loc.line;
        out.push_str(&format!("  {line_num} | {line_text}\n"));

        // Underline caret
        let padding = format!("{line_num}").len() + 3 + (loc.col as usize).saturating_sub(1);
        out.push_str(&" ".repeat(padding));
        out.push('^');
        out.push('\n');
    }

    out
}
