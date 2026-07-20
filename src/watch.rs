use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crossterm::cursor;
use crossterm::event::{self, Event};
use notify::{Event as NotifyEvent, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::preview_ui::{
    PreviewImage, PreviewTerminal, TerminalSession, combine, is_quit_event, pane_pixels,
};
use crate::render::Renderer;

const CHANGE_DEBOUNCE: Duration = Duration::from_millis(150);

struct State {
    path: PathBuf,
    renderer: Renderer,
    image: PreviewImage,
    error: Option<String>,
    viewport_top: u16,
    viewport: Rect,
}

impl State {
    fn new(path: PathBuf, viewport_top: u16, viewport: Rect) -> Self {
        Self {
            path,
            renderer: Renderer::new(),
            image: PreviewImage::new(),
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
                self.image.show(png);
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

    let mut session = TerminalSession::fixed(viewport)?;
    let mut state = State::new(source, viewport_top, viewport);
    let result = event_loop(session.terminal(), &mut state, &event_rx);
    let cleanup = session.cleanup(&mut state.image);
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
    terminal: &mut PreviewTerminal,
    state: &mut State,
    events: &Receiver<notify::Result<NotifyEvent>>,
) -> Result<(), String> {
    refresh_and_draw(terminal, state)?;
    loop {
        if event::poll(Duration::from_millis(25)).map_err(|error| error.to_string())? {
            let event = event::read().map_err(|error| error.to_string())?;
            if is_quit_event(&event) {
                return Ok(());
            }
            if matches!(event, Event::Resize(_, _)) {
                refresh_and_draw(terminal, state)?;
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

fn refresh_and_draw(terminal: &mut PreviewTerminal, state: &mut State) -> Result<(), String> {
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
    if state.image.png().is_some() {
        state
            .image
            .display(preview, terminal.backend_mut())
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
