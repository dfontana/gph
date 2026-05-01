use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, window_size, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;
use tui_textarea::TextArea;

struct State {
    file_path: PathBuf,
    owns_temp: bool,
    source: String,
    textarea: TextArea<'static>,
    render: Result<(Vec<u8>, usize, usize), String>,
    error_msg: Option<String>,
    split_pct: u16,
    dragging: bool,
}

pub fn run(file: Option<PathBuf>) -> Result<(), String> {
    let mut state = init_state(file)?;

    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture).map_err(|e| e.to_string())?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

    let result = event_loop(&mut terminal, &mut state);

    crate::kitty::delete_all(terminal.backend_mut());
    let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = disable_raw_mode();

    if state.owns_temp {
        let _ = std::fs::remove_file(&state.file_path);
    }

    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut State,
) -> Result<(), String> {
    loop {
        let sz = terminal.size().map_err(|e| e.to_string())?;
        let size = Rect::new(0, 0, sz.width, sz.height);
        let right_pane = compute_right_pane_inner(size, state.split_pct);

        terminal
            .draw(|frame| draw_frame(frame, state))
            .map_err(|e| e.to_string())?;

        crate::kitty::delete_all(terminal.backend_mut());
        render_kitty_in_pane(state, right_pane, terminal.backend_mut());

        match event::read().map_err(|e| e.to_string())? {
            Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => break,
            Event::Key(KeyEvent {
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => break,
            // Ctrl-E: open $EDITOR (overrides Emacs end-of-line; use End key instead)
            Event::Key(KeyEvent {
                code: KeyCode::Char('e'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                open_editor(state, terminal)?;
            }
            Event::Resize(_, _) => {
                terminal.clear().map_err(|e| e.to_string())?;
            }
            Event::Mouse(mouse_event) => {
                handle_mouse(mouse_event, state, size);
            }
            ev => {
                if state.textarea.input(ev) {
                    state.source = state.textarea.lines().join("\n");
                    save_file(state)?;
                    rerender(state);
                }
            }
        }
    }
    Ok(())
}

fn init_state(file: Option<PathBuf>) -> Result<State, String> {
    let (file_path, owns_temp) = match file {
        Some(p) => (p, false),
        None => {
            let tmp = env::temp_dir().join(format!("gph-tui-{}.gph", std::process::id()));
            std::fs::write(&tmp, "(graph lr\n  (-> a b)\n)\n")
                .map_err(|e| format!("failed to create temp file: {e}"))?;
            (tmp, true)
        }
    };

    let source = std::fs::read_to_string(&file_path)
        .map_err(|e| format!("cannot read '{}': {e}", file_path.display()))?;
    let textarea = make_textarea(&source, &file_path);

    let mut state = State {
        file_path,
        owns_temp,
        source,
        textarea,
        render: Err("not rendered yet".into()),
        error_msg: None,
        split_pct: 50,
        dragging: false,
    };
    rerender(&mut state);
    Ok(state)
}

fn make_textarea(content: &str, path: &Path) -> TextArea<'static> {
    let lines: Vec<String> = content.lines().map(str::to_owned).collect();
    let mut ta = TextArea::from(lines);
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("untitled");
    ta.set_block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {filename} ")),
    );
    ta.set_line_number_style(Style::default().fg(Color::DarkGray));
    ta
}

fn save_file(state: &State) -> Result<(), String> {
    if state.source.ends_with('\n') || state.source.is_empty() {
        std::fs::write(&state.file_path, state.source.as_bytes())
    } else {
        let mut content = state.source.clone();
        content.push('\n');
        std::fs::write(&state.file_path, content.as_bytes())
    }
    .map_err(|e| format!("save failed: {e}"))
}

fn rerender(state: &mut State) {
    match crate::render_to_rgba(&state.source) {
        Ok(data) => {
            state.render = Ok(data);
            state.error_msg = None;
        }
        Err(msg) => {
            state.error_msg = Some(msg);
        }
    }
}

fn open_editor(
    state: &mut State,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<(), String> {
    save_file(state)?;

    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )
    .map_err(|e| e.to_string())?;
    disable_raw_mode().map_err(|e| e.to_string())?;

    let editor = env::var("EDITOR")
        .or_else(|_| env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".into());

    Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"{}\"", state.file_path.display()))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("failed to launch editor '{editor}': {e}"))?;

    enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )
    .map_err(|e| e.to_string())?;
    terminal.clear().map_err(|e| e.to_string())?;

    state.source =
        std::fs::read_to_string(&state.file_path).map_err(|e| format!("reload failed: {e}"))?;
    state.textarea = make_textarea(&state.source, &state.file_path);
    rerender(state);
    Ok(())
}

fn split_direction(area: Rect) -> Direction {
    // Prefer real pixel dimensions so the split matches the visual aspect ratio.
    // Fall back to a 2:1 cell approximation if the terminal doesn't report pixel size.
    let taller = if let Ok(ws) = window_size() {
        if ws.width > 0 && ws.height > 0 {
            ws.height > ws.width
        } else {
            area.height as u32 * 2 > area.width as u32
        }
    } else {
        area.height as u32 * 2 > area.width as u32
    };
    if taller {
        Direction::Vertical
    } else {
        Direction::Horizontal
    }
}

fn make_split_constraints(split_pct: u16) -> [Constraint; 2] {
    [
        Constraint::Percentage(split_pct),
        Constraint::Percentage(100 - split_pct),
    ]
}

fn divider_position(area: Rect, split_pct: u16) -> u16 {
    match split_direction(area) {
        Direction::Horizontal => area.x + (area.width as u32 * split_pct as u32 / 100) as u16,
        Direction::Vertical => area.y + (area.height as u32 * split_pct as u32 / 100) as u16,
    }
}

fn handle_mouse(event: MouseEvent, state: &mut State, terminal_size: Rect) {
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let div = divider_position(terminal_size, state.split_pct);
            let on_divider = match split_direction(terminal_size) {
                Direction::Horizontal => event.column.abs_diff(div) <= 1,
                Direction::Vertical => event.row.abs_diff(div) <= 1,
            };
            if on_divider {
                state.dragging = true;
            }
        }
        MouseEventKind::Drag(MouseButton::Left) if state.dragging => {
            let new_pct = match split_direction(terminal_size) {
                Direction::Horizontal => {
                    if terminal_size.width == 0 {
                        return;
                    }
                    (event.column as u32 * 100 / terminal_size.width as u32) as u16
                }
                Direction::Vertical => {
                    if terminal_size.height == 0 {
                        return;
                    }
                    (event.row as u32 * 100 / terminal_size.height as u32) as u16
                }
            };
            state.split_pct = new_pct.clamp(10, 90);
        }
        MouseEventKind::Up(_) => {
            state.dragging = false;
        }
        _ => {}
    }
}

fn draw_frame(frame: &mut ratatui::Frame, state: &State) {
    let area = frame.area();
    let dir = split_direction(area);

    let halves = Layout::default()
        .direction(dir)
        .constraints(make_split_constraints(state.split_pct))
        .split(area);

    let editor_split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(halves[0]);

    frame.render_widget(&state.textarea, editor_split[0]);

    let status = match &state.error_msg {
        Some(msg) => Span::styled(format!(" {msg} "), Style::default().fg(Color::Red)),
        None => Span::styled(
            "  Ctrl-E=open in $EDITOR  Ctrl-Q=quit  ",
            Style::default().fg(Color::DarkGray),
        ),
    };
    frame.render_widget(
        Paragraph::new(Line::from(status)).style(Style::default().bg(Color::Black)),
        editor_split[1],
    );

    // Preview border — kitty renders inside after the frame flush
    frame.render_widget(
        Block::default().borders(Borders::ALL).title(" Preview "),
        halves[1],
    );
}

fn compute_right_pane_inner(size: Rect, split_pct: u16) -> Rect {
    let halves = Layout::default()
        .direction(split_direction(size))
        .constraints(make_split_constraints(split_pct))
        .split(size);
    Block::default().borders(Borders::ALL).inner(halves[1])
}

fn render_kitty_in_pane(state: &State, pane: Rect, out: &mut impl Write) {
    let Ok((rgba, pw, ph)) = &state.render else {
        return;
    };
    if pane.width == 0 || pane.height == 0 {
        return;
    }
    let (display_cols, display_rows) =
        crate::kitty::compute_display_cells(*pw, *ph, pane.width, pane.height);
    let _ = write!(out, "\x1b[{};{}H", pane.y + 1, pane.x + 1);
    crate::kitty::display_in_pane(rgba, *pw, *ph, display_cols, display_rows, out);
}
