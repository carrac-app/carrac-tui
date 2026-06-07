//! Add-alert form overlay.
//!
//! Opened with capital `A` from the Alerts view. A centered modal that lets the
//! user fill in symbol / condition / price / note and submit a POST to
//! `/api/alerts`.
//!
//! Layout (60 cols × 14 rows):
//!   ╔══ Create Alert ═════════════════════════════════════╗
//!   ║  Symbol    [ BTC___________ ]                       ║
//!   ║  Condition  [■ above ] / [ below ]                  ║
//!   ║  Price     [ 75000.00_____ ]                        ║
//!   ║  Note      [ ___________________________ ]          ║
//!   ║                                                     ║
//!   ║  Tab/Shift-Tab: move · Enter: submit · Esc: cancel  ║
//!   ║  error message (dim red, if any)                    ║
//!   ╚═════════════════════════════════════════════════════╝

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::app::state::{AddAlertField, App};
use crate::tui::layout::centered_rect;

const MODAL_W: u16 = 60;
const MODAL_H: u16 = 14;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let Some(form) = &app.add_alert_form else {
        return;
    };

    let popup_w = MODAL_W.min(area.width.saturating_sub(2)).max(40);
    let popup_h = MODAL_H.min(area.height.saturating_sub(2)).max(8);
    let popup = centered_rect(popup_w, popup_h, area);

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(Line::from(Span::styled(
            " Create Alert ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )))
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Field rows + footer.
    let mut lines: Vec<Line> = Vec::new();

    // ── Symbol ──
    let sym_focused = form.field == AddAlertField::Symbol;
    lines.push(field_row(
        "Symbol",
        sym_focused,
        Line::from(vec![
            Span::styled(" ", label_style(sym_focused)),
            Span::raw(form.symbol.clone()),
            cursor_span(sym_focused),
        ]),
    ));

    // ── Condition ──
    let cond_focused = form.field == AddAlertField::Condition;
    let above_style = if form.condition == crate::app::state::AlertCondition::Above {
        Style::default().bg(Color::Cyan).fg(Color::Black)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let below_style = if form.condition == crate::app::state::AlertCondition::Below {
        Style::default().bg(Color::Cyan).fg(Color::Black)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    lines.push(field_row_label(
        "Condition",
        cond_focused,
        Line::from(vec![
            Span::styled(" [ ", label_style(cond_focused)),
            Span::styled("above", above_style),
            Span::styled(" ] / [ ", label_style(cond_focused)),
            Span::styled("below", below_style),
            Span::styled(" ] ", label_style(cond_focused)),
            if cond_focused {
                Span::styled(
                    " ←/→ toggle",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                )
            } else {
                Span::raw("")
            },
        ]),
    ));

    // ── Price ──
    let price_focused = form.field == AddAlertField::Price;
    lines.push(field_row(
        "Price",
        price_focused,
        Line::from(vec![
            Span::styled(" ", label_style(price_focused)),
            Span::raw(form.price.clone()),
            cursor_span(price_focused),
        ]),
    ));

    // ── Note ──
    let note_focused = form.field == AddAlertField::Note;
    lines.push(field_row(
        "Note",
        note_focused,
        Line::from(vec![
            Span::styled(" ", label_style(note_focused)),
            Span::raw(form.note.clone()),
            cursor_span(note_focused),
        ]),
    ));

    // ── Spacer ──
    lines.push(Line::from(""));

    // ── Footer / status ──
    if form.submitting {
        lines.push(Line::from(Span::styled(
            "  submitting…",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::DIM),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "  Tab/Shift-Tab: move · Enter: submit · Esc: cancel",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        )));
    }

    // ── Error line ──
    if let Some(err) = &form.error {
        lines.push(Line::from(Span::styled(
            format!("  ✗ {}", err),
            Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
        )));
    }

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}

/// Render a labeled field row where the value occupies a text-input box.
fn field_row(label: &str, focused: bool, value: Line<'static>) -> Line<'static> {
    let label_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let box_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let mut spans = vec![Span::styled(format!("  {:<10} [", label), label_style)];
    spans.extend(value.spans);
    spans.push(Span::styled("]", box_style));
    Line::from(spans)
}

/// Like `field_row` but the value line already has its own full formatting
/// (used for the Condition row with toggle buttons).
fn field_row_label(label: &str, focused: bool, value: Line<'static>) -> Line<'static> {
    let label_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let mut spans = vec![Span::styled(format!("  {:<10} ", label), label_style)];
    spans.extend(value.spans);
    Line::from(spans)
}

fn label_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Reset)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn cursor_span(focused: bool) -> Span<'static> {
    if focused {
        Span::styled(
            "▏",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::SLOW_BLINK),
        )
    } else {
        Span::raw("")
    }
}
