mod ast;
mod codegen;
mod gph_printer;
mod lexer;
mod mermaid_parser;
mod parser;
pub mod tui;

fn lex_and_parse(src: &str) -> Result<ast::Graph, String> {
    let tokens =
        lexer::lex(src).map_err(|e| format!("lex error at {}:{}: {}", e.line, e.col, e.msg))?;
    parser::parse(&tokens).map_err(|e| format!("parse error at {}:{}: {}", e.line, e.col, e.msg))
}

pub fn compile(src: &str) -> Result<String, String> {
    Ok(codegen::generate(&lex_and_parse(src)?))
}

pub fn render_svg(src: &str) -> Result<String, String> {
    let mermaid = compile(src)?;
    mermaid_rs_renderer::render(&mermaid).map_err(|e| format!("render error: {e}"))
}

pub fn decompile(src: &str) -> Result<String, String> {
    let graph = mermaid_parser::parse(src)?;
    Ok(gph_printer::print(&graph))
}
