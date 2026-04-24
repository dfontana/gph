#[derive(Debug, Clone, PartialEq)]
pub enum Direction {
    LR,
    RL,
    TD,
    BT,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Shape {
    #[default]
    Box,
    Round,
    Diamond,
    Stadium,
    Hex,
    Sub,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Arrow {
    #[default]
    Normal,
    Dotted,
    Thick,
    Circle,
    Cross,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeDecl {
    pub id: String,
    pub label: Option<String>,
    pub shape: Shape,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EdgeDecl {
    pub chain: Vec<String>,
    pub label: Option<String>,
    pub arrow: Arrow,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Node(NodeDecl),
    Edge(EdgeDecl),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Graph {
    pub direction: Direction,
    pub stmts: Vec<Stmt>,
}
