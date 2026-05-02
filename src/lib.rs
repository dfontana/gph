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

pub fn svg_to_image(svg: &str) -> Result<image::DynamicImage, String> {
    let mut opt = usvg::Options::default();
    opt.fontdb_mut().load_system_fonts();
    let tree = usvg::Tree::from_str(svg, &opt).map_err(|e| format!("svg parse error: {e}"))?;
    let size = tree.size().to_int_size();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or_else(|| "failed to allocate pixmap".to_string())?;
    resvg::render(&tree, resvg::tiny_skia::Transform::default(), &mut pixmap.as_mut());
    let img = image::RgbaImage::from_raw(size.width(), size.height(), pixmap.data().to_vec())
        .ok_or_else(|| "failed to create image from pixmap".to_string())?;
    Ok(image::DynamicImage::ImageRgba8(img))
}

pub fn decompile(src: &str) -> Result<String, String> {
    let graph = mermaid_parser::parse(src)?;
    Ok(gph_printer::print(&graph))
}
