#[macro_use]
pub mod util;
#[macro_use]
pub mod inventory;
#[macro_use]
pub mod recipe;
#[macro_use]
pub mod config_util;
pub mod access;
pub mod action;
pub mod config;
pub mod detail_cache;
pub mod factory;
pub mod item;
pub mod lua_value;
pub mod process;
pub mod server;
pub mod storage;
pub mod turtle_rc;

use config::build_factory_from_json;
use crossterm::{
    event::{Event, EventStream},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use futures_util::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Margin},
    style::Color,
    text::Line,
    widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame, Terminal,
};
use std::{
    collections::VecDeque,
    io::{stdout},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{select, sync::Notify};
use tui_textarea::{CursorMove, Input, Key, TextArea};
use atty::Stream;

pub trait UiTrait: Send + Sync {
    fn log(&self, msg: String, color: u8);
}

#[derive(Default)]
pub struct Tui {
    on_redraw: Notify,
    on_input: Notify,
    logs: Mutex<VecDeque<Line<'static>>>,
    input_queue: Mutex<Vec<String>>,
    text_area: Mutex<TextArea<'static>>,
    main_list: Mutex<Vec<Line<'static>>>,
    main_scroll: Mutex<u16>,
    main_scroll_state: Mutex<ScrollbarState>,
}

impl UiTrait for Tui {
    fn log(&self, msg: String, color: u8) {
        let color = match color {
            0 => Color::Reset,
            1 => Color::LightYellow,
            3 => Color::LightBlue,
            6 => Color::LightRed,
            10 => Color::LightMagenta,
            13 => Color::Green,
            14 => Color::Red,
            _ => unreachable!(),
        };
        let mut logs = self.logs.lock().unwrap();
        logs.push_back(Line::styled(msg, color));
        self.on_redraw.notify_one();
    }
}

impl Tui {
    fn request_redraw(&self) { self.on_redraw.notify_one(); }

    fn set_main_list(&self, list: Vec<Line<'static>>) {
        let mut main_list = self.main_list.lock().unwrap();
        *main_list = list;
        let mut scroll = self.main_scroll.lock().unwrap();
        *scroll = scroll.min(main_list.len().max(1) as u16 - 1);
        let mut state = self.main_scroll_state.lock().unwrap();
        *state = state.position(*scroll as usize).content_length(main_list.len());
        self.request_redraw();
    }

    fn set_main_scroll(&self, upd: impl FnOnce(u16) -> u16) {
        let list = self.main_list.lock().unwrap();
        let mut scroll = self.main_scroll.lock().unwrap();
        *scroll = upd(*scroll).min(list.len().max(1) as u16 - 1);
        let mut state = self.main_scroll_state.lock().unwrap();
        *state = state.position(*scroll as usize).content_length(list.len());
    }

    fn frame(&self, frame: &mut Frame) {
        let layout = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).split(frame.area());
        frame.render_widget(&*self.text_area.lock().unwrap(), layout[1]);

        let log_size;
        let main_list = self.main_list.lock().unwrap();
        if main_list.is_empty() {
            log_size = layout[0];
        } else {
            let layout = Layout::horizontal([Constraint::Percentage(50), Constraint::Fill(1)]).split(layout[0]);
            log_size = layout[0];
            let main_list_size = layout[1];
            frame.render_widget(Paragraph::new(main_list.clone()).scroll((*self.main_scroll.lock().unwrap(), 0)), main_list_size);
            let scroll = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(
                scroll,
                main_list_size.inner(Margin { horizontal: 1, vertical: 0 }),
                &mut *self.main_scroll_state.lock().unwrap(),
            );
        }

        let mut log_buffer = self.logs.lock().unwrap();
        while log_buffer.len() > log_size.height as _ {
            log_buffer.pop_front();
        }
        frame.render_widget(Paragraph::new(Vec::from_iter(log_buffer.iter().cloned())), log_size);
    }
}

pub struct NonInteractiveTui {
    logs: Mutex<VecDeque<String>>,
}

impl NonInteractiveTui {
    pub fn new() -> Self {
        Self {
            logs: Mutex::new(VecDeque::new()),
        }
    }
}

impl UiTrait for NonInteractiveTui {
    fn log(&self, msg: String, _color: u8) {
        println!("{}", msg);
        let mut logs = self.logs.lock().unwrap();
        logs.push_back(msg);
        if logs.len() > 1000 {
            logs.pop_front();
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if atty::is(Stream::Stdout) {
        run_interactive().await;
    } else {
        run_noninteractive().await;
    }
}

async fn run_interactive() {
    enable_raw_mode().unwrap();
    stdout().execute(EnterAlternateScreen).unwrap();
    let mut evts = EventStream::new();
    let mut term = Terminal::new(CrosstermBackend::new(std::io::stderr())).unwrap();
    let tui = Arc::new(Tui::default());
    let factory = build_factory_from_json(tui.clone() as Arc<dyn UiTrait>, "config.json");
    let factory_ref = Arc::new(Mutex::new(Some(factory)));
    config::start_factory_hot_reload(tui.clone() as Arc<dyn UiTrait>, "config.json", factory_ref.clone());
    loop {
        term.draw(|frame| tui.frame(frame)).unwrap();
        let evt = select! {
            () = tui.on_redraw.notified() => None,
            evt = evts.next() => if let Some(Ok(x)) = evt { Some(x) } else { break }
        };
        if let Some(Event::Key(evt)) = evt {
            let evt = Input::from(evt);
            if evt.ctrl && (evt.key == Key::Char('c') || evt.key == Key::Char('d')) {
                break;
            } else if evt.ctrl && evt.key == Key::Char('l') {
                let mut logs = tui.logs.lock().unwrap();
                logs.clear();
            } else if evt.key == Key::PageUp {
                let mut scroll = tui.main_scroll.lock().unwrap();
                *scroll = scroll.saturating_sub(8);
            } else if evt.key == Key::PageDown {
                let mut scroll = tui.main_scroll.lock().unwrap();
                *scroll = scroll.saturating_add(8);
            } else if evt.ctrl && evt.key == Key::Char('m') || evt.key == Key::Enter {
                let mut text_area = tui.text_area.lock().unwrap();
                let line = text_area.lines().get(text_area.cursor().0).cloned().unwrap_or_default();
                tui.input_queue.lock().unwrap().push(line);
                text_area.move_cursor(CursorMove::End);
                text_area.insert_newline();
            } else {
                tui.text_area.lock().unwrap().input(evt);
            }
            tui.on_input.notify_waiters();
        }
    }
    disable_raw_mode().unwrap();
    stdout().execute(LeaveAlternateScreen).unwrap();
}

async fn run_noninteractive() {
    let tui = Arc::new(NonInteractiveTui::new()) as Arc<dyn UiTrait>;
    let factory = build_factory_from_json(tui.clone(), "config.json");
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}