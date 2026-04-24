use std::io::{self, Read};
use std::process;

fn main() {
    let mut render_mode = false;
    let mut output_path: Option<String> = None;
    let mut file_arg: Option<String> = None;
    let mut args = std::env::args().skip(1).peekable();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--render" | "-r" => render_mode = true,
            "--output" | "-o" => {
                output_path = args.next();
                if output_path.is_none() {
                    eprintln!("error: --output requires a path argument");
                    process::exit(1);
                }
            }
            _ => file_arg = Some(arg),
        }
    }

    // -o implies --render
    if output_path.is_some() {
        render_mode = true;
    }

    let src = match file_arg {
        Some(path) => std::fs::read_to_string(&path).unwrap_or_else(|e| {
            eprintln!("error reading '{}': {}", path, e);
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
        match output_path {
            Some(ref path) => match gph::render_svg(&src) {
                Ok(svg) => {
                    if let Err(e) = std::fs::write(path, svg) {
                        eprintln!("error writing '{}': {}", path, e);
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
