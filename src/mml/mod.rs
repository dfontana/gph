//! Experimental Lisp-like flowchart syntax.
//!
//! MML deliberately represents a small, round-trippable Mermaid flowchart
//! subset. [`from_mermaid`] rejects any Mermaid source it cannot represent
//! exactly instead of silently omitting unsupported constructs.

mod ast;
mod from_mermaid;
mod mermaid;
mod parser;

use std::fmt;

use ast::Graph;

/// A parsing or conversion error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl Error {
    fn mml(line: usize, col: usize, message: impl fmt::Display) -> Self {
        Self {
            message: format!("MML parse error at {line}:{col}: {message}"),
        }
    }

    fn mml_offset(source: &str, offset: usize, message: impl fmt::Display) -> Self {
        let prefix = &source[..offset.min(source.len())];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let col = prefix.rsplit('\n').next().unwrap_or(prefix).chars().count() + 1;
        Self::mml(line, col, message)
    }

    fn mermaid(line: usize, message: impl fmt::Display) -> Self {
        Self {
            message: format!("Mermaid parse error on line {line}: {message}"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(formatter)
    }
}

impl std::error::Error for Error {}

/// Result type returned by MML operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Parse a complete MML graph expression.
pub fn parse(source: &str) -> Result<Graph> {
    parser::parse(source)
}

/// Convert MML source to Mermaid flowchart text.
pub fn to_mermaid(source: &str) -> Result<String> {
    Ok(mermaid::generate(&parse(source)?))
}

/// Parse Mermaid flowchart text from the exact subset MML can represent.
///
/// This accepts all Mermaid emitted by [`to_mermaid`]. It intentionally rejects
/// unsupported Mermaid constructs and lossy edge chains rather than skipping or
/// partially converting them.
pub fn parse_mermaid(source: &str) -> Result<Graph> {
    from_mermaid::parse(source)
}

/// Convert supported Mermaid flowchart text to canonical MML source.
pub fn from_mermaid(source: &str) -> Result<String> {
    Ok(mermaid::print(&parse_mermaid(source)?))
}

#[cfg(test)]
mod tests {
    use super::{from_mermaid, parse, to_mermaid};

    fn convert(source: &str, expected: &str) {
        assert_eq!(to_mermaid(source).unwrap(), expected);
    }

    #[test]
    fn converts_mml_to_mermaid() {
        convert(
            r#"(graph lr
  (-> (login "Login" round) (validate "Validate Input" diamond))
  (-> validate (dashboard "Dashboard" stadium) "ok")
  (--> validate login "retry"))"#,
            "flowchart LR\n  login(\"Login\")\n  validate{\"Validate Input\"}\n  login --> validate\n  dashboard([\"Dashboard\"])\n  validate -->|ok| dashboard\n  validate -.->|retry| login",
        );
    }

    #[test]
    fn parses_all_supported_shapes_and_arrows() {
        let graph = parse(
            r#"(graph td
              (box "Box") (round "Round" round) (diamond "Diamond" diamond)
              (stadium "Stadium" stadium) (hex "Hex" hex) (sub "Sub" sub)
              (-> box round) (--> round diamond) (=> diamond stadium)
              (-o stadium hex) (-x hex sub))"#,
        )
        .unwrap();
        assert_eq!(graph.stmts.len(), 11);
    }

    #[test]
    fn accepts_comments_and_requires_a_complete_mml_document() {
        assert_eq!(
            to_mermaid("(graph lr ; heading\n  (a \"A\") ; node\n)").unwrap(),
            "flowchart LR\n  a[\"A\"]"
        );
        assert!(parse("(graph lr) (a \"A\")").is_err());
    }

    #[test]
    fn rejects_unknown_mml_escape_sequences() {
        let error = parse(r#"(graph lr (a "\q"))"#).unwrap_err();
        assert!(error.to_string().contains("MML parse error"));
    }

    #[test]
    fn reconstructs_labeled_chains_emitted_by_mml() {
        let source = "flowchart LR\n  a --> b\n  b -->|done| c";
        assert_eq!(
            from_mermaid(source).unwrap(),
            "(graph lr\n  (-> a b c \"done\"))"
        );
    }

    #[test]
    fn mml_mermaid_mml_round_trip() {
        let source = r#"(graph bt
  (first "First" hex)
  (-> first middle last "done")
  (-x last first "retry"))"#;
        let mml = from_mermaid(&to_mermaid(source).unwrap()).unwrap();
        assert_eq!(to_mermaid(&mml).unwrap(), to_mermaid(source).unwrap());
    }

    #[test]
    fn decodes_supported_mermaid_label_entities() {
        assert_eq!(
            from_mermaid("flowchart LR\n  a[\"A#quot;B\"]\n  a -->|go#124;now| b").unwrap(),
            "(graph lr\n  (a \"A\\\"B\")\n  (-> a b \"go|now\"))"
        );
    }

    #[test]
    fn rejects_unknown_mermaid_instead_of_skipping_it() {
        let error = from_mermaid("flowchart LR\n  classDef hot fill:#f00").unwrap_err();
        assert!(error.to_string().contains("Mermaid parse error"));
    }

    #[test]
    fn rejects_lossy_mermaid_chains() {
        let error = from_mermaid("flowchart LR\n  a -->|first| b --> c").unwrap_err();
        assert!(error.to_string().contains("final edge may have a label"));
        let error = from_mermaid("flowchart LR\n  a --> b -.-> c").unwrap_err();
        assert!(error.to_string().contains("mixed arrow styles"));
    }

    #[test]
    fn rejects_mermaid_trailing_syntax() {
        let error = from_mermaid("flowchart LR\n  a --> b ; ignored?").unwrap_err();
        assert!(error.to_string().contains("Mermaid parse error"));
    }
}
