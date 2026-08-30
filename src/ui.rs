use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::{
    app::{App, Focus},
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
    draw_body(frame, app, rows[1]);
    draw_footer(frame, app, rows[2]);

    if app.confirm_create {
        draw_confirmation(frame, app);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let title = format!(" BurnCloud Issue · {} ", app.repository);
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = padded(block.inner(area));
    frame.render_widget(block, area);

    let local = if app.local_repo.exists() {
        app.local_repo.display().to_string()
    } else {
        format!("{} (not found)", app.local_repo.display())
    };
    let busy = if let Some(started) = app.finalize_started {
        format!(
            "最终检查 {:02}:{:02}",
            started.elapsed().as_secs() / 60,
            started.elapsed().as_secs() % 60
        )
    } else if let Some(started) = app.ai_started {
        format!(
            "Codex {:02}:{:02}",
            started.elapsed().as_secs() / 60,
            started.elapsed().as_secs() % 60
        )
    } else if app.created_issue.is_some() {
        "已创建".into()
    } else {
        "对话中".into()
    };
    frame.render_widget(
        Paragraph::new(format!("本地代码: {local}\n状态: {busy}")),
        inner,
    );
}

fn draw_body(frame: &mut Frame, app: &App, area: Rect) {
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
        " 对话 [当前焦点 · ↑↓滚动 · →预览] "
    } else {
        " 对话 "
    };
    let border = if focused {
        Color::Blue
    } else {
        Color::DarkGray
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
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

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((app.chat_scroll, 0)),
        inner,
    );
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Input;
    let title = if focused {
        " 输入 [当前焦点 · Enter发送 · ←→移动光标] "
    } else {
        " 输入 "
    };
    let border = if focused {
        Color::Blue
    } else {
        Color::DarkGray
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(title);
    let inner = padded(block.inner(area));
    frame.render_widget(block, area);

    let width = inner.width.saturating_sub(1) as usize;
    let (visible, cursor_column) = app.input_view(width.max(2));
    let placeholder = if visible.is_empty() && !focused {
        "描述问题或回答 Codex 的问题…"
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
    let border = if focused {
        Color::Blue
    } else {
        Color::DarkGray
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(title);
    let inner = padded(block.inner(area));
    frame.render_widget(block, area);

    let content = preview_text(app);
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .scroll((app.preview_scroll, 0)),
        inner,
    );
}

fn preview_text(app: &App) -> String {
    let mut out = String::new();
    out.push_str("Issue Quality Gate\n");
    out.push_str(&format!(
        "状态: {}{}\n",
        if app.quality_gate.status.is_empty() {
            "尚未最终检查"
        } else {
            &app.quality_gate.status
        },
        if app.quality_finalized {
            " · FINAL"
        } else {
            ""
        }
    ));
    if !app.quality_gate.checks.is_empty() {
        for check in &app.quality_gate.checks {
            out.push_str(&format!(
                "  [{}] {} — {}\n",
                check.status, check.name, check.evidence
            ));
        }
    }
    for blocker in &app.quality_gate.blockers {
        out.push_str(&format!("  BLOCKER: {blocker}\n"));
    }

    out.push_str("\n重复 Issue 检查\n");
    if app.duplicates.is_empty() {
        out.push_str(if app.quality_finalized {
            "  未发现候选，或 Quality Gate 未认定重复。\n"
        } else {
            "  按 F2 时自动搜索 GitHub。\n"
        });
    } else {
        for item in &app.duplicates {
            out.push_str(&format!(
                "  #{} [{}] {}\n",
                item.number, item.state, item.title
            ));
        }
    }

    out.push_str("\n────────────────────────\n\n");
    if let Some(draft) = &app.draft {
        out.push_str(&format!("# {}\n\n", safe_text(&draft.title)));
        out.push_str(&safe_text(&draft.to_markdown(&app.repository)));
    } else {
        out.push_str("草稿尚未形成。继续和 Codex 对话；当信息足够时右侧会逐步出现 Issue。\n");
    }
    out
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let controls = "Tab切换  ↑↓滚动  ←→导航/光标  PgUp/PgDn翻页  Enter发送  F2最终检查  F4创建  Ctrl+C取消  Ctrl+Q退出";
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
        "目标仓库：{}\n\nIssue：{}\n\nQuality Gate：{}\n\nBurnCloud Issue 不允许 AI 自行提交。只有你在这个确认框中明确同意，才会执行 GitHub Create Issue。\n\n[Y] 我确认创建\n[N / Esc] 返回继续修改",
        app.repository, title, app.quality_gate.status
    );
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
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
}
