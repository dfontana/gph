use crate::ast::*;

fn escape_mermaid_node_label(s: &str) -> String {
    s.replace('"', "#quot;")
}

fn escape_mermaid_edge_label(s: &str) -> String {
    s.replace('|', "#124;").replace('"', "#quot;")
}

pub fn generate(graph: &Graph) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("flowchart {}", direction_str(&graph.direction)));

    for stmt in &graph.stmts {
        match stmt {
            Stmt::Node(n) => {
                if let Some(line) = gen_node(n) {
                    lines.push(line);
                }
            }
            Stmt::Edge(e) => {
                for line in gen_edge(e) {
                    lines.push(line);
                }
            }
        }
    }

    lines.join("\n")
}

fn direction_str(d: &Direction) -> &'static str {
    match d {
        Direction::LR => "LR",
        Direction::RL => "RL",
        Direction::TD => "TD",
        Direction::BT => "BT",
    }
}

/// Returns None for implicit nodes (no label); Some(line) for declared nodes.
fn gen_node(n: &NodeDecl) -> Option<String> {
    let label = n.label.as_ref()?;
    Some(format!("  {}", wrap_label(&n.id, label, &n.shape)))
}

fn wrap_label(id: &str, label: &str, shape: &Shape) -> String {
    let lbl = escape_mermaid_node_label(label);
    match shape {
        Shape::Box => format!("{}[\"{}\"]", id, lbl),
        Shape::Round => format!("{}(\"{}\")", id, lbl),
        Shape::Diamond => format!("{}{{\"{}\"}}", id, lbl),
        Shape::Stadium => format!("{}([\"{}\"])", id, lbl),
        Shape::Hex => format!("{}{{{{\"{}\"}}}}", id, lbl),
        Shape::Sub => format!("{}[[\"{}\"]]", id, lbl),
    }
}

fn arrow_str(a: &Arrow) -> &'static str {
    match a {
        Arrow::Normal => "-->",
        Arrow::Dotted => "-.->",
        Arrow::Thick => "==>",
        Arrow::Circle => "--o",
        Arrow::Cross => "--x",
    }
}

fn gen_edge(e: &EdgeDecl) -> Vec<String> {
    let arr = arrow_str(&e.arrow);
    match &e.label {
        None => {
            // All nodes chained on one line: a --> b --> c
            let chain = e.chain.join(&format!(" {} ", arr));
            vec![format!("  {}", chain)]
        }
        Some(lbl) => {
            let n = e.chain.len();
            let mut lines = Vec::new();

            if n > 2 {
                // Unlabeled prefix chain (all but the last hop)
                let prefix = e.chain[..n - 1].join(&format!(" {} ", arr));
                lines.push(format!("  {}", prefix));
            }

            // Last hop with label
            lines.push(format!(
                "  {} {}|{}| {}",
                e.chain[n - 2],
                arr,
                escape_mermaid_edge_label(lbl),
                e.chain[n - 1]
            ));

            lines
        }
    }
}
