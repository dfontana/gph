pub mod files;

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};

use merman::render::{
    HeadlessRenderer, RootBackgroundPostprocessor, SvgPipeline,
    raster::{RasterFitBox, RasterOptions},
};

const PREVIEW_DIAGRAM_ID: &str = "gph-preview";
/// Every output path renders the diagram over a transparent page background so previews and
/// exports composite cleanly onto the terminal, an editor pane, or another document. JPEG has
/// no alpha channel, so its raster fill still flattens this onto opaque white.
const TRANSPARENT_BACKGROUND: &str = "transparent";

pub enum RasterFormat {
    Png,
    Jpeg,
    Pdf,
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
                .with_diagram_id(PREVIEW_DIAGRAM_ID)
                // Drives the raster paths (PNG/JPEG/PDF), which render this pipeline before
                // encoding. The SVG path uses its own pipeline in `svg()` below.
                .with_svg_pipeline(transparent_background(SvgPipeline::resvg_safe())),
        }
    }

    pub fn svg(&self, source: &str) -> Result<String, String> {
        self.inner
            .clone()
            .with_diagram_id(&next_svg_diagram_id())
            // A parity preset leaves the SVG untouched apart from the transparent background,
            // rather than applying the raster-oriented `resvg_safe` cleanups to an SVG export.
            .render_svg_with_pipeline_sync(
                strip_bom(source),
                &transparent_background(SvgPipeline::parity()),
            )
            .map_err(|error| format!("render failed: {error}"))?
            .ok_or_else(|| "render failed: no Mermaid diagram detected".to_string())
    }

    /// Rasterize `source` to fit `width` x `height`, magnified by `zoom`.
    ///
    /// The fit box only ever shrinks an oversized diagram to the pane; `zoom` is applied
    /// as the raster scale, which is what actually enlarges the preview past its fitted
    /// (or natural) size so callers can zoom in on detail.
    pub fn png(&self, source: &str, width: u32, height: u32, zoom: f32) -> Result<Vec<u8>, String> {
        let options = RasterOptions::default()
            .with_fit_to(RasterFitBox::contain(width.max(1), height.max(1)))
            .with_scale(zoom.max(1.0));
        self.raster_with_options(source, RasterFormat::Png, &options)
    }

    pub fn raster(&self, source: &str, format: RasterFormat) -> Result<Vec<u8>, String> {
        self.raster_with_options(source, format, &RasterOptions::default())
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

/// Appends the transparent-background rewrite to `pipeline`, overriding the theme's default page
/// fill (typically opaque white) on the SVG root.
fn transparent_background(pipeline: SvgPipeline) -> SvgPipeline {
    pipeline.with_postprocessor(RootBackgroundPostprocessor::new(TRANSPARENT_BACKGROUND))
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
    fn renders_png_for_preview() {
        let png = Renderer::new().png(FLOWCHART, 640, 480, 1.0).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn svg_background_is_transparent() {
        let svg = Renderer::new().svg(FLOWCHART).unwrap();
        assert!(svg.contains("background-color: transparent"), "{svg}");
        assert!(!svg.contains("background-color: white"), "{svg}");
    }

    #[test]
    fn png_background_is_transparent() {
        let png = Renderer::new().png(FLOWCHART, 640, 480, 1.0).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgba);
        // The top-left corner sits in the page margin, so a transparent background leaves its
        // alpha at 0 rather than the theme's opaque white.
        let top_left_alpha = buf[3];
        assert_eq!(top_left_alpha, 0, "expected a transparent corner");
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
