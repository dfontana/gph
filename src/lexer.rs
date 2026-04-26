#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    LParen,
    RParen,
    Arrow(String),
    Ident(String),
    Str(String),
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug)]
pub struct LexError {
    pub msg: String,
    pub line: usize,
    pub col: usize,
}

pub fn lex(src: &str) -> Result<Vec<Token>, LexError> {
    let chars: Vec<char> = src.chars().collect();
    let mut pos = 0;
    let mut line = 1usize;
    let mut col = 1usize;
    let mut tokens = Vec::new();

    while pos < chars.len() {
        let c = chars[pos];

        // Whitespace
        if c.is_whitespace() {
            if c == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
            pos += 1;
            continue;
        }

        // Comment
        if c == ';' {
            while pos < chars.len() && chars[pos] != '\n' {
                pos += 1;
            }
            continue;
        }

        let tok_line = line;
        let tok_col = col;

        match c {
            '(' => {
                tokens.push(Token {
                    kind: TokenKind::LParen,
                    line: tok_line,
                    col: tok_col,
                });
                pos += 1;
                col += 1;
            }
            ')' => {
                tokens.push(Token {
                    kind: TokenKind::RParen,
                    line: tok_line,
                    col: tok_col,
                });
                pos += 1;
                col += 1;
            }
            '"' => {
                pos += 1;
                col += 1;
                let mut s = String::new();
                loop {
                    if pos >= chars.len() {
                        return Err(LexError {
                            msg: "unterminated string literal".into(),
                            line: tok_line,
                            col: tok_col,
                        });
                    }
                    match chars[pos] {
                        '"' => {
                            pos += 1;
                            col += 1;
                            break;
                        }
                        '\\' => {
                            pos += 1;
                            col += 1;
                            if pos >= chars.len() {
                                return Err(LexError {
                                    msg: "unexpected end of escape sequence".into(),
                                    line,
                                    col,
                                });
                            }
                            match chars[pos] {
                                '"' => s.push('"'),
                                '\\' => s.push('\\'),
                                'n' => s.push('\n'),
                                't' => s.push('\t'),
                                other => s.push(other),
                            }
                            pos += 1;
                            col += 1;
                        }
                        ch => {
                            s.push(ch);
                            if ch == '\n' {
                                line += 1;
                                col = 1;
                            } else {
                                col += 1;
                            }
                            pos += 1;
                        }
                    }
                }
                tokens.push(Token {
                    kind: TokenKind::Str(s),
                    line: tok_line,
                    col: tok_col,
                });
            }
            '-' => {
                let arrow = try_lex_arrow(&chars, pos, line, col)?;
                let len = arrow.len();
                tokens.push(Token {
                    kind: TokenKind::Arrow(arrow),
                    line: tok_line,
                    col: tok_col,
                });
                pos += len;
                col += len;
            }
            '=' => {
                if chars.get(pos + 1) == Some(&'>') {
                    tokens.push(Token {
                        kind: TokenKind::Arrow("=>".into()),
                        line: tok_line,
                        col: tok_col,
                    });
                    pos += 2;
                    col += 2;
                } else {
                    return Err(LexError {
                        msg: format!("unexpected character '{}'", c),
                        line: tok_line,
                        col: tok_col,
                    });
                }
            }
            // Identifier
            c if is_ident_start(c) => {
                let start = pos;
                while pos < chars.len() && is_ident_continue(chars[pos]) {
                    pos += 1;
                    col += 1;
                }
                let ident: String = chars[start..pos].iter().collect();
                tokens.push(Token {
                    kind: TokenKind::Ident(ident),
                    line: tok_line,
                    col: tok_col,
                });
            }
            other => {
                return Err(LexError {
                    msg: format!("unexpected character '{}'", other),
                    line: tok_line,
                    col: tok_col,
                });
            }
        }
    }

    Ok(tokens)
}

/// Try to lex an arrow token starting at `pos` (which must be `-`).
/// Returns the arrow string on success. Tries longest match first.
fn try_lex_arrow(chars: &[char], pos: usize, line: usize, col: usize) -> Result<String, LexError> {
    // Try -->  (dotted in DSL)
    if chars.get(pos + 1) == Some(&'-') && chars.get(pos + 2) == Some(&'>') {
        return Ok("-->".into());
    }
    // Try ->
    if chars.get(pos + 1) == Some(&'>') {
        return Ok("->".into());
    }
    // Try -o
    if chars.get(pos + 1) == Some(&'o') {
        return Ok("-o".into());
    }
    // Try -x
    if chars.get(pos + 1) == Some(&'x') {
        return Ok("-x".into());
    }
    Err(LexError {
        msg: "expected arrow (->, -->, =>, -o, -x)".into(),
        line,
        col,
    })
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}
