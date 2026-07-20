use super::ast::{Arrow, Direction, EdgeDecl, Graph, NodeDecl, Shape, Stmt};
use super::{Error, Result};
use winnow::combinator::{alt, opt, repeat};
use winnow::error::{ContextError, ModalResult};
use winnow::prelude::*;
use winnow::token::{one_of, take_till, take_while};

pub(super) fn parse(source: &str) -> Result<Graph> {
    graph
        .parse(source)
        .map_err(|error| Error::mml_offset(source, error.offset(), "invalid MML syntax"))
}

type ParseResult<T> = ModalResult<T, ContextError>;

fn graph(input: &mut &str) -> ParseResult<Graph> {
    trivia.parse_next(input)?;
    '('.parse_next(input)?;
    trivia.parse_next(input)?;
    graph_keyword.parse_next(input)?;
    trivia.parse_next(input)?;
    let direction = direction.parse_next(input)?;
    trivia.parse_next(input)?;
    let stmts = repeat(0.., statement)
        .fold(Vec::new, |mut all, mut stmts| {
            all.append(&mut stmts);
            all
        })
        .parse_next(input)?;
    ')'.parse_next(input)?;
    trivia.parse_next(input)?;
    Ok(Graph { direction, stmts })
}

fn statement(input: &mut &str) -> ParseResult<Vec<Stmt>> {
    alt((edge, node.map(|node| vec![Stmt::Node(node)]))).parse_next(input)
}

fn node(input: &mut &str) -> ParseResult<NodeDecl> {
    '('.parse_next(input)?;
    trivia.parse_next(input)?;
    let id = identifier.parse_next(input)?;
    trivia.parse_next(input)?;
    let label = opt(string).parse_next(input)?;
    trivia.parse_next(input)?;
    let shape = opt(shape).parse_next(input)?.unwrap_or_default();
    trivia.parse_next(input)?;
    ')'.parse_next(input)?;
    trivia.parse_next(input)?;
    Ok(NodeDecl { id, label, shape })
}

fn edge(input: &mut &str) -> ParseResult<Vec<Stmt>> {
    '('.parse_next(input)?;
    trivia.parse_next(input)?;
    let arrow = arrow.parse_next(input)?;
    trivia.parse_next(input)?;
    let endpoints = repeat(0.., endpoint)
        .fold(Vec::new, |mut endpoints, endpoint| {
            endpoints.push(endpoint);
            endpoints
        })
        .parse_next(input)?;
    if endpoints.len() < 2 {
        return Err(winnow::error::ErrMode::Cut(ContextError::new()));
    }
    let label = opt(string).parse_next(input)?;
    trivia.parse_next(input)?;
    ')'.parse_next(input)?;
    trivia.parse_next(input)?;

    let mut stmts = Vec::new();
    let mut chain = Vec::with_capacity(endpoints.len());
    for endpoint in endpoints {
        match endpoint {
            Endpoint::Id(id) => chain.push(id),
            Endpoint::Node(node) => {
                chain.push(node.id.clone());
                stmts.push(Stmt::Node(node));
            }
        }
    }
    stmts.push(Stmt::Edge(EdgeDecl {
        chain,
        label,
        arrow,
    }));
    Ok(stmts)
}

enum Endpoint {
    Id(String),
    Node(NodeDecl),
}

fn endpoint(input: &mut &str) -> ParseResult<Endpoint> {
    let endpoint =
        alt((node.map(Endpoint::Node), identifier.map(Endpoint::Id))).parse_next(input)?;
    trivia.parse_next(input)?;
    Ok(endpoint)
}

fn direction(input: &mut &str) -> ParseResult<Direction> {
    alt((
        "lr".value(Direction::LR),
        "rl".value(Direction::RL),
        "td".value(Direction::TD),
        "bt".value(Direction::BT),
    ))
    .parse_next(input)
}

fn shape(input: &mut &str) -> ParseResult<Shape> {
    alt((
        "box".value(Shape::Box),
        "round".value(Shape::Round),
        "diamond".value(Shape::Diamond),
        "stadium".value(Shape::Stadium),
        "hex".value(Shape::Hex),
        "sub".value(Shape::Sub),
    ))
    .parse_next(input)
}

fn arrow(input: &mut &str) -> ParseResult<Arrow> {
    alt((
        "-->".value(Arrow::Dotted),
        "->".value(Arrow::Normal),
        "=>".value(Arrow::Thick),
        "-o".value(Arrow::Circle),
        "-x".value(Arrow::Cross),
    ))
    .parse_next(input)
}

fn identifier(input: &mut &str) -> ParseResult<String> {
    let first = one_of(is_ident_start).parse_next(input)?;
    let rest: &str = take_while(0.., is_ident_continue).parse_next(input)?;
    Ok(format!("{first}{rest}"))
}

fn graph_keyword(input: &mut &str) -> ParseResult<()> {
    identifier
        .verify(|actual: &String| actual == "graph")
        .void()
        .parse_next(input)
}

fn string(input: &mut &str) -> ParseResult<String> {
    '"'.parse_next(input)?;
    let value = repeat(0.., string_fragment)
        .fold(String::new, |mut value, fragment| {
            value.push_str(&fragment);
            value
        })
        .parse_next(input)?;
    '"'.parse_next(input)?;
    Ok(value)
}

fn string_fragment(input: &mut &str) -> ParseResult<String> {
    alt((
        take_till(1.., ['"', '\\']).map(str::to_owned),
        (
            '\\',
            alt((
                '"'.value("\""),
                '\\'.value("\\"),
                'n'.value("\n"),
                't'.value("\t"),
            )),
        )
            .map(|(_, value)| value.to_owned()),
    ))
    .parse_next(input)
}

fn trivia(input: &mut &str) -> ParseResult<()> {
    repeat(
        0..,
        alt((
            take_while(1.., char::is_whitespace).void(),
            (';', take_till(0.., '\n'), opt('\n')).void(),
        )),
    )
    .fold(|| (), |_, _| ())
    .parse_next(input)
}

fn is_ident_start(character: char) -> bool {
    character.is_ascii_alphabetic() || character == '_'
}

fn is_ident_continue(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
}
