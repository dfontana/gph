use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Kitty-native live Mermaid editor and SVG exporter")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Edit Mermaid source with a live Kitty preview.
    Edit {
        /// Mermaid source file to edit.
        file: PathBuf,
        /// Create FILE with a small Mermaid starter; fail if it already exists.
        #[arg(long)]
        create: bool,
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
        Command::Edit { file, create } => edit(file, create),
        Command::Export { input, output } => export(&input, &output),
    };
    if let Err(error) = result {
        eprintln!("error: {error}");
        process::exit(1);
    }
}

fn edit(file: PathBuf, create: bool) -> Result<(), String> {
    if !gph::kitty::is_available() {
        return Err(
            "`gph edit` requires Kitty (set KITTY_WINDOW_ID or TERM=xterm-kitty)".to_string(),
        );
    }
    if create {
        create_file(&file)?;
    }
    if !file.is_file() {
        return Err(format!(
            "'{}' is not a readable Mermaid file",
            file.display()
        ));
    }
    gph::tui::run(file)
}

fn create_file(path: &Path) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("cannot create '{}': {error}", path.display()))?;
    let result = file
        .write_all(b"flowchart TD\n  A[Start] --> B[Done]\n")
        .and_then(|()| file.flush());
    if let Err(error) = result {
        let _ = std::fs::remove_file(path);
        return Err(format!("cannot create '{}': {error}", path.display()));
    }
    Ok(())
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
    let svg = gph::render::Renderer::new().svg(&source)?;
    gph::files::write_atomically(output, &svg)
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
    use super::read_input;
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
    fn create_file_never_overwrites_an_existing_file() {
        let path = temporary_path("existing");
        std::fs::write(&path, "existing").unwrap();
        assert!(super::create_file(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "existing");
        std::fs::remove_file(path).unwrap();
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
