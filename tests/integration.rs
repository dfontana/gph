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
fn svg_single_edge_has_path() {
    check_svg("(graph lr (-> a b))", "<path");
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
    check_svg(r#"(graph lr (n "Node" round))"#, "rx=");
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
    assert!(svg.contains('a') || svg.contains("<path"));
}

#[test]
fn svg_td_direction() {
    let got = gph::render_svg("(graph td (-> a b))").expect("render_svg failed");
    assert!(got.contains(r#"<svg"#));
    assert!(got.contains("<path"));
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

#[test]
fn inline_node_in_edge() {
    check(
        r#"(graph lr (-> (a "Node A") b))"#,
        "flowchart LR\n  a[\"Node A\"]\n  a --> b",
    );
}

#[test]
fn inline_node_with_shape() {
    check(
        r#"(graph lr (-> (a "A" round) (b "B" diamond)))"#,
        "flowchart LR\n  a(\"A\")\n  b{\"B\"}\n  a --> b",
    );
}

#[test]
fn inline_node_mixed_bare_and_sub() {
    check(
        r#"(graph lr (-> (a "A") b (c "C")))"#,
        "flowchart LR\n  a[\"A\"]\n  c[\"C\"]\n  a --> b --> c",
    );
}

#[test]
fn inline_node_referenced_again_bare() {
    check(
        r#"(graph lr (-> (a "A") (b "B")) (-> a b "again"))"#,
        "flowchart LR\n  a[\"A\"]\n  b[\"B\"]\n  a --> b\n  a -->|again| b",
    );
}

#[test]
fn inline_node_full_example() {
    let src = r#"(graph lr
  (-> (login "Login" round) (validate "Validate Input" diamond))
  (-> validate (dashboard "Dashboard" stadium) "ok")
  (-> validate (error "Error" round) "fail")
  (--> error login "retry"))"#;
    let out = gph::compile(src).unwrap();
    assert!(out.starts_with("flowchart LR"));
    assert!(out.contains("login(\"Login\")"));
    assert!(out.contains("validate{\"Validate Input\"}"));
    assert!(out.contains("dashboard([\"Dashboard\"])"));
    assert!(out.contains("error(\"Error\")"));
    assert!(out.contains("login --> validate"));
    assert!(out.contains("validate -->|ok| dashboard"));
    assert!(out.contains("validate -->|fail| error"));
    assert!(out.contains("error -.->|retry| login"));
}

// ---- gph parse (mermaid → gph) round-trip tests ----------------------------

fn check_decompile(mermaid: &str, expected_gph: &str) {
    let got = gph::decompile(mermaid).expect("decompile failed");
    assert_eq!(
        got.trim(),
        expected_gph.trim(),
        "\nMermaid input:\n{}",
        mermaid
    );
}

fn check_round_trip(src: &str) {
    let mermaid = gph::compile(src).expect("compile failed");
    let gph_out = gph::decompile(&mermaid).expect("decompile failed");
    let mermaid2 = gph::compile(&gph_out).expect("recompile failed");
    assert_eq!(
        mermaid, mermaid2,
        "round-trip mismatch\nOriginal gph:\n{}\nMermaid:\n{}\nDecompiled gph:\n{}",
        src, mermaid, gph_out
    );
}

#[test]
fn decompile_simple_edge() {
    check_decompile("flowchart LR\n  a --> b", "(graph lr\n  (-> a b))");
}

#[test]
fn decompile_empty_graph() {
    check_decompile("flowchart TD", "(graph td)");
}

#[test]
fn decompile_node_shapes() {
    check_decompile(
        "flowchart TD\n  n1[\"Box\"]\n  n2(\"Round\")\n  n3{\"Diamond\"}\n  n4([\"Stadium\"])\n  n5{{\"Hex\"}}\n  n6[[\"Sub\"]]",
        "(graph td\n  (n1 \"Box\")\n  (n2 \"Round\" round)\n  (n3 \"Diamond\" diamond)\n  (n4 \"Stadium\" stadium)\n  (n5 \"Hex\" hex)\n  (n6 \"Sub\" sub))",
    );
}

#[test]
fn decompile_all_arrow_types() {
    check_decompile("flowchart LR\n  a --> b", "(graph lr\n  (-> a b))");
    check_decompile("flowchart LR\n  a -.-> b", "(graph lr\n  (--> a b))");
    check_decompile("flowchart LR\n  a ==> b", "(graph lr\n  (=> a b))");
    check_decompile("flowchart LR\n  a --o b", "(graph lr\n  (-o a b))");
    check_decompile("flowchart LR\n  a --x b", "(graph lr\n  (-x a b))");
}

#[test]
fn decompile_labeled_edge() {
    check_decompile(
        "flowchart LR\n  a -->|ok| b",
        "(graph lr\n  (-> a b \"ok\"))",
    );
}

#[test]
fn decompile_chain_reconstructed() {
    // gph emits a labeled 3-node chain as two lines; parser must reconstruct it
    check_decompile(
        "flowchart LR\n  a --> b\n  b -->|done| c",
        "(graph lr\n  (-> a b c \"done\"))",
    );
}

#[test]
fn decompile_unlabeled_chain_on_one_line() {
    check_decompile("flowchart LR\n  a --> b --> c", "(graph lr\n  (-> a b c))");
}

#[test]
fn decompile_node_decl_interrupts_chain() {
    // A node decl between the prefix line and labeled hop breaks the chain.
    // Both resulting separate edges compile to the same mermaid output.
    check_decompile(
        "flowchart LR\n  a --> b\n  c[\"C\"]\n  b -->|label| c",
        "(graph lr\n  (-> a b)\n  (c \"C\")\n  (-> b c \"label\"))",
    );
}

#[test]
fn decompile_label_with_escaped_pipe() {
    check_decompile(
        "flowchart LR\n  a -->|yes#124;no| b",
        "(graph lr\n  (-> a b \"yes|no\"))",
    );
}

#[test]
fn decompile_label_with_escaped_quote() {
    check_decompile(
        "flowchart LR\n  n[\"say #quot;hi#quot;\"]",
        "(graph lr\n  (n \"say \\\"hi\\\"\"))",
    );
}

#[test]
fn round_trip_simple() {
    check_round_trip("(graph lr (-> a b))");
}

#[test]
fn round_trip_all_directions() {
    check_round_trip("(graph lr (-> a b))");
    check_round_trip("(graph rl (-> a b))");
    check_round_trip("(graph td (-> a b))");
    check_round_trip("(graph bt (-> a b))");
}

#[test]
fn round_trip_all_shapes() {
    check_round_trip(
        r#"(graph td
      (n1 "Box")
      (n2 "Round" round)
      (n3 "Diamond" diamond)
      (n4 "Stadium" stadium)
      (n5 "Hex" hex)
      (n6 "Sub" sub))"#,
    );
}

#[test]
fn round_trip_labeled_chain_three_nodes() {
    check_round_trip(r#"(graph lr (-> a b c "done"))"#);
}

#[test]
fn round_trip_labeled_chain_five_nodes() {
    check_round_trip(r#"(graph lr (-> a b c d e "done"))"#);
}

#[test]
fn round_trip_all_arrows() {
    check_round_trip("(graph lr (--> a b))");
    check_round_trip("(graph lr (=> a b))");
    check_round_trip("(graph lr (-o a b))");
    check_round_trip("(graph lr (-x a b))");
}

#[test]
fn round_trip_node_id_with_dashes() {
    check_round_trip("(graph lr (-> my-node other-node))");
}

#[test]
fn round_trip_label_escaped_pipe() {
    check_round_trip(r#"(graph lr (-> a b "yes|no"))"#);
}

#[test]
fn round_trip_label_escaped_quote() {
    check_round_trip("(graph lr (n \"say \\\"hi\\\"\"))");
}

#[test]
fn round_trip_full_example() {
    check_round_trip(
        r#"(graph lr
  (login "Login" round)
  (validate "Validate Input" diamond)
  (dashboard "Dashboard" stadium)
  (error "Error" round)
  (-> login validate)
  (-> validate dashboard "ok")
  (-> validate error "fail")
  (--> error login "retry"))"#,
    );
}

#[test]
fn round_trip_inline_nodes() {
    check_round_trip(
        r#"(graph lr
  (-> (login "Login" round) (validate "Validate Input" diamond))
  (-> validate (dashboard "Dashboard" stadium) "ok")
  (-> validate (error "Error" round) "fail")
  (--> error login "retry"))"#,
    );
}

#[test]
fn round_trip_example_file() {
    let src = r#"(graph lr
  (push "git push" stadium)
  (ci "Run CI" hex)
  (lint "Lint" round)
  (test "Tests" round)
  (build "Build" sub)
  (gate "Deploy?" diamond)
  (staging "Staging" round)
  (smoke "Smoke Tests" hex)
  (prod "Production" stadium)
  (rollback "Rollback" round)
  (notify "Notify Team")
  (-> push ci)
  (-> ci lint "start")
  (-> ci test "start")
  (-> lint build)
  (-> test build)
  (-> build gate)
  (-> gate staging "approve")
  (-x gate rollback "reject")
  (-> staging smoke)
  (-> smoke prod "pass")
  (--> smoke staging "retry")
  (=> prod notify "done"))"#;
    check_round_trip(src);
}
