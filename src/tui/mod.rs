pub mod events;
pub mod state;
pub mod ui;

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tracing::warn;

use crate::storage::StorageEngine;
use crate::tui::events::ServerEvent;
use crate::tui::state::TuiState;

pub struct TuiHandle {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TuiHandle {
    pub fn init() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    pub fn restore(&mut self) -> Result<()> {
        disable_raw_mode()?;
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
        self.terminal.show_cursor()?;
        Ok(())
    }
}

impl Drop for TuiHandle {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub async fn run(
    mut events: mpsc::UnboundedReceiver<ServerEvent>,
    engine: StorageEngine,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
) -> Result<()> {
    let mut handle = TuiHandle::init()?;
    let mut state = TuiState::default();
    let frame_budget = Duration::from_millis(33);
    let mut last_disk = Instant::now() - Duration::from_secs(10);

    loop {
        while let Ok(ev) = events.try_recv() {
            state.apply(ev);
        }

        if last_disk.elapsed() > Duration::from_secs(2) {
            if let Ok((used, cap)) = engine.disk_usage().await {
                state.disk_used = used;
                state.disk_capacity = cap;
            }
            state.tick_throughput();
            last_disk = Instant::now();
        }

        handle.terminal.draw(|f| ui::draw(f, &state))?;

        if event::poll(frame_budget)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    let quit = matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q'))
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL));
                    if quit {
                        let _ = shutdown_tx.send(true);
                        break;
                    }
                }
            }
        }

        if *shutdown_tx.borrow() {
            break;
        }
    }

    if let Err(err) = handle.restore() {
        warn!("tui restore: {err:#}");
    }
    Ok(())
}
