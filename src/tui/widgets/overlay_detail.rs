//! Detail overlay — opened with Enter on a row in positions/plays/alerts.
//!
//! Positions render via the existing `commands::positions::render_card`
//! (the vertical price-ladder card) converted to ratatui text. Plays render
//! a structured thesis + conditions block. Alerts dump the AlertFull payload.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::app::state::{App, DetailOverlay};
use crate::commands::chart::ansi_to_ratatui_text;
use crate::commands::positions::render_card;
use crate::tui::layout::centered_rect;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let Some(overlay) = &app.detail_overlay else {
        return;
    };

    let (title, text) = match overlay {
        DetailOverlay::Position(p) => {
            let body = render_card(p);
            (
                format!(" Position #{}  {} {}  ", p.id, p.asset, p.direction),
                ansi_to_ratatui_text(&body),
            )
        }
        DetailOverlay::Play(p) => (
            format!(" Play #{}  ·  {} ", p.id, truncate(&p.title, 40)),
            play_detail_text(p),
        ),
        DetailOverlay::Alert(a) => (
            format!(
                " Alert {}  ·  {} ",
                a.id_str(),
                a.symbol.clone().unwrap_or_else(|| "—".into())
            ),
            alert_detail_text(a),
        ),
    };

    let popup_w = 80u16.min(area.width.saturating_sub(2)).max(40);
    let popup_h = (area.height.saturating_sub(2)).max(8);
    let popup = centered_rect(popup_w, popup_h, area);

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        )))
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let scroll = app.detail_scroll;
    let para = Paragraph::new(text).scroll((scroll, 0));
    frame.render_widget(para, inner);
}

fn play_detail_text(p: &crate::types::plays::Play) -> ratatui::text::Text<'static> {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(label_value(
        "Asset",
        &p.primary_asset.clone().unwrap_or_else(|| "—".into()),
    ));
    if let Some(d) = &p.direction {
        let color = super::direction_color(d);
        lines.push(label_value_styled(
            "Direction",
            d,
            Style::default().fg(color),
        ));
    }
    if let Some(s) = &p.status {
        lines.push(label_value("Status", s));
    }
    if let Some(tf) = &p.timeframe {
        lines.push(label_value("Timeframe", tf));
    }
    if let Some(exp) = &p.expiry {
        lines.push(label_value("Expiry", exp));
    }
    let (met, total) = p.condition_counts();
    let prog_color = if total > 0 && met == total {
        Color::Green
    } else if met > 0 {
        Color::Yellow
    } else {
        Color::Reset
    };
    lines.push(label_value_styled(
        "Progress",
        &format!("{}/{}", met, total),
        Style::default().fg(prog_color),
    ));
    lines.push(Line::from(""));

    if let Some(thesis) = &p.thesis {
        lines.push(section_header("Thesis"));
        for w in wrap(thesis, 72) {
            lines.push(Line::from(format!("  {}", w)));
        }
        lines.push(Line::from(""));
    }
    if let Some(ap) = &p.action_plan {
        lines.push(section_header("Action Plan"));
        for w in wrap(ap, 72) {
            lines.push(Line::from(format!("  {}", w)));
        }
        lines.push(Line::from(""));
    }
    if let Some(conds) = &p.conditions {
        lines.push(section_header(&format!(
            "Conditions ({})",
            conds.logic.clone().unwrap_or_else(|| "AND".into())
        )));
        for r in &conds.rules {
            let icon = if r.met { "✓" } else { "·" };
            let color = if r.met { Color::Green } else { Color::DarkGray };
            let desc = r.description.clone().unwrap_or_else(|| "—".into());
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", icon), Style::default().fg(color)),
                Span::raw(desc),
            ]));
            if let Some(ev) = &r.evidence {
                lines.push(Line::from(Span::styled(
                    format!("       {}", ev),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                )));
            }
        }
        lines.push(Line::from(""));
    }
    if let Some(inv) = &p.invalidation {
        lines.push(section_header("Invalidation"));
        let icon = if inv.met { "✗ TRIPPED" } else { "•" };
        let color = if inv.met { Color::Red } else { Color::Reset };
        lines.push(Line::from(vec![
            Span::styled(format!("  {} ", icon), Style::default().fg(color)),
            Span::raw(inv.description.clone().unwrap_or_else(|| "—".into())),
        ]));
        if let Some(ev) = &inv.evidence {
            lines.push(Line::from(Span::styled(
                format!("       {}", ev),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
        }
    }
    ratatui::text::Text::from(lines)
}

fn alert_detail_text(a: &crate::types::alerts::AlertFull) -> ratatui::text::Text<'static> {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(label_value("ID", &a.id_str()));
    if let Some(s) = &a.symbol {
        lines.push(label_value("Symbol", s));
    }
    if let Some(t) = &a.alert_type {
        lines.push(label_value("Type", t));
    }
    if let Some(c) = &a.condition {
        lines.push(label_value("Condition", c));
    }
    if let Some(p) = a.price {
        lines.push(label_value("Price", &format!("{}", p)));
    }
    if let Some(p) = a.triggered_price {
        lines.push(label_value("Triggered @", &format!("{}", p)));
    }
    if let Some(t) = &a.triggered_at {
        lines.push(label_value("Triggered At", t));
    }
    if let Some(s) = &a.target_state {
        lines.push(label_value("Target State", s));
    }
    if let Some(i) = &a.indicator {
        lines.push(label_value("Indicator", i));
    }
    if let Some(f) = &a.frequency {
        lines.push(label_value("Frequency", f));
    }
    if let Some(active) = a.active {
        let s = if active { "active" } else { "inactive" };
        let color = if active {
            Color::Green
        } else {
            Color::DarkGray
        };
        lines.push(label_value_styled("State", s, Style::default().fg(color)));
    }
    if let Some(n) = &a.note
        && !n.is_empty()
    {
        lines.push(Line::from(""));
        lines.push(section_header("Note"));
        for w in wrap(n, 72) {
            lines.push(Line::from(format!("  {}", w)));
        }
    }
    ratatui::text::Text::from(lines)
}

fn label_value(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {:<13} ", label),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(value.to_string()),
    ])
}

fn label_value_styled(label: &str, value: &str, value_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {:<13} ", label),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(value.to_string(), value_style),
    ])
}

fn section_header(s: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {}", s),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", t)
    }
}

fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in s.split('\n') {
        let mut cur = String::new();
        for word in paragraph.split_whitespace() {
            if cur.chars().count() + word.chars().count() + 1 > width {
                if !cur.is_empty() {
                    out.push(cur.clone());
                }
                cur.clear();
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        if paragraph.is_empty() {
            out.push(String::new());
        }
    }
    out
}
