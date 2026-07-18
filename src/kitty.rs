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
    )
}

fn is_available_with(
    kitty_window_id: Option<&std::ffi::OsStr>,
    term: Option<&std::ffi::OsStr>,
) -> bool {
    kitty_window_id.is_some_and(|id| !id.is_empty())
        || term == Some(std::ffi::OsStr::new("xterm-kitty"))
}

/// Remove only this editor instance's image, never Kitty's whole image store.
pub fn delete_image(image_id: ImageId, out: &mut impl Write) -> io::Result<()> {
    write!(out, "\x1b_Ga=d,d=I,i={},q=2;\x1b\\", image_id.0)?;
    out.flush()
}

/// Replace this editor instance's image and place it in `pane` after Ratatui draws its frame.
pub fn display_png(
    image_id: ImageId,
    png: &[u8],
    pane: Rect,
    out: &mut impl Write,
) -> io::Result<()> {
    if pane.width == 0 || pane.height == 0 {
        return Ok(());
    }

    delete_image(image_id, out)?;
    upload_png(image_id, png, out)?;
    write!(
        out,
        "\x1b[s\x1b[{};{}H\x1b_Ga=p,i={},p={PLACEMENT_ID},C=1,c={},r={},q=2;\x1b\\\x1b[u",
        pane.y + 1,
        pane.x + 1,
        image_id.0,
        pane.width,
        pane.height,
    )?;
    out.flush()
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

    const FIRST_EDITOR_IMAGE: ImageId = ImageId(101);
    const SECOND_EDITOR_IMAGE: ImageId = ImageId(202);

    #[test]
    fn accepts_kitty_term_without_window_id() {
        assert!(is_available_with(
            None,
            Some(std::ffi::OsStr::new("xterm-kitty")),
        ));
    }

    #[test]
    fn rejects_non_kitty_term_without_window_id() {
        assert!(!is_available_with(
            None,
            Some(std::ffi::OsStr::new("xterm-256color")),
        ));
    }

    #[test]
    fn generates_nonzero_editor_image_ids() {
        assert_ne!(new_image_id().0, 0);
    }

    #[test]
    fn displays_a_png_in_the_requested_pane_with_cell_bounds() {
        let mut bytes = Vec::new();
        display_png(
            FIRST_EDITOR_IMAGE,
            b"png",
            Rect::new(42, 3, 50, 18),
            &mut bytes,
        )
        .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("a=d,d=I,i=101"));
        assert!(text.contains("a=t,f=100,t=d,i=101,m=0"));
        assert!(text.contains("\x1b[4;43H"));
        assert!(text.contains("a=p,i=101,p=1,C=1,c=50,r=18,q=2"));
        assert_all_commands_suppress_responses(&text);
    }

    #[test]
    fn editor_protocol_commands_are_isolated_by_image_id() {
        let mut first = Vec::new();
        display_png(
            FIRST_EDITOR_IMAGE,
            b"png",
            Rect::new(0, 0, 1, 1),
            &mut first,
        )
        .unwrap();
        delete_image(FIRST_EDITOR_IMAGE, &mut first).unwrap();

        let mut second = Vec::new();
        display_png(
            SECOND_EDITOR_IMAGE,
            b"png",
            Rect::new(1, 1, 2, 3),
            &mut second,
        )
        .unwrap();
        delete_image(SECOND_EDITOR_IMAGE, &mut second).unwrap();

        let first = String::from_utf8(first).unwrap();
        let second = String::from_utf8(second).unwrap();
        assert_protocol_uses_only(&first, FIRST_EDITOR_IMAGE, SECOND_EDITOR_IMAGE);
        assert_protocol_uses_only(&second, SECOND_EDITOR_IMAGE, FIRST_EDITOR_IMAGE);
    }

    #[test]
    fn splits_large_payloads_without_global_deletion() {
        let mut bytes = Vec::new();
        display_png(
            FIRST_EDITOR_IMAGE,
            &vec![0; 4_000],
            Rect::new(0, 0, 1, 1),
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
