use std::io::{self, Read};
use std::path::PathBuf;
use std::process;

use clap::Parser;

#[derive(Parser)]
#[command(about = "Lisp DSL compiler to Mermaid, SVG, and Kitty terminal")]
struct Cli {
    /// Input .gph file (reads stdin if omitted)
    file: Option<PathBuf>,

    /// Render to SVG or Kitty instead of emitting Mermaid text
    #[arg(short, long)]
    render: bool,

    /// Write SVG output to this path (implies --render)
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,
}

fn main() {
    let cli = Cli::parse();
    let render_mode = cli.render || cli.output.is_some();

    let src = match cli.file {
        Some(path) => std::fs::read_to_string(&path).unwrap_or_else(|e| {
            eprintln!("error reading '{}': {}", path.display(), e);
            process::exit(1);
        }),
        None => {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf).unwrap_or_else(|e| {
                eprintln!("error reading stdin: {}", e);
                process::exit(1);
            });
            buf
        }
    };

    if render_mode {
        match cli.output {
            Some(path) => match gph::render_svg(&src) {
                Ok(svg) => {
                    if let Err(e) = std::fs::write(&path, svg) {
                        eprintln!("error writing '{}': {}", path.display(), e);
                        process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("{}", e);
                    process::exit(1);
                }
            },
            None => {
                if !gph::kitty_supported() {
                    eprintln!("error: --render requires --output <path> or a Kitty terminal (KITTY_WINDOW_ID not set)");
                    process::exit(1);
                }
                if let Err(e) = gph::render_kitty(&src) {
                    eprintln!("{}", e);
                    process::exit(1);
                }
            }
        }
    } else {
        match gph::compile(&src) {
            Ok(out) => println!("{}", out),
            Err(e) => {
                eprintln!("{}", e);
                process::exit(1);
            }
        }
    }
}
