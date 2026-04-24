use crate::ast::{Arrow, Shape};
use crate::layout::{Layout, LayoutEdge, LayoutNode};

pub fn render(layout: &Layout) -> String {
    let w = layout.width.ceil() as u32 + 1;
    let h = layout.height.ceil() as u32 + 1;

    let mut out = String::new();
    out.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#
    ));
    out.push('\n');

    // Defs: arrow markers
    out.push_str("  <defs>\n");
    out.push_str(&svg_marker("arr-normal", "black", false));
    out.push_str(&svg_marker("arr-dotted", "black", false));
    out.push_str(&svg_marker("arr-thick", "#333", true));
    out.push_str(&svg_marker_circle("arr-circle"));
    out.push_str(&svg_marker_cross("arr-cross"));
    out.push_str("  </defs>\n");

    // White background
    out.push_str(&format!(
        r#"  <rect width="{w}" height="{h}" fill="white"/>"#
    ));
    out.push('\n');

    // Edges first (drawn under nodes)
    for e in &layout.edges {
        out.push_str(&svg_edge(e));
    }

    // Nodes
    for n in &layout.nodes {
        out.push_str(&svg_node(n));
    }

    out.push_str("</svg>\n");
    out
}

fn svg_node(n: &LayoutNode) -> String {
    let x = n.x;
    let y = n.y;
    let w = n.w;
    let h = n.h;
    let cx = x + w / 2.0;
    let cy = y + h / 2.0;
    let label = escape_xml(&n.label);

    let shape_el = match &n.shape {
        Shape::Box => format!(
            r#"  <rect x="{x:.1}" y="{y:.1}" width="{w:.1}" height="{h:.1}" rx="0" fill="white" stroke="black" stroke-width="1.5"/>"#
        ),
        Shape::Round => format!(
            r#"  <rect x="{x:.1}" y="{y:.1}" width="{w:.1}" height="{h:.1}" rx="8" ry="8" fill="white" stroke="black" stroke-width="1.5"/>"#
        ),
        Shape::Stadium => format!(
            r#"  <rect x="{x:.1}" y="{y:.1}" width="{w:.1}" height="{h:.1}" rx="{:.1}" ry="{:.1}" fill="white" stroke="black" stroke-width="1.5"/>"#,
            h / 2.0,
            h / 2.0
        ),
        Shape::Diamond => {
            let pts = format!(
                "{cx:.1},{y:.1} {:.1},{cy:.1} {cx:.1},{:.1} {x:.1},{cy:.1}",
                x + w,
                y + h
            );
            format!(r#"  <polygon points="{pts}" fill="white" stroke="black" stroke-width="1.5"/>"#)
        }
        Shape::Hex => {
            let inset = h * 0.3;
            let pts = format!(
                "{:.1},{cy:.1} {:.1},{y:.1} {:.1},{y:.1} {:.1},{cy:.1} {:.1},{:.1} {:.1},{:.1}",
                x,
                x + inset,
                x + w - inset,
                x + w,
                x + w - inset,
                y + h,
                x + inset,
                y + h,
            );
            format!(r#"  <polygon points="{pts}" fill="white" stroke="black" stroke-width="1.5"/>"#)
        }
        Shape::Sub => {
            let inner = 4.0;
            format!(
                r#"  <rect x="{x:.1}" y="{y:.1}" width="{w:.1}" height="{h:.1}" rx="0" fill="white" stroke="black" stroke-width="1.5"/>
  <line x1="{:.1}" y1="{y:.1}" x2="{:.1}" y2="{:.1}" stroke="black" stroke-width="1.5"/>
  <line x1="{:.1}" y1="{y:.1}" x2="{:.1}" y2="{:.1}" stroke="black" stroke-width="1.5"/>"#,
                x + inner,
                x + inner,
                y + h,
                x + w - inner,
                x + w - inner,
                y + h,
            )
        }
    };

    let text_el = format!(
        r#"  <text x="{cx:.1}" y="{cy:.1}" font-family="sans-serif" font-size="13" text-anchor="middle" dominant-baseline="middle">{label}</text>"#
    );

    format!("{shape_el}\n{text_el}\n")
}

fn svg_edge(e: &LayoutEdge) -> String {
    if e.points.len() < 2 {
        return String::new();
    }

    let pts = polyline_points(&e.points);
    let (stroke, dash, sw, marker_id) = arrow_style(&e.arrow);
    let dash_attr = if dash.is_empty() {
        String::new()
    } else {
        format!(r#" stroke-dasharray="{dash}""#)
    };

    let line = format!(
        r#"  <polyline points="{pts}" fill="none" stroke="{stroke}" stroke-width="{sw}"{dash_attr} marker-end="url(#{marker_id})"/>"#
    );

    let label_el = if let Some(lbl) = &e.label {
        let (lx, ly) = e.label_pos;
        let lbl = escape_xml(lbl);
        format!(
            "  <text x=\"{lx:.1}\" y=\"{ly:.1}\" font-family=\"sans-serif\" font-size=\"11\" text-anchor=\"middle\" fill=\"#555\">{lbl}</text>"
        )
    } else {
        String::new()
    };

    format!("{line}\n{label_el}\n")
}

fn arrow_style(arrow: &Arrow) -> (&'static str, &'static str, &'static str, &'static str) {
    // (stroke color, dash array, stroke-width, marker-id)
    match arrow {
        Arrow::Normal => ("black", "", "1.5", "arr-normal"),
        Arrow::Dotted => ("#555", "6 4", "1.5", "arr-dotted"),
        Arrow::Thick => ("#333", "", "3", "arr-thick"),
        Arrow::Circle => ("black", "", "1.5", "arr-circle"),
        Arrow::Cross => ("black", "", "1.5", "arr-cross"),
    }
}

fn svg_marker(id: &str, color: &str, bold: bool) -> String {
    let sw = if bold { "2" } else { "1" };
    format!(
        r#"    <marker id="{id}" markerWidth="10" markerHeight="7" refX="9" refY="3.5" orient="auto">
      <polygon points="0 0, 10 3.5, 0 7" fill="{color}" stroke="{color}" stroke-width="{sw}"/>
    </marker>
"#
    )
}

fn svg_marker_circle(id: &str) -> String {
    format!(
        r#"    <marker id="{id}" markerWidth="8" markerHeight="8" refX="4" refY="4" orient="auto">
      <circle cx="4" cy="4" r="3" fill="none" stroke="black" stroke-width="1.5"/>
    </marker>
"#
    )
}

fn svg_marker_cross(id: &str) -> String {
    format!(
        r#"    <marker id="{id}" markerWidth="8" markerHeight="8" refX="4" refY="4" orient="auto">
      <line x1="1" y1="1" x2="7" y2="7" stroke="black" stroke-width="1.5"/>
      <line x1="7" y1="1" x2="1" y2="7" stroke="black" stroke-width="1.5"/>
    </marker>
"#
    )
}

fn polyline_points(pts: &[(f32, f32)]) -> String {
    pts.iter()
        .map(|(x, y)| format!("{:.1},{:.1}", x, y))
        .collect::<Vec<_>>()
        .join(" ")
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
