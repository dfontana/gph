mod ast;
mod codegen;
mod kitty;
mod layout;
mod lexer;
mod parser;
mod svg;
pub mod tui;

pub fn compile(src: &str) -> Result<String, String> {
    let tokens =
        lexer::lex(src).map_err(|e| format!("lex error at {}:{}: {}", e.line, e.col, e.msg))?;
    let graph = parser::parse(&tokens)
        .map_err(|e| format!("parse error at {}:{}: {}", e.line, e.col, e.msg))?;
    Ok(codegen::generate(&graph))
}

pub fn render_svg(src: &str) -> Result<String, String> {
    let tokens =
        lexer::lex(src).map_err(|e| format!("lex error at {}:{}: {}", e.line, e.col, e.msg))?;
    let graph = parser::parse(&tokens)
        .map_err(|e| format!("parse error at {}:{}: {}", e.line, e.col, e.msg))?;
    let lay = layout::compute(&graph);
    Ok(svg::render(&lay))
}

pub fn render_kitty(src: &str) -> Result<(), String> {
    let tokens =
        lexer::lex(src).map_err(|e| format!("lex error at {}:{}: {}", e.line, e.col, e.msg))?;
    let graph = parser::parse(&tokens)
        .map_err(|e| format!("parse error at {}:{}: {}", e.line, e.col, e.msg))?;
    let lay = layout::compute(&graph);
    kitty::display(&lay);
    Ok(())
}

pub fn kitty_supported() -> bool {
    kitty::is_supported()
}

pub fn render_to_rgba(src: &str) -> Result<(Vec<u8>, usize, usize), String> {
    let tokens =
        lexer::lex(src).map_err(|e| format!("lex error at {}:{}: {}", e.line, e.col, e.msg))?;
    let graph = parser::parse(&tokens)
        .map_err(|e| format!("parse error at {}:{}: {}", e.line, e.col, e.msg))?;
    let lay = layout::compute(&graph);
    Ok(kitty::render_to_rgba(&lay))
}
