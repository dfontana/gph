use std::io;

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, window_size,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{TerminalOptions, Viewport};

use crate::kitty;

pub(crate) type PreviewTerminal = Terminal<CrosstermBackend<io::Stdout>>;

/// The raw-mode terminal state shared by the two interactive preview modes.
pub(crate) struct TerminalSession {
    terminal: PreviewTerminal,
    alternate_screen: bool,
}

impl TerminalSession {
    /// Start a fixed viewport below existing terminal output without changing screens.
    pub(crate) fn fixed(viewport: Rect) -> Result<Self, String> {
        Self::new(Viewport::Fixed(viewport), false)
    }

    /// Start a full-screen preview in the terminal's alternate screen.
    pub(crate) fn alternate() -> Result<Self, String> {
        Self::new(Viewport::Fullscreen, true)
    }

    fn new(viewport: Viewport, alternate_screen: bool) -> Result<Self, String> {
        enable_raw_mode().map_err(|error| format!("cannot enable raw mode: {error}"))?;
        let mut stdout = io::stdout();
        if alternate_screen && let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let cleanup = disable_raw_mode().map_err(|cleanup| cleanup.to_string());
            return terminal_initialization_error(
                format!("cannot enter alternate screen: {error}"),
                cleanup,
            );
        }
        // Mouse capture drives click-and-drag panning of a zoomed preview.
        if let Err(error) = execute!(stdout, EnableMouseCapture) {
            return terminal_initialization_error(
                format!("cannot capture the mouse: {error}"),
                restore_terminal(alternate_screen),
            );
        }

        let backend = CrosstermBackend::new(stdout);
        match Terminal::with_options(backend, TerminalOptions { viewport }) {
            Ok(terminal) => Ok(Self {
                terminal,
                alternate_screen,
            }),
            Err(error) => terminal_initialization_error(
                format!("cannot initialize terminal: {error}"),
                restore_terminal(alternate_screen),
            ),
        }
    }

    pub(crate) fn terminal(&mut self) -> &mut PreviewTerminal {
        &mut self.terminal
    }

    /// Remove this session's Kitty image and restore the terminal mode it changed.
    pub(crate) fn cleanup(&mut self, image: &mut PreviewImage) -> Result<(), String> {
        let image = image.cleanup(self.terminal.backend_mut());
        let terminal = restore_terminal(self.alternate_screen);
        combine(image, terminal)
    }
}

fn terminal_initialization_error(
    primary: String,
    cleanup: Result<(), String>,
) -> Result<TerminalSession, String> {
    match combine(Err(primary), cleanup) {
        Err(error) => Err(error),
        Ok(()) => unreachable!("terminal initialization must fail"),
    }
}

fn restore_terminal(alternate_screen: bool) -> Result<(), String> {
    let mut stdout = io::stdout();
    let mouse = execute!(stdout, DisableMouseCapture).map_err(|error| error.to_string());
    let screen = if alternate_screen {
        leave_alternate_screen(&mut stdout).map_err(|error| error.to_string())
    } else {
        Ok(())
    };
    let raw = disable_raw_mode().map_err(|error| error.to_string());
    combine(combine(mouse, screen), raw)
}

fn leave_alternate_screen(out: &mut impl io::Write) -> io::Result<()> {
    execute!(out, LeaveAlternateScreen)
}

/// Smallest and largest magnification the preview allows, and the per-keystroke ratio.
const ZOOM_MIN: f32 = 1.0;
const ZOOM_MAX: f32 = 8.0;
const ZOOM_STEP: f32 = 1.25;

/// The Kitty image currently owned by one preview session.
pub(crate) struct PreviewImage {
    id: kitty::ImageId,
    png: Option<Vec<u8>>,
    displayed: bool,
    dirty: bool,
    zoom: f32,
    /// How far the visible window is panned from center, in whole cells.
    pan: (i32, i32),
    /// Largest pan the current placement allows; refreshed on each display.
    pan_limit: (i32, i32),
    /// Whether the current pixels still need transmitting to Kitty. Panning leaves
    /// this false so the image is only re-placed, never re-uploaded (no flicker).
    needs_upload: bool,
}

impl PreviewImage {
    pub(crate) fn new() -> Self {
        Self {
            id: kitty::new_image_id(),
            png: None,
            displayed: false,
            dirty: true,
            zoom: ZOOM_MIN,
            pan: (0, 0),
            pan_limit: (0, 0),
            needs_upload: false,
        }
    }

    /// The magnification the source should be rasterized at; 1.0 fits the pane.
    pub(crate) fn zoom(&self) -> f32 {
        self.zoom
    }

    /// Slide the visible window by a mouse drag of `(dx, dy)` cells, moving the diagram
    /// with the cursor and stopping at the image edges. Reports whether the view moved so
    /// the caller can redraw only when it must.
    pub(crate) fn pan(&mut self, dx: i32, dy: i32) -> bool {
        let (limit_x, limit_y) = self.pan_limit;
        let pan = (
            (self.pan.0 - dx).clamp(-limit_x, limit_x),
            (self.pan.1 - dy).clamp(-limit_y, limit_y),
        );
        if pan == self.pan {
            return false;
        }
        self.pan = pan;
        self.dirty = true;
        true
    }

    /// Apply a zoom action, reporting whether it changed the magnification so the
    /// caller can re-render only when it must.
    pub(crate) fn apply_zoom(&mut self, action: ZoomAction) -> bool {
        let zoom = match action {
            ZoomAction::In => self.zoom * ZOOM_STEP,
            ZoomAction::Out => self.zoom / ZOOM_STEP,
            ZoomAction::Reset => ZOOM_MIN,
        };
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        // Snap back to an exact fit so leaving zoom always restores the untouched preview.
        let zoom = if (zoom - ZOOM_MIN).abs() < 1e-3 {
            ZOOM_MIN
        } else {
            zoom
        };
        if (zoom - self.zoom).abs() < 1e-6 {
            return false;
        }
        self.zoom = zoom;
        // A fit leaves no room to pan, so recenter for the next zoom-in.
        if zoom == ZOOM_MIN {
            self.pan = (0, 0);
        }
        self.dirty = true;
        true
    }

    pub(crate) fn show(&mut self, png: Vec<u8>) {
        let changed = self.png.as_ref() != Some(&png);
        self.dirty |= changed;
        // New pixels must be transmitted; identical ones can be re-placed as-is.
        self.needs_upload |= changed;
        self.png = Some(png);
    }

    pub(crate) fn clear(&mut self) {
        if self.png.take().is_some() {
            self.dirty = true;
            self.needs_upload = true;
        }
    }

    pub(crate) fn png(&self) -> Option<&[u8]> {
        self.png.as_deref()
    }

    /// Redraw after the terminal has invalidated graphics, such as after a resize.
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
        // A resize or graphics reset drops Kitty's copy, so the pixels must be re-sent.
        self.needs_upload = true;
    }

    /// Display the current image even when its bytes have not changed.
    pub(crate) fn display(&mut self, pane: Rect, out: &mut impl io::Write) -> io::Result<()> {
        self.display_inner(pane, out)?;
        self.dirty = false;
        Ok(())
    }

    /// Display or clear the image only after its contents or placement changed.
    pub(crate) fn display_if_dirty(
        &mut self,
        pane: Rect,
        out: &mut impl io::Write,
    ) -> io::Result<()> {
        if self.dirty {
            self.display(pane, out)?;
        }
        Ok(())
    }

    fn display_inner(&mut self, pane: Rect, out: &mut impl io::Write) -> io::Result<()> {
        if pane.width == 0 || pane.height == 0 {
            if self.displayed {
                kitty::delete_image(self.id, out)?;
                self.displayed = false;
                // The image data is gone, so its next appearance must re-upload.
                self.needs_upload = true;
            }
            return Ok(());
        }

        match self.png() {
            Some(png) => {
                let placement = kitty::place_png(pane, png, self.pan);
                if self.needs_upload {
                    kitty::display_png(self.id, png, placement.cell, placement.crop, out)?;
                    self.needs_upload = false;
                } else {
                    // Pixels are already uploaded; just slide the visible window.
                    kitty::place_image(self.id, placement.cell, placement.crop, out)?;
                }
                self.pan_limit = placement.pan_limit;
                self.displayed = true;
            }
            None if self.displayed => {
                kitty::delete_image(self.id, out)?;
                self.displayed = false;
            }
            None => {}
        }
        Ok(())
    }

    fn cleanup(&mut self, out: &mut impl io::Write) -> Result<(), String> {
        kitty::delete_image(self.id, out).map_err(|error| error.to_string())
    }
}

/// A magnification change requested from the keyboard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ZoomAction {
    In,
    Out,
    Reset,
}

/// Map a key event to a zoom action: `+`/`=` in, `-`/`_` out, `0` back to fit.
pub(crate) fn zoom_action(event: &Event) -> Option<ZoomAction> {
    let Event::Key(KeyEvent { code, .. }) = event else {
        return None;
    };
    match code {
        KeyCode::Char('+') | KeyCode::Char('=') => Some(ZoomAction::In),
        KeyCode::Char('-') | KeyCode::Char('_') => Some(ZoomAction::Out),
        KeyCode::Char('0') => Some(ZoomAction::Reset),
        _ => None,
    }
}

/// Turns a left-button mouse drag into per-move cell deltas for panning.
///
/// Every terminal event is fed through `delta`; it remembers where the button went
/// down and reports how far each subsequent drag moved, in whole cells.
#[derive(Default)]
pub(crate) struct DragTracker {
    last: Option<(u16, u16)>,
}

impl DragTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The cell delta of an in-progress left-drag, or `None` for any other event.
    pub(crate) fn delta(&mut self, event: &Event) -> Option<(i32, i32)> {
        let Event::Mouse(MouseEvent {
            kind, column, row, ..
        }) = event
        else {
            return None;
        };
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.last = Some((*column, *row));
                None
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let (last_column, last_row) = self.last?;
                self.last = Some((*column, *row));
                let delta = (
                    i32::from(*column) - i32::from(last_column),
                    i32::from(*row) - i32::from(last_row),
                );
                (delta != (0, 0)).then_some(delta)
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.last = None;
                None
            }
            _ => None,
        }
    }
}

/// The pan delta, in cells, for a scroll-wheel or trackpad event, or `None` for
/// any other event.
///
/// Scrolling moves the view the way scrolling a document does — a wheel-down
/// reveals the lower part of the diagram, opposite a same-direction drag.
/// Holding Shift turns a vertical wheel into horizontal panning for mice that
/// lack a tilt wheel.
pub(crate) fn scroll_delta(event: &Event) -> Option<(i32, i32)> {
    /// Cells moved per scroll event. One keeps a trackpad's flurry of events
    /// feeling smooth, at the cost of a mouse wheel travelling a cell per notch.
    const STEP: i32 = 1;
    let Event::Mouse(MouseEvent {
        kind, modifiers, ..
    }) = event
    else {
        return None;
    };
    let vertical = match kind {
        MouseEventKind::ScrollDown => -STEP,
        MouseEventKind::ScrollUp => STEP,
        MouseEventKind::ScrollRight => return Some((-STEP, 0)),
        MouseEventKind::ScrollLeft => return Some((STEP, 0)),
        _ => return None,
    };
    if modifiers.contains(KeyModifiers::SHIFT) {
        Some((vertical, 0))
    } else {
        Some((0, vertical))
    }
}

pub(crate) fn pane_pixels(pane: Rect) -> (u32, u32) {
    let fallback = (
        (u32::from(pane.width) * 8).max(1),
        (u32::from(pane.height) * 16).max(1),
    );
    let Ok(size) = window_size() else {
        return fallback;
    };
    if size.columns == 0 || size.rows == 0 || size.width == 0 || size.height == 0 {
        return fallback;
    }
    (
        (u32::from(size.width) * u32::from(pane.width) / u32::from(size.columns)).max(1),
        (u32::from(size.height) * u32::from(pane.height) / u32::from(size.rows)).max(1),
    )
}

pub(crate) fn is_quit_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Char('c') | KeyCode::Char('q'),
            modifiers: KeyModifiers::CONTROL,
            ..
        })
    )
}

pub(crate) fn combine(
    primary: Result<(), String>,
    cleanup: Result<(), String>,
) -> Result<(), String> {
    combine_with_context(primary, cleanup, "cleanup")
}

pub(crate) fn combine_with_context(
    primary: Result<(), String>,
    cleanup: Result<(), String>,
    context: &str,
) -> Result<(), String> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(format!("{primary}; {context} failed: {cleanup}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_events_require_control_c_or_q() {
        let quit = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        let plain = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        let other = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));

        assert!(is_quit_event(&quit));
        assert!(!is_quit_event(&plain));
        assert!(!is_quit_event(&other));
    }

    #[test]
    fn zoom_keys_map_to_their_actions() {
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        assert_eq!(zoom_action(&key(KeyCode::Char('+'))), Some(ZoomAction::In));
        assert_eq!(zoom_action(&key(KeyCode::Char('='))), Some(ZoomAction::In));
        assert_eq!(zoom_action(&key(KeyCode::Char('-'))), Some(ZoomAction::Out));
        assert_eq!(zoom_action(&key(KeyCode::Char('_'))), Some(ZoomAction::Out));
        assert_eq!(
            zoom_action(&key(KeyCode::Char('0'))),
            Some(ZoomAction::Reset)
        );
        assert_eq!(zoom_action(&key(KeyCode::Char('x'))), None);
    }

    #[test]
    fn zoom_steps_clamp_and_snap_back_to_a_fit() {
        let mut image = PreviewImage::new();
        assert_eq!(image.zoom(), 1.0);
        // Already at the minimum, so zooming out is a no-op.
        assert!(!image.apply_zoom(ZoomAction::Out));

        assert!(image.apply_zoom(ZoomAction::In));
        assert!(image.zoom() > 1.0);

        for _ in 0..64 {
            image.apply_zoom(ZoomAction::In);
        }
        assert_eq!(image.zoom(), ZOOM_MAX);
        assert!(!image.apply_zoom(ZoomAction::In));

        assert!(image.apply_zoom(ZoomAction::Reset));
        assert_eq!(image.zoom(), 1.0);

        // A single step out from the fit snaps exactly back to it.
        image.apply_zoom(ZoomAction::In);
        assert!(image.apply_zoom(ZoomAction::Out));
        assert_eq!(image.zoom(), 1.0);
    }

    #[test]
    fn a_left_drag_reports_cell_deltas_between_moves() {
        let mouse = |kind, column, row| {
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        let mut drag = DragTracker::new();

        assert_eq!(
            drag.delta(&mouse(MouseEventKind::Down(MouseButton::Left), 10, 5)),
            None
        );
        assert_eq!(
            drag.delta(&mouse(MouseEventKind::Drag(MouseButton::Left), 13, 4)),
            Some((3, -1))
        );
        // Deltas are measured from the previous move, not the button-down point.
        assert_eq!(
            drag.delta(&mouse(MouseEventKind::Drag(MouseButton::Left), 13, 4)),
            None
        );
        assert_eq!(
            drag.delta(&mouse(MouseEventKind::Drag(MouseButton::Left), 11, 4)),
            Some((-2, 0))
        );
        // Releasing ends the drag, so a later move without a press reports nothing.
        assert_eq!(
            drag.delta(&mouse(MouseEventKind::Up(MouseButton::Left), 11, 4)),
            None
        );
        assert_eq!(
            drag.delta(&mouse(MouseEventKind::Drag(MouseButton::Left), 20, 20)),
            None
        );
    }

    #[test]
    fn scrolling_pans_along_the_wheel_axis_and_shift_flips_it_horizontal() {
        let scroll = |kind, modifiers| {
            Event::Mouse(MouseEvent {
                kind,
                column: 0,
                row: 0,
                modifiers,
            })
        };

        // A wheel-down scrolls the view down the diagram, opposite a downward drag.
        assert_eq!(
            scroll_delta(&scroll(MouseEventKind::ScrollDown, KeyModifiers::NONE)),
            Some((0, -1))
        );
        assert_eq!(
            scroll_delta(&scroll(MouseEventKind::ScrollUp, KeyModifiers::NONE)),
            Some((0, 1))
        );
        // A tilt wheel (or trackpad) pans horizontally on its own.
        assert_eq!(
            scroll_delta(&scroll(MouseEventKind::ScrollRight, KeyModifiers::NONE)),
            Some((-1, 0))
        );
        // Shift redirects a vertical wheel into horizontal panning.
        assert_eq!(
            scroll_delta(&scroll(MouseEventKind::ScrollDown, KeyModifiers::SHIFT)),
            Some((-1, 0))
        );
        // Non-scroll mouse events and key events yield no pan.
        assert_eq!(
            scroll_delta(&scroll(MouseEventKind::Moved, KeyModifiers::NONE)),
            None
        );
        assert_eq!(scroll_delta(&Event::FocusGained), None);
    }

    #[test]
    fn panning_moves_the_view_opposite_the_drag_and_clamps_to_the_limit() {
        let mut image = PreviewImage::new();
        // No placement has run, so there is no room to pan yet.
        assert!(!image.pan(3, 3));

        image.apply_zoom(ZoomAction::In);
        image.pan_limit = (5, 5);
        // Dragging right and down moves the window left and up (the diagram follows the cursor).
        assert!(image.pan(2, 1));
        assert_eq!(image.pan, (-2, -1));
        // Panning stops at the limit rather than accumulating past it.
        assert!(image.pan(-99, -99));
        assert_eq!(image.pan, (5, 5));
        assert!(!image.pan(-99, -99));

        // Returning to a fit recenters the view for the next zoom-in.
        assert!(image.apply_zoom(ZoomAction::Reset));
        assert_eq!(image.pan, (0, 0));
    }

    #[test]
    fn re_displaying_an_unchanged_image_replaces_it_without_re_uploading() {
        let mut image = PreviewImage::new();
        let pane = Rect::new(2, 3, 20, 10);

        let mut first = Vec::new();
        image.show(b"png".to_vec());
        image.display(pane, &mut first).unwrap();
        assert!(
            String::from_utf8(first)
                .unwrap()
                .contains("a=t,f=100,t=d,i=")
        );

        // A second display of the same pixels (as panning does) only re-places them.
        let mut second = Vec::new();
        image.display(pane, &mut second).unwrap();
        let text = String::from_utf8(second).unwrap();
        assert!(text.contains("a=p,i="));
        assert!(!text.contains("a=t"));
        assert!(!text.contains("a=d,d=I"));
    }

    #[test]
    fn combines_primary_and_cleanup_errors() {
        assert_eq!(
            combine(Err("primary".to_string()), Err("cleanup".to_string())),
            Err("primary; cleanup failed: cleanup".to_string())
        );
    }

    #[test]
    fn alternate_sessions_emit_the_leave_screen_escape_sequence() {
        let mut output = Vec::new();
        leave_alternate_screen(&mut output).unwrap();
        assert_eq!(output, b"\x1b[?1049l");
    }

    #[test]
    fn image_lifecycle_skips_unchanged_uploads_and_clears_displayed_images() {
        let mut image = PreviewImage::new();
        let pane = Rect::new(2, 3, 20, 10);
        let mut output = Vec::new();

        image.show(b"png".to_vec());
        image.display_if_dirty(pane, &mut output).unwrap();
        let uploaded = output.len();
        image.display_if_dirty(pane, &mut output).unwrap();
        assert_eq!(output.len(), uploaded);

        image.clear();
        image.display_if_dirty(pane, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("a=t,f=100,t=d,i="));
        assert!(output.matches("a=d,d=I,i=").count() >= 2);
    }
}
