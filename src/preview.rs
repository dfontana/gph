use std::collections::BTreeMap;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::preview_ui::{
    PreviewImage, PreviewTerminal, TerminalSession, combine, is_quit_event, pane_pixels,
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
    image: PreviewImage,
    render_error: Option<String>,
}

impl LspPreviewState {
    fn new() -> Self {
        Self {
            documents: BTreeMap::new(),
            sequence: 0,
            renderer: Renderer::new(),
            image: PreviewImage::new(),
            render_error: None,
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
        self.image.show(png);
    }

    fn clear_preview(&mut self) {
        self.image.clear();
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
            state
                .image
                .display_if_dirty(pane, terminal.backend_mut())
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
        let event = event::read().map_err(|error| error.to_string())?;
        if is_quit_event(&event) {
            return Ok(());
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
        assert_eq!(state.image.png(), Some(cache.png.as_slice()));
        assert!(state.render_error.is_some());
    }
}
