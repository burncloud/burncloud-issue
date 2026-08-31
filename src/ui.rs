use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::{
    app::{App, Focus, Screen},
    models::MessageRole,
};

pub fn draw(frame: &mut Frame, app: &App) {
    let root = frame.area().inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(10),
            Constraint::Length(2),
        ])
        .split(root);

    draw_header(frame, app, rows[0]);
    match app.screen {
        Screen::Tree => draw_tree_body(frame, app, rows[1]),
        Screen::Chat => draw_chat_body(frame, app, rows[1]),
    }
    draw_footer(frame, app, rows[2]);

    if app.confirm_create {
        draw_confirmation(frame, app);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let (done, total, percent) = app.snapshot.overall_progress();
    let title = format!(
        " BurnCloud Issue · {} · Required {}/{} · {}% ",
        app.repository, done, total, percent
    );
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = padded(block.inner(area));
    frame.render_widget(block, area);

    let local = if app.local_repo.exists() {
        app.local_repo.display().to_string()
    } else {
        format!("{} (not found)", app.local_repo.display())
    };
    let activity = if let Some(started) = app.sync_started {
        elapsed_label("GitHub sync", started.elapsed().as_secs())
    } else if let Some(started) = app.finalize_started {
        elapsed_label("Issue Quality Gate", started.elapsed().as_secs())
    } else if let Some(started) = app.ai_started {
        elapsed_label("Codex", started.elapsed().as_secs())
    } else {
        match app.screen {
            Screen::Tree => format!("任务树 · Ready {}", app.snapshot.ready_issues().len()),
            Screen::Chat => "Issue 对话".into(),
        }
    };
    frame.render_widget(
        Paragraph::new(format!("本地代码: {local}\n状态: {activity}")),
        inner,
    );
}

fn draw_tree_body(frame: &mut Frame, app: &App, area: Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);
    draw_tree(frame, app, columns[0]);
    draw_detail(frame, app, columns[1]);
}

fn draw_tree(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Tree;
    let title = if focused {
        " 任务树 [当前焦点 · ↑↓选择 · ←→层级 · Enter展开] "
    } else {
        " 任务树 "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(title);
    let inner = padded(block.inner(area));
    frame.render_widget(block, area);

    let rows = app.tree_rows();
    if rows.is_empty() {
        let message = if app.syncing() {
            "正在同步 GitHub Issue / PR…"
        } else {
            "没有可显示的 Issue。按 R 刷新，按 C 创建任务。"
        };
        frame.render_widget(Paragraph::new(message), inner);
        return;
    }

    let height = inner.height.max(1) as usize;
    let start = app
        .tree_selected
        .saturating_add(1)
        .saturating_sub(height)
        .min(rows.len().saturating_sub(1));
    let end = (start + height).min(rows.len());
    let mut lines = Vec::new();
    for (index, row) in rows[start..end].iter().enumerate() {
        let absolute = start + index;
        let prefix = if row.expandable {
            if row.expanded { "▼" } else { "▶" }
        } else {
            "•"
        };
        let indent = "  ".repeat(row.depth);
        let text = format!("{indent}{prefix} {}", row.label);
        if absolute == app.tree_selected {
            lines.push(Line::from(Span::styled(
                text,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
        } else {
            lines.push(Line::raw(text));
        }
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Detail;
    let title = if focused {
        " 详情 [阅读模式 · ↑↓滚动 · PgUp/PgDn翻页 · ←返回] "
    } else {
        " 详情 [→ 或 Tab 进入] "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(title);
    let inner = padded(block.inner(area));
    frame.render_widget(block, area);

    let content = safe_text(&app.detail_text());
    let paragraph = Paragraph::new(content).wrap(Wrap { trim: false });
    let scroll = safe_scroll(&paragraph, app.detail_scroll, inner);
    frame.render_widget(paragraph.scroll((scroll, 0)), inner);
}

fn draw_chat_body(frame: &mut Frame, app: &App, area: Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(area);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(5)])
        .split(columns[0]);

    draw_chat(frame, app, left[0]);
    draw_input(frame, app, left[1]);
    draw_preview(frame, app, columns[1]);
}

fn draw_chat(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Chat;
    let title = if focused {
        " Issue 对话 [当前焦点 · ↑↓滚动 · →预览] "
    } else {
        " Issue 对话 "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(title);
    let inner = padded(block.inner(area));
    frame.render_widget(block, area);

    let mut lines = Vec::new();
    for message in &app.messages {
        let (prefix, style) = match message.role {
            MessageRole::User => (
                "你  ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            MessageRole::Assistant => (
                "AI  ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            MessageRole::System => ("系统", Style::default().fg(Color::Yellow)),
        };
        let content = safe_text(&message.content);
        let mut parts = content.lines();
        if let Some(first) = parts.next() {
            lines.push(Line::from(vec![
                Span::styled(format!("{prefix}: "), style),
                Span::raw(first.to_string()),
            ]));
        }
        for line in parts {
            lines.push(Line::from(format!("      {line}")));
        }
        lines.push(Line::raw(""));
    }

    let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    let scroll = safe_scroll(&paragraph, app.chat_scroll, inner);
    frame.render_widget(paragraph.scroll((scroll, 0)), inner);
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Input;
    let title = if focused {
        " 输入 [当前焦点 · Enter发送 · ←→移动光标] "
    } else {
        " 输入 "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(title);
    let inner = padded(block.inner(area));
    frame.render_widget(block, area);

    let width = inner.width.saturating_sub(1) as usize;
    let (visible, cursor_column) = app.input_view(width.max(2));
    let placeholder = if visible.is_empty() && !focused {
        "描述当前节点下面还需要完成的工程任务…"
    } else {
        &visible
    };
    frame.render_widget(
        Paragraph::new(placeholder.to_string()).style(if visible.is_empty() {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        }),
        inner,
    );

    if focused {
        frame.set_cursor_position((inner.x.saturating_add(cursor_column), inner.y));
    }
}

fn draw_preview(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Preview;
    let title = if focused {
        " Issue 草稿 [当前焦点 · ↑↓/PgUp/PgDn滚动 · ←返回] "
    } else {
        " Issue 草稿 "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(focus_border(focused))
        .title(title);
    let inner = padded(block.inner(area));
    frame.render_widget(block, area);

    let content = safe_text(&app.preview_text());
    let paragraph = Paragraph::new(content).wrap(Wrap { trim: false });
    let scroll = safe_scroll(&paragraph, app.preview_scroll, inner);
    frame.render_widget(paragraph.scroll((scroll, 0)), inner);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let controls = match app.screen {
        Screen::Tree => {
            "↑↓选择  ←→层级/阅读  Enter展开  Tab切换  PgUp/PgDn详情  C创建子Issue  R刷新  Ctrl+Q退出"
        }
        Screen::Chat => {
            "Enter发送  Tab切换  ↑↓滚动  F2最终检查  F4创建  Esc任务树  Ctrl+C取消AI  Ctrl+Q退出"
        }
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                safe_text(&app.status),
                Style::default().fg(Color::Gray),
            )),
            Line::from(Span::styled(controls, Style::default().fg(Color::DarkGray))),
        ]),
        area,
    );
}

fn draw_confirmation(frame: &mut Frame, app: &App) {
    let area = centered_rect(72, 42, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" 最终确认：这一步会写入 GitHub ");
    let inner = padded(block.inner(area));
    frame.render_widget(block, area);

    let title = app
        .draft
        .as_ref()
        .map(|draft| safe_text(&draft.title))
        .unwrap_or_else(|| "<missing draft>".into());
    let text = format!(
        "目标仓库：{}\n\nIssue：{}\n\nQuality Gate：{}\n\n父 Issue：{}\nMilestone：{}\n\nBurnCloud Issue 不允许 AI 自行提交。只有你在这个确认框中明确同意，才会执行 GitHub Create Issue。\n\n[Y] 我确认创建\n[N / Esc] 返回继续修改",
        app.repository,
        title,
        app.quality_gate.status,
        app.draft
            .as_ref()
            .and_then(|draft| draft.parent_issue)
            .map(|number| format!("#{number}"))
            .unwrap_or_else(|| "None".into()),
        app.draft
            .as_ref()
            .and_then(|draft| draft.milestone)
            .map(|number| format!("#{number}"))
            .unwrap_or_else(|| "None".into())
    );
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
}

fn focus_border(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Blue)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn elapsed_label(name: &str, seconds: u64) -> String {
    format!("{name} {:02}:{:02}", seconds / 60, seconds % 60)
}

fn safe_scroll(paragraph: &Paragraph<'_>, requested: u16, area: Rect) -> u16 {
    if area.width == 0 || area.height == 0 {
        return 0;
    }

    let max_without_overflow = u16::MAX.saturating_sub(area.height);
    let max_for_content = paragraph
        .line_count(area.width)
        .saturating_sub(area.height as usize)
        .min(max_without_overflow as usize) as u16;

    if requested == u16::MAX {
        max_for_content
    } else {
        requested.min(max_for_content)
    }
}

fn padded(area: Rect) -> Rect {
    if area.width <= 2 || area.height <= 2 {
        area
    } else {
        area.inner(Margin {
            horizontal: 1,
            vertical: 1,
        })
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn safe_text(value: &str) -> String {
    value
        .chars()
        .filter(|ch| *ch == '\n' || *ch == '\t' || !ch.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_controls_are_removed_from_display_text() {
        assert_eq!(safe_text("abc\u{1b}[31mdef\rghi"), "abc[31mdefghi");
    }

    #[test]
    fn end_scroll_sentinel_is_resolved_to_content_bottom() {
        let paragraph = Paragraph::new("one\ntwo\nthree\nfour").wrap(Wrap { trim: false });
        let area = Rect::new(0, 0, 20, 2);
        assert_eq!(safe_scroll(&paragraph, u16::MAX, area), 2);
    }

    #[test]
    fn resolved_scroll_cannot_overflow_ratatui_render_math() {
        let content = "line\n".repeat(70_000);
        let paragraph = Paragraph::new(content).wrap(Wrap { trim: false });
        let area = Rect::new(0, 0, 20, 10);
        let scroll = safe_scroll(&paragraph, u16::MAX, area);
        assert!(scroll <= u16::MAX - area.height);
    }
}
