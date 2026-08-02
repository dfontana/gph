use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::preview_ui::{
    DragTracker, PreviewImage, PreviewTerminal, TerminalSession, combine, is_quit_event,
    pane_pixels, scroll_delta, zoom_action,
};
use crate::render::Renderer;

/// A document update received from one LSP client connection.
#[derive(Debug)]
pub enum PreviewEvent {
    Set {
        client: u64,
        uri: String,
        text: String,
        version: i64,
    },
    Close {
        client: u64,
        uri: String,
    },
    Disconnect {
        client: u64,
    },
    /// The LSP accept loop could not continue; the preview must exit.
    Fatal(String),
}

#[derive(Clone)]
struct PreviewCache {
    source: String,
    png: Vec<u8>,
    size: (u32, u32),
    zoom: f32,
}

struct PreviewDocument {
    source: String,
    version: i64,
    changed: u64,
    cache: Option<PreviewCache>,
}

#[derive(Default)]
struct ExportPrompt {
    input: String,
    error: Option<String>,
}

struct LspPreviewState {
    documents: BTreeMap<(u64, String), PreviewDocument>,
    sequence: u64,
    renderer: Renderer,
    image: PreviewImage,
    render_error: Option<String>,
    /// Set only for the real LSP daemon. Terminal-only render previews leave this disabled.
    export_dir: Option<PathBuf>,
    prompt: Option<ExportPrompt>,
}

impl LspPreviewState {
    fn new(export_dir: Option<PathBuf>) -> Self {
        Self {
            documents: BTreeMap::new(),
            sequence: 0,
            renderer: Renderer::new(),
            image: PreviewImage::new(),
            render_error: None,
            export_dir,
            prompt: None,
        }
    }

    fn apply(&mut self, event: PreviewEvent) -> bool {
        self.sequence = self.sequence.wrapping_add(1);
        match event {
            PreviewEvent::Set {
                client,
                uri,
                text,
                version,
            } => {
                let key = (client, uri);
                if self
                    .documents
                    .get(&key)
                    .is_some_and(|document| version <= document.version)
                {
                    return false;
                }
                let cache = self
                    .documents
                    .remove(&key)
                    .and_then(|document| document.cache);
                self.documents.insert(
                    key,
                    PreviewDocument {
                        source: text,
                        version,
                        changed: self.sequence,
                        cache,
                    },
                );
                true
            }
            PreviewEvent::Close { client, uri } => self.documents.remove(&(client, uri)).is_some(),
            PreviewEvent::Disconnect { client } => {
                let before = self.documents.len();
                self.documents.retain(|(owner, _), _| *owner != client);
                self.documents.len() != before
            }
            PreviewEvent::Fatal(_) => false,
        }
    }

    fn selected(&self) -> Option<(&str, &str)> {
        self.documents
            .iter()
            .max_by_key(|(_, document)| document.changed)
            .map(|((_, uri), document)| (uri.as_str(), document.source.as_str()))
    }

    fn render(&mut self, size @ (width, height): (u32, u32)) {
        let Some(key) = self.selected_key() else {
            self.render_error = None;
            self.clear_preview();
            return;
        };
        let zoom = self.image.zoom();
        let source = self.documents[&key].source.clone();
        match self.renderer.png(&source, width, height, zoom) {
            Ok(png) => {
                self.set_preview(&key, source, png, size, zoom);
                self.render_error = None;
            }
            Err(error) => {
                self.render_error = Some(error);
                let Some(mut cache) = self.documents[&key].cache.clone() else {
                    self.clear_preview();
                    return;
                };
                if cache.size != size || cache.zoom != zoom {
                    let Ok(png) = self.renderer.png(&cache.source, width, height, zoom) else {
                        self.clear_preview();
                        return;
                    };
                    cache.png = png;
                    cache.size = size;
                    cache.zoom = zoom;
                    self.documents.get_mut(&key).unwrap().cache = Some(cache.clone());
                }
                self.show_preview(cache.png);
            }
        }
    }

    fn selected_key(&self) -> Option<(u64, String)> {
        self.documents
            .iter()
            .max_by_key(|(_, document)| document.changed)
            .map(|(key, _)| key.clone())
    }

    fn set_preview(
        &mut self,
        key: &(u64, String),
        source: String,
        png: Vec<u8>,
        size: (u32, u32),
        zoom: f32,
    ) {
        self.documents.get_mut(key).unwrap().cache = Some(PreviewCache {
            source,
            png: png.clone(),
            size,
            zoom,
        });
        self.show_preview(png);
    }

    fn show_preview(&mut self, png: Vec<u8>) {
        self.image.show(png);
    }

    fn clear_preview(&mut self) {
        self.image.clear();
    }

    /// Left-aligned header title: the pane name and open-document count.
    fn header_left(&self) -> String {
        format!(" LSP Preview  {} open document(s) ", self.documents.len())
    }

    /// Right-aligned header title: the current zoom level and control hints.
    fn header_right(&self) -> String {
        format!(
            " {}% · +/- zoom · drag or scroll to pan{} ",
            (self.image.zoom() * 100.0).round() as u32,
            self.export_dir.as_ref().map_or("", |_| " · e export")
        )
    }

    fn open_export_prompt(&mut self) -> bool {
        if self.export_dir.is_none() || self.selected().is_none() {
            return false;
        }
        self.prompt = Some(ExportPrompt::default());
        true
    }

    fn handle_prompt_event(&mut self, event: &Event) -> bool {
        let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event
        else {
            return false;
        };
        if modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        match code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Backspace => {
                let prompt = self.prompt.as_mut().unwrap();
                prompt.input.pop();
                prompt.error = None;
            }
            KeyCode::Char(character) => {
                let prompt = self.prompt.as_mut().unwrap();
                prompt.input.push(*character);
                prompt.error = None;
            }
            KeyCode::Enter => {
                let result = self.export(&self.prompt.as_ref().unwrap().input);
                match result {
                    Ok(()) => self.prompt = None,
                    Err(error) => self.prompt.as_mut().unwrap().error = Some(error),
                }
            }
            _ => return false,
        }
        true
    }

    fn export(&self, filename: &str) -> Result<(), String> {
        let destination = export_destination(self.export_dir.as_ref().unwrap(), filename)?;
        let source = self
            .selected()
            .ok_or_else(|| "no active LSP document to export".to_string())?
            .1;
        crate::render::export(source, &destination, None, None)
    }

    /// The footer line: the selected document's URI, or a waiting/error message.
    fn footer(&self) -> String {
        if let Some(error) = &self.render_error {
            return error.clone();
        }
        match self.selected() {
            Some((uri, _)) => format!(" {uri} "),
            None => " Waiting for a Mermaid document from an LSP client ".to_string(),
        }
    }
}

fn export_destination(directory: &Path, filename: &str) -> Result<PathBuf, String> {
    let path = Path::new(filename);
    (directory.is_absolute() && !filename.is_empty() && path.file_name() == Some(path.as_os_str()))
        .then(|| directory.join(path))
        .ok_or_else(|| "export filename must be a file name".to_string())
}

/// Seed one document event, then run the exact viewer path used by `gph lsp`.
pub fn run_source_preview(uri: String, source: String) -> Result<(), String> {
    if !crate::kitty::is_available() {
        return Err(
            "`gph render` without --out requires Kitty (set KITTY_WINDOW_ID or TERM=xterm-kitty)"
                .to_string(),
        );
    }
    let (updates, receiver) = std::sync::mpsc::channel();
    updates
        .send(PreviewEvent::Set {
            client: 0,
            uri,
            text: source,
            version: 1,
        })
        .map_err(|_| "gph preview viewer stopped before it started".to_string())?;

    // Keep the source open until the interactive viewer exits. Otherwise its receiver sees a
    // disconnected LSP event channel immediately after consuming the initial document.
    let result = run_preview(LspPreviewState::new(None), receiver);
    drop(updates);
    result
}

/// Run the dedicated Kitty pane that previews the most recently changed LSP document.
pub fn run_lsp_preview(receiver: Receiver<PreviewEvent>) -> Result<(), String> {
    let export_dir = std::env::current_dir()
        .map_err(|error| format!("cannot determine the LSP launch directory: {error}"))?;
    run_preview(LspPreviewState::new(Some(export_dir)), receiver)
}

fn run_preview(mut state: LspPreviewState, receiver: Receiver<PreviewEvent>) -> Result<(), String> {
    let mut session = TerminalSession::alternate()?;
    let result = lsp_preview_loop(session.terminal(), &receiver, &mut state);
    let cleanup = session.cleanup(&mut state.image);
    combine(result, cleanup)
}

fn lsp_preview_loop(
    terminal: &mut PreviewTerminal,
    receiver: &Receiver<PreviewEvent>,
    state: &mut LspPreviewState,
) -> Result<(), String> {
    const DEBOUNCE: Duration = Duration::from_millis(120);
    let mut dirty = true;
    let mut preview_dirty = true;
    let mut deadline = None;
    let mut drag = DragTracker::new();

    loop {
        let changed = drain_preview_events(receiver, state)?;
        if changed {
            dirty = true;
            preview_dirty = true;
            deadline = Some(Instant::now() + DEBOUNCE);
        }

        if preview_dirty && deadline.is_none_or(|time| Instant::now() >= time) {
            let size = terminal.size().map_err(|error| error.to_string())?;
            let pane = lsp_preview_panes(Rect::new(0, 0, size.width, size.height)).preview;
            state.render(pane_pixels(pane));
            preview_dirty = false;
            deadline = None;
            dirty = true;
        }

        if dirty {
            terminal
                .draw(|frame| draw_lsp_preview(frame, state))
                .map_err(|error| error.to_string())?;
            let size = terminal.size().map_err(|error| error.to_string())?;
            let pane = lsp_preview_panes(Rect::new(0, 0, size.width, size.height)).preview;
            if state.prompt.is_none() {
                state
                    .image
                    .display_if_dirty(pane, terminal.backend_mut())
                    .map_err(|error| format!("Kitty preview failed: {error}"))?;
            }
            dirty = false;
        }

        let wait = deadline
            .map(|time| {
                time.saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(50))
            })
            .unwrap_or(Duration::from_millis(50));
        if !event::poll(wait).map_err(|error| error.to_string())? {
            continue;
        }
        let event = event::read().map_err(|error| error.to_string())?;
        if is_quit_event(&event) {
            return Ok(());
        }
        if state.prompt.is_some() {
            let enter = matches!(event, Event::Key(key) if key.code == KeyCode::Enter);
            if enter && drain_preview_events(receiver, state)? {
                dirty = true;
                preview_dirty = true;
                deadline = Some(Instant::now() + DEBOUNCE);
            }
            dirty |= state.handle_prompt_event(&event);
            if state.prompt.is_none() {
                state.image.mark_dirty();
                drag = DragTracker::new();
            }
            if !matches!(event, Event::Resize(_, _)) {
                continue;
            }
        }
        if matches!(event, Event::Key(key) if key.code == KeyCode::Char('e') && key.modifiers == KeyModifiers::NONE)
            && state.open_export_prompt()
        {
            state
                .image
                .hide(terminal.backend_mut())
                .map_err(|error| format!("Kitty preview failed: {error}"))?;
            dirty = true;
            drag = DragTracker::new();
            continue;
        }
        if let Some(action) = zoom_action(&event) {
            if state.image.apply_zoom(action) {
                preview_dirty = true;
                deadline = Some(Instant::now());
                state.image.mark_dirty();
                dirty = true;
            }
            continue;
        }
        if matches!(event, Event::Mouse(_)) {
            // Panning only re-places the existing image, so it skips the render.
            // A drag pans by cursor motion; a scroll/trackpad swipe by notch.
            if let Some((dx, dy)) = drag.delta(&event).or_else(|| scroll_delta(&event))
                && state.image.pan(dx, dy)
            {
                dirty = true;
            }
            continue;
        }
        if matches!(event, Event::Resize(_, _)) {
            terminal.clear().map_err(|error| error.to_string())?;
            preview_dirty = true;
            deadline = Some(Instant::now());
            state.image.mark_dirty();
            dirty = true;
        }
    }
}

fn drain_preview_events(
    receiver: &Receiver<PreviewEvent>,
    state: &mut LspPreviewState,
) -> Result<bool, String> {
    let mut changed = false;
    loop {
        match receiver.try_recv() {
            Ok(PreviewEvent::Fatal(error)) => return Err(error),
            Ok(event) => changed |= state.apply(event),
            Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(changed),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err("gph LSP event channel disconnected".to_string());
            }
        }
    }
}

struct LspPreviewPanes {
    status: Rect,
    preview_border: Rect,
    preview: Rect,
}

fn lsp_preview_panes(area: Rect) -> LspPreviewPanes {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    let preview_border = rows[0];
    LspPreviewPanes {
        status: rows[1],
        preview: Block::default().borders(Borders::ALL).inner(preview_border),
        preview_border,
    }
}

fn draw_lsp_preview(frame: &mut ratatui::Frame, state: &LspPreviewState) {
    let panes = lsp_preview_panes(frame.area());
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title_top(Line::from(state.header_left()))
            .title_top(Line::from(state.header_right()).right_aligned()),
        panes.preview_border,
    );
    let style = if state.render_error.is_some() {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(state.footer(), style))),
        panes.status,
    );
    if let Some(prompt) = &state.prompt {
        draw_export_prompt(frame, prompt);
    }
}

fn draw_export_prompt(frame: &mut ratatui::Frame, prompt: &ExportPrompt) {
    let area = frame.area();
    let width = area.width.min(60);
    let height = area.height.min(if prompt.error.is_some() { 6 } else { 5 });
    if width == 0 || height == 0 {
        return;
    }
    let popup = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let mut text = format!("Name: {}\nEnter export · Esc cancel", prompt.input);
    if let Some(error) = &prompt.error {
        text.push_str(&format!("\nError: {error}"));
    }
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Export filename "),
        ),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn set_document(state: &mut LspPreviewState, text: &str, version: i64) {
        assert!(state.apply(PreviewEvent::Set {
            client: 1,
            uri: "file:///diagram.mmd".to_string(),
            text: text.to_string(),
            version,
        }));
    }

    fn type_filename(state: &mut LspPreviewState, filename: &str) {
        for character in filename.chars() {
            assert!(state.handle_prompt_event(&key(KeyCode::Char(character))));
        }
    }

    #[test]
    fn export_prompt_uses_current_source_and_preserves_failed_output() {
        let root = std::env::temp_dir().join(format!("gph-preview-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).unwrap();
        assert!(export_destination(&root, "../diagram.svg").is_err());
        let mut state = LspPreviewState::new(Some(root.clone()));
        set_document(&mut state, "flowchart TD\nA[Old] --> B\n", 1);
        assert!(state.open_export_prompt());
        type_filename(&mut state, "diagram.svg");
        let (updates, receiver) = std::sync::mpsc::channel();
        updates
            .send(PreviewEvent::Set {
                client: 1,
                uri: "file:///diagram.mmd".to_string(),
                text: "flowchart TD\nA[Queued] --> B\n".to_string(),
                version: 2,
            })
            .unwrap();
        assert!(drain_preview_events(&receiver, &mut state).unwrap());
        assert!(state.handle_prompt_event(&key(KeyCode::Enter)));
        let output = root.join("diagram.svg");
        assert!(state.prompt.is_none());
        assert!(std::fs::read_to_string(&output).unwrap().contains("Queued"));

        std::fs::write(&output, b"keep existing output").unwrap();
        set_document(&mut state, "not Mermaid", 3);
        assert!(state.open_export_prompt());
        type_filename(&mut state, "diagram.svg");
        assert!(state.handle_prompt_event(&key(KeyCode::Enter)));
        assert!(
            state
                .prompt
                .as_ref()
                .is_some_and(|prompt| prompt.error.is_some())
        );
        assert_eq!(std::fs::read(&output).unwrap(), b"keep existing output");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lsp_preview_returns_to_another_client_when_the_latest_client_disconnects() {
        let mut state = LspPreviewState::new(None);
        state.apply(PreviewEvent::Set {
            client: 1,
            uri: "file:///first.mmd".to_string(),
            text: "first".to_string(),
            version: 1,
        });
        state.apply(PreviewEvent::Set {
            client: 2,
            uri: "file:///second.mmd".to_string(),
            text: "second".to_string(),
            version: 1,
        });
        assert_eq!(state.selected(), Some(("file:///second.mmd", "second")));

        state.apply(PreviewEvent::Disconnect { client: 2 });
        assert_eq!(state.selected(), Some(("file:///first.mmd", "first")));

        state.apply(PreviewEvent::Close {
            client: 1,
            uri: "file:///first.mmd".to_string(),
        });
        assert_eq!(state.selected(), None);
    }

    #[test]
    fn lsp_preview_ignores_stale_document_versions() {
        let mut state = LspPreviewState::new(None);
        assert!(state.apply(PreviewEvent::Set {
            client: 1,
            uri: "file:///diagram.mmd".to_string(),
            text: "new".to_string(),
            version: 2,
        }));
        assert!(!state.apply(PreviewEvent::Set {
            client: 1,
            uri: "file:///diagram.mmd".to_string(),
            text: "stale".to_string(),
            version: 1,
        }));
        assert_eq!(state.selected(), Some(("file:///diagram.mmd", "new")));
    }

    #[test]
    fn invalid_selected_document_never_reuses_another_documents_preview() {
        let mut state = LspPreviewState::new(None);
        state.apply(PreviewEvent::Set {
            client: 1,
            uri: "file:///a.mmd".to_string(),
            text: "flowchart TD\nA --> B\n".to_string(),
            version: 1,
        });
        state.render((100, 100));
        let preview_a = state.image.png().unwrap().to_vec();

        state.apply(PreviewEvent::Set {
            client: 2,
            uri: "file:///b.mmd".to_string(),
            text: "not Mermaid".to_string(),
            version: 1,
        });
        state.render((100, 100));
        assert!(state.image.png().is_none());
        assert!(state.render_error.is_some());

        state.render((200, 100));
        assert!(state.image.png().is_none());
        assert_ne!(state.image.png(), Some(preview_a.as_slice()));
    }

    #[test]
    fn invalid_selected_document_resizes_only_its_own_cached_preview() {
        let mut state = LspPreviewState::new(None);
        state.apply(PreviewEvent::Set {
            client: 1,
            uri: "file:///a.mmd".to_string(),
            text: "flowchart TD\nA --> B\n".to_string(),
            version: 1,
        });
        state.render((100, 100));
        state.apply(PreviewEvent::Set {
            client: 2,
            uri: "file:///b.mmd".to_string(),
            text: "flowchart TD\nB --> C\n".to_string(),
            version: 1,
        });
        state.render((100, 100));
        let key = (2, "file:///b.mmd".to_string());
        state.apply(PreviewEvent::Set {
            client: 2,
            uri: "file:///b.mmd".to_string(),
            text: "not Mermaid".to_string(),
            version: 2,
        });
        state.render((200, 100));

        let cache = state.documents[&key].cache.as_ref().unwrap();
        assert_eq!(cache.source, "flowchart TD\nB --> C\n");
        assert_eq!(cache.size, (200, 100));
        assert_eq!(state.image.png(), Some(cache.png.as_slice()));
        assert!(state.render_error.is_some());
    }
}
