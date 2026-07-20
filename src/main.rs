#![forbid(unsafe_code)]

mod files;
mod kitty;
mod render;
mod watch;

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Kitty-native Mermaid previewer and SVG exporter")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Watch Mermaid source with a live Kitty preview.
    Watch {
        /// Mermaid source file to watch.
        file: PathBuf,
    },
    /// Render Mermaid source as an SVG file.
    Export {
        /// Mermaid input file, or - to read stdin.
        input: PathBuf,
        /// SVG destination. Its extension must be .svg.
        #[arg(short, long, value_name = "OUTPUT.svg")]
        output: PathBuf,
    },
}

fn main() {
    let result = match Cli::parse().command {
        Command::Watch { file } => watch(file),
        Command::Export { input, output } => export(&input, &output),
    };
    if let Err(error) = result {
        eprintln!("error: {error}");
        process::exit(1);
    }
}

fn watch(file: PathBuf) -> Result<(), String> {
    if !kitty::is_available() {
        return Err(
            "`gph watch` requires Kitty (set KITTY_WINDOW_ID or TERM=xterm-kitty)".to_string(),
        );
    }
    watch::run(file)
}

fn export(input: &Path, output: &Path) -> Result<(), String> {
    if !output
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
    {
        return Err(format!(
            "output '{}' must have a .svg extension",
            output.display()
        ));
    }
    let source = read_input(input)?;
    let svg = render::Renderer::new().svg(&source)?;
    files::write_atomically(output, &svg)
        .map_err(|error| format!("cannot write '{}': {error}", output.display()))
}

fn read_input(path: &Path) -> Result<String, String> {
    if path == Path::new("-") {
        let mut source = String::new();
        io::stdin()
            .read_to_string(&mut source)
            .map_err(|error| format!("cannot read stdin: {error}"))?;
        Ok(source)
    } else {
        std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read '{}': {error}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, read_input};
    use clap::Parser;
    use std::path::Path;

    #[test]
    fn reads_files() {
        let path = std::env::temp_dir().join(format!("gph-read-input-{}", std::process::id()));
        std::fs::write(&path, "flowchart TD").unwrap();
        assert_eq!(read_input(&path).unwrap(), "flowchart TD");
        std::fs::remove_file(path).unwrap();
    }

    fn temporary_path(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("gph-{name}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn parses_watch_command() {
        let cli = Cli::try_parse_from(["gph", "watch", "diagram.mmd"]).unwrap();
        let Command::Watch { file } = cli.command else {
            panic!("expected watch command");
        };
        assert_eq!(file, Path::new("diagram.mmd"));
    }

    #[test]
    fn export_writes_svg_after_a_successful_render() {
        let input = temporary_path("input");
        let output = temporary_path("output").with_extension("svg");
        std::fs::write(&input, "flowchart TD\nA --> B\n").unwrap();

        super::export(&input, &output).unwrap();
        assert!(
            std::fs::read_to_string(&output)
                .unwrap()
                .starts_with("<svg")
        );

        std::fs::remove_file(input).unwrap();
        std::fs::remove_file(output).unwrap();
    }

    #[test]
    fn failed_export_preserves_existing_destination() {
        let input = temporary_path("invalid-input");
        let output = temporary_path("existing-output").with_extension("svg");
        std::fs::write(&input, "not Mermaid").unwrap();
        std::fs::write(&output, "existing output").unwrap();

        assert!(super::export(&input, &output).is_err());
        assert_eq!(std::fs::read_to_string(&output).unwrap(), "existing output");

        std::fs::remove_file(input).unwrap();
        std::fs::remove_file(output).unwrap();
    }

    #[test]
    fn export_requires_svg_extension() {
        let error = super::export(Path::new("missing.mmd"), Path::new("output.png")).unwrap_err();
        assert!(error.contains(".svg extension"));
    }
}
