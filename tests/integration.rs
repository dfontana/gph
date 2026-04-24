fn check(input: &str, expected: &str) {
    let got = gph::compile(input).expect("compile failed");
    assert_eq!(got.trim(), expected.trim(), "\nInput: {}", input);
}

fn check_err(input: &str, fragment: &str) {
    let err = gph::compile(input).expect_err("expected compile error");
    assert!(
        err.contains(fragment),
        "expected {:?} in error message, got: {:?}\nInput: {}",
        fragment,
        err,
        input
    );
}

// 1. Minimal graph
#[test]
fn minimal_graph() {
    check("(graph lr (-> a b))", "flowchart LR\n  a --> b");
}

// 2. All node shapes
#[test]
fn node_shapes() {
    check(
        r#"(graph td
             (n1 "Box")
             (n2 "Round" round)
             (n3 "Diamond" diamond)
             (n4 "Stadium" stadium)
             (n5 "Hex" hex)
             (n6 "Sub" sub))"#,
        "flowchart TD\n  n1[\"Box\"]\n  n2(\"Round\")\n  n3{\"Diamond\"}\n  n4([\"Stadium\"])\n  n5{{\"Hex\"}}\n  n6[[\"Sub\"]]",
    );
}

// 3. Edge chain of 3 nodes (single chained line, no label)
#[test]
fn edge_chain_three_nodes() {
    check("(graph lr (-> a b c))", "flowchart LR\n  a --> b --> c");
}

// 4. Edge chain with label — label on last hop only
#[test]
fn edge_label_last_hop() {
    check(
        r#"(graph lr (-> a b c "done"))"#,
        "flowchart LR\n  a --> b\n  b -->|done| c",
    );
}

// Two-node edge with label
#[test]
fn edge_label_two_nodes() {
    check(r#"(graph lr (-> a b "go"))"#, "flowchart LR\n  a -->|go| b");
}

// 5. All directions
#[test]
fn all_directions() {
    for (dir, merm) in [("lr", "LR"), ("rl", "RL"), ("td", "TD"), ("bt", "BT")] {
        check(
            &format!("(graph {} (-> a b))", dir),
            &format!("flowchart {}\n  a --> b", merm),
        );
    }
}

// 6. All arrow types
#[test]
fn all_arrow_types() {
    check("(graph lr (-> a b))", "flowchart LR\n  a --> b");
    check("(graph lr (--> a b))", "flowchart LR\n  a -.-> b");
    check("(graph lr (=> a b))", "flowchart LR\n  a ==> b");
    check("(graph lr (-o a b))", "flowchart LR\n  a --o b");
    check("(graph lr (-x a b))", "flowchart LR\n  a --x b");
}

// 7. Mixed explicit and implicit nodes
#[test]
fn explicit_and_implicit_nodes() {
    check(
        r#"(graph lr
             (validate "Validate Input" diamond)
             (-> login validate dashboard)
             (-> validate error "fail")
             (-> validate dashboard "ok"))"#,
        "flowchart LR\n  validate{\"Validate Input\"}\n  login --> validate --> dashboard\n  validate -->|fail| error\n  validate -->|ok| dashboard",
    );
}

// 8. Comments are ignored
#[test]
fn comments_ignored() {
    check(
        "(graph lr ; this is a comment\n  (-> a b))",
        "flowchart LR\n  a --> b",
    );
}

// 9. Error: unknown direction
#[test]
fn error_unknown_direction() {
    check_err("(graph xx (-> a b))", "direction");
}

// 10. Error: edge with fewer than two nodes
#[test]
fn error_edge_too_few_nodes() {
    check_err("(graph lr (-> a))", "two");
}

// 11. Error: missing closing paren
#[test]
fn error_missing_closing_paren() {
    check_err("(graph lr (-> a b)", "closing");
}

// 12. Node with label, default (box) shape
#[test]
fn node_default_box_shape() {
    check(
        r#"(graph lr (start "Begin"))"#,
        "flowchart LR\n  start[\"Begin\"]",
    );
}

// 13. Implicit node (no label) emits no declaration line
#[test]
fn implicit_node_no_declaration() {
    check("(graph lr (a) (-> a b))", "flowchart LR\n  a --> b");
}

// 14. Long chain with five nodes
#[test]
fn edge_chain_five_nodes() {
    check(
        "(graph lr (-> a b c d e))",
        "flowchart LR\n  a --> b --> c --> d --> e",
    );
}

// ---- SVG rendering tests ---------------------------------------------------

fn check_svg(input: &str, fragment: &str) {
    let got = gph::render_svg(input).expect("render_svg failed");
    assert!(
        got.contains(fragment),
        "expected {:?} in SVG output\nGot:\n{}",
        fragment,
        &got[..got.len().min(800)]
    );
}

#[test]
fn svg_produces_valid_header() {
    check_svg(
        "(graph lr (-> a b))",
        r#"<svg xmlns="http://www.w3.org/2000/svg""#,
    );
}

#[test]
fn svg_single_edge_has_polyline() {
    check_svg("(graph lr (-> a b))", "<polyline");
}

#[test]
fn svg_node_labels_present() {
    check_svg(
        r#"(graph lr (start "Login" round) (-> start end))"#,
        "Login",
    );
}

#[test]
fn svg_round_shape_has_rx() {
    check_svg(r#"(graph lr (n "Node" round))"#, r#"rx="8""#);
}

#[test]
fn svg_diamond_shape_has_polygon() {
    check_svg(r#"(graph lr (v "Check" diamond))"#, "<polygon");
}

#[test]
fn svg_edge_label_present() {
    check_svg(r#"(graph lr (-> a b "yes"))"#, "yes");
}

#[test]
fn svg_dotted_arrow_has_dasharray() {
    check_svg("(graph lr (--> a b))", "stroke-dasharray");
}

#[test]
fn svg_spec_example_all_labels() {
    let src = r#"(graph lr
      (login "Login" round)
      (validate "Validate Input" diamond)
      (dashboard "Dashboard" stadium)
      (error "Error" round)
      (-> login validate)
      (-> validate dashboard "ok")
      (-> validate error "fail")
      (--> error login "retry"))"#;
    check_svg(src, "Login");
    check_svg(src, "Validate Input");
    check_svg(src, "Dashboard");
    check_svg(src, "Error");
}

#[test]
fn svg_back_edge_does_not_panic() {
    let got = gph::render_svg("(graph lr (-> a b) (-> b a))");
    assert!(got.is_ok(), "render_svg panicked on back edge");
    let svg = got.unwrap();
    assert!(svg.contains('a') || svg.contains("polyline"));
}

#[test]
fn svg_td_direction() {
    let got = gph::render_svg("(graph td (-> a b))").expect("render_svg failed");
    assert!(got.contains(r#"<svg"#));
    assert!(got.contains("polyline"));
}

// ---- Additional coverage ---------------------------------------------------

#[test]
fn empty_graph() {
    check("(graph lr)", "flowchart LR");
}

#[test]
fn node_id_with_dashes() {
    check(
        "(graph lr (-> my-node other-node))",
        "flowchart LR\n  my-node --> other-node",
    );
}

#[test]
fn edge_label_pipe_escaped() {
    check(
        r#"(graph lr (-> a b "yes|no"))"#,
        "flowchart LR\n  a -->|yes#124;no| b",
    );
}

#[test]
fn node_label_quote_escaped() {
    check(
        "(graph lr (n \"say \\\"hi\\\"\"))",
        "flowchart LR\n  n[\"say #quot;hi#quot;\"]",
    );
}
