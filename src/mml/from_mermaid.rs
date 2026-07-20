use super::ast::{Arrow, Direction, EdgeDecl, Graph, NodeDecl, Shape, Stmt};
use super::{Error, Result};
use winnow::combinator::{alt, delimited, opt, repeat};
use winnow::error::{ContextError, ModalResult};
use winnow::prelude::*;
use winnow::token::{one_of, take_till, take_while};

struct Hop {
    from: String,
    arrow: Arrow,
    to: String,
    label: Option<String>,
}

pub(super) fn parse(source: &str) -> Result<Graph> {
    let mut lines = source.lines().enumerate().filter_map(|(index, line)| {
        let line = line.trim();
        (!line.is_empty()).then_some((index + 1, line))
    });
    let Some((header_line, header)) = lines.next() else {
        return Err(Error::mermaid(1, "empty input"));
    };
    let direction = parse_line(header_direction, header)
        .map_err(|message| Error::mermaid(header_line, message))?;

    let mut statements = Vec::new();
    let mut pending = None;
    for (line_number, line) in lines {
        if line.starts_with("%%") {
            continue;
        }
        if let Ok(node) = parse_line(node_line, line) {
            flush_pending(&mut pending, &mut statements);
            statements.push(Stmt::Node(node));
            continue;
        }

        let hops =
            parse_line(edge_line, line).map_err(|message| Error::mermaid(line_number, message))?;
        if hops.len() == 1 && hops[0].label.is_some() {
            let hop = &hops[0];
            if pending
                .as_ref()
                .is_some_and(|(chain, arrow): &(Vec<String>, Arrow)| {
                    *arrow == hop.arrow && chain.last() == Some(&hop.from)
                })
            {
                let (mut chain, arrow) = pending.take().expect("checked above");
                chain.push(hop.to.clone());
                statements.push(Stmt::Edge(EdgeDecl {
                    chain,
                    arrow,
                    label: hop.label.clone(),
                }));
            } else {
                flush_pending(&mut pending, &mut statements);
                statements.push(Stmt::Edge(EdgeDecl {
                    chain: vec![hop.from.clone(), hop.to.clone()],
                    arrow: hop.arrow,
                    label: hop.label.clone(),
                }));
            }
            continue;
        }

        flush_pending(&mut pending, &mut statements);
        let arrow = hops[0].arrow;
        if hops.iter().any(|hop| hop.arrow != arrow) {
            return Err(Error::mermaid(
                line_number,
                "mixed arrow styles cannot be represented by MML",
            ));
        }
        if hops[..hops.len() - 1].iter().any(|hop| hop.label.is_some()) {
            return Err(Error::mermaid(
                line_number,
                "only a chain's final edge may have a label in MML",
            ));
        }
        let mut chain = vec![hops[0].from.clone()];
        chain.extend(hops.iter().map(|hop| hop.to.clone()));
        if let Some(label) = hops.last().and_then(|hop| hop.label.clone()) {
            statements.push(Stmt::Edge(EdgeDecl {
                chain,
                arrow,
                label: Some(label),
            }));
        } else {
            pending = Some((chain, arrow));
        }
    }
    flush_pending(&mut pending, &mut statements);
    Ok(Graph {
        direction,
        stmts: statements,
    })
}

fn parse_line<'a, O>(
    mut parser: impl Parser<&'a str, O, winnow::error::ErrMode<ContextError>>,
    line: &'a str,
) -> std::result::Result<O, String> {
    parser
        .parse(line)
        .map_err(|_| "invalid or unsupported Mermaid syntax".into())
}

fn flush_pending(pending: &mut Option<(Vec<String>, Arrow)>, statements: &mut Vec<Stmt>) {
    if let Some((chain, arrow)) = pending.take() {
        statements.push(Stmt::Edge(EdgeDecl {
            chain,
            label: None,
            arrow,
        }));
    }
}

type ParseResult<T> = ModalResult<T, ContextError>;

fn header_direction(input: &mut &str) -> ParseResult<Direction> {
    "flowchart".parse_next(input)?;
    horizontal1.parse_next(input)?;
    let direction = alt((
        "LR".value(Direction::LR),
        "RL".value(Direction::RL),
        "TD".value(Direction::TD),
        "TB".value(Direction::TD),
        "BT".value(Direction::BT),
    ))
    .parse_next(input)?;
    horizontal0.parse_next(input)?;
    Ok(direction)
}

fn node_line(input: &mut &str) -> ParseResult<NodeDecl> {
    let id = identifier.parse_next(input)?;
    let (shape, label) = alt((
        delimited("[[", label, "]]").map(|label| (Shape::Sub, label)),
        delimited("([", label, "])").map(|label| (Shape::Stadium, label)),
        delimited("{{", label, "}}").map(|label| (Shape::Hex, label)),
        delimited('[', label, ']').map(|label| (Shape::Box, label)),
        delimited('(', label, ')').map(|label| (Shape::Round, label)),
        delimited('{', label, '}').map(|label| (Shape::Diamond, label)),
    ))
    .parse_next(input)?;
    horizontal0.parse_next(input)?;
    Ok(NodeDecl {
        id,
        label: Some(label),
        shape,
    })
}

fn edge_line(input: &mut &str) -> ParseResult<Vec<Hop>> {
    let from = identifier.parse_next(input)?;
    horizontal0.parse_next(input)?;
    let (arrow, label) = arrow.parse_next(input)?;
    horizontal0.parse_next(input)?;
    let to = identifier.parse_next(input)?;
    let mut hops = vec![Hop {
        from: from.clone(),
        arrow,
        to: to.clone(),
        label,
    }];
    let rest = repeat(0.., edge_tail)
        .fold(Vec::new, |mut tails, tail| {
            tails.push(tail);
            tails
        })
        .parse_next(input)?;
    let mut from = to;
    for (arrow, label, to) in rest {
        hops.push(Hop {
            from: from.clone(),
            arrow,
            to: to.clone(),
            label,
        });
        from = to;
    }
    Ok(hops)
}

fn edge_tail(input: &mut &str) -> ParseResult<(Arrow, Option<String>, String)> {
    horizontal0.parse_next(input)?;
    let (arrow, label) = arrow.parse_next(input)?;
    horizontal0.parse_next(input)?;
    let to = identifier.parse_next(input)?;
    Ok((arrow, label, to))
}

fn identifier(input: &mut &str) -> ParseResult<String> {
    let first = one_of(is_ident_start).parse_next(input)?;
    let rest: &str = take_while(0.., is_ident_continue).parse_next(input)?;
    Ok(format!("{first}{rest}"))
}

fn label(input: &mut &str) -> ParseResult<String> {
    delimited('"', take_till(0.., '"'), '"')
        .map(|label: &str| label.replace("#quot;", "\""))
        .parse_next(input)
}

fn arrow(input: &mut &str) -> ParseResult<(Arrow, Option<String>)> {
    let arrow = alt((
        "-.->".value(Arrow::Dotted),
        "==>".value(Arrow::Thick),
        "-->".value(Arrow::Normal),
        "--o".value(Arrow::Circle),
        "--x".value(Arrow::Cross),
    ))
    .parse_next(input)?;
    let label = opt(delimited('|', take_till(0.., '|'), '|'))
        .map(|label: Option<&str>| {
            label.map(|label| label.replace("#124;", "|").replace("#quot;", "\""))
        })
        .parse_next(input)?;
    Ok((arrow, label))
}

fn horizontal0(input: &mut &str) -> ParseResult<()> {
    take_while(0.., [' ', '\t']).void().parse_next(input)
}

fn horizontal1(input: &mut &str) -> ParseResult<()> {
    take_while(1.., [' ', '\t']).void().parse_next(input)
}

fn is_ident_start(character: char) -> bool {
    character.is_ascii_alphabetic() || character == '_'
}

fn is_ident_continue(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
}
