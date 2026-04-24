mod ast;
mod codegen;
mod kitty;
mod layout;
mod lexer;
mod parser;
mod svg;

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
