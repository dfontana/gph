#![forbid(unsafe_code)]

mod kitty;
mod lsp;
// MML is intentionally not a CLI filetype yet; keep it compiled for its future
// shorthand integration without exposing a separate library target.
#[allow(dead_code)]
mod mml;
mod preview;
mod preview_ui;
mod render;
mod watch;

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(version, about = "Kitty-native Mermaid previewer and renderer")]
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
    /// Run the Kitty workspace preview daemon for unsaved LSP documents.
    Lsp,
    /// Bridge an LSP client's stdin/stdout to the workspace preview daemon.
    LspConnect,
    /// Render Mermaid source as a file or in the active Kitty terminal.
    #[command(visible_alias = "export")]
    Render {
        /// Mermaid input file, or - to read stdin. Defaults to stdin.
        #[arg(default_value = "-")]
        input: PathBuf,
        /// Destination file. Omit to display the diagram in Kitty.
        #[arg(short = 'o', long = "out", alias = "output", value_name = "OUTPUT")]
        output: Option<PathBuf>,
        /// Output format. Inferred from OUTPUT when omitted.
        #[arg(short, long, value_enum)]
        format: Option<OutputFormat>,
        /// Pixel scale for PNG/JPEG file exports. Defaults to 10.0.
        #[arg(
            long,
            value_name = "FACTOR",
            value_parser = parse_scale,
            allow_negative_numbers = true
        )]
        scale: Option<f32>,
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
        Command::Lsp => lsp::run_daemon(),
        Command::LspConnect => lsp::run_connect(),
        Command::Render {
            input,
            output,
            format,
            scale,
        } => render(&input, output.as_deref(), format, scale),
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
    output: Option<&Path>,
    requested_format: Option<OutputFormat>,
    requested_scale: Option<f32>,
) -> Result<(), String> {
    let source = read_input(input)?;
    match output {
        Some(output) => {
            let renderer = render::Renderer::new();
            let format = output_format(output, requested_format)?;
            validate_file_scale(format, requested_scale)?;
            let raster_scale = file_raster_scale(requested_scale);
            let bytes = match format {
                OutputFormat::Svg => renderer.svg(&source)?.into_bytes(),
                OutputFormat::Png => {
                    renderer.raster_with_scale(&source, render::RasterFormat::Png, raster_scale)?
                }
                OutputFormat::Jpeg => {
                    renderer.raster_with_scale(&source, render::RasterFormat::Jpeg, raster_scale)?
                }
                OutputFormat::Pdf => renderer.raster(&source, render::RasterFormat::Pdf)?,
            };
            render::files::write_atomically(output, &bytes)
                .map_err(|error| format!("cannot write '{}': {error}", output.display()))
        }
        None => {
            if requested_scale.is_some() {
                return Err("--scale only applies to PNG and JPEG file exports".to_string());
            }
            if requested_format.is_some_and(|format| !matches!(format, OutputFormat::Png)) {
                return Err(
                    "terminal display output is PNG; omit --format or use --format png".to_string(),
                );
            }
            preview::run_source_preview(input.display().to_string(), source)
        }
    }
}

fn validate_file_scale(format: OutputFormat, requested_scale: Option<f32>) -> Result<(), String> {
    if requested_scale.is_some() && matches!(format, OutputFormat::Svg | OutputFormat::Pdf) {
        return Err("--scale only applies to PNG and JPEG file exports".to_string());
    }
    Ok(())
}

fn file_raster_scale(requested_scale: Option<f32>) -> f32 {
    requested_scale.unwrap_or(render::DEFAULT_FILE_RASTER_SCALE)
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

fn parse_scale(value: &str) -> Result<f32, String> {
    let scale = value
        .parse::<f32>()
        .map_err(|_| format!("invalid scale '{value}': expected a number greater than zero"))?;
    if !scale.is_finite() {
        return Err(format!("invalid scale '{value}': value must be finite"));
    }
    if scale <= 0.0 {
        return Err(format!(
            "invalid scale '{value}': value must be greater than zero"
        ));
    }
    Ok(scale)
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
    use super::{Cli, Command, OutputFormat, file_raster_scale, read_input, validate_file_scale};
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

    #[test]
    fn render_defaults_to_stdin_and_terminal_display() {
        let cli = Cli::try_parse_from(["gph", "render"]).unwrap();
        let Command::Render {
            input,
            output,
            format,
            scale,
        } = cli.command
        else {
            panic!("expected render command");
        };
        assert_eq!(input, Path::new("-"));
        assert!(output.is_none());
        assert!(format.is_none());
        assert!(scale.is_none());
    }

    #[test]
    fn parses_explicit_render_scale() {
        let cli = Cli::try_parse_from([
            "gph",
            "render",
            "diagram.mmd",
            "-o",
            "diagram.png",
            "--scale",
            "1",
        ])
        .unwrap();
        let Command::Render { scale, .. } = cli.command else {
            panic!("expected render command");
        };
        assert_eq!(scale, Some(1.0));
    }

    #[test]
    fn export_alias_preserves_render_invocation_compatibility() {
        let cli =
            Cli::try_parse_from(["gph", "export", "diagram.mmd", "-o", "diagram.png"]).unwrap();
        let Command::Render {
            input,
            output,
            format,
            scale,
        } = cli.command
        else {
            panic!("expected render command");
        };
        assert_eq!(input, Path::new("diagram.mmd"));
        assert_eq!(output.as_deref(), Some(Path::new("diagram.png")));
        assert!(format.is_none());
        assert!(scale.is_none());
    }

    #[test]
    fn rejects_invalid_render_scales() {
        for (value, expected) in [
            ("0", "greater than zero"),
            ("-1", "greater than zero"),
            ("NaN", "must be finite"),
            ("inf", "must be finite"),
            ("-inf", "must be finite"),
        ] {
            let scale_argument = format!("--scale={value}");
            let error = match Cli::try_parse_from(["gph", "render", &scale_argument]) {
                Ok(_) => panic!("expected invalid scale: {value}"),
                Err(error) => error,
            };
            assert!(error.to_string().contains(expected), "{value}: {error}");
        }
    }

    #[test]
    fn file_raster_scale_defaults_only_pixel_exports_and_rejects_vector_scale() {
        assert_eq!(file_raster_scale(None), 10.0);
        assert_eq!(file_raster_scale(Some(1.0)), 1.0);
        assert!(validate_file_scale(OutputFormat::Svg, Some(1.0)).is_err());
        assert!(validate_file_scale(OutputFormat::Pdf, Some(1.0)).is_err());
        assert!(validate_file_scale(OutputFormat::Png, Some(1.0)).is_ok());
        assert!(validate_file_scale(OutputFormat::Jpeg, None).is_ok());
    }
}
