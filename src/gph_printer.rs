use crate::ast::*;

pub fn print(graph: &Graph) -> String {
    let dir = match graph.direction {
        Direction::LR => "lr",
        Direction::RL => "rl",
        Direction::TD => "td",
        Direction::BT => "bt",
    };

    if graph.stmts.is_empty() {
        return format!("(graph {})", dir);
    }

    let stmts: Vec<String> = graph.stmts.iter().map(print_stmt).collect();
    format!("(graph {}\n  {})", dir, stmts.join("\n  "))
}

fn print_stmt(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Node(n) => print_node(n),
        Stmt::Edge(e) => print_edge(e),
    }
}

fn print_node(n: &NodeDecl) -> String {
    let shape_suffix = match n.shape {
        Shape::Box => "",
        Shape::Round => " round",
        Shape::Diamond => " diamond",
        Shape::Stadium => " stadium",
        Shape::Hex => " hex",
        Shape::Sub => " sub",
    };
    match &n.label {
        Some(lbl) => format!("({} \"{}\"{})", n.id, escape(lbl), shape_suffix),
        None => format!("({})", n.id),
    }
}

fn print_edge(e: &EdgeDecl) -> String {
    let arrow = match e.arrow {
        Arrow::Normal => "->",
        Arrow::Dotted => "-->",
        Arrow::Thick => "=>",
        Arrow::Circle => "-o",
        Arrow::Cross => "-x",
    };
    let chain = e.chain.join(" ");
    match &e.label {
        Some(lbl) => format!("({} {} \"{}\")", arrow, chain, escape(lbl)),
        None => format!("({} {})", arrow, chain),
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
