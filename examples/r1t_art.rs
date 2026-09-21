//! Offline artwork preview: cargo run --example r1t_art
//! Export real ratatui cells: cargo run --example r1t_art -- --export /tmp/r1t.json
#[path = "../src/vehicle_art.rs"]
mod vehicle_art;

use std::{io, path::PathBuf};

use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{backend::TestBackend, prelude::*, widgets::*};
use vehicle_art::{R1tArt, BACKGROUND};

#[derive(Parser)]
struct Options {
    /// Save the rendered terminal cells instead of opening an interactive preview.
    #[arg(long)]
    export: Option<PathBuf>,
    #[arg(long, default_value_t = 132)]
    width: u16,
    #[arg(long, default_value_t = 40)]
    height: u16,
}

fn draw(frame: &mut Frame) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(BACKGROUND)),
        area,
    );
    let compare = area.width >= 128 && area.height >= 36;
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(if compare { 14 } else { 0 }),
        Constraint::Length(2),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new("R I V I A N   R 1 T")
            .fg(Color::Rgb(191, 206, 192))
            .alignment(Alignment::Center),
        Rect::new(rows[0].x, rows[0].y + 1, rows[0].width, 1),
    );
    if let Some(art) = R1tArt::fit(rows[1].width, rows[1].height) {
        frame.render_widget(art, rows[1]);
    }
    if compare {
        let cols = Layout::horizontal([
            Constraint::Length(34),
            Constraint::Length(40),
            Constraint::Length(54),
        ])
        .flex(layout::Flex::Center)
        .split(rows[2]);
        for (area, width) in cols.iter().zip([32, 38, 52]) {
            let art = R1tArt::fit(width, 12).expect("preview sizes fit");
            frame.render_widget(art, Rect::new(area.x, area.y, area.width, 12));
            frame.render_widget(
                Paragraph::new(format!("{} × {}", art.width, art.height))
                    .fg(Color::Rgb(104, 118, 123))
                    .alignment(Alignment::Center),
                Rect::new(area.x, area.y + 12, area.width, 1),
            );
        }
    }
    frame.render_widget(
        Paragraph::new("Resize to preview · q to close")
            .fg(Color::Rgb(104, 118, 123))
            .alignment(Alignment::Center),
        rows[3],
    );
}

fn rgb(color: Color) -> [u8; 3] {
    match color {
        Color::Rgb(r, g, b) => [r, g, b],
        _ => [16, 21, 24],
    }
}

fn main() -> Result<()> {
    let options = Options::parse();
    if let Some(path) = options.export {
        let mut terminal = Terminal::new(TestBackend::new(options.width, options.height))?;
        terminal.draw(draw)?;
        let cells: Vec<_> = terminal.backend().buffer().content.iter().map(|cell| {
            serde_json::json!({"symbol": cell.symbol(), "fg": rgb(cell.fg), "bg": rgb(cell.bg)})
        }).collect();
        std::fs::write(
            path,
            serde_json::to_vec(&serde_json::json!({
                "width": options.width, "height": options.height, "cells": cells,
            }))?,
        )?;
        return Ok(());
    }

    let mut terminal = ratatui::init();
    let result = (|| -> io::Result<()> {
        loop {
            terminal.draw(draw)?;
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press
                    && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                {
                    break Ok(());
                }
            }
        }
    })();
    ratatui::restore();
    result?;
    Ok(())
}
