/// The direction in which Mermaid lays out a flowchart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    LR,
    RL,
    TD,
    BT,
}

/// A node's Mermaid shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shape {
    #[default]
    Box,
    Round,
    Diamond,
    Stadium,
    Hex,
    Sub,
}

/// An edge's Mermaid arrow style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Arrow {
    #[default]
    Normal,
    Dotted,
    Thick,
    Circle,
    Cross,
}

/// An optionally labelled node declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeDecl {
    pub id: String,
    pub label: Option<String>,
    pub shape: Shape,
}

/// A same-style chain of two or more edges.
///
/// When `label` is present, it applies only to the final hop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeDecl {
    pub chain: Vec<String>,
    pub label: Option<String>,
    pub arrow: Arrow,
}

/// A graph statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Node(NodeDecl),
    Edge(EdgeDecl),
}

/// The parsed MML graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graph {
    pub direction: Direction,
    pub stmts: Vec<Stmt>,
}
