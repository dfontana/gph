use std::io;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
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
    let screen = if alternate_screen {
        let mut stdout = io::stdout();
        leave_alternate_screen(&mut stdout).map_err(|error| error.to_string())
    } else {
        Ok(())
    };
    let raw = disable_raw_mode().map_err(|error| error.to_string());
    combine(screen, raw)
}

fn leave_alternate_screen(out: &mut impl io::Write) -> io::Result<()> {
    execute!(out, LeaveAlternateScreen)
}

/// The Kitty image currently owned by one preview session.
pub(crate) struct PreviewImage {
    id: kitty::ImageId,
    png: Option<Vec<u8>>,
    displayed: bool,
    dirty: bool,
}

impl PreviewImage {
    pub(crate) fn new() -> Self {
        Self {
            id: kitty::new_image_id(),
            png: None,
            displayed: false,
            dirty: true,
        }
    }

    pub(crate) fn show(&mut self, png: Vec<u8>) {
        self.dirty |= self.png.as_ref() != Some(&png);
        self.png = Some(png);
    }

    pub(crate) fn clear(&mut self) {
        self.dirty |= self.png.take().is_some();
    }

    pub(crate) fn png(&self) -> Option<&[u8]> {
        self.png.as_deref()
    }

    /// Redraw after the terminal has invalidated graphics, such as after a resize.
    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
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
            }
            return Ok(());
        }

        match self.png() {
            Some(png) => {
                kitty::display_png(self.id, png, kitty::centered_pane(pane, png), out)?;
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
