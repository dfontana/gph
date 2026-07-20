use super::ast::{Arrow, Direction, EdgeDecl, Graph, NodeDecl, Shape, Stmt};

pub(super) fn generate(graph: &Graph) -> String {
    let mut lines = vec![format!("flowchart {}", direction_name(graph.direction))];
    for statement in &graph.stmts {
        match statement {
            Stmt::Node(node) => {
                if let Some(line) = generate_node(node) {
                    lines.push(line);
                }
            }
            Stmt::Edge(edge) => lines.extend(generate_edge(edge)),
        }
    }
    lines.join("\n")
}

fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::LR => "LR",
        Direction::RL => "RL",
        Direction::TD => "TD",
        Direction::BT => "BT",
    }
}

fn generate_node(node: &NodeDecl) -> Option<String> {
    let label = node.label.as_ref()?;
    Some(format!("  {}", wrap_label(&node.id, label, node.shape)))
}

fn wrap_label(id: &str, label: &str, shape: Shape) -> String {
    let label = label.replace('"', "#quot;");
    match shape {
        Shape::Box => format!("{id}[\"{label}\"]"),
        Shape::Round => format!("{id}(\"{label}\")"),
        Shape::Diamond => format!("{id}{{\"{label}\"}}"),
        Shape::Stadium => format!("{id}([\"{label}\"])"),
        Shape::Hex => format!("{id}{{{{\"{label}\"}}}}"),
        Shape::Sub => format!("{id}[[\"{label}\"]]"),
    }
}

fn generate_edge(edge: &EdgeDecl) -> Vec<String> {
    debug_assert!(edge.chain.len() >= 2, "MML edges always contain two nodes");
    let arrow = arrow_name(edge.arrow);
    match &edge.label {
        None => vec![format!("  {}", edge.chain.join(&format!(" {arrow} ")))],
        Some(label) => {
            let last = edge.chain.len() - 1;
            let mut lines = Vec::new();
            if last > 1 {
                lines.push(format!(
                    "  {}",
                    edge.chain[..last].join(&format!(" {arrow} "))
                ));
            }
            lines.push(format!(
                "  {} {arrow}|{}| {}",
                edge.chain[last - 1],
                label.replace('|', "#124;").replace('"', "#quot;"),
                edge.chain[last]
            ));
            lines
        }
    }
}

fn arrow_name(arrow: Arrow) -> &'static str {
    match arrow {
        Arrow::Normal => "-->",
        Arrow::Dotted => "-.->",
        Arrow::Thick => "==>",
        Arrow::Circle => "--o",
        Arrow::Cross => "--x",
    }
}

pub(super) fn print(graph: &Graph) -> String {
    let direction = match graph.direction {
        Direction::LR => "lr",
        Direction::RL => "rl",
        Direction::TD => "td",
        Direction::BT => "bt",
    };
    if graph.stmts.is_empty() {
        return format!("(graph {direction})");
    }
    let statements = graph.stmts.iter().map(print_stmt).collect::<Vec<_>>();
    format!("(graph {direction}\n  {})", statements.join("\n  "))
}

fn print_stmt(statement: &Stmt) -> String {
    match statement {
        Stmt::Node(node) => print_node(node),
        Stmt::Edge(edge) => print_edge(edge),
    }
}

fn print_node(node: &NodeDecl) -> String {
    let shape = match node.shape {
        Shape::Box => "",
        Shape::Round => " round",
        Shape::Diamond => " diamond",
        Shape::Stadium => " stadium",
        Shape::Hex => " hex",
        Shape::Sub => " sub",
    };
    match &node.label {
        Some(label) => format!("({} \"{}\"{shape})", node.id, escape(label)),
        None => format!("({})", node.id),
    }
}

fn print_edge(edge: &EdgeDecl) -> String {
    let arrow = match edge.arrow {
        Arrow::Normal => "->",
        Arrow::Dotted => "-->",
        Arrow::Thick => "=>",
        Arrow::Circle => "-o",
        Arrow::Cross => "-x",
    };
    let chain = edge.chain.join(" ");
    match &edge.label {
        Some(label) => format!("({arrow} {chain} \"{}\")", escape(label)),
        None => format!("({arrow} {chain})"),
    }
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}
