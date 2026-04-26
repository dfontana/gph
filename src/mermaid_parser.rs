use crate::ast::*;

struct Hop {
    from: String,
    arrow: Arrow,
    to: String,
    label: Option<String>,
}

pub fn parse(src: &str) -> Result<Graph, String> {
    let mut lines = src.lines();

    let direction = loop {
        match lines.next() {
            None => return Err("empty input".to_string()),
            Some(l) => {
                let t = l.trim();
                if !t.is_empty() {
                    break parse_direction(t)?;
                }
            }
        }
    };

    let mut stmts: Vec<Stmt> = Vec::new();
    // Pending: an unlabeled chain that may be extended by a subsequent labeled hop.
    let mut pending: Option<(Vec<String>, Arrow)> = None;

    for raw in lines {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        let bytes = line.as_bytes();

        if let Some(node) = try_parse_node_decl(bytes) {
            flush_pending(&mut pending, &mut stmts);
            stmts.push(Stmt::Node(node));
            continue;
        }

        if let Some(hops) = try_parse_edge_line(bytes) {
            if hops.is_empty() {
                continue;
            }

            // A single labeled hop may be the labeled tail of a pending chain.
            // (gph codegen splits labeled chains with n>2 into an unlabeled
            // prefix line followed by a single labeled hop line.)
            if hops.len() == 1 && hops[0].label.is_some() {
                let h = &hops[0];
                let can_extend = pending.as_ref().is_some_and(|(chain, arr)| {
                    *arr == h.arrow && chain.last().is_some_and(|s| s == &h.from)
                });
                if can_extend {
                    let (mut chain, arr) = pending.take().unwrap();
                    chain.push(h.to.clone());
                    stmts.push(Stmt::Edge(EdgeDecl {
                        chain,
                        arrow: arr,
                        label: h.label.clone(),
                    }));
                } else {
                    flush_pending(&mut pending, &mut stmts);
                    stmts.push(Stmt::Edge(EdgeDecl {
                        chain: vec![h.from.clone(), h.to.clone()],
                        arrow: h.arrow.clone(),
                        label: h.label.clone(),
                    }));
                }
                continue;
            }

            // Multi-hop or single unlabeled hop: flush pending and start fresh.
            // gph emits unlabeled chains entirely on one line, so a new unlabeled
            // line always begins a new chain rather than extending the previous one.
            flush_pending(&mut pending, &mut stmts);

            let arrow = hops[0].arrow.clone();
            let mut chain = vec![hops[0].from.clone()];
            for h in &hops {
                chain.push(h.to.clone());
            }
            // Last hop may carry a label (hand-written mermaid edge case).
            let label = hops.last().and_then(|h| h.label.clone());
            if let Some(lbl) = label {
                stmts.push(Stmt::Edge(EdgeDecl {
                    chain,
                    arrow,
                    label: Some(lbl),
                }));
            } else {
                pending = Some((chain, arrow));
            }
        }
        // Lines that are neither node decls nor edge lines are silently skipped.
    }

    flush_pending(&mut pending, &mut stmts);
    Ok(Graph { direction, stmts })
}

fn flush_pending(pending: &mut Option<(Vec<String>, Arrow)>, stmts: &mut Vec<Stmt>) {
    if let Some((chain, arrow)) = pending.take() {
        stmts.push(Stmt::Edge(EdgeDecl {
            chain,
            arrow,
            label: None,
        }));
    }
}

fn parse_direction(header: &str) -> Result<Direction, String> {
    let mut parts = header.split_ascii_whitespace();
    if parts.next() != Some("flowchart") {
        return Err(format!("expected 'flowchart <DIR>', got {:?}", header));
    }
    match parts.next() {
        Some("LR") => Ok(Direction::LR),
        Some("RL") => Ok(Direction::RL),
        Some("TD") | Some("TB") => Ok(Direction::TD),
        Some("BT") => Ok(Direction::BT),
        Some(d) => Err(format!("unknown flowchart direction {:?}", d)),
        None => Err("missing direction after 'flowchart'".to_string()),
    }
}

fn try_parse_node_decl(bytes: &[u8]) -> Option<NodeDecl> {
    let mut pos = 0;
    let id = eat_id(bytes, &mut pos)?;
    if pos >= bytes.len() {
        return None;
    }
    // Node decls have the shape delimiter immediately after the id (no space).
    match bytes[pos] {
        b'[' | b'(' | b'{' => {}
        _ => return None,
    }
    let rest = std::str::from_utf8(&bytes[pos..]).ok()?;

    // Match longest delimiter first to avoid ambiguity ([[  vs [, {{ vs {, ([ vs ().
    let (shape, inner) = if let Some(s) = strip_balanced(rest, "[[", "]]") {
        (Shape::Sub, s)
    } else if let Some(s) = strip_balanced(rest, "([", "])") {
        (Shape::Stadium, s)
    } else if let Some(s) = strip_balanced(rest, "{{", "}}") {
        (Shape::Hex, s)
    } else if let Some(s) = strip_balanced(rest, "[", "]") {
        (Shape::Box, s)
    } else if let Some(s) = strip_balanced(rest, "(", ")") {
        (Shape::Round, s)
    } else if let Some(s) = strip_balanced(rest, "{", "}") {
        (Shape::Diamond, s)
    } else {
        return None;
    };

    let raw = inner.strip_prefix('"')?.strip_suffix('"')?;
    Some(NodeDecl {
        id,
        label: Some(unescape_node_label(raw)),
        shape,
    })
}

fn strip_balanced<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    s.strip_prefix(open)?.strip_suffix(close)
}

fn try_parse_edge_line(bytes: &[u8]) -> Option<Vec<Hop>> {
    let mut pos = 0;
    let from = eat_id(bytes, &mut pos)?;
    eat_ws(bytes, &mut pos);
    let (arrow, label) = eat_arrow(bytes, &mut pos)?;
    eat_ws(bytes, &mut pos);
    let to = eat_id(bytes, &mut pos)?;

    let mut hops = vec![Hop {
        from: from.clone(),
        arrow: arrow.clone(),
        to: to.clone(),
        label,
    }];
    let mut last = to;

    loop {
        eat_ws(bytes, &mut pos);
        if pos >= bytes.len() {
            break;
        }
        let (arr, lbl) = match eat_arrow(bytes, &mut pos) {
            Some(x) => x,
            None => break,
        };
        eat_ws(bytes, &mut pos);
        let next = match eat_id(bytes, &mut pos) {
            Some(id) => id,
            None => break,
        };
        hops.push(Hop {
            from: last.clone(),
            arrow: arr,
            to: next.clone(),
            label: lbl,
        });
        last = next;
    }

    Some(hops)
}

fn eat_id(bytes: &[u8], pos: &mut usize) -> Option<String> {
    let start = *pos;
    if *pos >= bytes.len() || (!bytes[*pos].is_ascii_alphanumeric() && bytes[*pos] != b'_') {
        return None;
    }
    while *pos < bytes.len()
        && (bytes[*pos].is_ascii_alphanumeric() || bytes[*pos] == b'_' || bytes[*pos] == b'-')
    {
        *pos += 1;
    }
    Some(String::from_utf8_lossy(&bytes[start..*pos]).into_owned())
}

fn eat_ws(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() && (bytes[*pos] == b' ' || bytes[*pos] == b'\t') {
        *pos += 1;
    }
}

fn eat_arrow(bytes: &[u8], pos: &mut usize) -> Option<(Arrow, Option<String>)> {
    let rest = &bytes[*pos..];
    let (arrow, len) = if rest.starts_with(b"==>") {
        (Arrow::Thick, 3)
    } else if rest.starts_with(b"-.->") {
        (Arrow::Dotted, 4)
    } else if rest.starts_with(b"-->") {
        (Arrow::Normal, 3)
    } else if rest.starts_with(b"--o") {
        (Arrow::Circle, 3)
    } else if rest.starts_with(b"--x") {
        (Arrow::Cross, 3)
    } else {
        return None;
    };
    *pos += len;

    let label = if *pos < bytes.len() && bytes[*pos] == b'|' {
        *pos += 1;
        let start = *pos;
        while *pos < bytes.len() && bytes[*pos] != b'|' {
            *pos += 1;
        }
        let s = String::from_utf8_lossy(&bytes[start..*pos]).into_owned();
        if *pos < bytes.len() {
            *pos += 1; // closing '|'
        }
        Some(unescape_edge_label(&s))
    } else {
        None
    };

    Some((arrow, label))
}

fn unescape_node_label(s: &str) -> String {
    s.replace("#quot;", "\"")
}

fn unescape_edge_label(s: &str) -> String {
    s.replace("#124;", "|").replace("#quot;", "\"")
}
