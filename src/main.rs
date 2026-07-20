#![forbid(unsafe_code)]

mod kitty;
// MML is intentionally not a CLI filetype yet; keep it compiled for its future
// shorthand integration without exposing a separate library target.
#[allow(dead_code)]
mod mml;
mod render;
mod watch;

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(about = "Kitty-native Mermaid previewer and renderer")]
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
    /// Render Mermaid source as SVG, PNG, JPEG, or PDF.
    #[command(visible_alias = "export")]
    Render {
        /// Mermaid input file, or - to read stdin.
        input: PathBuf,
        /// Destination file.
        #[arg(short = 'o', long = "out", alias = "output", value_name = "OUTPUT")]
        output: PathBuf,
        /// Output format. Inferred from OUTPUT when omitted.
        #[arg(short, long, value_enum)]
        format: Option<OutputFormat>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Svg,
    Png,
    #[value(alias = "jpg")]
    Jpeg,
    Pdf,
}

fn main() {
    let result = match Cli::parse().command {
        Command::Watch { file } => watch(file),
        Command::Render {
            input,
            output,
            format,
        } => render(&input, &output, format),
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

fn render(
    input: &Path,
    output: &Path,
    requested_format: Option<OutputFormat>,
) -> Result<(), String> {
    let format = output_format(output, requested_format)?;
    let source = read_input(input)?;
    let renderer = render::Renderer::new();
    let bytes = match format {
        OutputFormat::Svg => renderer.svg(&source)?.into_bytes(),
        OutputFormat::Png => renderer.raster(&source, render::RasterFormat::Png)?,
        OutputFormat::Jpeg => renderer.raster(&source, render::RasterFormat::Jpeg)?,
        OutputFormat::Pdf => renderer.raster(&source, render::RasterFormat::Pdf)?,
    };
    render::files::write_atomically(output, &bytes)
        .map_err(|error| format!("cannot write '{}': {error}", output.display()))
}

fn output_format(
    output: &Path,
    requested_format: Option<OutputFormat>,
) -> Result<OutputFormat, String> {
    if let Some(format) = requested_format {
        return Ok(format);
    }
    match output.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("svg") => Ok(OutputFormat::Svg),
        Some(extension) if extension.eq_ignore_ascii_case("png") => Ok(OutputFormat::Png),
        Some(extension)
            if extension.eq_ignore_ascii_case("jpeg") || extension.eq_ignore_ascii_case("jpg") =>
        {
            Ok(OutputFormat::Jpeg)
        }
        Some(extension) if extension.eq_ignore_ascii_case("pdf") => Ok(OutputFormat::Pdf),
        _ => Err(format!(
            "cannot infer a format from '{}'; pass --format",
            output.display()
        )),
    }
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

    #[test]
    fn parses_watch_command() {
        let cli = Cli::try_parse_from(["gph", "watch", "diagram.mmd"]).unwrap();
        let Command::Watch { file } = cli.command else {
            panic!("expected watch command");
        };
        assert_eq!(file, Path::new("diagram.mmd"));
    }
}
