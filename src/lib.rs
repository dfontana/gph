mod ast;
mod codegen;
mod gph_printer;
mod kitty;
mod layout;
mod lexer;
mod mermaid_parser;
mod parser;
mod svg;
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
    let lay = layout::compute(&lex_and_parse(src)?);
    Ok(svg::render(&lay))
}

pub fn render_kitty(src: &str) -> Result<(), String> {
    let lay = layout::compute(&lex_and_parse(src)?);
    kitty::display(&lay);
    Ok(())
}

pub fn decompile(src: &str) -> Result<String, String> {
    let graph = mermaid_parser::parse(src)?;
    Ok(gph_printer::print(&graph))
}

pub fn kitty_supported() -> bool {
    kitty::is_supported()
}

pub fn render_to_rgba(src: &str) -> Result<(Vec<u8>, usize, usize), String> {
    let lay = layout::compute(&lex_and_parse(src)?);
    Ok(kitty::render_to_rgba(&lay))
}
