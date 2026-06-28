use std::io;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
    Terminal,
};
use tokio::sync::RwLock;

use crate::types::AppState;

pub async fn run_dashboard(state: Arc<RwLock<AppState>>) -> Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut tbl_state = TableState::default();
    tbl_state.select(Some(0));

    loop {
        let snap = state.read().await.clone();
        terminal.draw(|f| render(f, &snap, &mut tbl_state))?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(k) = event::read()? {
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Down => {
                        let n = snap.analyses.len();
                        if n > 0 {
                            let i = tbl_state.selected().unwrap_or(0);
                            tbl_state.select(Some((i + 1) % n));
                        }
                    }
                    KeyCode::Up => {
                        let n = snap.analyses.len();
                        if n > 0 {
                            let i = tbl_state.selected().unwrap_or(0);
                            tbl_state.select(Some(i.saturating_sub(1)));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

fn render(f: &mut ratatui::Frame, state: &AppState, tbl_state: &mut TableState) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(6),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.size());

    render_header(f, areas[0]);
    render_feeds(f, areas[1], state);
    render_positions(f, areas[2], state, tbl_state);
    render_footer(f, areas[3]);
}

fn render_header(f: &mut ratatui::Frame, area: ratatui::layout::Rect) {
    f.render_widget(
        Paragraph::new("  MIDNIGHT SHADOW MONITOR  ─  latent bad debt quantifier  ─  Morpho Midnight")
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn render_feeds(f: &mut ratatui::Frame, area: ratatui::layout::Rect, state: &AppState) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let cex_lines = match &state.cex {
        Some(c) => vec![
            Line::from(format!("  mid   ${:.2}", c.mid)),
            Line::from(format!("  bid   ${:.2}", c.bid)),
            Line::from(format!("  ask   ${:.2}", c.ask)),
            Line::from(format!("  sprd  {:.1} bps", c.spread_bps())),
        ],
        None => vec![Line::from("  connecting...")],
    };

    let oracle_lines = match &state.oracle {
        Some(o) => {
            let age = o.age_secs();
            let age_style = if age > 120.0 {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else if age > 45.0 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::White)
            };

            // No staleness check on-chain — what the protocol sees is always stale data
            let eta_line = match o.eta_secs {
                Some(eta) if eta > 0.0 => Line::from(vec![
                    Span::raw("  eta    "),
                    Span::styled(
                        format!("~{:.0}s", eta),
                        Style::default().fg(Color::Yellow),
                    ),
                ]),
                Some(_) | None if age > 60.0 => Line::from(vec![
                    Span::raw("  eta    "),
                    Span::styled(
                        "OVERDUE — fire imminent",
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    ),
                ]),
                _ => Line::from("  eta    —"),
            };

            vec![
                Line::from(format!("  price  ${:.2}", o.price)),
                Line::from(vec![
                    Span::raw("  age    "),
                    Span::styled(format!("{:.0}s", age), age_style),
                ]),
                Line::from(format!("  round  #{}", o.round_id)),
                eta_line,
            ]
        }
        None => vec![Line::from("  waiting...")],
    };

    f.render_widget(
        Paragraph::new(cex_lines)
            .block(Block::default().borders(Borders::ALL).title(" CEX (Binance) ")),
        cols[0],
    );
    f.render_widget(
        Paragraph::new(oracle_lines)
            .block(Block::default().borders(Borders::ALL).title(
                " Oracle (Chainlink sim) — NO staleness check on-chain "
            )),
        cols[1],
    );
}

fn render_positions(
    f: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    state: &AppState,
    tbl_state: &mut TableState,
) {
    let header = Row::new(vec![
        "Market", "Tier", "LIF", "Oracle h-LTV", "Shadow h-LTV", "Lag↓", "MEV Est.", "Status",
    ])
    .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
    .height(1);

    let rows: Vec<Row> = state.analyses.iter().map(|a| {
        let shadow_style = if a.shadow_ltv > 1.0 {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else if a.shadow_ltv > 0.92 {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Green)
        };

        let (status_str, status_style) = status_cell(a);

        let mev_cell = if a.full_liq_required {
            Cell::from(Span::styled(
                format!("${:.0} FULL", a.first_touch_mev),
                Style::default().fg(Color::Magenta),
            ))
        } else if a.first_touch_mev > 0.0 {
            Cell::from(Span::styled(
                format!("${:.0}", a.first_touch_mev),
                Style::default().fg(Color::Cyan),
            ))
        } else {
            Cell::from(Span::styled("—", Style::default().fg(Color::DarkGray)))
        };

        // Dutch auction overrides normal MEV display
        let mev_cell = if let Some(dl) = a.dutch_lif {
            let label = match a.dutch_mev {
                Some(m) if m > 0.0 => format!("${:.0} DUTCH {:.3}x", m, dl),
                _ => format!("DUTCH {:.3}x", dl),
            };
            Cell::from(Span::styled(label, Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)))
        } else {
            mev_cell
        };

        Row::new(vec![
            Cell::from(a.market_id.clone()),
            Cell::from(format!("{:.3}", a.lltv_tier)),
            Cell::from(format!("{:.3}x", a.blended_lif)),
            Cell::from(format!("{:.2}%", a.oracle_ltv * 100.0)),
            Cell::from(Span::styled(
                format!("{:.2}%", a.shadow_ltv * 100.0),
                shadow_style,
            )),
            Cell::from(if a.worst_lag_pct > 0.0 {
                Span::styled(
                    format!("{:.2}%", a.worst_lag_pct * 100.0),
                    if a.cliff_imminent {
                        Style::default().fg(Color::Red)
                    } else {
                        Style::default().fg(Color::Yellow)
                    },
                )
            } else {
                Span::styled("—", Style::default().fg(Color::DarkGray))
            }),
            mev_cell,
            Cell::from(Span::styled(status_str, status_style)),
        ])
    }).collect();

    f.render_stateful_widget(
        Table::new(rows)
            .header(header)
            .widths(&[
                Constraint::Length(24),
                Constraint::Length(7),
                Constraint::Length(8),
                Constraint::Length(13),
                Constraint::Length(13),
                Constraint::Length(8),
                Constraint::Length(13),
                Constraint::Min(20),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Shadow Position Analysis  [h-LTV = debt / maxDebt = debt / Σ cᵢ·pᵢ·LLTVᵢ] "),
            )
            .highlight_style(
                Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD),
            ),
        area,
        tbl_state,
    );
}

fn status_cell(a: &crate::types::ShadowAnalysis) -> (&'static str, Style) {
    if a.overdue {
        ("⏰ MATURED — DUTCH AUCTION", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))
    } else if a.cliff_imminent && a.latent_bad_debt > 0.0 {
        ("⚡ CLIFF — BAD DEBT", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
    } else if a.cliff_imminent {
        ("⚡ CLIFF — LIQ PENDING", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
    } else if a.shadow_ltv > 1.0 {
        ("☠  UNDERWATER", Style::default().fg(Color::Magenta))
    } else if a.worst_lag_pct > 0.0 {
        ("⚠  LAG↓", Style::default().fg(Color::Yellow))
    } else {
        ("✓  HEALTHY", Style::default().fg(Color::Green))
    }
}

fn render_footer(f: &mut ratatui::Frame, area: ratatui::layout::Rect) {
    f.render_widget(
        Paragraph::new(
            "  ↑↓ navigate   q quit   │   h-LTV > 1.0 = liquidatable   │  Lag↓ = downward oracle divergence only"
        )
        .style(Style::default().fg(Color::DarkGray)),
        area,
    );
}
