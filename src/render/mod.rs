pub mod files;

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::Path;
use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};

use clap::ValueEnum;
use merman::render::{
    HeadlessRenderer, HostThemeAppearance, HostThemeOutput, HostThemeProfile, HostThemeRoles,
    HostThemeRootBackground,
    raster::{RasterOptions, svg_raster_plan, svg_to_png},
};

const PREVIEW_DIAGRAM_ID: &str = "gph-preview";
/// Default device-pixel scale for file PNG and JPEG exports.
const DEFAULT_FILE_RASTER_SCALE: f32 = 10.0;
/// Clear space between a fitted preview diagram and the edge of its viewport.
const PREVIEW_EDGE_PADDING: u32 = 15;

/// Rosé Pine Dawn palette roles used across every renderer-owned output path.
///
/// This is a merman host theme profile rather than custom CSS: merman expands its semantic roles
/// into the per-diagram Mermaid configuration and carries its raster-safe output pipeline with the
/// renderer.
fn rose_pine_dawn_theme() -> HostThemeProfile {
    let mut output = HostThemeOutput::resvg_safe_editor();
    output.root_background = HostThemeRootBackground::Color("transparent".to_string());

    HostThemeProfile::builder()
        .appearance(HostThemeAppearance::Light)
        .roles(HostThemeRoles {
            canvas: Some("#faf4ed".to_string()),
            surface: Some("#fffaf3".to_string()),
            surface_alt: Some("#f2e9e1".to_string()),
            surface_muted: Some("#f4ede8".to_string()),
            text: Some("#575279".to_string()),
            subtle_text: Some("#797593".to_string()),
            border: Some("#cecacd".to_string()),
            line: Some("#286983".to_string()),
            edge_label_background: Some("#faf4ed".to_string()),
            cluster_background: Some("#f2e9e1".to_string()),
            cluster_border: Some("#dfdad9".to_string()),
            note_background: Some("#f4ede8".to_string()),
            note_border: Some("#ea9d34".to_string()),
            note_text: Some("#575279".to_string()),
            actor_background: Some("#f2e9e1".to_string()),
            actor_border: Some("#cecacd".to_string()),
            actor_text: Some("#575279".to_string()),
            activation_background: Some("#f4ede8".to_string()),
            activation_border: Some("#cecacd".to_string()),
            error: Some("#b4637a".to_string()),
            warning: Some("#ea9d34".to_string()),
            success: Some("#56949f".to_string()),
        })
        .series_palette([
            "#286983", "#56949f", "#ea9d34", "#907aa9", "#d7827e", "#b4637a",
        ])
        .output(output)
        .build()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    Svg,
    Png,
    #[value(alias = "jpg")]
    Jpeg,
    Pdf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RasterFormat {
    Png,
    Jpeg,
    Pdf,
}

/// Render `source` completely before atomically replacing `output`.
pub fn export(
    source: &str,
    output: &Path,
    requested_format: Option<OutputFormat>,
    scale: Option<f32>,
) -> Result<(), String> {
    let format = requested_format
        .or_else(|| {
            let extension = output.extension()?.to_str()?.to_ascii_lowercase();
            match extension.as_str() {
                "svg" => Some(OutputFormat::Svg),
                "png" => Some(OutputFormat::Png),
                "jpg" | "jpeg" => Some(OutputFormat::Jpeg),
                "pdf" => Some(OutputFormat::Pdf),
                _ => None,
            }
        })
        .ok_or_else(|| {
            format!(
                "cannot infer a format from '{}'; pass --format",
                output.display()
            )
        })?;
    if scale.is_some() && matches!(format, OutputFormat::Svg | OutputFormat::Pdf) {
        return Err("--scale only applies to PNG and JPEG file exports".to_string());
    }
    let renderer = Renderer::new();
    let scale = scale.unwrap_or(DEFAULT_FILE_RASTER_SCALE);
    let bytes = match format {
        OutputFormat::Svg => renderer.svg(source)?.into_bytes(),
        OutputFormat::Png => renderer.raster_with_scale(source, RasterFormat::Png, scale)?,
        OutputFormat::Jpeg => renderer.raster_with_scale(source, RasterFormat::Jpeg, scale)?,
        OutputFormat::Pdf => renderer.raster(source, RasterFormat::Pdf)?,
    };
    files::write_atomically(output, bytes)
        .map_err(|error| format!("cannot write '{}': {error}", output.display()))
}

static SVG_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SVG_PROCESS_NONCE: OnceLock<u64> = OnceLock::new();

/// The sole boundary between gph and Mermaid parsing, layout, and rendering.
pub struct Renderer {
    inner: HeadlessRenderer,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            inner: HeadlessRenderer::new()
                .with_strict_parsing()
                .with_host_theme(&rose_pine_dawn_theme())
                .with_diagram_id(PREVIEW_DIAGRAM_ID),
        }
    }

    pub fn svg(&self, source: &str) -> Result<String, String> {
        self.inner
            .clone()
            .with_diagram_id(&next_svg_diagram_id())
            .render_svg_sync(strip_bom(source))
            .map_err(|error| format!("render failed: {error}"))?
            .ok_or_else(|| "render failed: no Mermaid diagram detected".to_string())
    }

    /// Rasterize `source` to fill a comfortably inset `width` x `height` preview, magnified by
    /// `zoom`.
    ///
    /// The initial scale fits either small or oversized diagrams into the padded viewport.
    /// Further magnification deliberately lets the diagram overflow so it can be panned.
    pub fn png(&self, source: &str, width: u32, height: u32, zoom: f32) -> Result<Vec<u8>, String> {
        let svg = self
            .inner
            .render_svg_sync(strip_bom(source))
            .map_err(|error| format!("render failed: {error}"))?
            .ok_or_else(|| "render failed: no Mermaid diagram detected".to_string())?;
        let plan = svg_raster_plan(&svg, &RasterOptions::default().with_unbounded_size())
            .map_err(|error| format!("render failed: {error}"))?;
        let options = RasterOptions::default().with_scale(preview_scale(
            plan.requested_width_px,
            plan.requested_height_px,
            width,
            height,
            zoom,
        ));
        svg_to_png(&svg, &options).map_err(|error| format!("render failed: {error}"))
    }

    pub fn raster(&self, source: &str, format: RasterFormat) -> Result<Vec<u8>, String> {
        self.raster_with_scale(source, format, 1.0)
    }

    pub fn raster_with_scale(
        &self,
        source: &str,
        format: RasterFormat,
        scale: f32,
    ) -> Result<Vec<u8>, String> {
        let options = RasterOptions::default().with_scale(scale);
        self.raster_with_options(source, format, &options)
    }

    fn raster_with_options(
        &self,
        source: &str,
        format: RasterFormat,
        options: &RasterOptions,
    ) -> Result<Vec<u8>, String> {
        let rendered = match format {
            RasterFormat::Png => self.inner.render_png_sync(strip_bom(source), options),
            RasterFormat::Jpeg => self.inner.render_jpeg_sync(strip_bom(source), options),
            RasterFormat::Pdf => self.inner.render_pdf_sync(strip_bom(source)),
        };
        rendered
            .map_err(|error| format!("render failed: {error}"))?
            .ok_or_else(|| "render failed: no Mermaid diagram detected".to_string())
    }
}

/// The scale that fills a padded preview viewport while preserving the diagram's aspect ratio.
fn preview_scale(
    diagram_width: u32,
    diagram_height: u32,
    viewport_width: u32,
    viewport_height: u32,
    zoom: f32,
) -> f32 {
    let (fit_width, fit_height) = preview_fit_box(viewport_width, viewport_height);
    let fit = (fit_width as f32 / diagram_width.max(1) as f32)
        .min(fit_height as f32 / diagram_height.max(1) as f32);
    // Rounding a scale up can turn an exact fit into a one-pixel overflow.
    fit.next_down() * zoom.max(1.0)
}

/// The padded rectangle that a default preview is allowed to occupy.
///
/// A tiny terminal may not have room for the full margin, but raster dimensions must always stay
/// positive for the renderer.
fn preview_fit_box(width: u32, height: u32) -> (u32, u32) {
    let inset = PREVIEW_EDGE_PADDING * 2;
    (
        width.saturating_sub(inset).max(1),
        height.saturating_sub(inset).max(1),
    )
}

fn next_svg_diagram_id() -> String {
    let nonce = *SVG_PROCESS_NONCE.get_or_init(entropy_nonce);
    next_svg_diagram_id_with(nonce, &SVG_ID_SEQUENCE)
}

fn next_svg_diagram_id_with(nonce: u64, sequence: &AtomicU64) -> String {
    svg_diagram_id(nonce, sequence.fetch_add(1, Ordering::Relaxed))
}

fn svg_diagram_id(nonce: u64, sequence: u64) -> String {
    format!("gph-svg-{nonce:016x}-{sequence}")
}

fn entropy_nonce() -> u64 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(std::process::id().into());
    hasher.finish()
}

fn strip_bom(source: &str) -> &str {
    source.strip_prefix('\u{feff}').unwrap_or(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    const FLOWCHART: &str = "flowchart TD\n  A[Start] --> B[Done]\n";

    #[test]
    fn renders_svg() {
        let svg = Renderer::new().svg(FLOWCHART).unwrap();
        assert!(svg.starts_with("<svg"), "{svg}");
        assert!(svg.contains("Start"));
    }

    #[test]
    fn svg_ids_are_unique_for_distinct_process_nonces_and_prefix_definitions() {
        let first = svg_diagram_id(0x1234_5678_90ab_cdef, 0);
        let second = svg_diagram_id(0xfedc_ba09_8765_4321, 0);

        assert_ne!(first, second);
        assert_ne!(
            format!("{first}-drop-shadow"),
            format!("{second}-drop-shadow")
        );
        assert_ne!(
            format!("{first}_flowchart-v2-pointEnd"),
            format!("{second}_flowchart-v2-pointEnd")
        );
    }

    #[test]
    fn svg_ids_are_unique_within_a_process() {
        let sequence = AtomicU64::new(0);
        assert_ne!(
            next_svg_diagram_id_with(0x1234_5678_90ab_cdef, &sequence),
            next_svg_diagram_id_with(0x1234_5678_90ab_cdef, &sequence)
        );
    }

    #[test]
    fn svg_exports_have_unique_prefixed_definition_ids() {
        let renderer = Renderer::new();
        let first = renderer.svg(FLOWCHART).unwrap();
        let second = renderer.svg(FLOWCHART).unwrap();
        let first_id = root_id(&first);
        let second_id = root_id(&second);

        assert_ne!(first_id, second_id);
        for (svg, id) in [(&first, first_id), (&second, second_id)] {
            assert!(svg.contains(&format!("id=\"{id}-drop-shadow\"")));
            assert!(svg.contains(&format!("id=\"{id}_flowchart-v2-pointEnd\"")));
            assert!(svg.contains(&format!("url(#{id}_flowchart-v2-pointEnd)")));
        }
    }

    fn root_id(svg: &str) -> &str {
        let prefix = "<svg id=\"";
        let start = svg.find(prefix).expect("SVG root ID") + prefix.len();
        let end = svg[start..].find('\"').expect("end of SVG root ID") + start;
        &svg[start..end]
    }

    #[test]
    fn preview_fit_box_leaves_a_comfortable_margin() {
        assert_eq!(preview_fit_box(640, 480), (610, 450));
        // Keep preview rendering valid even in an unusually small terminal.
        assert_eq!(preview_fit_box(20, 20), (1, 1));
    }

    #[test]
    fn preview_scale_fills_the_padded_viewport_at_default_zoom() {
        let scale = preview_scale(100, 200, 640, 480, 1.0);
        assert!(scale > 2.24 && scale < 2.25);
        // The default fit enlarges small diagrams, while an explicit zoom still compounds it.
        assert!(scale > 1.0);
        assert!((preview_scale(100, 200, 640, 480, 1.25) - scale * 1.25).abs() < f32::EPSILON);
    }

    #[test]
    fn renders_png_for_preview() {
        let png = Renderer::new().png(FLOWCHART, 640, 480, 1.0).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let reader = decoder.read_info().unwrap();
        // The small diagram is enlarged until its height reaches the padded boundary.
        assert_eq!(reader.info().height, 450, "{:#?}", reader.info());
    }

    #[test]
    fn svg_uses_rose_pine_dawn_roles_with_a_transparent_background() {
        let svg = Renderer::new().svg(FLOWCHART).unwrap();
        let root = svg.split_once('>').expect("SVG root").0;
        assert!(root.starts_with("<svg "), "{root}");
        assert!(root.contains("style=\""), "{root}");
        assert!(root.contains("background-color: transparent;"), "{root}");
        assert!(svg.contains("#575279"), "{svg}");
        assert!(svg.contains("#286983"), "{svg}");
    }

    #[test]
    fn file_raster_scale_changes_png_dimensions() {
        let renderer = Renderer::new();
        let one = renderer
            .raster_with_scale(FLOWCHART, RasterFormat::Png, 1.0)
            .unwrap();
        let three = renderer
            .raster_with_scale(FLOWCHART, RasterFormat::Png, 3.0)
            .unwrap();
        let dimensions = |png: &[u8]| {
            (
                u32::from_be_bytes(png[16..20].try_into().unwrap()),
                u32::from_be_bytes(png[20..24].try_into().unwrap()),
            )
        };
        let (one_width, one_height) = dimensions(&one);
        let (three_width, three_height) = dimensions(&three);
        assert_eq!(three_width, one_width * 3);
        assert_eq!(three_height, one_height * 3);
    }

    #[test]
    fn preview_and_export_png_keep_a_transparent_background() {
        let renderer = Renderer::new();
        let preview = renderer.png(FLOWCHART, 640, 480, 1.0).unwrap();
        let exported = renderer.raster(FLOWCHART, RasterFormat::Png).unwrap();

        for png in [&preview, &exported] {
            let decoder = png::Decoder::new(std::io::Cursor::new(png));
            let mut reader = decoder.read_info().unwrap();
            let mut buf = vec![0; reader.output_buffer_size().unwrap()];
            let info = reader.next_frame(&mut buf).unwrap();
            assert_eq!(info.color_type, png::ColorType::Rgba);
            assert_eq!(buf[3], 0, "expected a transparent corner");
        }
    }

    #[test]
    fn zoom_enlarges_the_rendered_png() {
        let renderer = Renderer::new();
        let fit = renderer.png(FLOWCHART, 640, 480, 1.0).unwrap();
        let zoomed = renderer.png(FLOWCHART, 640, 480, 2.0).unwrap();
        let dims = |png: &[u8]| {
            (
                u32::from_be_bytes(png[16..20].try_into().unwrap()),
                u32::from_be_bytes(png[20..24].try_into().unwrap()),
            )
        };
        let (fit_width, fit_height) = dims(&fit);
        let (zoomed_width, zoomed_height) = dims(&zoomed);
        assert!(zoomed_width > fit_width, "{zoomed_width} vs {fit_width}");
        assert!(
            zoomed_height > fit_height,
            "{zoomed_height} vs {fit_height}"
        );
    }

    #[test]
    fn renders_bom_prefixed_svg() {
        let svg = Renderer::new()
            .svg(&format!("\u{feff}{FLOWCHART}"))
            .unwrap();
        assert!(svg.starts_with("<svg"), "{svg}");
    }

    #[test]
    fn renders_bom_prefixed_png() {
        let png = Renderer::new()
            .png(&format!("\u{feff}{FLOWCHART}"), 640, 480, 1.0)
            .unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn rejects_non_diagrams() {
        assert!(Renderer::new().svg("not Mermaid").is_err());
    }
}
