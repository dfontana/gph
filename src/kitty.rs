use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ratatui::layout::Rect;

const PLACEMENT_ID: u32 = 1;
const CHUNK_SIZE: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageId(u32);

pub fn new_image_id() -> ImageId {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(std::process::id().into());
    ImageId((hasher.finish() as u32).max(1))
}

pub fn is_available() -> bool {
    is_available_with(
        std::env::var_os("KITTY_WINDOW_ID").as_deref(),
        std::env::var_os("TERM").as_deref(),
        std::env::var_os("TERM_PROGRAM").as_deref(),
    )
}

fn is_available_with(
    kitty_window_id: Option<&std::ffi::OsStr>,
    term: Option<&std::ffi::OsStr>,
    term_program: Option<&std::ffi::OsStr>,
) -> bool {
    kitty_window_id.is_some_and(|id| !id.is_empty())
        || term == Some(std::ffi::OsStr::new("xterm-kitty"))
        // Multiplexers such as zellij rewrite TERM but leave TERM_PROGRAM intact.
        || term_program == Some(std::ffi::OsStr::new("kitty"))
}

/// Remove only this preview instance's image, never Kitty's whole image store.
pub fn delete_image(image_id: ImageId, out: &mut impl Write) -> io::Result<()> {
    write!(out, "\x1b_Ga=d,d=I,i={},q=2;\x1b\\", image_id.0)?;
    out.flush()
}

/// A source-pixel rectangle cropped out of a PNG before it is placed.
///
/// Kitty displays only this window of the image, letting an oversized (zoomed)
/// render fill the pane while its off-pane pixels are clipped instead of spilling
/// past the border.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Where a PNG lands in the pane: the cell rectangle to draw into, plus the source
/// window to draw from when the image is larger than the pane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Placement {
    pub cell: Rect,
    pub crop: Option<Crop>,
}

/// Center a PNG within `pane`, cropping to the pane when the image overflows it.
///
/// Kitty places a natural-size image from the cursor cell. A preview rendered at
/// zoom 1 is never larger than its pane, so it is simply centered; a zoomed render
/// can exceed the pane, so its centered pane-sized window is cropped out and the rest
/// clipped.
pub fn place_png(pane: Rect, png: &[u8]) -> Placement {
    let uncropped = Placement {
        cell: pane,
        crop: None,
    };
    let Some((image_width, image_height)) = png_dimensions(png) else {
        return uncropped;
    };
    let Ok(terminal) = crossterm::terminal::window_size() else {
        return uncropped;
    };
    if terminal.columns == 0 || terminal.rows == 0 || terminal.width == 0 || terminal.height == 0 {
        return uncropped;
    }
    let cell_width = u32::from(terminal.width) / u32::from(terminal.columns);
    let cell_height = u32::from(terminal.height) / u32::from(terminal.rows);
    place(pane, image_width, image_height, cell_width, cell_height)
}

fn place(
    pane: Rect,
    image_width: u32,
    image_height: u32,
    cell_width: u32,
    cell_height: u32,
) -> Placement {
    if cell_width == 0 || cell_height == 0 {
        return Placement {
            cell: pane,
            crop: None,
        };
    }
    let full_columns = image_width.div_ceil(cell_width);
    let full_rows = image_height.div_ceil(cell_height);
    let columns = full_columns.min(u32::from(pane.width));
    let rows = full_rows.min(u32::from(pane.height));
    let cell = Rect::new(
        pane.x + (pane.width - columns as u16) / 2,
        pane.y + (pane.height - rows as u16) / 2,
        columns as u16,
        rows as u16,
    );
    let crop =
        (full_columns > u32::from(pane.width) || full_rows > u32::from(pane.height)).then(|| {
            // Snap the window to whole cells so the visible pixels map one-to-one and
            // stay crisp, then clamp to the image for panes that outsize it.
            let width = (columns * cell_width).min(image_width);
            let height = (rows * cell_height).min(image_height);
            Crop {
                x: (image_width - width) / 2,
                y: (image_height - height) / 2,
                width,
                height,
            }
        });
    Placement { cell, crop }
}

/// Replace this preview instance's image and place it at its natural aspect ratio.
///
/// The caller rasterizes the image to fit (or overfill, when zoomed) the pane. Omitting
/// Kitty's cell dimensions lets Kitty derive the placement from the PNG's pixels instead
/// of stretching it to the pane's (usually different) aspect ratio; `crop` restricts the
/// drawn source window so a zoomed image never spills past `cell`.
pub fn display_png(
    image_id: ImageId,
    png: &[u8],
    cell: Rect,
    crop: Option<Crop>,
    out: &mut impl Write,
) -> io::Result<()> {
    if cell.width == 0 || cell.height == 0 {
        return Ok(());
    }

    delete_image(image_id, out)?;
    upload_png(image_id, png, out)?;
    let mut command = format!("a=p,i={},p={PLACEMENT_ID},C=1,q=2", image_id.0);
    if let Some(crop) = crop {
        use std::fmt::Write as _;
        let _ = write!(
            command,
            ",x={},y={},w={},h={}",
            crop.x, crop.y, crop.width, crop.height
        );
    }
    write!(
        out,
        "\x1b[s\x1b[{};{}H\x1b_G{command};\x1b\\\x1b[u",
        cell.y + 1,
        cell.x + 1,
    )?;
    out.flush()
}

fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
    if png.get(..8)? != PNG_SIGNATURE {
        return None;
    }
    let width = u32::from_be_bytes(png.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(png.get(20..24)?.try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

fn upload_png(image_id: ImageId, png: &[u8], out: &mut impl Write) -> io::Result<()> {
    let encoded = STANDARD.encode(png);
    let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(CHUNK_SIZE).collect();
    debug_assert!(!chunks.is_empty());

    for (index, chunk) in chunks.iter().enumerate() {
        let more = u8::from(index + 1 < chunks.len());
        if index == 0 {
            write!(out, "\x1b_Ga=t,f=100,t=d,i={},m={more},q=2;", image_id.0)?;
        } else {
            write!(out, "\x1b_Gm={more},q=2;")?;
        }
        out.write_all(chunk)?;
        out.write_all(b"\x1b\\")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST_PREVIEW_IMAGE: ImageId = ImageId(101);
    const SECOND_PREVIEW_IMAGE: ImageId = ImageId(202);

    #[test]
    fn accepts_kitty_term_without_window_id() {
        assert!(is_available_with(
            None,
            Some(std::ffi::OsStr::new("xterm-kitty")),
            None,
        ));
    }

    #[test]
    fn accepts_kitty_term_program_when_multiplexer_rewrites_term() {
        assert!(is_available_with(
            None,
            Some(std::ffi::OsStr::new("xterm-256color")),
            Some(std::ffi::OsStr::new("kitty")),
        ));
    }

    #[test]
    fn rejects_non_kitty_term_without_window_id() {
        assert!(!is_available_with(
            None,
            Some(std::ffi::OsStr::new("xterm-256color")),
            None,
        ));
    }

    #[test]
    fn generates_nonzero_preview_image_ids() {
        assert_ne!(new_image_id().0, 0);
    }

    #[test]
    fn centers_natural_size_pngs_in_the_available_pane() {
        assert_eq!(
            place(Rect::new(10, 5, 80, 20), 160, 80, 8, 16),
            Placement {
                cell: Rect::new(40, 12, 20, 5),
                crop: None,
            },
        );
    }

    #[test]
    fn oversized_pngs_fill_the_pane_and_crop_their_overflow_to_its_center() {
        assert_eq!(
            place(Rect::new(10, 5, 80, 20), 2_000, 800, 8, 16),
            Placement {
                cell: Rect::new(10, 5, 80, 20),
                // 80 cols * 8px = 640, 20 rows * 16px = 320, centered in a 2000x800 image.
                crop: Some(Crop {
                    x: 680,
                    y: 240,
                    width: 640,
                    height: 320,
                }),
            },
        );
    }

    #[test]
    fn displays_a_png_at_its_natural_aspect_ratio_in_the_requested_pane() {
        let mut bytes = Vec::new();
        display_png(
            FIRST_PREVIEW_IMAGE,
            b"png",
            Rect::new(42, 3, 50, 18),
            None,
            &mut bytes,
        )
        .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("a=d,d=I,i=101"));
        assert!(text.contains("a=t,f=100,t=d,i=101,m=0"));
        assert!(text.contains("\x1b[4;43H"));
        assert!(text.contains("a=p,i=101,p=1,C=1,q=2"));
        assert!(!text.contains(",c="));
        assert!(!text.contains(",r="));
        assert!(!text.contains(",w="));
        assert!(!text.contains(",h="));
        assert_all_commands_suppress_responses(&text);
    }

    #[test]
    fn cropped_placements_emit_the_source_window() {
        let mut bytes = Vec::new();
        display_png(
            FIRST_PREVIEW_IMAGE,
            b"png",
            Rect::new(10, 5, 80, 20),
            Some(Crop {
                x: 680,
                y: 240,
                width: 640,
                height: 320,
            }),
            &mut bytes,
        )
        .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("a=p,i=101,p=1,C=1,q=2,x=680,y=240,w=640,h=320"));
        assert_all_commands_suppress_responses(&text);
    }

    #[test]
    fn preview_protocol_commands_are_isolated_by_image_id() {
        let mut first = Vec::new();
        display_png(
            FIRST_PREVIEW_IMAGE,
            b"png",
            Rect::new(0, 0, 1, 1),
            None,
            &mut first,
        )
        .unwrap();
        delete_image(FIRST_PREVIEW_IMAGE, &mut first).unwrap();

        let mut second = Vec::new();
        display_png(
            SECOND_PREVIEW_IMAGE,
            b"png",
            Rect::new(1, 1, 2, 3),
            None,
            &mut second,
        )
        .unwrap();
        delete_image(SECOND_PREVIEW_IMAGE, &mut second).unwrap();

        let first = String::from_utf8(first).unwrap();
        let second = String::from_utf8(second).unwrap();
        assert_protocol_uses_only(&first, FIRST_PREVIEW_IMAGE, SECOND_PREVIEW_IMAGE);
        assert_protocol_uses_only(&second, SECOND_PREVIEW_IMAGE, FIRST_PREVIEW_IMAGE);
    }

    #[test]
    fn splits_large_payloads_without_global_deletion() {
        let mut bytes = Vec::new();
        display_png(
            FIRST_PREVIEW_IMAGE,
            &vec![0; 4_000],
            Rect::new(0, 0, 1, 1),
            None,
            &mut bytes,
        )
        .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("m=1,q=2;"));
        assert!(text.contains("\x1b_Gm=0,q=2;"));
        assert!(!text.contains("d=A"));
        assert_all_commands_suppress_responses(&text);
    }

    fn assert_protocol_uses_only(output: &str, image_id: ImageId, other_image_id: ImageId) {
        assert!(output.contains(&format!("a=d,d=I,i={}", image_id.0)));
        assert!(output.contains(&format!("a=t,f=100,t=d,i={}", image_id.0)));
        assert!(output.contains(&format!("a=p,i={}", image_id.0)));
        assert!(!output.contains(&format!("i={}", other_image_id.0)));
        assert!(!output.contains("d=A"));
        assert_all_commands_suppress_responses(output);
    }

    fn assert_all_commands_suppress_responses(output: &str) {
        for command in output.split("\x1b_G").skip(1) {
            let command = command.split("\x1b\\").next().unwrap();
            assert!(command.contains("q=2"), "missing q=2 in {command:?}");
        }
    }
}
