use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, window_size,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui_textarea::{CursorMove, Input, Key, TextArea};

use crate::kitty;
use crate::render::Renderer;

#[derive(Clone)]
struct EditorSnapshot {
    lines: Vec<String>,
    endings_before: Vec<Option<String>>,
    cursor: (usize, usize),
}

struct State {
    path: PathBuf,
    has_bom: bool,
    endings_before: Vec<Option<String>>,
    default_line_ending: String,
    textarea: TextArea<'static>,
    undo: Vec<EditorSnapshot>,
    redo: Vec<EditorSnapshot>,
    renderer: Renderer,
    image_id: kitty::ImageId,
    preview: Option<Vec<u8>>,
    last_valid_source: Option<String>,
    preview_size: Option<(u32, u32)>,
    save_error: Option<String>,
    render_error: Option<String>,
    dirty: bool,
    preview_dirty: bool,
    graphics_dirty: bool,
}

impl State {
    fn load(path: PathBuf) -> Result<Self, String> {
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read '{}': {error}", path.display()))?;
        let has_bom = source.starts_with('\u{feff}');
        let logical_source = source.strip_prefix('\u{feff}').unwrap_or(&source);
        let (lines, endings_before) = logical_lines_and_endings(logical_source);
        let default_line_ending = endings_before
            .iter()
            .flatten()
            .next()
            .cloned()
            .unwrap_or_else(|| "\n".to_string());
        Ok(Self {
            textarea: configured_textarea(lines, &path),
            path,
            has_bom,
            endings_before,
            default_line_ending,
            undo: Vec::new(),
            redo: Vec::new(),
            renderer: Renderer::new(),
            image_id: kitty::new_image_id(),
            preview: None,
            last_valid_source: None,
            preview_size: None,
            save_error: None,
            render_error: None,
            dirty: true,
            preview_dirty: true,
            graphics_dirty: true,
        })
    }

    fn source(&self) -> String {
        let source = source_from_lines(
            self.textarea.lines(),
            &self.endings_before,
            &self.default_line_ending,
        );
        if self.has_bom {
            format!("\u{feff}{source}")
        } else {
            source
        }
    }

    fn save(&mut self) {
        let source = self.source();
        match crate::files::write_atomically(&self.path, &source) {
            Ok(()) => {
                let (_, endings_before) = logical_lines_and_endings(&source);
                self.endings_before = endings_before;
                self.save_error = None;
            }
            Err(error) => {
                self.save_error = Some(format!("cannot save '{}': {error}", self.path.display()));
            }
        }
    }

    fn render(&mut self, size @ (width, height): (u32, u32)) -> bool {
        let source = self.source();
        match self.renderer.png(&source, width, height) {
            Ok(png) => {
                self.preview = Some(png);
                self.last_valid_source = Some(source);
                self.preview_size = Some(size);
                self.render_error = None;
                true
            }
            Err(error) => {
                self.render_error = Some(error);
                if self.preview_size != Some(size)
                    && let Some(last_valid_source) = self.last_valid_source.clone()
                    && let Ok(png) = self.renderer.png(&last_valid_source, width, height)
                {
                    self.preview = Some(png);
                    self.preview_size = Some(size);
                    return true;
                }
                false
            }
        }
    }

    fn snapshot(&self) -> EditorSnapshot {
        let cursor = self.textarea.cursor();
        EditorSnapshot {
            lines: self.textarea.lines().to_vec(),
            endings_before: self.endings_before.clone(),
            cursor: (cursor.0, cursor.1),
        }
    }

    fn restore(&mut self, snapshot: EditorSnapshot) {
        let yank = self.textarea.yank_text();
        let cursor = snapshot.cursor;
        self.textarea = configured_textarea(snapshot.lines, &self.path);
        self.textarea.set_yank_text(yank);
        restore_cursor(&mut self.textarea, cursor);
        self.endings_before = snapshot.endings_before;
    }

    fn push_undo(&mut self, snapshot: EditorSnapshot) {
        if self.undo.len() == 50 {
            self.undo.remove(0);
        }
        self.undo.push(snapshot);
    }

    fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snapshot());
        self.restore(snapshot);
        true
    }

    fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        self.push_undo(self.snapshot());
        self.restore(snapshot);
        true
    }

    fn error(&self) -> Option<&str> {
        self.save_error.as_deref().or(self.render_error.as_deref())
    }
}

fn restore_cursor(textarea: &mut TextArea<'static>, cursor: (usize, usize)) {
    const MAX_JUMP: usize = u16::MAX as usize;
    let (row, column) = cursor;

    if row <= MAX_JUMP && column <= MAX_JUMP {
        textarea.move_cursor(CursorMove::Jump(row as u16, column as u16));
        return;
    }

    let last_row = textarea.lines().len().saturating_sub(1);
    if row <= MAX_JUMP {
        textarea.move_cursor(CursorMove::Jump(row as u16, 0));
    } else if row - MAX_JUMP <= last_row - row {
        textarea.move_cursor(CursorMove::Jump(u16::MAX, 0));
        for _ in MAX_JUMP..row {
            textarea.move_cursor(CursorMove::Down);
        }
    } else {
        textarea.move_cursor(CursorMove::Bottom);
        for _ in row..last_row {
            textarea.move_cursor(CursorMove::Up);
        }
    }

    let line_length = textarea.lines()[row].chars().count();
    if column <= line_length - column {
        for _ in 0..column {
            textarea.move_cursor(CursorMove::Forward);
        }
    } else {
        textarea.move_cursor(CursorMove::End);
        for _ in column..line_length {
            textarea.move_cursor(CursorMove::Back);
        }
    }
}

pub fn run(path: PathBuf) -> Result<(), String> {
    let mut state = State::load(path)?;
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

    let result = event_loop(&mut terminal, &mut state);
    let cleanup = cleanup(&mut terminal, state.image_id);
    combine(result, cleanup)
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut State,
) -> Result<(), String> {
    let result = (|| {
        let mut image_displayed = false;
        loop {
            if state.dirty {
                let size = terminal.size().map_err(|error| error.to_string())?;
                let panes = panes(Rect::new(0, 0, size.width, size.height));
                refresh_preview(state, panes.preview);

                terminal
                    .draw(|frame| draw(frame, state))
                    .map_err(|error| error.to_string())?;
                display_preview_if_dirty(
                    state.image_id,
                    state.preview.as_deref(),
                    panes.preview,
                    &mut image_displayed,
                    &mut state.graphics_dirty,
                    terminal.backend_mut(),
                )
                .map_err(|error| format!("Kitty preview failed: {error}"))?;
                state.dirty = false;
            }

            if !event::poll(Duration::from_millis(100)).map_err(|error| error.to_string())? {
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
                    mark_resized(state);
                }
                event => handle_editor_event(state, event),
            }
        }
    })();
    finalize_event_loop_result(result, state.save_error.as_deref())
}

fn finalize_event_loop_result(
    result: Result<(), String>,
    save_error: Option<&str>,
) -> Result<(), String> {
    match (result, save_error) {
        (Ok(()), Some(save_error)) => Err(save_error.to_owned()),
        (Err(error), Some(save_error)) if error == save_error => Err(error),
        (Err(error), Some(save_error)) => Err(format!("{error}; {save_error}")),
        (result, None) => result,
    }
}

fn handle_editor_event(state: &mut State, event: Event) {
    let input = Input::from(event.clone());
    let history_action = match input {
        Input {
            key: Key::Char('u'),
            ctrl: true,
            alt: false,
            ..
        } => Some(true),
        Input {
            key: Key::Char('r'),
            ctrl: true,
            alt: false,
            ..
        } => Some(false),
        _ => None,
    };

    if let Some(undo) = history_action {
        let modified = if undo { state.undo() } else { state.redo() };
        state.dirty = true;
        if modified {
            state.save();
            state.preview_dirty = true;
        }
        return;
    }

    let snapshot = state.snapshot();
    let old_line_count = snapshot.lines.len();
    let selection = state
        .textarea
        .selection_range()
        .filter(|(start, end)| start != end);
    let cursor = state.textarea.cursor();
    let _ = state.textarea.input(event);
    let modified = state.textarea.lines() != snapshot.lines;
    state.dirty = true;

    if modified {
        let new_cursor = state.textarea.cursor();
        let new_line_count = state.textarea.lines().len();
        let (start_row, removed_breaks) = selection.map_or_else(
            || {
                (
                    cursor.0.min(new_cursor.0),
                    old_line_count.saturating_sub(new_line_count),
                )
            },
            |(start, end)| (start.0, end.0 - start.0),
        );
        let inserted_breaks = new_line_count + removed_breaks - old_line_count;
        apply_line_edit(
            &mut state.endings_before,
            start_row,
            removed_breaks,
            inserted_breaks,
            &state.default_line_ending,
        );
        state.push_undo(snapshot);
        state.redo.clear();
        state.save();
        state.preview_dirty = true;
    }
}

fn mark_resized(state: &mut State) {
    state.dirty = true;
    state.preview_dirty = true;
    state.graphics_dirty = true;
}

fn refresh_preview(state: &mut State, pane: Rect) {
    if state.preview_dirty {
        state.graphics_dirty |= state.render(pane_pixels(pane));
        state.preview_dirty = false;
    }
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

fn configured_textarea(lines: Vec<String>, path: &Path) -> TextArea<'static> {
    let mut textarea = TextArea::from(lines);
    textarea.set_max_histories(0);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("untitled");
    textarea.set_block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {name} ")),
    );
    textarea.set_line_number_style(Style::default().fg(Color::DarkGray));
    textarea
}

fn logical_lines_and_endings(source: &str) -> (Vec<String>, Vec<Option<String>>) {
    let bytes = source.as_bytes();
    let mut lines = Vec::new();
    let mut endings_before = vec![None];
    let mut line_start = 0;
    let mut index = 0;

    while index < bytes.len() {
        let separator_length = match bytes[index] {
            b'\n' => 1,
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => 2,
            b'\r' => 1,
            _ => {
                index += 1;
                continue;
            }
        };
        lines.push(source[line_start..index].to_owned());
        endings_before.push(Some(source[index..index + separator_length].to_owned()));
        index += separator_length;
        line_start = index;
    }
    lines.push(source[line_start..].to_owned());

    (lines, endings_before)
}

fn apply_line_edit(
    endings: &mut Vec<Option<String>>,
    start_row: usize,
    removed_breaks: usize,
    inserted_breaks: usize,
    fallback: &str,
) {
    let boundary = (start_row + 1).min(endings.len());
    let removed_end = (boundary + removed_breaks).min(endings.len());
    endings.drain(boundary..removed_end);
    endings.splice(
        boundary..boundary,
        std::iter::repeat_n(Some(fallback.to_owned()), inserted_breaks),
    );
    if let Some(first) = endings.first_mut() {
        *first = None;
    }
}

fn source_from_lines(
    lines: &[String],
    endings_before: &[Option<String>],
    fallback: &str,
) -> String {
    let mut source = String::new();
    for (row, line) in lines.iter().enumerate() {
        if row > 0 {
            source.push_str(
                endings_before
                    .get(row)
                    .and_then(Option::as_deref)
                    .unwrap_or(fallback),
            );
        }
        source.push_str(line);
    }
    source
}

fn draw(frame: &mut ratatui::Frame, state: &State) {
    let panes = panes(frame.area());
    frame.render_widget(&state.textarea, panes.editor);
    let status = match state.error() {
        Some(error) => Span::styled(format!(" {error} "), Style::default().fg(Color::Red)),
        None => Span::styled(
            " Ctrl-Q quit · edits save and preview live ",
            Style::default().fg(Color::DarkGray),
        ),
    };
    frame.render_widget(Paragraph::new(Line::from(status)), panes.status);
    frame.render_widget(
        Block::default().borders(Borders::ALL).title(" Preview "),
        panes.preview_border,
    );
}

struct Panes {
    editor: Rect,
    status: Rect,
    preview_border: Rect,
    preview: Rect,
}

fn panes(area: Rect) -> Panes {
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    let source = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(horizontal[0]);
    let preview_border = horizontal[1];
    Panes {
        editor: source[0],
        status: source[1],
        preview: Block::default().borders(Borders::ALL).inner(preview_border),
        preview_border,
    }
}

fn cleanup(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    image_id: kitty::ImageId,
) -> Result<(), String> {
    let image =
        kitty::delete_image(image_id, terminal.backend_mut()).map_err(|error| error.to_string());
    let screen =
        execute!(terminal.backend_mut(), LeaveAlternateScreen).map_err(|error| error.to_string());
    let raw = disable_raw_mode().map_err(|error| error.to_string());
    combine(combine(image, screen), raw)
}

fn combine(primary: Result<(), String>, cleanup: Result<(), String>) -> Result<(), String> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(format!("{primary}; cleanup failed: {cleanup}")),
    }
}

fn pane_pixels(pane: Rect) -> (u32, u32) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn state_with_source(name: &str, source: &str) -> State {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("gph-{name}-{}-{sequence}.mmd", std::process::id()));
        std::fs::write(&path, source).unwrap();
        State::load(path).unwrap()
    }

    fn finish(state: State) {
        std::fs::remove_file(state.path).unwrap();
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn preview_pane_is_inside_the_right_half() {
        assert_eq!(
            panes(Rect::new(0, 0, 100, 20)).preview,
            Rect::new(51, 1, 48, 18)
        );
    }

    #[test]
    fn preserves_mixed_line_endings_when_line_count_is_unchanged() {
        let source = "a\rb\r\nc\nd\r";
        let (lines, endings_before) = logical_lines_and_endings(source);

        assert_eq!(lines, ["a", "b", "c", "d", ""]);
        assert_eq!(source_from_lines(&lines, &endings_before, "\n"), source);
    }

    #[test]
    fn inserted_line_gets_the_default_without_moving_existing_endings() {
        let lines = ["a", "b", "new", "c", ""].map(str::to_owned);
        let (_, mut endings) = logical_lines_and_endings("a\rb\nc\r\n");
        apply_line_edit(&mut endings, 1, 0, 1, "\r");

        assert_eq!(
            source_from_lines(&lines, &endings, "\r"),
            "a\rb\rnew\nc\r\n"
        );
    }

    #[test]
    fn deleted_boundary_keeps_untouched_later_endings() {
        let lines = ["ab", "c", ""].map(str::to_owned);
        let (_, mut endings) = logical_lines_and_endings("a\rb\nc\r\n");
        apply_line_edit(&mut endings, 0, 1, 0, "\r");

        assert_eq!(source_from_lines(&lines, &endings, "\r"), "ab\nc\r\n");
    }

    #[test]
    fn bare_cr_source_uses_separate_editor_rows_and_preserves_insertions() {
        let original = "a\rb\r\nc\nd\r";
        let mut state = state_with_source("bare-cr-insert", original);

        assert_eq!(state.default_line_ending, "\r");
        assert_eq!(state.textarea.lines(), ["a", "b", "c", "d", ""]);
        for row in 1..=4 {
            handle_editor_event(&mut state, key(KeyCode::Down, KeyModifiers::NONE));
            assert_eq!(state.textarea.cursor().0, row);
        }

        state.textarea.move_cursor(CursorMove::Jump(1, 0));
        handle_editor_event(&mut state, key(KeyCode::Enter, KeyModifiers::NONE));
        let inserted = "a\r\rb\r\nc\nd\r";
        assert_eq!(state.source(), inserted);
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), inserted);

        handle_editor_event(&mut state, key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(state.source(), original);
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), original);

        handle_editor_event(&mut state, key(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(state.source(), inserted);
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), inserted);
        finish(state);
    }

    #[test]
    fn bare_cr_source_preserves_untouched_boundaries_after_deletion_and_history() {
        let original = "a\rb\r\nc\nd\r";
        let mut state = state_with_source("bare-cr-delete", original);
        state.textarea.move_cursor(CursorMove::Jump(1, 0));

        handle_editor_event(&mut state, key(KeyCode::Backspace, KeyModifiers::NONE));
        let deleted = "ab\r\nc\nd\r";
        assert_eq!(state.source(), deleted);
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), deleted);

        handle_editor_event(&mut state, key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(state.source(), original);
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), original);

        handle_editor_event(&mut state, key(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(state.source(), deleted);
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), deleted);
        finish(state);
    }

    #[test]
    fn leading_bom_stays_outside_the_editor_and_survives_history() {
        let original = "\u{feff}flowchart TD\nA --> B\n";
        let mut state = state_with_source("bom-space", original);

        assert!(state.has_bom);
        assert_eq!(state.textarea.lines()[0], "flowchart TD");
        assert!(!state.textarea.lines()[0].starts_with('\u{feff}'));

        handle_editor_event(&mut state, key(KeyCode::Char(' '), KeyModifiers::NONE));
        let edited = "\u{feff} flowchart TD\nA --> B\n";
        assert_eq!(state.source(), edited);
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), edited);
        assert_eq!(edited.matches('\u{feff}').count(), 1);
        assert!(state.render((100, 100)));

        handle_editor_event(&mut state, key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(state.source(), original);
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), original);

        handle_editor_event(&mut state, key(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(state.source(), edited);
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), edited);
        finish(state);
    }

    #[test]
    fn leading_bom_stays_at_the_boundary_when_enter_is_inserted_first() {
        let original = "\u{feff}flowchart TD\nA --> B\n";
        let mut state = state_with_source("bom-enter", original);

        handle_editor_event(&mut state, key(KeyCode::Enter, KeyModifiers::NONE));

        let edited = "\u{feff}\nflowchart TD\nA --> B\n";
        assert_eq!(state.source(), edited);
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), edited);
        assert_eq!(edited.matches('\u{feff}').count(), 1);
        assert!(state.render((100, 100)));
        finish(state);
    }

    #[test]
    fn blank_line_ctrl_k_deletes_the_selected_boundary_only() {
        let mut state = state_with_source("ctrl-k", "a\r\n\r\nc\n");
        state.textarea.move_cursor(CursorMove::Jump(1, 0));

        handle_editor_event(&mut state, key(KeyCode::Char('k'), KeyModifiers::CONTROL));

        assert_eq!(state.source(), "a\r\nc\n");
        assert_eq!(
            std::fs::read_to_string(&state.path).unwrap(),
            state.source()
        );
        finish(state);
    }

    #[test]
    fn ctrl_k_after_selection_returns_to_its_origin_deletes_a_blank_line() {
        let mut state = state_with_source("empty-selection-ctrl-k", "a\r\n\r\nc\n");
        state.textarea.move_cursor(CursorMove::Jump(1, 0));
        state.textarea.start_selection();
        state.textarea.move_cursor(CursorMove::Up);
        state.textarea.move_cursor(CursorMove::Down);

        assert_eq!(state.textarea.selection_range(), Some(((1, 0), (1, 0))));

        handle_editor_event(&mut state, key(KeyCode::Char('k'), KeyModifiers::CONTROL));

        assert_eq!(state.source(), "a\r\nc\n");
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), "a\r\nc\n");
        finish(state);
    }

    #[test]
    fn multiline_selection_deletion_retains_the_left_boundary() {
        let mut state =
            state_with_source("selection", "flowchart TD\nA --> B\nB --> C\r\nC --> D\n");
        state.textarea.move_cursor(CursorMove::Jump(1, 0));
        state.textarea.start_selection();
        state.textarea.move_cursor(CursorMove::Jump(3, 0));

        handle_editor_event(&mut state, key(KeyCode::Char('x'), KeyModifiers::CONTROL));

        assert_eq!(state.source(), "flowchart TD\nC --> D\n");
        finish(state);
    }

    #[test]
    fn enter_on_a_blank_line_inserts_only_the_default_boundary() {
        let mut state = state_with_source("blank-enter", "a\r\n\r\nb\n");
        state.textarea.move_cursor(CursorMove::Jump(1, 0));

        handle_editor_event(&mut state, key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(state.source(), "a\r\n\r\n\r\nb\n");
        finish(state);
    }

    #[test]
    fn multiline_yank_inserts_default_boundaries() {
        let mut state = state_with_source("yank", "a\r\nb\nc\r\n");
        state.textarea.move_cursor(CursorMove::Jump(1, 0));
        state.textarea.set_yank_text("new1\nnew2\nnew3");

        handle_editor_event(&mut state, key(KeyCode::Char('y'), KeyModifiers::CONTROL));

        assert_eq!(state.source(), "a\r\nnew1\r\nnew2\r\nnew3b\nc\r\n");
        finish(state);
    }

    #[test]
    fn ctrl_y_with_empty_yank_saves_selection_deletion_and_undoes_it() {
        let original = "one\r\ntwo\n";
        let mut state = state_with_source("empty-yank", original);
        state.preview_dirty = false;
        state.textarea.set_yank_text("");
        state.textarea.move_cursor(CursorMove::Jump(0, 0));
        state.textarea.start_selection();
        state.textarea.move_cursor(CursorMove::Jump(1, 0));

        handle_editor_event(&mut state, key(KeyCode::Char('y'), KeyModifiers::CONTROL));

        assert_eq!(state.source(), "two\n");
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), "two\n");
        assert!(state.preview_dirty);

        handle_editor_event(&mut state, key(KeyCode::Char('u'), KeyModifiers::CONTROL));

        assert_eq!(state.source(), original);
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), original);
        finish(state);
    }

    #[test]
    fn undo_and_redo_restore_exact_line_endings() {
        let original = "a\r\nb\nc\r\n";
        let mut state = state_with_source("history", original);
        state.textarea.move_cursor(CursorMove::Jump(2, 0));

        handle_editor_event(&mut state, key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(state.source(), "a\r\nbc\r\n");

        handle_editor_event(&mut state, key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(state.source(), original);

        handle_editor_event(&mut state, key(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(state.source(), "a\r\nbc\r\n");
        finish(state);
    }

    #[test]
    fn undo_and_redo_restore_cursors_beyond_the_jump_limit() {
        let beyond = usize::from(u16::MAX) + 1;
        let source = format!("{}{}\n", "\n".repeat(beyond), "x".repeat(beyond));
        let mut state = state_with_source("large-cursor", &source);

        state.textarea.move_cursor(CursorMove::Jump(u16::MAX, 0));
        state.textarea.move_cursor(CursorMove::Down);
        state.textarea.move_cursor(CursorMove::End);
        assert_eq!(state.textarea.cursor(), (beyond, beyond));

        let snapshot = state.snapshot();
        state.push_undo(snapshot);
        state.textarea.insert_char('!');

        assert!(state.undo());
        assert_eq!(state.textarea.cursor(), (beyond, beyond));

        assert!(state.redo());
        assert_eq!(state.textarea.cursor(), (beyond, beyond + 1));
        finish(state);
    }

    #[test]
    fn event_loop_error_includes_a_pending_save_error() {
        assert_eq!(
            finalize_event_loop_result(
                Err("event read failed".to_string()),
                Some("cannot save 'diagram.mmd': permission denied"),
            ),
            Err("event read failed; cannot save 'diagram.mmd': permission denied".to_string())
        );
    }

    #[test]
    fn successful_event_loop_returns_a_pending_save_error() {
        assert_eq!(
            finalize_event_loop_result(
                Ok(()),
                Some("cannot save 'diagram.mmd': permission denied"),
            ),
            Err("cannot save 'diagram.mmd': permission denied".to_string())
        );
    }

    #[test]
    fn event_loop_result_is_unchanged_without_a_save_error() {
        assert_eq!(
            finalize_event_loop_result(Err("event read failed".to_string()), None),
            Err("event read failed".to_string())
        );
    }

    #[test]
    fn navigation_redraws_text_without_emitting_graphics() {
        let mut state = state_with_source("navigation", "one\ntwo");
        let path = state.path.clone();
        state.preview = Some(b"png".to_vec());
        state.dirty = false;
        state.preview_dirty = false;
        state.graphics_dirty = false;
        let before = state.textarea.cursor();

        handle_editor_event(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        );

        let mut bytes = Vec::new();
        let mut image_displayed = true;
        display_preview_if_dirty(
            state.image_id,
            state.preview.as_deref(),
            Rect::new(0, 0, 20, 10),
            &mut image_displayed,
            &mut state.graphics_dirty,
            &mut bytes,
        )
        .unwrap();

        assert_ne!(state.textarea.cursor(), before);
        assert!(state.dirty);
        assert!(!state.preview_dirty);
        assert!(bytes.is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo");
        finish(state);
    }

    #[test]
    fn changed_preview_or_geometry_emits_graphics() {
        let mut state = state_with_source("graphics", "flowchart TD\nA --> B\n");
        state.preview = Some(b"changed".to_vec());
        state.preview_dirty = false;
        state.graphics_dirty = true;
        let mut image_displayed = false;
        let mut changed = Vec::new();

        display_preview_if_dirty(
            state.image_id,
            state.preview.as_deref(),
            Rect::new(0, 0, 20, 10),
            &mut image_displayed,
            &mut state.graphics_dirty,
            &mut changed,
        )
        .unwrap();
        assert!(!changed.is_empty());

        mark_resized(&mut state);
        refresh_preview(&mut state, Rect::new(1, 1, 20, 10));
        let mut geometry = Vec::new();
        display_preview_if_dirty(
            state.image_id,
            state.preview.as_deref(),
            Rect::new(1, 1, 20, 10),
            &mut image_displayed,
            &mut state.graphics_dirty,
            &mut geometry,
        )
        .unwrap();

        assert!(!state.preview_dirty);
        assert!(!geometry.is_empty());
        finish(state);
    }

    #[test]
    fn missing_preview_deletes_a_previously_displayed_image() {
        let mut bytes = Vec::new();
        let mut image_displayed = true;

        display_preview(
            kitty::new_image_id(),
            None,
            Rect::new(0, 0, 20, 10),
            &mut image_displayed,
            &mut bytes,
        )
        .unwrap();

        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("a=d,d=I"));
        assert!(!text.contains("a=t"));
        assert!(!text.contains("a=p"));
        assert!(!image_displayed);
    }

    #[test]
    fn empty_preview_pane_deletes_a_previously_displayed_image() {
        let mut bytes = Vec::new();
        let mut image_displayed = true;

        display_preview(
            kitty::new_image_id(),
            Some(b"png"),
            Rect::new(0, 0, 0, 10),
            &mut image_displayed,
            &mut bytes,
        )
        .unwrap();

        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("a=d,d=I"));
        assert!(!text.contains("a=t"));
        assert!(!text.contains("a=p"));
        assert!(!image_displayed);
    }

    #[test]
    fn failed_render_retains_the_last_valid_preview_without_graphics_output() {
        let mut state = state_with_source("stale-preview", "not Mermaid");
        let preview = Renderer::new()
            .png("flowchart TD\nA --> B\n", 100, 100)
            .unwrap();
        state.preview = Some(preview.clone());
        state.graphics_dirty = false;

        assert!(!state.render((100, 100)));
        assert_eq!(state.preview.as_deref(), Some(preview.as_slice()));
        assert!(state.render_error.is_some());

        let mut bytes = Vec::new();
        let mut image_displayed = true;
        display_preview_if_dirty(
            state.image_id,
            state.preview.as_deref(),
            Rect::new(0, 0, 20, 10),
            &mut image_displayed,
            &mut state.graphics_dirty,
            &mut bytes,
        )
        .unwrap();
        assert!(bytes.is_empty());
        finish(state);
    }

    #[test]
    fn resize_reemits_a_cached_preview_when_pixels_are_unchanged() {
        let mut state = state_with_source("same-size-resize", "flowchart TD\nA --> B\n");
        let pane = Rect::new(0, 0, 20, 10);
        let size = pane_pixels(pane);
        assert!(state.render(size));

        state.textarea = configured_textarea(vec!["not Mermaid".to_string()], &state.path);
        state.preview_dirty = true;
        state.graphics_dirty = false;
        refresh_preview(&mut state, pane);
        assert!(state.render_error.is_some());
        assert!(!state.graphics_dirty);

        mark_resized(&mut state);
        refresh_preview(&mut state, pane);

        let mut output = Vec::new();
        let mut image_displayed = true;
        display_preview_if_dirty(
            state.image_id,
            state.preview.as_deref(),
            pane,
            &mut image_displayed,
            &mut state.graphics_dirty,
            &mut output,
        )
        .unwrap();

        assert!(!output.is_empty());
        finish(state);
    }

    #[test]
    fn invalid_source_resizes_the_retained_preview_without_clearing_its_error() {
        let mut state = state_with_source("retained-resize", "flowchart TD\nA --> B\n");
        let pane_a = Rect::new(0, 0, 20, 10);
        let size_a = pane_pixels(pane_a);
        assert!(state.render(size_a));
        let preview_a = state.preview.clone().unwrap();

        state.textarea = configured_textarea(vec!["not Mermaid".to_string()], &state.path);
        state.preview_dirty = true;
        state.graphics_dirty = false;
        refresh_preview(&mut state, pane_a);

        assert_eq!(state.preview.as_deref(), Some(preview_a.as_slice()));
        assert_eq!(state.preview_size, Some(size_a));
        assert!(state.render_error.is_some());
        assert!(!state.graphics_dirty);
        let mut unchanged = Vec::new();
        let mut image_displayed = true;
        display_preview_if_dirty(
            state.image_id,
            state.preview.as_deref(),
            pane_a,
            &mut image_displayed,
            &mut state.graphics_dirty,
            &mut unchanged,
        )
        .unwrap();
        assert!(unchanged.is_empty());

        let pane_b = Rect::new(0, 0, 30, 12);
        let size_b = pane_pixels(pane_b);
        assert_ne!(size_b, size_a);
        mark_resized(&mut state);
        refresh_preview(&mut state, pane_b);
        let preview_b = state.preview.clone().unwrap();

        assert_ne!(preview_b, preview_a);
        assert_eq!(state.preview_size, Some(size_b));
        assert!(state.render_error.is_some());
        assert!(state.graphics_dirty);
        let mut resized = Vec::new();
        display_preview_if_dirty(
            state.image_id,
            state.preview.as_deref(),
            pane_b,
            &mut image_displayed,
            &mut state.graphics_dirty,
            &mut resized,
        )
        .unwrap();
        assert!(!resized.is_empty());
        let mut duplicate = Vec::new();
        display_preview_if_dirty(
            state.image_id,
            state.preview.as_deref(),
            pane_b,
            &mut image_displayed,
            &mut state.graphics_dirty,
            &mut duplicate,
        )
        .unwrap();
        assert!(duplicate.is_empty());

        let pane_c = Rect::new(0, 0, 10, 5);
        let size_c = pane_pixels(pane_c);
        assert_ne!(size_c, size_b);
        mark_resized(&mut state);
        refresh_preview(&mut state, pane_c);
        assert_eq!(state.preview_size, Some(size_c));
        assert_ne!(state.preview.as_deref(), Some(preview_b.as_slice()));
        assert!(state.render_error.is_some());
        assert!(state.graphics_dirty);
        finish(state);
    }

    #[test]
    fn combines_primary_and_cleanup_errors() {
        assert_eq!(
            combine(Err("primary".to_string()), Err("cleanup".to_string())),
            Err("primary; cleanup failed: cleanup".to_string())
        );
    }

    #[test]
    fn pane_pixels_never_returns_zero() {
        let pixels = pane_pixels(Rect::new(0, 0, 0, 0));
        assert!(pixels.0 >= 1);
        assert!(pixels.1 >= 1);
    }
}
