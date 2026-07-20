use std::collections::BTreeMap;
use std::io;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::kitty;
use crate::render::Renderer;
use crate::watch::{cleanup, combine, pane_pixels};

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
}

struct PreviewDocument {
    source: String,
    version: i64,
    changed: u64,
    cache: Option<PreviewCache>,
}

struct LspPreviewState {
    documents: BTreeMap<(u64, String), PreviewDocument>,
    sequence: u64,
    renderer: Renderer,
    image_id: kitty::ImageId,
    preview: Option<Vec<u8>>,
    render_error: Option<String>,
    graphics_dirty: bool,
}

impl LspPreviewState {
    fn new() -> Self {
        Self {
            documents: BTreeMap::new(),
            sequence: 0,
            renderer: Renderer::new(),
            image_id: kitty::new_image_id(),
            preview: None,
            render_error: None,
            graphics_dirty: true,
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
        let source = self.documents[&key].source.clone();
        match self.renderer.png(&source, width, height) {
            Ok(png) => {
                self.set_preview(&key, source, png, size);
                self.render_error = None;
            }
            Err(error) => {
                self.render_error = Some(error);
                let Some(mut cache) = self.documents[&key].cache.clone() else {
                    self.clear_preview();
                    return;
                };
                if cache.size != size {
                    let Ok(png) = self.renderer.png(&cache.source, width, height) else {
                        self.clear_preview();
                        return;
                    };
                    cache.png = png;
                    cache.size = size;
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

    fn set_preview(&mut self, key: &(u64, String), source: String, png: Vec<u8>, size: (u32, u32)) {
        self.documents.get_mut(key).unwrap().cache = Some(PreviewCache {
            source,
            png: png.clone(),
            size,
        });
        self.show_preview(png);
    }

    fn show_preview(&mut self, png: Vec<u8>) {
        self.graphics_dirty |= self.preview.as_ref() != Some(&png);
        self.preview = Some(png);
    }

    fn clear_preview(&mut self) {
        self.graphics_dirty |= self.preview.take().is_some();
    }

    fn status(&self) -> String {
        if let Some(error) = &self.render_error {
            return error.clone();
        }
        match self.selected() {
            Some((uri, _)) => format!(" {} open document(s) · {uri}", self.documents.len()),
            None => " Waiting for a Mermaid document from an LSP client ".to_string(),
        }
    }
}

/// Run the dedicated Kitty pane that previews the most recently changed LSP document.
pub fn run_lsp_preview(receiver: Receiver<PreviewEvent>) -> Result<(), String> {
    let mut state = LspPreviewState::new();
    enable_raw_mode().map_err(|error| format!("cannot enable raw mode: {error}"))?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen) {
        let cleanup = disable_raw_mode().map_err(|cleanup| cleanup.to_string());
        return combine(
            Err(format!("cannot enter alternate screen: {error}")),
            cleanup,
        );
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => {
            let mut stdout = io::stdout();
            let leave =
                execute!(stdout, LeaveAlternateScreen).map_err(|cleanup| cleanup.to_string());
            let raw = disable_raw_mode().map_err(|cleanup| cleanup.to_string());
            return combine(
                Err(format!("cannot initialize terminal: {error}")),
                combine(leave, raw),
            );
        }
    };

    let result = lsp_preview_loop(&mut terminal, &receiver, &mut state);
    let cleanup = cleanup(&mut terminal, state.image_id);
    combine(result, cleanup)
}

fn lsp_preview_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    receiver: &Receiver<PreviewEvent>,
    state: &mut LspPreviewState,
) -> Result<(), String> {
    const DEBOUNCE: Duration = Duration::from_millis(120);
    let mut dirty = true;
    let mut preview_dirty = true;
    let mut deadline = None;
    let mut image_displayed = false;

    loop {
        let mut changed = false;
        loop {
            match receiver.try_recv() {
                Ok(PreviewEvent::Fatal(error)) => return Err(error),
                Ok(event) => changed |= state.apply(event),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Err("gph LSP event channel disconnected".to_string());
                }
            }
        }
        if changed {
            dirty = true;
            preview_dirty = true;
            deadline = Some(Instant::now() + DEBOUNCE);
        }

        if preview_dirty && deadline.is_none_or(|time| Instant::now() >= time) {
            let size = terminal.size().map_err(|error| error.to_string())?;
            state.render(pane_pixels(
                lsp_preview_panes(Rect::new(0, 0, size.width, size.height)).preview,
            ));
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
            display_preview_if_dirty(
                state.image_id,
                state.preview.as_deref(),
                pane,
                &mut image_displayed,
                &mut state.graphics_dirty,
                terminal.backend_mut(),
            )
            .map_err(|error| format!("Kitty preview failed: {error}"))?;
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
        match event::read().map_err(|error| error.to_string())? {
            Event::Key(KeyEvent {
                code: KeyCode::Char('c') | KeyCode::Char('q'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => return Ok(()),
            Event::Resize(_, _) => {
                terminal.clear().map_err(|error| error.to_string())?;
                preview_dirty = true;
                deadline = Some(Instant::now());
                state.graphics_dirty = true;
                dirty = true;
            }
            _ => {}
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
            .title(" LSP Preview "),
        panes.preview_border,
    );
    let style = if state.render_error.is_some() {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(state.status(), style))),
        panes.status,
    );
}

fn display_preview_if_dirty(
    image_id: kitty::ImageId,
    preview: Option<&[u8]>,
    pane: Rect,
    image_displayed: &mut bool,
    graphics_dirty: &mut bool,
    out: &mut impl io::Write,
) -> io::Result<()> {
    if !*graphics_dirty {
        return Ok(());
    }
    display_preview(image_id, preview, pane, image_displayed, out)?;
    *graphics_dirty = false;
    Ok(())
}

fn display_preview(
    image_id: kitty::ImageId,
    preview: Option<&[u8]>,
    pane: Rect,
    image_displayed: &mut bool,
    out: &mut impl io::Write,
) -> io::Result<()> {
    if pane.width == 0 || pane.height == 0 {
        if *image_displayed {
            kitty::delete_image(image_id, out)?;
            *image_displayed = false;
        }
        return Ok(());
    }

    match preview {
        Some(png) => {
            kitty::display_png(image_id, png, pane, out)?;
            *image_displayed = true;
        }
        None if *image_displayed => {
            kitty::delete_image(image_id, out)?;
            *image_displayed = false;
        }
        None => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_preview_returns_to_another_client_when_the_latest_client_disconnects() {
        let mut state = LspPreviewState::new();
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
        let mut state = LspPreviewState::new();
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
        let mut state = LspPreviewState::new();
        state.apply(PreviewEvent::Set {
            client: 1,
            uri: "file:///a.mmd".to_string(),
            text: "flowchart TD\nA --> B\n".to_string(),
            version: 1,
        });
        state.render((100, 100));
        let preview_a = state.preview.clone().unwrap();

        state.apply(PreviewEvent::Set {
            client: 2,
            uri: "file:///b.mmd".to_string(),
            text: "not Mermaid".to_string(),
            version: 1,
        });
        state.render((100, 100));
        assert!(state.preview.is_none());
        assert!(state.render_error.is_some());

        state.render((200, 100));
        assert!(state.preview.is_none());
        assert_ne!(state.preview.as_ref(), Some(&preview_a));
    }

    #[test]
    fn invalid_selected_document_resizes_only_its_own_cached_preview() {
        let mut state = LspPreviewState::new();
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
        assert_eq!(state.preview.as_deref(), Some(cache.png.as_slice()));
        assert!(state.render_error.is_some());
    }
}
