use crate::ast::*;
use std::collections::HashMap;

const NODE_H: f32 = 40.0;
const H_GAP: f32 = 80.0;
const V_GAP: f32 = 24.0;
const MARGIN: f32 = 20.0;
const CHAR_W: f32 = 8.0;
const PAD: f32 = 24.0;

pub struct LayoutNode {
    pub label: String,
    pub shape: Shape,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub struct LayoutEdge {
    pub points: Vec<(f32, f32)>,
    pub label: Option<String>,
    pub label_pos: (f32, f32),
    pub arrow: Arrow,
}

pub struct Layout {
    pub nodes: Vec<LayoutNode>,
    pub edges: Vec<LayoutEdge>,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone)]
struct RawEdge {
    from: usize,
    to: usize,
    label: Option<String>,
    arrow: Arrow,
    reversed: bool,
}

#[derive(Clone)]
struct NodeEntry {
    label: String,
    shape: Shape,
    dummy: bool,
}

fn ensure_node(id: &str, node_ids: &mut Vec<String>, node_map: &mut HashMap<String, usize>) {
    if !node_map.contains_key(id) {
        let idx = node_ids.len();
        node_map.insert(id.to_string(), idx);
        node_ids.push(id.to_string());
    }
}

pub fn compute(graph: &Graph) -> Layout {
    // --- Phase 0: collect nodes and expand edge chains ---
    let mut node_ids: Vec<String> = Vec::new();
    let mut node_map: HashMap<String, usize> = HashMap::new();
    let mut labels: HashMap<String, String> = HashMap::new();
    let mut shapes: HashMap<String, Shape> = HashMap::new();

    for stmt in &graph.stmts {
        match stmt {
            Stmt::Node(n) => {
                ensure_node(&n.id, &mut node_ids, &mut node_map);
                if let Some(lbl) = &n.label {
                    labels.insert(n.id.clone(), lbl.clone());
                }
                shapes.insert(n.id.clone(), n.shape.clone());
            }
            Stmt::Edge(e) => {
                for id in &e.chain {
                    ensure_node(id, &mut node_ids, &mut node_map);
                }
            }
        }
    }

    let mut raw_edges: Vec<RawEdge> = Vec::new();
    for stmt in &graph.stmts {
        if let Stmt::Edge(e) = stmt {
            let n = e.chain.len();
            for i in 0..n - 1 {
                let lbl = if i == n - 2 { e.label.clone() } else { None };
                raw_edges.push(RawEdge {
                    from: node_map[&e.chain[i]],
                    to: node_map[&e.chain[i + 1]],
                    label: lbl,
                    arrow: e.arrow.clone(),
                    reversed: false,
                });
            }
        }
    }

    let n = node_ids.len();
    if n == 0 {
        return Layout {
            nodes: vec![],
            edges: vec![],
            width: 0.0,
            height: 0.0,
        };
    }

    // --- Phase 1: cycle removal via DFS ---
    remove_cycles(&mut raw_edges, n);

    // --- Phase 2: layer assignment (longest path) ---
    let layers = assign_layers(n, &raw_edges);
    let num_layers = layers.iter().copied().max().unwrap_or(0) + 1;

    // Build node entries
    let mut entries: Vec<NodeEntry> = node_ids
        .iter()
        .map(|id| NodeEntry {
            label: labels.get(id).cloned().unwrap_or_else(|| id.clone()),
            shape: shapes.get(id).cloned().unwrap_or_default(),
            dummy: false,
        })
        .collect();

    // --- Phase 3: dummy node insertion ---
    // Returns the full waypoint chain (node indices) for each original edge
    let mut edges_for_crossings = raw_edges.clone();
    let mut entry_layers = layers.clone();
    let chains = insert_dummies(
        &mut entries,
        &mut edges_for_crossings,
        &mut entry_layers,
        &raw_edges,
    );

    // --- Phase 4: crossing minimization ---
    let total = entries.len();
    let mut layer_nodes: Vec<Vec<usize>> = vec![vec![]; num_layers];
    for (i, &l) in entry_layers.iter().enumerate().take(total) {
        if l < num_layers {
            layer_nodes[l].push(i);
        }
    }
    minimize_crossings(&mut layer_nodes, &edges_for_crossings, 4);

    // --- Phase 5: coordinate assignment ---
    // Per-layer max width (based on widest real node in each layer)
    let node_w: Vec<f32> = entries
        .iter()
        .map(|e| {
            if e.dummy {
                0.0
            } else {
                (e.label.len() as f32 * CHAR_W + PAD).max(120.0)
            }
        })
        .collect();

    let layer_w: Vec<f32> = (0..num_layers)
        .map(|l| {
            layer_nodes[l]
                .iter()
                .map(|&i| node_w[i])
                .fold(0.0_f32, f32::max)
        })
        .collect();

    // x offset per layer
    let mut layer_x = vec![MARGIN; num_layers];
    for l in 1..num_layers {
        layer_x[l] = layer_x[l - 1] + layer_w[l - 1] + H_GAP;
    }

    let mut coords: Vec<(f32, f32)> = vec![(0.0, 0.0); total];
    for l in 0..num_layers {
        for (rank, &idx) in layer_nodes[l].iter().enumerate() {
            let x = layer_x[l];
            let y = MARGIN + rank as f32 * (NODE_H + V_GAP);
            coords[idx] = (x, y);
        }
    }

    // Canvas dimensions (before direction transform)
    let max_rank = layer_nodes.iter().map(|v| v.len()).max().unwrap_or(0);
    let lr_w = layer_x[num_layers - 1] + layer_w[num_layers - 1] + MARGIN;
    let lr_h = MARGIN + max_rank as f32 * (NODE_H + V_GAP) + MARGIN;

    // Direction-aware canvas size and coordinate transform
    let (canvas_w, canvas_h) = match &graph.direction {
        Direction::TD | Direction::BT => (lr_h, lr_w),
        _ => (lr_w, lr_h),
    };

    // Transform (x, y) in LR space to final canvas coords
    let tx = |x: f32, y: f32| -> (f32, f32) {
        match &graph.direction {
            Direction::LR => (x, y),
            Direction::RL => (lr_w - x, y),
            Direction::TD => (y, x),
            Direction::BT => (y, lr_w - x),
        }
    };

    // Edge entry/exit points based on direction
    // In LR coords: source exits from right-center, target enters from left-center
    let src_port = |cx: f32, cy: f32, w: f32| -> (f32, f32) {
        match &graph.direction {
            Direction::LR => (cx + w, cy + NODE_H / 2.0),
            Direction::RL => (cx, cy + NODE_H / 2.0),
            Direction::TD => (cx + w / 2.0, cy + NODE_H),
            Direction::BT => (cx + w / 2.0, cy),
        }
    };
    let dst_port = |cx: f32, cy: f32, w: f32| -> (f32, f32) {
        match &graph.direction {
            Direction::LR => (cx, cy + NODE_H / 2.0),
            Direction::RL => (cx + w, cy + NODE_H / 2.0),
            Direction::TD => (cx + w / 2.0, cy),
            Direction::BT => (cx + w / 2.0, cy + NODE_H),
        }
    };
    let dummy_port = |cx: f32, cy: f32| -> (f32, f32) { (cx, cy + NODE_H / 2.0) };

    // --- Build output ---
    // Map from original node index to output node index
    let mut out_nodes: Vec<LayoutNode> = Vec::new();

    for (idx, entry) in entries.iter().enumerate() {
        if entry.dummy {
            continue;
        }
        let (cx, cy) = coords[idx];
        let w = node_w[idx];
        let (tx, ty) = tx(cx, cy);
        out_nodes.push(LayoutNode {
            label: entry.label.clone(),
            shape: entry.shape.clone(),
            x: tx,
            y: ty,
            w,
            h: NODE_H,
        });
    }

    // Build output edges using precomputed chains
    let mut out_edges: Vec<LayoutEdge> = Vec::new();
    for (orig_idx, orig) in raw_edges.iter().enumerate() {
        let chain = &chains[orig_idx];

        let mut points: Vec<(f32, f32)> = Vec::new();
        for (ci, &node_idx) in chain.iter().enumerate() {
            let (cx, cy) = coords[node_idx];
            let w = node_w[node_idx];
            let is_first = ci == 0;
            let is_last = ci == chain.len() - 1;
            let raw = if is_first && !is_last {
                src_port(cx, cy, w)
            } else if is_last && !is_first {
                dst_port(cx, cy, w)
            } else if is_first && is_last {
                // single-node degenerate — skip
                continue;
            } else {
                dummy_port(cx, cy)
            };
            let (px, py) = tx(raw.0, raw.1);
            points.push((px, py));
        }

        let lp = if points.len() >= 2 {
            let mid = points.len() / 2;
            let (ax, ay) = points[mid - 1];
            let (bx, by) = points[mid];
            ((ax + bx) / 2.0, (ay + by) / 2.0 - 12.0)
        } else {
            (0.0, 0.0)
        };

        out_edges.push(LayoutEdge {
            points,
            label: orig.label.clone(),
            label_pos: lp,
            arrow: orig.arrow.clone(),
        });
    }

    Layout {
        nodes: out_nodes,
        edges: out_edges,
        width: canvas_w,
        height: canvas_h,
    }
}

fn remove_cycles(edges: &mut [RawEdge], n: usize) {
    let mut color = vec![0u8; n]; // 0=white, 1=gray(on stack), 2=black
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
    for (i, e) in edges.iter().enumerate() {
        adj[e.from].push(i);
    }
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for start in 0..n {
        if color[start] != 0 {
            continue;
        }
        stack.push((start, 0));
        color[start] = 1;
        while let Some((u, ai)) = stack.last_mut() {
            let u = *u;
            if *ai < adj[u].len() {
                let ei = adj[u][*ai];
                *ai += 1;
                let v = edges[ei].to;
                if color[v] == 1 {
                    // back edge — reverse it
                    edges[ei].reversed = true;
                    std::mem::swap(&mut edges[ei].from, &mut edges[ei].to);
                } else if color[v] == 0 {
                    color[v] = 1;
                    stack.push((v, 0));
                }
            } else {
                color[u] = 2;
                stack.pop();
            }
        }
    }
}

fn assign_layers(n: usize, edges: &[RawEdge]) -> Vec<usize> {
    let mut in_deg = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
    for e in edges {
        in_deg[e.to] += 1;
        adj[e.from].push(e.to);
    }
    let mut layer = vec![0usize; n];
    let mut queue: Vec<usize> = (0..n).filter(|&i| in_deg[i] == 0).collect();
    let mut head = 0;
    while head < queue.len() {
        let u = queue[head];
        head += 1;
        for &v in &adj[u] {
            if layer[u] + 1 > layer[v] {
                layer[v] = layer[u] + 1;
            }
            in_deg[v] -= 1;
            if in_deg[v] == 0 {
                queue.push(v);
            }
        }
    }
    layer
}

/// Insert dummy nodes for edges spanning multiple layers.
/// Returns one Vec<usize> per original edge: the full chain of node indices
/// from source to destination (including any intermediate dummy nodes).
fn insert_dummies(
    entries: &mut Vec<NodeEntry>,
    edges_for_crossings: &mut Vec<RawEdge>,
    layers: &mut Vec<usize>,
    orig_edges: &[RawEdge],
) -> Vec<Vec<usize>> {
    let orig_len = orig_edges.len();
    let mut chains: Vec<Vec<usize>> = orig_edges.iter().map(|e| vec![e.from, e.to]).collect();

    for i in 0..orig_len {
        let from = orig_edges[i].from;
        let to = orig_edges[i].to;
        let span = layers[to].saturating_sub(layers[from]);
        if span <= 1 {
            continue;
        }

        let src_layer = layers[from];
        let mut prev = from;
        chains[i] = vec![prev];

        for d in 1..span {
            let dummy_idx = entries.len();
            entries.push(NodeEntry {
                label: String::new(),
                shape: Shape::Box,
                dummy: true,
            });
            layers.push(src_layer + d);
            edges_for_crossings.push(RawEdge {
                from: prev,
                to: dummy_idx,
                label: None,
                arrow: Arrow::Normal,
                reversed: false,
            });
            chains[i].push(dummy_idx);
            prev = dummy_idx;
        }
        chains[i].push(to);

        // Update the original edge segment for crossing minimization:
        // it becomes last-dummy → to (the final unit-length segment)
        edges_for_crossings[i].from = prev;
        edges_for_crossings[i].to = to;
    }

    chains
}

fn minimize_crossings(layer_nodes: &mut [Vec<usize>], edges: &[RawEdge], passes: usize) {
    let num_layers = layer_nodes.len();
    let max_node = layer_nodes
        .iter()
        .flat_map(|l| l.iter())
        .copied()
        .max()
        .map(|m| m + 1)
        .unwrap_or(0);
    if max_node == 0 {
        return;
    }

    let mut pos = vec![0.0f64; max_node];
    for layer in layer_nodes.iter() {
        for (r, &n) in layer.iter().enumerate() {
            pos[n] = r as f64;
        }
    }

    let mut above: Vec<Vec<usize>> = vec![vec![]; max_node];
    let mut below: Vec<Vec<usize>> = vec![vec![]; max_node];
    for e in edges {
        if e.from < max_node && e.to < max_node {
            below[e.from].push(e.to);
            above[e.to].push(e.from);
        }
    }

    for pass in 0..passes {
        let downward = pass % 2 == 0;
        let range: Vec<usize> = if downward {
            (1..num_layers).collect()
        } else {
            (0..num_layers.saturating_sub(1)).rev().collect()
        };
        for l in range {
            let neighbors = if downward { &above } else { &below };
            let mut bary: Vec<(f64, usize)> = layer_nodes[l]
                .iter()
                .map(|&n| {
                    let nb = &neighbors[n];
                    let bc = if nb.is_empty() {
                        pos[n]
                    } else {
                        nb.iter()
                            .map(|&m| if m < max_node { pos[m] } else { 0.0 })
                            .sum::<f64>()
                            / nb.len() as f64
                    };
                    (bc, n)
                })
                .collect();
            bary.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(a.1.cmp(&b.1)));
            for (r, &(_, n)) in bary.iter().enumerate() {
                pos[n] = r as f64;
            }
            layer_nodes[l] = bary.into_iter().map(|(_, n)| n).collect();
        }
    }
}
