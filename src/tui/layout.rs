use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Rects produced by a single `build_layout` call.
///
/// 3-region model: no bottom bar — overlays surface mode,
/// and refresh staleness is shown next to the TF on the asset row.
pub struct LayoutAreas {
    pub menubar: Rect,
    pub asset_bar: Rect,
    pub main: Rect,
}

pub fn build_layout(area: Rect) -> LayoutAreas {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

    LayoutAreas {
        menubar: chunks[0],
        asset_bar: chunks[1],
        main: chunks[2],
    }
}

/// Center a fixed-width / fixed-height popup over a parent area.
pub fn centered_rect(width: u16, height: u16, parent: Rect) -> Rect {
    let w = width.min(parent.width);
    let h = height.min(parent.height);
    let x = parent.x + (parent.width.saturating_sub(w)) / 2;
    let y = parent.y + (parent.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}
