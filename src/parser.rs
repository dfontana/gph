use crate::ast::*;
use crate::lexer::{Token, TokenKind};

#[derive(Debug)]
pub struct ParseError {
    pub msg: String,
    pub line: usize,
    pub col: usize,
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn peek_kind(&self) -> Option<&TokenKind> {
        self.peek().map(|t| &t.kind)
    }

    fn advance(&mut self) -> Option<&Token> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn current_pos(&self) -> (usize, usize) {
        match self.peek() {
            Some(t) => (t.line, t.col),
            None => {
                // Use position of last token
                self.tokens
                    .last()
                    .map(|t| (t.line, t.col))
                    .unwrap_or((1, 1))
            }
        }
    }

    fn expect_lparen(&mut self) -> Result<(), ParseError> {
        match self.peek_kind() {
            Some(TokenKind::LParen) => {
                self.advance();
                Ok(())
            }
            _ => {
                let (l, c) = self.current_pos();
                Err(ParseError {
                    msg: "expected '('".into(),
                    line: l,
                    col: c,
                })
            }
        }
    }

    fn expect_rparen(&mut self) -> Result<(), ParseError> {
        match self.peek_kind() {
            Some(TokenKind::RParen) => {
                self.advance();
                Ok(())
            }
            _ => {
                let (l, c) = self.current_pos();
                Err(ParseError {
                    msg: "expected closing ')'".into(),
                    line: l,
                    col: c,
                })
            }
        }
    }

    fn expect_ident(&mut self) -> Result<String, ParseError> {
        match self.peek_kind() {
            Some(TokenKind::Ident(_)) => {
                if let Some(t) = self.advance() {
                    if let TokenKind::Ident(s) = &t.kind {
                        return Ok(s.clone());
                    }
                }
                unreachable!()
            }
            _ => {
                let (l, c) = self.current_pos();
                Err(ParseError {
                    msg: "expected identifier".into(),
                    line: l,
                    col: c,
                })
            }
        }
    }

    fn parse_graph(&mut self) -> Result<Graph, ParseError> {
        self.expect_lparen()?;

        // Expect keyword 'graph'
        let (kw_line, kw_col) = self.current_pos();
        let kw = self.expect_ident()?;
        if kw != "graph" {
            return Err(ParseError {
                msg: format!("expected 'graph', found '{}'", kw),
                line: kw_line,
                col: kw_col,
            });
        }

        // Direction
        let (dir_line, dir_col) = self.current_pos();
        let dir_tok = self.expect_ident()?;
        let direction = parse_direction(&dir_tok).ok_or_else(|| ParseError {
            msg: format!(
                "unknown direction '{}'; expected lr, rl, td, or bt",
                dir_tok
            ),
            line: dir_line,
            col: dir_col,
        })?;

        // Statements until closing paren
        let mut stmts = Vec::new();
        loop {
            match self.peek_kind() {
                Some(TokenKind::RParen) => {
                    self.advance();
                    break;
                }
                Some(TokenKind::LParen) => stmts.push(self.parse_stmt()?),
                None => {
                    let (l, c) = self.current_pos();
                    return Err(ParseError {
                        msg: "unexpected end of input; expected closing ')'".into(),
                        line: l,
                        col: c,
                    });
                }
                _ => {
                    let (l, c) = self.current_pos();
                    return Err(ParseError {
                        msg: "expected '(' or ')'".into(),
                        line: l,
                        col: c,
                    });
                }
            }
        }

        Ok(Graph { direction, stmts })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.expect_lparen()?;

        match self.peek_kind() {
            Some(TokenKind::Arrow(_)) => self.parse_edge(),
            Some(TokenKind::Ident(_)) => self.parse_node(),
            _ => {
                let (l, c) = self.current_pos();
                Err(ParseError {
                    msg: "expected arrow or identifier after '('".into(),
                    line: l,
                    col: c,
                })
            }
        }
    }

    fn parse_edge(&mut self) -> Result<Stmt, ParseError> {
        let (arr_line, arr_col) = self.current_pos();
        let arrow_str = match self.advance() {
            Some(Token {
                kind: TokenKind::Arrow(s),
                ..
            }) => s.clone(),
            _ => unreachable!(),
        };
        let arrow = parse_arrow(&arrow_str).ok_or_else(|| ParseError {
            msg: format!("unknown arrow '{}'", arrow_str),
            line: arr_line,
            col: arr_col,
        })?;

        // Collect identifiers
        let mut chain = Vec::new();
        while let Some(TokenKind::Ident(_)) = self.peek_kind() {
            chain.push(self.expect_ident()?);
        }

        if chain.len() < 2 {
            return Err(ParseError {
                msg: format!("edge requires at least two nodes, got {}", chain.len()),
                line: arr_line,
                col: arr_col,
            });
        }

        // Optional label
        let label = match self.peek_kind() {
            Some(TokenKind::Str(_)) => {
                if let Some(t) = self.advance() {
                    if let TokenKind::Str(s) = &t.kind {
                        Some(s.clone())
                    } else {
                        unreachable!()
                    }
                } else {
                    unreachable!()
                }
            }
            _ => None,
        };

        self.expect_rparen()?;
        Ok(Stmt::Edge(EdgeDecl {
            chain,
            label,
            arrow,
        }))
    }

    fn parse_node(&mut self) -> Result<Stmt, ParseError> {
        let id = self.expect_ident()?;

        // Optional label
        let label = match self.peek_kind() {
            Some(TokenKind::Str(_)) => {
                if let Some(t) = self.advance() {
                    if let TokenKind::Str(s) = &t.kind {
                        Some(s.clone())
                    } else {
                        unreachable!()
                    }
                } else {
                    unreachable!()
                }
            }
            _ => None,
        };

        // Optional shape (only if label was present)
        let shape = match self.peek_kind() {
            Some(TokenKind::Ident(s)) => {
                let s = s.clone();
                if let Some(sh) = parse_shape(&s) {
                    self.advance();
                    sh
                } else {
                    // Not a shape keyword — leave it, let rparen handle it
                    Shape::default()
                }
            }
            _ => Shape::default(),
        };

        self.expect_rparen()?;
        Ok(Stmt::Node(NodeDecl { id, label, shape }))
    }
}

fn parse_direction(s: &str) -> Option<Direction> {
    match s {
        "lr" => Some(Direction::LR),
        "rl" => Some(Direction::RL),
        "td" => Some(Direction::TD),
        "bt" => Some(Direction::BT),
        _ => None,
    }
}

fn parse_shape(s: &str) -> Option<Shape> {
    match s {
        "box" => Some(Shape::Box),
        "round" => Some(Shape::Round),
        "diamond" => Some(Shape::Diamond),
        "stadium" => Some(Shape::Stadium),
        "hex" => Some(Shape::Hex),
        "sub" => Some(Shape::Sub),
        _ => None,
    }
}

fn parse_arrow(s: &str) -> Option<Arrow> {
    match s {
        "->" => Some(Arrow::Normal),
        "-->" => Some(Arrow::Dotted),
        "=>" => Some(Arrow::Thick),
        "-o" => Some(Arrow::Circle),
        "-x" => Some(Arrow::Cross),
        _ => None,
    }
}

pub fn parse(tokens: &[Token]) -> Result<Graph, ParseError> {
    let mut parser = Parser::new(tokens);
    let graph = parser.parse_graph()?;

    // Ensure no trailing tokens
    if parser.peek().is_some() {
        let (l, c) = parser.current_pos();
        return Err(ParseError {
            msg: "unexpected tokens after graph expression".into(),
            line: l,
            col: c,
        });
    }

    Ok(graph)
}
