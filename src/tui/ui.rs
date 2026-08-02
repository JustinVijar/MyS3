use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Row, Sparkline, Table};
use ratatui::Frame;

use crate::tui::state::TuiState;

pub fn draw(frame: &mut Frame, state: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(7),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_gauge(frame, chunks[0], state);
    draw_traffic(frame, chunks[1], state);
    draw_table(frame, chunks[2], state);
    frame.render_widget(
        Paragraph::new("q / Ctrl+C quit · MyS3 engine"),
        chunks[3],
    );
}

fn draw_gauge(frame: &mut Frame, area: Rect, state: &TuiState) {
    let ratio = if state.disk_capacity == 0 {
        0.0
    } else {
        (state.disk_used as f64 / state.disk_capacity as f64).clamp(0.0, 1.0)
    };
    let label = format!(
        "Disk {:.1}%  {} / {}  objects={}",
        ratio * 100.0,
        human(state.disk_used),
        human(state.disk_capacity),
        state.object_count
    );
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title("Storage"))
        .gauge_style(Style::default().fg(Color::Cyan))
        .ratio(ratio)
        .label(label);
    frame.render_widget(gauge, area);
}

fn draw_traffic(frame: &mut Frame, area: Rect, state: &TuiState) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);

    let data: Vec<u64> = state.throughput_samples.iter().copied().collect();
    let spark = Sparkline::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(
                    "Throughput ↑{} ↓{} peers={}",
                    human(state.bytes_up),
                    human(state.bytes_down),
                    state.peers.len()
                )),
        )
        .data(&data)
        .style(Style::default().fg(Color::Green));
    frame.render_widget(spark, cols[0]);

    let peers = if state.peers.is_empty() {
        "none".to_string()
    } else {
        state.peers.join("\n")
    };
    frame.render_widget(
        Paragraph::new(peers).block(Block::default().borders(Borders::ALL).title("Peers")),
        cols[1],
    );
}

fn draw_table(frame: &mut Frame, area: Rect, state: &TuiState) {
    let header = Row::new(vec!["Filename", "Size", "ETag", "Algorithm"])
        .style(Style::default().add_modifier(Modifier::BOLD));
    let rows = state.recent.iter().map(|o| {
        let etag = if o.etag.len() > 12 {
            format!("{}…", &o.etag[..12])
        } else if o.etag.is_empty() {
            "—".into()
        } else {
            o.etag.clone()
        };
        Row::new(vec![
            o.filename.clone(),
            human(o.size as u64),
            etag,
            if o.algorithm.is_empty() {
                "—".into()
            } else {
                o.algorithm.clone()
            },
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(40),
            Constraint::Percentage(15),
            Constraint::Percentage(25),
            Constraint::Percentage(20),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Recent objects"),
    );
    frame.render_widget(table, area);
}

fn human(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let f = n as f64;
    if f >= GB {
        format!("{:.2} GiB", f / GB)
    } else if f >= MB {
        format!("{:.1} MiB", f / MB)
    } else if f >= KB {
        format!("{:.1} KiB", f / KB)
    } else {
        format!("{n} B")
    }
}
