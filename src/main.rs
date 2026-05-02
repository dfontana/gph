use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand};

#[derive(Subcommand)]
enum Commands {
    /// Convert ```gph fences to ```mermaid in markdown files (in-place)
    Md {
        /// Markdown files to process (use shell globbing for multiple: **/*.md)
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Open interactive split-pane TUI with live preview
    Tui {
        /// .gph file to edit; creates a temp file if omitted
        file: Option<PathBuf>,
    },
    /// Parse a Mermaid flowchart from stdin and emit gph syntax
    Parse,
}

#[derive(Parser)]
#[command(about = "Lisp DSL compiler to Mermaid, SVG, and rendered terminal graphics")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Render inline to terminal instead of emitting Mermaid text
    #[arg(short, long)]
    render: bool,

    /// Write rendered SVG output to this path
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,
}

fn process_md_file(path: &Path) -> bool {
    let original = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading '{}': {}", path.display(), e);
            return false;
        }
    };

    let lines: Vec<&str> = original.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        if line.trim_end() == "```gph" {
            let open_idx = i;
            let mut content: Vec<&str> = vec![];
            i += 1;
            while i < lines.len() && lines[i].trim_end() != "```" {
                content.push(lines[i]);
                i += 1;
            }
            let close_found = i < lines.len();
            if close_found {
                match gph::compile(&content.join("\n")) {
                    Ok(mermaid) => {
                        out.push("```mermaid".to_string());
                        out.extend(mermaid.lines().map(str::to_owned));
                        out.push("```".to_string());
                    }
                    Err(e) => {
                        eprintln!("{}:{}: {}", path.display(), open_idx + 1, e);
                        out.push(line.to_owned());
                        out.extend(content.iter().map(|s| s.to_string()));
                        out.push(lines[i].to_owned());
                    }
                }
                i += 1;
            } else {
                out.push(line.to_owned());
                out.extend(content.iter().map(|s| s.to_string()));
            }
        } else {
            out.push(line.to_owned());
            i += 1;
        }
    }

    let mut new_content = out.join("\n");
    if original.ends_with('\n') {
        new_content.push('\n');
    }

    if new_content == original {
        return true;
    }

    if let Err(e) = std::fs::write(path, &new_content) {
        eprintln!("error writing '{}': {}", path.display(), e);
        return false;
    }
    true
}

fn render_to_file(src: &str, path: &PathBuf) -> Result<(), String> {
    let svg = gph::render_svg(src)?;
    std::fs::write(path, svg).map_err(|e| format!("error writing '{}': {e}", path.display()))
}

fn render_inline(src: &str) -> Result<(), String> {
    let svg = gph::render_svg(src)?;
    print!("{svg}");
    Ok(())
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Md { files }) => {
            let ok = files.iter().all(|p| process_md_file(p));
            if !ok {
                process::exit(1);
            }
            return;
        }
        Some(Commands::Tui { file }) => {
            if let Err(e) = gph::tui::run(file) {
                eprintln!("tui error: {e}");
                process::exit(1);
            }
            return;
        }
        Some(Commands::Parse) => {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf).unwrap_or_else(|e| {
                eprintln!("error reading stdin: {}", e);
                process::exit(1);
            });
            match gph::decompile(&buf) {
                Ok(out) => println!("{}", out),
                Err(e) => {
                    eprintln!("{}", e);
                    process::exit(1);
                }
            }
            return;
        }
        None => {}
    }

    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf).unwrap_or_else(|e| {
        eprintln!("error reading stdin: {}", e);
        process::exit(1);
    });
    let src = buf;

    if let Some(ref path) = cli.output {
        if let Err(e) = render_to_file(&src, path) {
            eprintln!("{e}");
            process::exit(1);
        }
    } else if cli.render {
        if let Err(e) = render_inline(&src) {
            eprintln!("{e}");
            process::exit(1);
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
