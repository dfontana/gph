use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crossterm::cursor;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, window_size};
use notify::{Event as NotifyEvent, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{TerminalOptions, Viewport};

use crate::kitty;
use crate::render::Renderer;

const CHANGE_DEBOUNCE: Duration = Duration::from_millis(150);

struct State {
    path: PathBuf,
    renderer: Renderer,
    image_id: kitty::ImageId,
    preview: Option<Vec<u8>>,
    error: Option<String>,
    viewport_top: u16,
    viewport: Rect,
}

impl State {
    fn new(path: PathBuf, viewport_top: u16, viewport: Rect) -> Self {
        Self {
            path,
            renderer: Renderer::new(),
            image_id: kitty::new_image_id(),
            preview: None,
            error: None,
            viewport_top,
            viewport,
        }
    }

    fn refresh(&mut self, pane: Rect) {
        let source = match fs::read_to_string(&self.path) {
            Ok(source) => source,
            Err(error) => {
                self.error = Some(format!("cannot read '{}': {error}", self.path.display()));
                return;
            }
        };
        let (width, height) = pane_pixels(pane);
        match self.renderer.png(&source, width, height) {
            Ok(png) => {
                self.preview = Some(png);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}

/// Watch `path` with native file-system notifications below the current terminal output.
pub fn run(path: PathBuf) -> Result<(), String> {
    let source = canonical_file(&path)?;
    let parent = source.parent().ok_or_else(|| {
        format!(
            "cannot watch '{}': it has no parent directory",
            source.display()
        )
    })?;
    let (event_tx, event_rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(
        move |event| {
            let _ = event_tx.send(event);
        },
        notify::Config::default(),
    )
    .map_err(|error| format!("cannot watch '{}': {error}", source.display()))?;
    watcher
        .watch(parent, RecursiveMode::NonRecursive)
        .map_err(|error| format!("cannot watch '{}': {error}", source.display()))?;

    let (_, viewport_top) =
        cursor::position().map_err(|error| format!("cannot determine cursor position: {error}"))?;
    let (width, height) = crossterm::terminal::size()
        .map_err(|error| format!("cannot determine terminal size: {error}"))?;
    let viewport = watch_viewport(viewport_top, width, height);

    enable_raw_mode().map_err(|error| format!("cannot enable raw mode: {error}"))?;
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Fixed(viewport),
        },
    )
    .map_err(|error| error.to_string())?;
    let mut state = State::new(source, viewport_top, viewport);
    let result = event_loop(&mut terminal, &mut state, &event_rx);
    let cleanup = cleanup(&mut terminal, state.image_id);
    combine(result, cleanup)
}

fn canonical_file(path: &Path) -> Result<PathBuf, String> {
    if !path.is_file() {
        return Err(format!(
            "'{}' is not a readable Mermaid file",
            path.display()
        ));
    }
    path.canonicalize()
        .map_err(|error| format!("cannot resolve '{}': {error}", path.display()))
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut State,
    events: &Receiver<notify::Result<NotifyEvent>>,
) -> Result<(), String> {
    refresh_and_draw(terminal, state)?;
    loop {
        if event::poll(Duration::from_millis(25)).map_err(|error| error.to_string())? {
            match event::read().map_err(|error| error.to_string())? {
                Event::Key(KeyEvent {
                    code: KeyCode::Char('c') | KeyCode::Char('q'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }) => return Ok(()),
                Event::Resize(_, _) => refresh_and_draw(terminal, state)?,
                _ => {}
            }
        }

        let Ok(event) = events.try_recv() else {
            continue;
        };
        let event = event.map_err(|error| format!("file watch failed: {error}"))?;
        if !event_affects(&event, &state.path) {
            continue;
        }
        wait_for_quiet(events, &state.path)?;
        refresh_and_draw(terminal, state)?;
    }
}

fn wait_for_quiet(
    events: &Receiver<notify::Result<NotifyEvent>>,
    source: &Path,
) -> Result<(), String> {
    let mut last_change = Instant::now();
    loop {
        let remaining = CHANGE_DEBOUNCE.saturating_sub(last_change.elapsed());
        match events.recv_timeout(remaining) {
            Ok(Ok(event)) if event_affects(&event, source) => last_change = Instant::now(),
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(format!("file watch failed: {error}")),
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("file watcher unexpectedly stopped".to_string());
            }
        }
    }
}

fn event_affects(event: &NotifyEvent, source: &Path) -> bool {
    event.paths.iter().any(|path| path == source)
}

fn refresh_and_draw(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut State,
) -> Result<(), String> {
    let size = terminal.size().map_err(|error| error.to_string())?;
    let viewport = watch_viewport(state.viewport_top, size.width, size.height);
    if viewport != state.viewport {
        terminal
            .resize(viewport)
            .map_err(|error| error.to_string())?;
        state.viewport = viewport;
    }
    state.refresh(preview_pane(viewport));

    // The buffer area is the absolute screen rectangle that Ratatui actually drew,
    // so it is the coordinate system Kitty needs for cursor placement.
    let preview = {
        let frame = terminal
            .draw(|frame| draw(frame, state))
            .map_err(|error| error.to_string())?;
        preview_pane(frame.buffer.area)
    };
    if let Some(png) = state.preview.as_deref() {
        kitty::display_png(
            state.image_id,
            png,
            kitty::centered_pane(preview, png),
            terminal.backend_mut(),
        )
        .map_err(|error| format!("Kitty preview failed: {error}"))?;
    }
    Ok(())
}

fn draw(frame: &mut ratatui::Frame, state: &State) {
    let area = frame.area();
    let title = state.error.as_deref().map_or_else(
        || format!(" {} ", state.path.display()),
        |error| format!(" {error} "),
    );
    let title_style = if state.error.is_some() {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, title_style)),
        area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " Watching for changes · Ctrl-C quit ",
            Style::default().fg(Color::DarkGray),
        ))),
        Rect::new(
            area.x,
            area.y + area.height.saturating_sub(1),
            area.width,
            1,
        ),
    );
}

fn watch_viewport(viewport_top: u16, width: u16, terminal_height: u16) -> Rect {
    let top = viewport_top.min(terminal_height.saturating_sub(1));
    Rect::new(0, top, width, terminal_height.saturating_sub(top))
}

fn preview_pane(area: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(area)
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

pub(crate) fn cleanup(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    image_id: kitty::ImageId,
) -> Result<(), String> {
    let image =
        kitty::delete_image(image_id, terminal.backend_mut()).map_err(|error| error.to_string());
    let raw = disable_raw_mode().map_err(|error| error.to_string());
    combine(image, raw)
}

pub(crate) fn combine(
    primary: Result<(), String>,
    cleanup: Result<(), String>,
) -> Result<(), String> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(format!("{primary}; cleanup failed: {cleanup}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::EventKind;

    #[test]
    fn file_events_are_limited_to_the_watched_source() {
        let source = Path::new("/tmp/diagram.mmd");
        let mut matching = NotifyEvent::new(EventKind::Any);
        matching.paths.push(source.to_path_buf());
        let mut unrelated = NotifyEvent::new(EventKind::Any);
        unrelated.paths.push(PathBuf::from("/tmp/other.mmd"));

        assert!(event_affects(&matching, source));
        assert!(!event_affects(&unrelated, source));
    }

    #[test]
    fn preview_pane_reserves_its_border() {
        assert_eq!(
            preview_pane(Rect::new(0, 0, 80, 24)),
            Rect::new(1, 1, 78, 22)
        );
    }

    #[test]
    fn viewport_grows_downward_from_its_initial_cursor_row() {
        assert_eq!(watch_viewport(6, 80, 24), Rect::new(0, 6, 80, 18));
        assert_eq!(watch_viewport(6, 120, 40), Rect::new(0, 6, 120, 34));
    }
}
