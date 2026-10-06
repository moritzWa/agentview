use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::prelude::{Color, Frame, Line, Modifier, Span, Style};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};
use unicode_segmentation::UnicodeSegmentation;

use crate::app::{
    display_directory, hidden_picker_rows_for_height, is_active_session_state, App, ComposerMode,
    ConfirmTarget, Overlay, SelectionKey, ViewMode, MIGRATION_PICKER_PAGE_SIZE,
    MODEL_PICKER_PAGE_SIZE,
};
use crate::domain::{AgentSession, Capability, Provider, SessionState};

fn palette() -> crate::theme::Palette {
    crate::theme::active_palette()
}

pub fn render(frame: &mut Frame<'_>, app: &App) {
    crate::theme::set_active_palette(crate::theme::Palette::for_scheme(app.color_scheme));
    let area = frame.size();
    frame.render_widget(
        Block::default().style(Style::default().bg(palette().bg).fg(palette().fg)),
        area,
    );
    if area.width < 32 || area.height < 8 {
        let message = if area.width >= 18 && area.height >= 2 {
            vec![Line::from("agentview needs"), Line::from("at least 32×8")]
        } else {
            vec![Line::from("needs 32×8")]
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().bg(palette().bg).fg(palette().attention))
                .alignment(Alignment::Center),
            area,
        );
        return;
    }

    let composer_height = match app.overlay {
        Overlay::Peek => (6 + input_line_count(&app.input).saturating_sub(1))
            .min(10)
            .min(area.height.saturating_sub(5)),
        Overlay::Help => {
            let help_lines = help_lines(app, area.width.saturating_sub(4) as usize).len() as u16;
            (4 + help_lines).min(area.height.saturating_sub(5))
        }
        Overlay::HarnessPicker | Overlay::Composer(_) => composer_draft_layout(app, area.width)
            .map_or(3, |layout| 2 + layout.visible_rows() as u16)
            .min(area.height.saturating_sub(5)),
        _ => 3,
    };
    let header_height = if area.height < 16 { 2 } else { 4 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(1),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .split(area);

    render_header(frame, app, chunks[0]);
    render_session_list(frame, app, chunks[1]);
    render_bottom_panel(frame, app, chunks[2]);
    render_footer(frame, app, chunks[3]);

    if let Overlay::Confirm(target) = &app.overlay {
        render_confirmation(frame, app, target, area);
    } else if app.overlay == Overlay::HarnessPicker {
        render_harness_picker(frame, app, area);
    } else if app.overlay == Overlay::ModelPicker {
        render_model_picker(frame, app, area);
    } else if app.overlay == Overlay::HiddenPicker {
        render_hidden_picker(frame, app, area);
    } else if app.overlay == Overlay::DirectoryPicker {
        render_directory_picker(frame, app, area);
    } else if matches!(app.overlay, Overlay::MigrationTargetPicker { .. }) {
        render_migration_target_picker(frame, app, area);
    }
}

/// Terminal tab title, such as `2 need input · 1 working · 3 done · agentview`.
pub fn terminal_title(app: &App) -> String {
    let counts = [
        (app.session_count(SessionState::NeedsInput), "need input"),
        (app.session_count(SessionState::Working), "working"),
        (
            app.session_count(SessionState::ReadyForReview)
                + app.session_count(SessionState::Completed),
            "done",
        ),
    ];
    let mut parts = counts
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("{count} {label}"))
        .collect::<Vec<_>>();
    parts.push("agentview".into());
    parts.join(" · ")
}

fn render_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let cwd = header_directory(app);
    let awaiting = app.session_count(SessionState::NeedsInput);
    let working =
        app.session_count(SessionState::ReadyForReview) + app.session_count(SessionState::Working);
    let completed = app.session_count(SessionState::Completed);
    let mut completed_status = if app.includes_completed {
        format!("{completed} completed (/completed hide)")
    } else {
        "completed hidden (/completed show)".into()
    };
    if app.includes_completed
        && app
            .snapshot
            .warnings
            .iter()
            .any(|warning| is_history_cap_warning(warning))
    {
        completed_status.push_str(" · history capped");
    }
    let providers = provider_summary(app);
    let title = format!("agentview v{}", env!("CARGO_PKG_VERSION"));
    let mode = match app.view_mode {
        ViewMode::Status => "status",
        ViewMode::Directory => "directory",
    };

    let lines = if area.height <= 2 {
        vec![
            Line::from(vec![
                Span::styled("● ", Style::default().fg(palette().accent)),
                Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    if app.yolo { "  ⚠ YOLO" } else { "" },
                    Style::default()
                        .fg(palette().attention)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(format!(
                "{awaiting} awaiting · {working} working · {completed_status}"
            )),
        ]
    } else if area.width >= 70 {
        let ring = Style::default().fg(palette().dim);
        let core = Style::default()
            .fg(Color::Rgb(80, 108, 247))
            .add_modifier(Modifier::BOLD);
        vec![
            Line::from(vec![
                Span::styled("   ○ ○   ", ring),
                Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
            ]),
            Line::from(vec![
                Span::styled("  ○ ", ring),
                Span::styled("●", core),
                Span::styled(" ○  ", ring),
                Span::styled(format!("{providers} · {cwd}"), Style::default().fg(palette().dim)),
            ]),
            Line::from(vec![
                Span::styled("   ○ ○   ", ring),
                Span::styled(
                    format!(
                        "{awaiting} awaiting input · {working} working · {completed_status} · {mode} view"
                    ),
                    Style::default().fg(palette().dim),
                ),
            ]),
            Line::from(Span::styled(
                if app.yolo {
                    "         ⚠ YOLO MODE · native permission safeguards are relaxed"
                } else {
                    ""
                },
                Style::default()
                    .fg(palette().attention)
                    .add_modifier(Modifier::BOLD),
            )),
        ]
    } else {
        vec![
            Line::from(Span::styled(
                title,
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("{providers} · {cwd}"),
                Style::default().fg(palette().dim),
            )),
            Line::from(Span::styled(
                format!("{awaiting} awaiting · {working} working · {completed_status}"),
                Style::default().fg(palette().dim),
            )),
            Line::from(Span::styled(
                if app.yolo {
                    "⚠ YOLO · permission safeguards relaxed"
                } else {
                    ""
                },
                Style::default()
                    .fg(palette().attention)
                    .add_modifier(Modifier::BOLD),
            )),
        ]
    };
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().bg).fg(palette().fg)),
        area,
    );
    render_usage(frame, app, area);
}

fn render_session_list(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut lines = Vec::new();
    let mut selected_line = None;
    let selection_visible = !matches!(
        app.overlay,
        Overlay::Composer(_)
            | Overlay::HarnessPicker
            | Overlay::ModelPicker
            | Overlay::HiddenPicker
            | Overlay::DirectoryPicker
    );

    let groups = app.groups();
    // A provider column that repeats the same name on every row is noise. It is
    // decided across the whole list, not per page, so the layout never shifts
    // while scrolling or expanding groups.
    let mut providers = groups
        .iter()
        .flat_map(|group| group.sessions.iter())
        .map(|index| &app.snapshot.sessions[*index].provider);
    let show_provider = providers
        .next()
        .is_some_and(|first| providers.any(|provider| provider != first));

    for (group_position, group) in groups.iter().enumerate() {
        if group_position > 0 {
            lines.push(Line::default());
        }
        let is_selected =
            selection_visible && app.selection == Some(SelectionKey::Group(group.key.clone()));
        if is_selected {
            selected_line = Some(lines.len());
        }
        let collapsed = app.collapsed.contains(&group.key);
        let suffix = collapsed.then(|| format!(" {}", group.sessions.len()));
        let mut spans = vec![Span::raw(format!(
            "{}{}",
            sanitize_inline(&group.label),
            suffix.unwrap_or_default()
        ))];
        if let Some(verb) = is_selected.then(|| app.pending_removal_verb()).flatten() {
            spans.push(Span::styled(
                format!("  ctrl+x again to {verb}"),
                Style::default().fg(palette().danger),
            ));
        }
        lines.push(styled_line(spans, is_selected));

        if collapsed {
            continue;
        }
        let visible = app.visible_session_count(group);
        for index in group.sessions.iter().take(visible) {
            let session = &app.snapshot.sessions[*index];
            let is_selected = selection_visible
                && app.selection == Some(SelectionKey::Session(session.id.clone()));
            if is_selected {
                selected_line = Some(lines.len());
            }
            lines.push(render_session_row(
                session,
                app.view_mode,
                area.width,
                show_provider,
                is_selected,
                app.live_animation_visible(),
                is_selected.then(|| app.pending_removal_verb()).flatten(),
            ));
        }
        let hidden = app.hidden_session_count(group);
        if hidden > 0 {
            let is_selected = selection_visible
                && app.selection == Some(SelectionKey::ShowMore(group.key.clone()));
            if is_selected {
                selected_line = Some(lines.len());
            }
            lines.push(render_show_more_row(
                hidden,
                app.session_page_size(),
                is_selected,
            ));
        }
    }

    if lines.is_empty() {
        if app.filter.is_empty() && area.width >= 60 && area.height >= 12 {
            lines.extend(empty_state_lines(app.includes_completed));
        } else {
            lines.push(Line::from(Span::styled(
                if app.filter.is_empty() {
                    "No coding-agent sessions found"
                } else {
                    "No sessions match the current filter"
                },
                Style::default().fg(palette().dim),
            )));
        }
    }

    // Keep the viewport page-aligned. A one-line sliding window rewrites every
    // visible row for every arrow repeat once the selection reaches the bottom,
    // which creates a large output backlog on SSH/tmux with long session lists.
    // Within a page, navigation now changes only the old and new selected rows.
    let page_height = area.height.max(1) as usize;
    let scroll = selected_line
        .map(|line| (line / page_height) * page_height)
        .unwrap_or(0)
        .min(u16::MAX as usize);
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().bg(palette().bg).fg(palette().fg))
            .scroll((scroll as u16, 0)),
        area,
    );
}

fn render_show_more_row(hidden: usize, page_size: usize, selected: bool) -> Line<'static> {
    let next = hidden.min(page_size);
    styled_line(
        vec![
            Span::styled("  ↓ ", Style::default().fg(palette().accent)),
            Span::styled(
                format!("Show {next} more · {hidden} hidden"),
                Style::default().fg(palette().dim),
            ),
        ],
        selected,
    )
}

fn empty_state_lines(includes_completed: bool) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "Needs input",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            " Sessions that have a question or need your decision land here",
            Style::default().fg(palette().dim),
        )),
        Line::default(),
        Line::from(Span::styled(
            "Working",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            " Sessions your coding agents are actively working on",
            Style::default().fg(palette().dim),
        )),
        Line::default(),
        Line::from(Span::styled(
            "Completed",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            if includes_completed {
                " Finished sessions wait here for you to review"
            } else {
                " Hidden for this view · use /completed show or restart without --hide-completed"
            },
            Style::default().fg(palette().dim),
        )),
        Line::default(),
        Line::from(Span::styled(
            "Hand off a substantial task below. agentview will organize it by status so you can see when it needs you.",
            Style::default().fg(palette().dim),
        )),
    ]
}

fn render_session_row(
    session: &AgentSession,
    view_mode: ViewMode,
    width: u16,
    show_provider: bool,
    selected: bool,
    live_animation_visible: bool,
    pending_removal: Option<&str>,
) -> Line<'static> {
    let symbol = state_symbol(session.state, live_animation_visible);
    let symbol_style = Style::default().fg(state_color(session.state));
    // Provider identity is primary row information when the list mixes
    // providers. Keep the complete names of every built-in provider visible
    // instead of requiring users to decode the old C@H/X@D-style marker.
    // Runtime details remain available in Peek.
    let provider_width = 14;
    let base_name_width = if width >= 140 {
        44
    } else if width >= 120 {
        38
    } else if width >= 100 {
        32
    } else if width >= 70 {
        24
    } else if width >= 50 {
        16
    } else {
        10
    };
    let name_width = if show_provider {
        base_name_width
    } else {
        base_name_width + provider_width
    };
    let provider_column = if show_provider {
        format!(
            "{} ",
            pad_to_width(
                truncate(session.provider.label(), provider_width),
                provider_width,
            )
        )
    } else {
        String::new()
    };
    let state_label = (view_mode == ViewMode::Directory)
        .then(|| short_state(session.state))
        .unwrap_or_default();
    let age = format_age(session.age(SystemTime::now()));
    let prs = session
        .pull_requests
        .map(|count| format!("{count} PR{}", if count == 1 { "" } else { "s" }))
        .unwrap_or_default();
    // The right edge is a real column, not text appended to the summary. Keep
    // it padded even when there are no pull requests so a truncated summary
    // can never visually run into the age (for example, `summary…1d`).
    let right = if prs.is_empty() {
        format!("{age:>5}")
    } else {
        format!("{prs:>7} {age:>5}")
    };
    let name_gap = if width >= 70 { "   " } else { " " };
    let fixed =
        3 + name_gap.len() + name_width + display_width(&provider_column) + display_width(&right);
    let summary_width = (width as usize).saturating_sub(fixed).max(1);
    let name = pad_to_width(truncate(&session.name, name_width), name_width);
    let mut spans = vec![
        Span::styled(format!(" {symbol} "), symbol_style),
        Span::raw(name),
        Span::styled(
            format!("{name_gap}{provider_column}"),
            Style::default().fg(palette().dim),
        ),
    ];
    if let Some(verb) = pending_removal {
        spans.push(Span::styled(
            pad_to_width(
                truncate(&format!("ctrl+x again to {verb}"), summary_width),
                summary_width,
            ),
            Style::default().fg(palette().danger),
        ));
        spans.push(Span::styled(right, Style::default().fg(palette().dim)));
        return styled_line(spans, selected);
    }
    let label = truncate(state_label, summary_width);
    let label_width = display_width(&label);
    if label_width > 0 {
        spans.push(Span::styled(label, symbol_style));
    }
    let separator = if label_width > 0 && !session.summary.is_empty() {
        " · "
    } else {
        ""
    };
    let rest = summary_width.saturating_sub(label_width);
    if rest > 0 {
        let summary = truncate(&format!("{separator}{}", session.summary), rest);
        spans.push(Span::styled(
            pad_to_width(summary, rest),
            Style::default().fg(palette().dim),
        ));
    }
    spans.push(Span::styled(right, Style::default().fg(palette().dim)));
    styled_line(spans, selected)
}

fn render_bottom_panel(frame: &mut Frame<'_>, app: &App, area: Rect) {
    match &app.overlay {
        Overlay::Peek => render_peek(frame, app, area),
        Overlay::Help => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3.min(area.height)), Constraint::Min(0)])
                .split(area);
            render_composer(frame, app, chunks[0]);
            render_help(frame, app, chunks[1]);
        }
        _ => render_composer(frame, app, area),
    }
}

fn render_composer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let renaming = matches!(app.overlay, Overlay::Composer(ComposerMode::Rename { .. }));
    let naming_migration = matches!(
        app.overlay,
        Overlay::Composer(ComposerMode::MigrationName { .. })
    );
    let mut block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(if renaming || naming_migration {
            palette().accent
        } else {
            palette().dim
        }))
        .style(Style::default().bg(palette().bg));
    if let Overlay::Composer(ComposerMode::Rename { session_id }) = &app.overlay {
        const LABEL: &str = " rename session ";
        let mut title = vec![Span::styled(
            LABEL,
            Style::default()
                .fg(palette().accent)
                .add_modifier(Modifier::BOLD),
        )];
        if let Some(session) = app.session_by_id(session_id) {
            let room = usize::from(area.width).saturating_sub(display_width(LABEL) + 14);
            if room > 0 {
                title.push(Span::styled(
                    format!("· current: {} ", truncate(&session.name, room)),
                    Style::default().fg(palette().dim),
                ));
            }
        }
        block = block.title(Line::from(title));
    } else if let Overlay::Composer(ComposerMode::MigrationName { target, .. }) = &app.overlay {
        block = block.title(Span::styled(
            format!(" migrate to {} · choose local name ", target.label()),
            Style::default()
                .fg(palette().accent)
                .add_modifier(Modifier::BOLD),
        ));
    } else if matches!(
        app.overlay,
        Overlay::Composer(ComposerMode::NewSession) | Overlay::HarnessPicker | Overlay::ModelPicker
    ) {
        let launch_option = app.launch_model.as_deref().unwrap_or("default");
        let option_label = if app.launch_provider == Provider::Terminal {
            "shell"
        } else {
            "model"
        };
        let yolo = if app.yolo { " · ⚠ YOLO" } else { "" };
        block = block.title(Span::styled(
            format!(
                " {} · harness {} · {option_label} {launch_option}{yolo} ",
                launch_directory_name(app),
                app.launch_provider.label(),
            ),
            if app.yolo {
                Style::default()
                    .fg(palette().attention)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        ));
    }
    let (prefix, content, editable) = match &app.overlay {
        Overlay::Composer(ComposerMode::NewSession) => ("❯ ", app.input.as_str(), true),
        Overlay::HarnessPicker | Overlay::ModelPicker => ("❯ ", app.input.as_str(), false),
        Overlay::Composer(ComposerMode::Rename { .. }) => ("name ❯ ", app.input.as_str(), true),
        Overlay::Composer(ComposerMode::MigrationName { .. }) => {
            ("name ❯ ", app.input.as_str(), true)
        }
        Overlay::Composer(ComposerMode::Filter) => ("❯ filter ", app.input.as_str(), true),
        _ => (
            "❯ ",
            if app.filter.is_empty() {
                "describe a task · /help for commands"
            } else {
                "describe a task · ctrl+f to change filter"
            },
            false,
        ),
    };
    let text_style = if editable {
        Style::default().fg(palette().fg)
    } else {
        Style::default().fg(palette().dim)
    };
    let layout = composer_draft_layout(app, area.width);
    let content_lines = if let Some(layout) = &layout {
        let start = layout.visible_start();
        layout.rows[start..start + layout.visible_rows()]
            .iter()
            .enumerate()
            .map(|(index, line)| {
                Line::from(vec![
                    Span::styled(if start + index == 0 { prefix } else { "" }, {
                        let style = Style::default().fg(if renaming || naming_migration {
                            palette().accent
                        } else {
                            palette().fg
                        });
                        if renaming || naming_migration {
                            style.add_modifier(Modifier::BOLD)
                        } else {
                            style
                        }
                    }),
                    Span::styled(line.clone(), text_style),
                ])
            })
            .collect::<Vec<_>>()
    } else {
        vec![Line::from(vec![
            Span::styled(
                prefix,
                Style::default().fg(if renaming || naming_migration {
                    palette().accent
                } else {
                    palette().fg
                }),
            ),
            Span::styled(content.to_owned(), text_style),
        ])]
    };
    frame.render_widget(Paragraph::new(content_lines).block(block), area);
    if let Some(layout) = layout.filter(|_| editable) {
        let row = (layout.cursor_row - layout.visible_start()) as u16;
        frame.set_cursor(
            (area.x + layout.cursor_column as u16).min(area.right().saturating_sub(1)),
            (area.y + 1 + row).min(area.bottom().saturating_sub(2)),
        );
    }
}

fn render_peek(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let Some(session) = app.selected_session() else {
        render_composer(frame, app, area);
        return;
    };
    let block = Block::default()
        .title(sanitize_inline(&format!(
            " {} · {} · {} ",
            session.name,
            session.provider,
            session.runtime.label()
        )))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette().accent))
        .style(Style::default().bg(palette().bg));
    let summary = app.selected_detail().unwrap_or_else(|| {
        if session.summary.is_empty() {
            "No summary is available from this provider."
        } else {
            &session.summary
        }
    });
    let can_approve = session.capabilities.contains(&Capability::Approve);
    let can_decline = session.capabilities.contains(&Capability::Decline);
    let can_respond = session.capabilities.contains(&Capability::Respond);
    let can_reply = session.capabilities.contains(&Capability::Reply);
    let response = if can_approve || can_decline {
        let choices = match (can_approve, can_decline) {
            (true, true) => "y allow once · n deny",
            (true, false) => "y allow once",
            (false, true) => "n deny",
            (false, false) => unreachable!(),
        };
        vec![Line::from(Span::styled(
            choices,
            Style::default().fg(palette().attention),
        ))]
    } else if can_respond {
        editable_response_lines("answer", &app.input)
    } else if can_reply {
        editable_response_lines("reply", &app.input)
    } else {
        vec![Line::from(Span::styled(
            "enter to open native session",
            Style::default().fg(palette().dim),
        ))]
    };
    let summary_capacity = area.height.saturating_sub(3 + response.len() as u16).max(1) as usize;
    let summary = sanitize_multiline(summary);
    let summary_lines: Vec<_> = summary.lines().collect();
    let summary_start = summary_lines.len().saturating_sub(summary_capacity);
    let mut lines: Vec<Line<'_>> = summary_lines[summary_start..]
        .iter()
        .map(|line| Line::from((*line).to_owned()))
        .collect();
    lines.push(Line::default());
    lines.extend(response);
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        area,
    );
    if can_respond || can_reply {
        let last_line = app.input.rsplit('\n').next().unwrap_or_default();
        let prefix_width = if app.input.contains('\n') { 0 } else { 2 };
        frame.set_cursor(
            (area.x + 1 + prefix_width + display_width(&expand_input_tabs(last_line)) as u16)
                .min(area.right().saturating_sub(2)),
            area.bottom().saturating_sub(2),
        );
    }
}

fn editable_response_lines(label: &str, input: &str) -> Vec<Line<'static>> {
    if input.is_empty() {
        return vec![Line::from(Span::styled(
            format!("❯ {label}"),
            Style::default().fg(palette().dim),
        ))];
    }
    input_lines(input)
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            Line::from(Span::styled(
                format!(
                    "{}{}",
                    if index == 0 { "❯ " } else { "" },
                    sanitize_inline(&line)
                ),
                Style::default().fg(palette().fg),
            ))
        })
        .collect()
}

fn input_lines(input: &str) -> Vec<String> {
    let lines = input.split('\n').collect::<Vec<_>>();
    lines
        .iter()
        .skip(lines.len().saturating_sub(5))
        .map(|line| expand_input_tabs(line))
        .collect()
}

/// Ratatui skips zero-width characters, so a pasted tab would vanish from the
/// draft on screen. Show it as spaces; the prompt itself keeps the tab.
fn expand_input_tabs(line: &str) -> String {
    line.replace('\t', INPUT_TAB)
}

const INPUT_TAB: &str = "    ";

fn input_line_count(input: &str) -> u16 {
    input.split('\n').count().min(5) as u16
}

/// Rows of the composer draft that fit on screen at once; longer drafts
/// scroll to keep the cursor visible.
const MAX_DRAFT_ROWS: usize = 10;

#[derive(Debug, Eq, PartialEq)]
struct DraftLayout {
    rows: Vec<String>,
    cursor_row: usize,
    /// Includes the prompt prefix on the first row.
    cursor_column: usize,
}

impl DraftLayout {
    fn visible_start(&self) -> usize {
        (self.cursor_row + 1).saturating_sub(MAX_DRAFT_ROWS)
    }

    fn visible_rows(&self) -> usize {
        (self.rows.len() - self.visible_start()).min(MAX_DRAFT_ROWS)
    }
}

/// Wrap the draft into rows of at most `width` columns, with the prompt
/// prefix taking the start of the first row. One column stays free so the
/// cursor can sit after the last character of a full row.
fn layout_draft(input: &str, cursor: usize, prefix_width: usize, width: usize) -> DraftLayout {
    let limit = width.saturating_sub(1).max(prefix_width + 1);
    let mut rows = vec![String::new()];
    let mut column = prefix_width;
    let mut cursor_at = (0, prefix_width);
    for (line_index, line) in input.split('\n').enumerate() {
        if line_index > 0 {
            rows.push(String::new());
            column = 0;
        }
        let line_start = line.as_ptr() as usize - input.as_ptr() as usize;
        for (offset, character) in line.char_indices() {
            let shown = if character == '\t' {
                INPUT_TAB.to_owned()
            } else {
                character.to_string()
            };
            let character_width = display_width(&shown);
            if column + character_width > limit && column > 0 {
                rows.push(String::new());
                column = 0;
            }
            if line_start + offset == cursor {
                cursor_at = (rows.len() - 1, column);
            }
            rows.last_mut()
                .expect("rows is never empty")
                .push_str(&shown);
            column += character_width;
        }
        if line_start + line.len() == cursor {
            cursor_at = (rows.len() - 1, column);
        }
    }
    DraftLayout {
        rows,
        cursor_row: cursor_at.0,
        cursor_column: cursor_at.1,
    }
}

fn composer_draft_layout(app: &App, width: u16) -> Option<DraftLayout> {
    let prefix = match &app.overlay {
        Overlay::Composer(ComposerMode::NewSession)
        | Overlay::HarnessPicker
        | Overlay::ModelPicker => "❯ ",
        Overlay::Composer(ComposerMode::Rename { .. } | ComposerMode::MigrationName { .. }) => {
            "name ❯ "
        }
        Overlay::Composer(ComposerMode::Filter) => "❯ filter ",
        _ => return None,
    };
    Some(layout_draft(
        &app.input,
        app.input_cursor(),
        display_width(prefix),
        width as usize,
    ))
}

fn render_help(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(palette().accent))
        .title(Span::styled(
            " shortcuts ",
            Style::default()
                .fg(palette().accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(palette().bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = help_lines(app, area.width.saturating_sub(4) as usize);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(palette().bg).fg(palette().fg)),
        inner,
    );
}

fn render_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let footer = if let Some(notice) = &app.notice {
        sanitize_inline(notice)
    } else if app.overlay != Overlay::None {
        contextual_footer(app, area.width)
    } else if let Some(warning) = app
        .snapshot
        .warnings
        .iter()
        .find(|warning| !is_history_cap_warning(warning))
    {
        format!("warning: {}", sanitize_inline(warning))
    } else {
        contextual_footer(app, area.width)
    };
    frame.render_widget(
        Paragraph::new(footer)
            .style(Style::default().bg(palette().bg).fg(palette().dim))
            .alignment(Alignment::Left),
        area,
    );
}

/// Right end of the header's title row, which is otherwise empty.
fn render_usage(frame: &mut Frame<'_>, app: &App, area: Rect) {
    const TITLE_ROOM: usize = 30;
    const SEPARATOR: &str = "  │  ";
    let now = crate::usage::now();
    let mut entries = app
        .usage
        .iter()
        .map(|usage| usage.summary(now))
        .collect::<Vec<_>>();
    let line_width = |entries: &[(String, bool)]| {
        entries
            .iter()
            .map(|(text, _)| display_width(text))
            .sum::<usize>()
            + display_width(SEPARATOR) * entries.len().saturating_sub(1)
            + 1
    };
    while !entries.is_empty() && TITLE_ROOM + line_width(&entries) > usize::from(area.width) {
        entries.pop();
    }
    if area.height == 0 || entries.is_empty() {
        return;
    }
    let width = line_width(&entries);
    let spans = entries
        .into_iter()
        .enumerate()
        .flat_map(|(index, (text, alert))| {
            let style = Style::default().fg(if alert {
                palette().attention
            } else {
                palette().dim
            });
            let separator =
                (index > 0).then(|| Span::styled(SEPARATOR, Style::default().fg(palette().dim)));
            separator.into_iter().chain([Span::styled(text, style)])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(palette().bg)),
        Rect {
            x: area.x + area.width - width as u16,
            width: width as u16,
            height: 1,
            ..area
        },
    );
}

/// The header already reports these as "history capped".
fn is_history_cap_warning(warning: &str) -> bool {
    warning.contains("history is limited")
}

fn contextual_footer(app: &App, width: u16) -> String {
    match &app.overlay {
        Overlay::Composer(ComposerMode::Filter) => "enter to apply · esc to cancel".into(),
        Overlay::Composer(ComposerMode::Rename { .. }) if width >= 85 => {
            "rename session · type a new name · enter save · empty resets to provider name · esc cancel".into()
        }
        Overlay::Composer(ComposerMode::Rename { .. }) if width >= 55 => {
            "rename session · type name · enter save · empty reset · esc cancel".into()
        }
        Overlay::Composer(ComposerMode::Rename { .. }) => {
            "rename · type name · enter save · esc cancel".into()
        }
        Overlay::Composer(ComposerMode::MigrationName { .. }) if width >= 70 => {
            "migration name is local to agentview · enter migrate · esc target picker".into()
        }
        Overlay::Composer(ComposerMode::MigrationName { .. }) => {
            "enter migrate · esc targets".into()
        }
        Overlay::Composer(ComposerMode::NewSession) if width >= 100 => format!(
            "tab harness · shift+tab {} · ctrl+o folder · ctrl+g past sessions · ctrl+v image · /help",
            if app.launch_provider == Provider::Terminal { "shell" } else { "model" }
        ),
        Overlay::Composer(ComposerMode::NewSession) if width >= 70 => format!(
            "tab harness · shift+tab {} · ctrl+o folder · ctrl+g past sessions",
            if app.launch_provider == Provider::Terminal { "shell" } else { "model" }
        ),
        Overlay::Composer(ComposerMode::NewSession) if width >= 45 => {
            "tab harness · ctrl+g past sessions · /help".into()
        }
        Overlay::Composer(ComposerMode::NewSession) => "tab harness · ctrl+g past".into(),
        Overlay::HarnessPicker if width >= 55 => format!(
            "↑/↓ or tab to choose · 1–{} direct · enter select · esc back",
            app.launch_targets.len().min(9)
        ),
        Overlay::HarnessPicker => "↑/↓ choose · enter · esc".into(),
        Overlay::ModelPicker if width >= 70 => format!(
            "type to filter · ↑/↓ move · page up/down · enter {} · esc back",
            if selected_shell_install(app) { "install" } else { "select" }
        ),
        Overlay::ModelPicker => "type filter · ↑/↓ · enter · esc".into(),
        Overlay::HiddenPicker if width >= 70 => {
            "type to search hidden and older sessions · ↑/↓ move · enter restore · esc close".into()
        }
        Overlay::HiddenPicker => "type to search · ↑/↓ · enter restore · esc".into(),
        Overlay::DirectoryPicker if width >= 70 => {
            "type to search folders or a path · ↑/↓ move · enter choose · esc close".into()
        }
        Overlay::DirectoryPicker => "type to search · ↑/↓ · enter · esc".into(),
        Overlay::MigrationTargetPicker { .. } if width >= 58 => {
            "↑/↓ move · page up/down · enter choose · esc cancel".into()
        }
        Overlay::MigrationTargetPicker { .. } => "↑/↓ · enter · esc".into(),
        Overlay::Peek
            if app.selected_session().is_some_and(|session| {
                session.capabilities.contains(&Capability::Approve)
                    || session.capabilities.contains(&Capability::Decline)
            }) =>
        {
            let session = app.selected_session().expect("selection checked");
            match (
                session.capabilities.contains(&Capability::Approve),
                session.capabilities.contains(&Capability::Decline),
            ) {
                (true, true) => "y to allow once · n to deny · esc to close".into(),
                (true, false) => "y to allow once · esc to close".into(),
                (false, true) => "n to deny · esc to close".into(),
                (false, false) => unreachable!(),
            }
        }
        Overlay::Peek
            if app
                .selected_session()
                .is_some_and(|session| session.capabilities.contains(&Capability::Respond)) =>
        {
            "enter to submit answer · esc to close".into()
        }
        Overlay::Peek if app.input.is_empty() && width >= 80 => format!(
            "enter to open · space to close{}",
            app.selected_session()
                .map(session_control_suffix)
                .unwrap_or_default()
        ),
        Overlay::Peek if app.input.is_empty() && width >= 55 => {
            "enter to open · space to close".into()
        }
        Overlay::Peek if app.input.is_empty() => "enter to open".into(),
        Overlay::Peek => "enter to send · esc to close".into(),
        Overlay::Help => "? to close help · esc also closes".into(),
        Overlay::None if matches!(app.selection, Some(SelectionKey::ShowMore(_))) => {
            if width >= 55 {
                "enter to show more · ↑/↓ to select · ? for shortcuts".into()
            } else {
                "enter to show more · ? for shortcuts".into()
            }
        }
        Overlay::None if matches!(app.selection, Some(SelectionKey::Group(_))) => {
            let action = if selected_group_can_delete(app) {
                " · ctrl+x to delete all"
            } else {
                ""
            };
            let verb = app
                .selected_group()
                .map(|group| {
                    if app.collapsed.contains(&group.key) {
                        "expand"
                    } else {
                        "collapse"
                    }
                })
                .unwrap_or("collapse/expand");
            if width >= 80 {
                format!("enter to {verb}{action} · ? for shortcuts")
            } else {
                format!("enter to {verb} · ? for shortcuts")
            }
        }
        Overlay::None if app.selected_session().is_some() && width >= 80 => {
            let session = app.selected_session().expect("selection checked");
            let peek = session_peek_suffix(session);
            let control = session_control_suffix(session);
            let back = if width >= 110 {
                " · back: ←/→ twice"
            } else {
                ""
            };
            format!("enter to open{back}{peek}{control} · ? for shortcuts")
        }
        Overlay::None if app.selected_session().is_some() && width >= 55 => {
            let session = app.selected_session().expect("selection checked");
            format!(
                "enter/right open{} · ? shortcuts",
                session_peek_suffix(session)
            )
        }
        Overlay::None if app.selected_session().is_some() => {
            "enter/right to open · ? for shortcuts".into()
        }
        _ if width >= 90 => {
            "type to create · ctrl+o to pick folder · ctrl+f to filter · ? for shortcuts".into()
        }
        _ if width >= 70 => "type to create · ↑/↓ to select · /completed · ? for shortcuts".into(),
        _ => "↑/↓ to select · ? for shortcuts".into(),
    }
}

fn selected_group_can_delete(app: &App) -> bool {
    app.selected_group().is_some_and(|group| {
        group.sessions.iter().all(|index| {
            let session = &app.snapshot.sessions[*index];
            !is_active_session_state(session.state)
                && session.capabilities.contains(&Capability::Delete)
        })
    })
}

fn session_control_suffix(session: &AgentSession) -> &'static str {
    if session.capabilities.contains(&Capability::Interrupt) {
        " · ctrl+x to stop"
    } else if session.capabilities.contains(&Capability::Delete)
        && session.capabilities.contains(&Capability::Archive)
    {
        " · ctrl+a to archive · ctrl+x to delete"
    } else if session.capabilities.contains(&Capability::Delete) {
        " · ctrl+x to delete"
    } else {
        " · ctrl+x to hide locally"
    }
}

fn session_peek_suffix(session: &AgentSession) -> &'static str {
    if session.capabilities.contains(&Capability::Approve)
        || session.capabilities.contains(&Capability::Decline)
    {
        " · space to review request"
    } else if session.capabilities.contains(&Capability::Respond) {
        " · space to answer"
    } else if session.capabilities.contains(&Capability::Reply) {
        " · space to reply"
    } else if session.capabilities.contains(&Capability::Inspect) {
        " · space to inspect"
    } else {
        ""
    }
}

struct HelpSection {
    title: &'static str,
    entries: Vec<(&'static str, &'static str)>,
}

const FOLDER_PICKER_KEYS: &str = if cfg!(target_os = "macos") {
    "ctrl+o / cmd+o"
} else {
    "ctrl+o"
};

fn help_sections(app: &App) -> Vec<HelpSection> {
    let mut sections = vec![HelpSection {
        title: "dashboard",
        entries: vec![
            ("ctrl+f", "filter sessions"),
            ("ctrl+g", "bring back a hidden session"),
            ("ctrl+s", "switch view"),
            ("ctrl+l", "refresh"),
            ("esc", "quit"),
        ],
    }];
    let mut selected = Vec::new();
    if matches!(app.selection, Some(SelectionKey::ShowMore(_))) {
        selected.push(("enter", "show more"));
    }
    if let Some(session) = app.selected_session() {
        selected.push(("enter / →", "open session"));
        selected.push(("← / → twice", "back, at the prompt edge"));
        selected.push(("shift+← / →", "back immediately"));
        if session.capabilities.contains(&Capability::Approve)
            || session.capabilities.contains(&Capability::Decline)
        {
            selected.push(("space", "review request"));
            if session.capabilities.contains(&Capability::Approve) {
                selected.push(("y", "allow once"));
            }
            if session.capabilities.contains(&Capability::Decline) {
                selected.push(("n", "deny"));
            }
        } else if session.capabilities.contains(&Capability::Respond) {
            selected.push(("space", "answer request"));
        } else if session.capabilities.contains(&Capability::Reply) {
            selected.push(("space", "inspect or reply"));
        } else if session.capabilities.contains(&Capability::Inspect) {
            selected.push(("space", "inspect"));
        }
        let removal = if session.capabilities.contains(&Capability::Interrupt) {
            "stop"
        } else if session.capabilities.contains(&Capability::Delete) {
            "delete"
        } else {
            "hide locally"
        };
        selected.push(("ctrl+x", removal));
        if session.provider == Provider::OpenCode {
            selected.push(("", "also works inside OpenCode"));
        }
        if !is_active_session_state(session.state)
            && session.capabilities.contains(&Capability::Archive)
        {
            selected.push(("ctrl+a", "archive"));
        }
        selected.push(("ctrl+r", "rename"));
        selected.push(("ctrl+t / ctrl+p", "pause"));
        if session.runtime == crate::domain::Runtime::Host
            && crate::migration::provider_format(&session.provider).is_some()
        {
            selected.push(("ctrl+m", "migrate to another harness"));
        }
        selected.push(("option+↑ / ↓", "reorder within group"));
    } else if selected_group_can_delete(app) {
        selected.push(("ctrl+x", "delete all in group"));
    }
    if !selected.is_empty() {
        sections.push(HelpSection {
            title: "selected session",
            entries: selected,
        });
    }
    sections.push(HelpSection {
        title: "new session",
        entries: vec![
            ("tab", "pick harness"),
            (FOLDER_PICKER_KEYS, "pick folder"),
            ("ctrl+j", "newline"),
            ("ctrl+v", "paste image"),
        ],
    });
    sections.push(HelpSection {
        title: "commands",
        entries: vec![
            ("/filter text", "filter sessions"),
            ("/restore", "bring back a hidden session"),
            ("/completed", "show or hide finished"),
            ("/cd path", "start in any directory"),
            ("/harness name", "switch harness"),
            ("/model name", "pick model or Terminal shell"),
            ("/shell name", "pick Terminal shell"),
            ("/setup", "install or sign in"),
            ("/login", "open native setup"),
        ],
    });
    sections
}

/// Lays sections out in as many side-by-side columns as fit, keeping each
/// section whole and balancing column heights.
fn help_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    const KEY_GAP: usize = 2;
    const COLUMN_GAP: usize = 4;
    let sections = help_sections(app);
    let key_width = sections
        .iter()
        .flat_map(|section| section.entries.iter())
        .map(|(key, _)| display_width(key))
        .max()
        .unwrap_or(0);
    let column_width = sections
        .iter()
        .flat_map(|section| {
            section
                .entries
                .iter()
                .map(|(_, description)| key_width + KEY_GAP + display_width(description))
                .chain([display_width(section.title)])
        })
        .max()
        .unwrap_or(0);
    let section_height = |section: &HelpSection| section.entries.len() + 1;
    let fits = (width.saturating_sub(1) + COLUMN_GAP) / (column_width + COLUMN_GAP);
    let columns = fits.clamp(1, sections.len().max(1));
    let total = sections.iter().map(section_height).sum::<usize>() + sections.len() - 1;
    let target = total.div_ceil(columns);

    let mut grouped: Vec<Vec<&HelpSection>> = vec![Vec::new()];
    let mut height = 0;
    for section in &sections {
        let added = section_height(section) + usize::from(height > 0);
        if height > 0 && height + added > target && grouped.len() < columns {
            grouped.push(Vec::new());
            height = 0;
        }
        height += section_height(section) + usize::from(height > 0);
        grouped.last_mut().expect("help has a column").push(section);
    }

    let title_style = Style::default()
        .fg(palette().dim)
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default()
        .fg(palette().accent)
        .add_modifier(Modifier::BOLD);
    let description_style = Style::default().fg(palette().fg);
    let rendered_columns = grouped
        .into_iter()
        .map(|column| {
            let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
            for section in column {
                if !rows.is_empty() {
                    rows.push(Vec::new());
                }
                rows.push(vec![Span::styled(section.title, title_style)]);
                for (key, description) in &section.entries {
                    let padding = key_width.saturating_sub(display_width(key)) + KEY_GAP;
                    rows.push(vec![
                        Span::styled(*key, key_style),
                        Span::raw(" ".repeat(padding)),
                        Span::styled(*description, description_style),
                    ]);
                }
            }
            rows
        })
        .collect::<Vec<_>>();
    let row_count = rendered_columns.iter().map(Vec::len).max().unwrap_or(0);
    (0..row_count)
        .map(|row| {
            let mut spans = vec![Span::raw(" ")];
            for (index, column) in rendered_columns.iter().enumerate() {
                if index > 0 {
                    let used = spans.iter().map(Span::width).sum::<usize>() - 1;
                    let start = index * (column_width + COLUMN_GAP);
                    spans.push(Span::raw(" ".repeat(start.saturating_sub(used))));
                }
                if let Some(cells) = column.get(row) {
                    spans.extend(cells.iter().cloned());
                }
            }
            Line::from(spans)
        })
        .collect()
}

fn render_harness_picker(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if app.launch_targets.is_empty() {
        return;
    }
    let popup_width = area.width.saturating_sub(2).min(58).max(28);
    let desired_height = app.launch_targets.len() as u16 + 3;
    let popup_height = desired_height.min(area.height.saturating_sub(2)).max(5);
    let popup = Rect::new(
        area.x + area.width.saturating_sub(popup_width) / 2,
        area.y + area.height.saturating_sub(popup_height) / 2,
        popup_width,
        popup_height,
    );
    let visible_rows = popup_height.saturating_sub(3).max(1) as usize;
    let start = (app.harness_selection / visible_rows) * visible_rows;
    let mut lines = app
        .launch_targets
        .iter()
        .enumerate()
        .skip(start)
        .take(visible_rows)
        .map(|(index, target)| {
            let selected = index == app.harness_selection;
            let current = target.provider == app.launch_provider;
            let yolo_unavailable =
                app.yolo && !app.yolo_supported_providers.contains(&target.provider);
            let detail = if yolo_unavailable {
                "YOLO unavailable"
            } else if popup_width >= 46 {
                if target.supports_model {
                    "selectable model"
                } else {
                    "default model"
                }
            } else if target.supports_model {
                "model"
            } else {
                "default"
            };
            let label = format!(
                " {} {}  {:<16} {}{}",
                if selected { "›" } else { " " },
                index + 1,
                sanitize_inline(target.provider.label()),
                detail,
                if current { " · current" } else { "" }
            );
            Line::from(label).style(if selected {
                Style::default()
                    .bg(palette().selected_bg)
                    .fg(palette().selected_fg)
                    .add_modifier(Modifier::BOLD)
            } else if yolo_unavailable {
                Style::default().bg(palette().bg).fg(palette().attention)
            } else if current {
                Style::default().bg(palette().bg).fg(palette().accent)
            } else {
                Style::default().bg(palette().bg).fg(palette().fg)
            })
        })
        .collect::<Vec<_>>();
    lines.push(
        Line::from(if popup_width >= 46 {
            " ↑/↓ or tab move · enter select · esc back"
        } else {
            " ↑/↓ · enter · esc"
        })
        .style(Style::default().fg(palette().dim)),
    );

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(format!(
                        " choose harness · {}/{} ",
                        app.harness_selection + 1,
                        app.launch_targets.len()
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette().accent)),
            )
            .style(Style::default().bg(palette().bg).fg(palette().fg)),
        popup,
    );
}

fn render_migration_target_picker(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if app.migration_targets.is_empty() {
        return;
    }
    let popup_width = area.width.saturating_sub(2).min(64).max(30);
    let visible_rows = MIGRATION_PICKER_PAGE_SIZE
        .min(area.height.saturating_sub(6).max(1) as usize)
        .max(1);
    let result_rows = app.migration_targets.len().clamp(1, visible_rows);
    let popup_height = (result_rows as u16 + 4)
        .min(area.height.saturating_sub(2))
        .max(6);
    let popup = Rect::new(
        area.x + area.width.saturating_sub(popup_width) / 2,
        area.y + area.height.saturating_sub(popup_height) / 2,
        popup_width,
        popup_height,
    );
    let start = (app.migration_selection / visible_rows) * visible_rows;
    let source = app
        .selected_session()
        .map(|session| session.provider.label())
        .unwrap_or("session");
    let mut lines = vec![Line::from(vec![
        Span::styled(" from  ", Style::default().fg(palette().dim)),
        Span::styled(source, Style::default().fg(palette().fg)),
    ])];
    lines.extend(
        app.migration_targets
            .iter()
            .enumerate()
            .skip(start)
            .take(visible_rows)
            .map(|(index, provider)| {
                let selected = index == app.migration_selection;
                Line::from(format!(
                    " {} {}",
                    if selected { "›" } else { " " },
                    sanitize_inline(provider.public_name())
                ))
                .style(if selected {
                    Style::default()
                        .bg(palette().selected_bg)
                        .fg(palette().selected_fg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().bg(palette().bg).fg(palette().fg)
                })
            }),
    );
    lines.push(
        Line::from(if popup_width >= 52 {
            " ↑/↓ move · PgUp/PgDn page · enter choose · esc cancel"
        } else {
            " ↑/↓ · enter · esc"
        })
        .style(Style::default().fg(palette().dim)),
    );
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(format!(
                        " migrate session · target {}/{} ",
                        app.migration_selection + 1,
                        app.migration_targets.len()
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette().accent)),
            )
            .style(Style::default().bg(palette().bg).fg(palette().fg)),
        popup,
    );
}

const HIDDEN_PICKER_MIN_NAME_WIDTH: usize = 8;

fn render_hidden_picker(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let choices = app.hidden_choices();
    let popup_width = area.width.saturating_sub(2).min(84).max(30);
    let visible_rows = hidden_picker_rows_for_height(area.height);
    let result_rows = choices.len().clamp(1, visible_rows);
    // Inside the borders and padding, less the leading "  " and the quotes.
    let snippet = app.hidden_snippet(popup_width.saturating_sub(8) as usize);
    let popup_height = (result_rows as u16 + 3 + u16::from(snippet.is_some()))
        .min(area.height.saturating_sub(2))
        .max(5);
    let popup = Rect::new(
        area.x + area.width.saturating_sub(popup_width) / 2,
        area.y + area.height.saturating_sub(popup_height) / 2,
        popup_width,
        popup_height,
    );
    let start = if choices.is_empty() {
        0
    } else {
        (app.hidden_selection / visible_rows) * visible_rows
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("search  ", Style::default().fg(palette().dim)),
        Span::styled(
            if app.hidden_filter.is_empty() {
                "type a name, folder, ID, or words from the chat".into()
            } else {
                sanitize_inline(&app.hidden_filter)
            },
            if app.hidden_filter.is_empty() {
                Style::default().fg(palette().dim)
            } else {
                Style::default().fg(palette().fg)
            },
        ),
    ])];
    if choices.is_empty() {
        lines.push(Line::from(Span::styled(
            if app.restorable_loading {
                "  Loading saved sessions…"
            } else {
                "  No sessions match"
            },
            Style::default().fg(palette().dim),
        )));
    } else {
        let now = SystemTime::now();
        // Inside the borders and padding, each row is "› " + name + tail,
        // with the tail right-aligned.
        let inner_width = popup_width.saturating_sub(4) as usize;
        lines.extend(
            choices
                .iter()
                .enumerate()
                .skip(start)
                .take(visible_rows)
                .map(|(index, record)| {
                    let selected = index == app.hidden_selection;
                    let provider = record
                        .provider
                        .as_ref()
                        .map(|provider| provider.label().to_owned())
                        .unwrap_or_else(|| "unknown".into());
                    let hidden_for = now
                        .duration_since(
                            UNIX_EPOCH + std::time::Duration::from_millis(record.hidden_at_ms),
                        )
                        .ok();
                    let age = format!("{:>4}", format_age(hidden_for));
                    let name = sanitize_inline(record.name.as_deref().unwrap_or(&record.id));
                    let folder = app
                        .restorable
                        .get(&record.id)
                        .map(|session| {
                            format!(" · {}", sanitize_inline(&abbreviate_path(&session.cwd)))
                        })
                        .unwrap_or_default();
                    let mut tail = format!("  {provider}{folder}  {age}");
                    if inner_width < display_width(&tail) + 2 + HIDDEN_PICKER_MIN_NAME_WIDTH {
                        tail = format!("  {provider}  {age}");
                    }
                    if inner_width < display_width(&tail) + 2 + HIDDEN_PICKER_MIN_NAME_WIDTH {
                        // Narrow popup: keep the age and drop the harness
                        // rather than pushing the suffix past the border.
                        tail = format!("  {age}");
                    }
                    let name_width = inner_width.saturating_sub(display_width(&tail) + 2);
                    let name = truncate(&name, name_width.max(1));
                    let gap = " ".repeat(name_width.saturating_sub(display_width(&name)));
                    let meta_style = if selected {
                        Style::default()
                    } else {
                        Style::default().fg(palette().dim)
                    };
                    Line::from(vec![
                        Span::raw(format!("{} {name}{gap}", if selected { "›" } else { " " })),
                        Span::styled(tail, meta_style),
                    ])
                    .style(if selected {
                        Style::default()
                            .bg(palette().selected_bg)
                            .fg(palette().selected_fg)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().bg(palette().bg).fg(palette().fg)
                    })
                }),
        );
    }
    if let Some(snippet) = snippet {
        lines.push(Line::from(Span::styled(
            format!("  “{}”", sanitize_inline(&snippet)),
            Style::default().fg(palette().dim),
        )));
    }
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(format!(
                        " restore session · {} of {}{} ",
                        choices.len(),
                        app.hidden_candidates.len(),
                        if app.restorable_loading {
                            " · loading saved sessions…"
                        } else {
                            ""
                        }
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette().accent))
                    .padding(Padding::horizontal(1)),
            )
            .style(Style::default().bg(palette().bg).fg(palette().fg)),
        popup,
    );
}

fn render_directory_picker(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let choices = app.directory_choices();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let popup_width = area.width.saturating_sub(2).min(84).max(30);
    let visible_rows = hidden_picker_rows_for_height(area.height);
    let result_rows = choices.len().clamp(1, visible_rows);
    let popup_height = (result_rows as u16 + 4)
        .min(area.height.saturating_sub(2))
        .max(5);
    let popup = Rect::new(
        area.x + area.width.saturating_sub(popup_width) / 2,
        area.y + area.height.saturating_sub(popup_height) / 2,
        popup_width,
        popup_height,
    );
    let start = if choices.is_empty() {
        0
    } else {
        (app.directory_selection / visible_rows) * visible_rows
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(" search  ", Style::default().fg(palette().dim)),
        if app.directory_filter.is_empty() {
            Span::styled(
                "type part of a folder name, or a path like ~/Code/app",
                Style::default().fg(palette().dim),
            )
        } else {
            Span::styled(
                sanitize_inline(&app.directory_filter),
                Style::default().fg(palette().fg),
            )
        },
    ])];
    if choices.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No folders match",
            Style::default().fg(palette().dim),
        )));
    } else {
        let inner_width = popup_width.saturating_sub(2) as usize;
        lines.extend(
            choices
                .iter()
                .enumerate()
                .skip(start)
                .take(visible_rows)
                .map(|(index, path)| {
                    let selected = index == app.directory_selection;
                    let label = sanitize_inline(&display_directory(path, home.as_deref()));
                    let label = truncate(&label, inner_width.saturating_sub(3).max(1));
                    Line::from(format!(" {} {label}", if selected { "›" } else { " " })).style(
                        if selected {
                            Style::default()
                                .bg(palette().selected_bg)
                                .fg(palette().selected_fg)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().bg(palette().bg).fg(palette().fg)
                        },
                    )
                }),
        );
    }
    lines.push(
        Line::from(if popup_width >= 60 {
            " ↑/↓ move · PgUp/PgDn page · enter start a session here · esc close"
        } else {
            " ↑/↓ · enter choose · esc"
        })
        .style(Style::default().fg(palette().dim)),
    );
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(format!(" new session folder · {} ", choices.len()))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette().accent)),
            )
            .style(Style::default().bg(palette().bg).fg(palette().fg)),
        popup,
    );
}

fn render_model_picker(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let terminal_shells = app.launch_provider == Provider::Terminal;
    let choices = app.model_choices();
    let popup_width = area.width.saturating_sub(2).min(76).max(28);
    let error_lines = if choices.is_empty() && !app.models_loading {
        app.models_error
            .as_deref()
            .map(|error| {
                wrap_inline_bounded(
                    &sanitize_inline(error),
                    popup_width.saturating_sub(6) as usize,
                    4,
                )
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let visible_rows = MODEL_PICKER_PAGE_SIZE
        .min(area.height.saturating_sub(7).max(1) as usize)
        .max(1);
    let empty_rows = error_lines
        .len()
        .max(1)
        .saturating_add(usize::from(app.has_valid_custom_model_input()));
    let result_rows = if choices.is_empty() {
        empty_rows
    } else {
        choices.len().clamp(1, visible_rows)
    };
    let popup_height = (result_rows as u16 + 5)
        .min(area.height.saturating_sub(2))
        .max(6);
    let popup = Rect::new(
        area.x + area.width.saturating_sub(popup_width) / 2,
        area.y + area.height.saturating_sub(popup_height) / 2,
        popup_width,
        popup_height,
    );
    let start = if choices.is_empty() {
        0
    } else {
        (app.model_selection / visible_rows) * visible_rows
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(" filter  ", Style::default().fg(palette().dim)),
        Span::styled(
            if app.model_filter.is_empty() {
                "type to search".into()
            } else {
                sanitize_inline(&app.model_filter)
            },
            if app.model_filter.is_empty() {
                Style::default().fg(palette().dim)
            } else {
                Style::default().fg(palette().fg)
            },
        ),
    ])];
    if choices.is_empty() {
        if app.models_loading {
            lines.push(Line::from(Span::styled(
                if terminal_shells {
                    "  ◌ Finding installed shells…"
                } else {
                    "  ◌ Loading models…"
                },
                Style::default().fg(palette().accent),
            )));
        } else if !error_lines.is_empty() {
            lines.extend(error_lines.iter().map(|line| {
                Line::from(Span::styled(
                    format!("  {line}"),
                    Style::default().fg(palette().attention),
                ))
            }));
        } else {
            lines.push(Line::from(Span::styled(
                if terminal_shells {
                    "  No matching shells"
                } else {
                    "  No matching models"
                },
                Style::default().fg(palette().dim),
            )));
        }
        if app.has_valid_custom_model_input() {
            lines.push(
                Line::from(format!(
                    " › Use exact model ID {}",
                    sanitize_inline(&app.model_filter)
                ))
                .style(
                    Style::default()
                        .bg(palette().selected_bg)
                        .fg(palette().selected_fg)
                        .add_modifier(Modifier::BOLD),
                ),
            );
        }
    } else {
        lines.extend(
            choices
                .iter()
                .enumerate()
                .skip(start)
                .take(visible_rows)
                .map(|(index, model)| {
                    let selected = index == app.model_selection;
                    let label = match model {
                        None if terminal_shells => "Default shell".to_owned(),
                        None => "Default".to_owned(),
                        Some(value) => crate::adapters::shell_install_name(value)
                            .filter(|_| terminal_shells)
                            .map(|shell| format!("{shell} · install"))
                            .unwrap_or_else(|| (*value).to_owned()),
                    };
                    let current = *model == app.launch_model.as_deref();
                    Line::from(format!(
                        " {} {}{}",
                        if selected { "›" } else { " " },
                        sanitize_inline(&label),
                        if current { " · current" } else { "" }
                    ))
                    .style(if selected {
                        Style::default()
                            .bg(palette().selected_bg)
                            .fg(palette().selected_fg)
                            .add_modifier(Modifier::BOLD)
                    } else if current {
                        Style::default().bg(palette().bg).fg(palette().accent)
                    } else {
                        Style::default().bg(palette().bg).fg(palette().fg)
                    })
                }),
        );
    }
    if app.models_loading && !choices.is_empty() {
        lines.push(Line::from(Span::styled(
            if terminal_shells {
                " Finding installed shells…"
            } else {
                " Discovering available models…"
            },
            Style::default().fg(palette().dim),
        )));
    }
    lines.push(
        Line::from(if app.has_valid_custom_model_input() {
            " enter exact ID · ctrl+r retry · l native setup · esc back"
        } else if app.models_error.is_some() && app.models_auth_available {
            " enter/l native setup · ctrl+r retry · esc back"
        } else if app.models_error.is_some() {
            " ctrl+r retry · esc back"
        } else if selected_shell_install(app) {
            " enter install in native package manager · esc back"
        } else if popup_width >= 58 {
            " ↑/↓ move · PgUp/PgDn page · enter select · esc back"
        } else {
            " ↑/↓ · enter · esc"
        })
        .style(Style::default().fg(palette().dim)),
    );

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(format!(
                        " choose {} {} · {} result{} ",
                        app.launch_provider.label(),
                        if terminal_shells { "shell" } else { "model" },
                        choices.len(),
                        if choices.len() == 1 { "" } else { "s" }
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette().accent)),
            )
            .style(Style::default().bg(palette().bg).fg(palette().fg)),
        popup,
    );
    let filter_prefix = " filter  ";
    frame.set_cursor(
        (popup.x
            + 1
            + display_width(filter_prefix) as u16
            + display_width(&app.model_filter) as u16)
            .min(popup.right().saturating_sub(2)),
        popup.y + 1,
    );
}

fn selected_shell_install(app: &App) -> bool {
    app.launch_provider == Provider::Terminal
        && app
            .model_choices()
            .get(app.model_selection)
            .and_then(|choice| *choice)
            .is_some_and(crate::adapters::is_shell_install_choice)
}

fn render_confirmation(frame: &mut Frame<'_>, _: &App, target: &ConfirmTarget, area: Rect) {
    let popup = centered_rect(72, 30, area);
    frame.render_widget(Clear, popup);
    let message = match target {
        ConfirmTarget::Archive { id } => {
            format!("Archive the exact session?\n\n{id}\n\nEnter confirms; escape keeps it.")
        }
    };
    frame.render_widget(
        Paragraph::new(sanitize_multiline(&message))
            .block(
                Block::default()
                    .title(" confirm action ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette().attention)),
            )
            .style(Style::default().bg(palette().bg).fg(palette().fg))
            .wrap(Wrap { trim: true }),
        popup,
    );
}

fn provider_summary(app: &App) -> String {
    if app.provider_labels().is_empty() {
        "Coding agents".into()
    } else {
        sanitize_inline(&app.provider_labels().join(" + "))
    }
}

fn styled_line(spans: Vec<Span<'static>>, selected: bool) -> Line<'static> {
    let style = if selected {
        Style::default()
            .bg(palette().selected_bg)
            .fg(palette().selected_fg)
    } else {
        Style::default().bg(palette().bg).fg(palette().fg)
    };
    Line::from(spans).style(style)
}

fn state_symbol(state: SessionState, live_animation_visible: bool) -> &'static str {
    match state {
        SessionState::ReadyForReview => "✱",
        SessionState::NeedsInput => "✱",
        SessionState::Working if live_animation_visible => "✳",
        SessionState::Working => "·",
        SessionState::Completed => "•",
        SessionState::Unknown => "?",
    }
}

fn state_color(state: SessionState) -> Color {
    match state {
        SessionState::ReadyForReview | SessionState::NeedsInput => palette().attention,
        SessionState::Working => palette().fg,
        SessionState::Completed => palette().complete,
        SessionState::Unknown => palette().dim,
    }
}

fn short_state(state: SessionState) -> &'static str {
    match state {
        SessionState::ReadyForReview => "Review",
        SessionState::NeedsInput => "Needs input",
        SessionState::Working => "Working",
        SessionState::Completed => "Done",
        SessionState::Unknown => "Unknown",
    }
}

fn format_age(age: Option<Duration>) -> String {
    let Some(age) = age else {
        return "—".into();
    };
    let seconds = age.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

fn truncate(input: &str, width: usize) -> String {
    let input = sanitize_inline(input);
    if display_width(&input) <= width {
        return input;
    }
    if width <= 1 {
        return "…".into();
    }
    let mut output = String::new();
    let mut used = 0;
    for grapheme in input.graphemes(true) {
        let grapheme_width = display_width(grapheme);
        if used + grapheme_width + 1 > width {
            break;
        }
        output.push_str(grapheme);
        used += grapheme_width;
    }
    output.push('…');
    output
}

fn wrap_inline_bounded(input: &str, width: usize, max_lines: usize) -> Vec<String> {
    if input.is_empty() || width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let words = input.split_whitespace().collect::<Vec<_>>();
    let mut lines = Vec::new();
    let mut index = 0;
    while index < words.len() && lines.len() < max_lines {
        if lines.len() + 1 == max_lines {
            lines.push(truncate(&words[index..].join(" "), width));
            break;
        }
        let mut line = String::new();
        while index < words.len() {
            let candidate = if line.is_empty() {
                words[index].to_owned()
            } else {
                format!("{line} {}", words[index])
            };
            if display_width(&candidate) > width {
                break;
            }
            line = candidate;
            index += 1;
        }
        if line.is_empty() {
            line = truncate(words[index], width);
            index += 1;
        }
        lines.push(line);
    }
    lines
}

fn pad_to_width(mut input: String, width: usize) -> String {
    let padding = width.saturating_sub(display_width(&input));
    input.extend(std::iter::repeat(' ').take(padding));
    input
}

fn display_width(input: &str) -> usize {
    Line::from(input).width()
}

fn sanitize_inline(input: &str) -> String {
    input
        .chars()
        .map(|character| {
            if character.is_control() {
                '�'
            } else {
                character
            }
        })
        .collect()
}

fn sanitize_multiline(input: &str) -> String {
    input
        .chars()
        .map(|character| match character {
            '\n' => '\n',
            '\t' => ' ',
            character if character.is_control() => '�',
            character => character,
        })
        .collect()
}

fn header_directory(app: &App) -> String {
    app.launch_directory()
        .or_else(|| std::env::current_dir().ok())
        .map(|path| sanitize_inline(&abbreviate_path(&path)))
        .unwrap_or_else(|| "unknown directory".into())
}

fn launch_directory_name(app: &App) -> String {
    app.launch_directory()
        .or_else(|| std::env::current_dir().ok())
        .map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        })
        .map(|name| sanitize_inline(&name))
        .unwrap_or_else(|| "unknown directory".into())
}

fn abbreviate_path(path: &std::path::Path) -> String {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .and_then(|home| path.strip_prefix(home).ok().map(std::path::PathBuf::from))
        .map(|relative| format!("~/{}", relative.display()))
        .unwrap_or_else(|| path.display().to_string())
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::domain::{
        AgentSession, Capability, LaunchTarget, Provider, Runtime, SessionKind, SessionSnapshot,
    };

    use super::*;

    #[test]
    fn draft_layout_wraps_long_lines_and_tracks_the_cursor() {
        let layout = layout_draft("abcdefgh\nxy", 4, 2, 7);
        assert_eq!(layout.rows, vec!["abcd", "efgh", "xy"]);
        assert_eq!((layout.cursor_row, layout.cursor_column), (1, 0));

        let at_end = layout_draft("abcdefgh\nxy", 11, 2, 7);
        assert_eq!((at_end.cursor_row, at_end.cursor_column), (2, 2));

        let long = layout_draft(&"line\n".repeat(30), 0, 2, 40);
        assert_eq!(long.visible_start(), 0);
        assert_eq!(long.visible_rows(), MAX_DRAFT_ROWS);
        let bottom = layout_draft(&"line\n".repeat(30), 150, 2, 40);
        assert_eq!(bottom.cursor_row, 30);
        assert_eq!(bottom.visible_start(), 30 + 1 - MAX_DRAFT_ROWS);
    }

    #[test]
    fn dashboard_renders_reference_sections_and_composer() {
        let snapshot = SessionSnapshot {
            sessions: vec![
                session("review", SessionState::ReadyForReview),
                session("blocked", SessionState::NeedsInput),
                session("worker", SessionState::Working),
                session("done", SessionState::Completed),
            ],
            warnings: vec![],
        };
        let app = App::new(snapshot);
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("agentview"));
        assert!(rendered.contains("Ready for review"));
        assert!(rendered.contains("Needs input"));
        assert!(rendered.contains("Working"));
        assert!(rendered.contains("Completed"));
        assert!(rendered.contains("describe a task · /help for commands"));
        assert!(rendered.contains("1 awaiting input · 2 working · 1 completed"));
    }

    #[test]
    fn terminal_title_counts_states_and_omits_empty_ones() {
        let app = App::new(SessionSnapshot {
            sessions: vec![
                session("review", SessionState::ReadyForReview),
                session("blocked", SessionState::NeedsInput),
                session("asks", SessionState::NeedsInput),
                session("done", SessionState::Completed),
            ],
            warnings: vec![],
        });
        assert_eq!(terminal_title(&app), "2 need input · 2 done · agentview");
        assert_eq!(
            terminal_title(&App::new(SessionSnapshot::default())),
            "agentview"
        );
    }

    #[test]
    fn pasted_tabs_render_as_spaces_and_keep_the_cursor_after_them() {
        let mut app = App::new(SessionSnapshot::default());
        app.paste_input("col\tvalue");
        assert_eq!(app.input, "col\tvalue");
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("col    value"), "{rendered}");
        let (cursor_x, cursor_y) = terminal.get_cursor().unwrap();
        let row = rendered.lines().nth(cursor_y as usize).unwrap();
        let text_end = row.find("value").unwrap() + "value".len();
        assert_eq!(
            row.chars()
                .take(cursor_x as usize)
                .collect::<String>()
                .len(),
            text_end
        );
    }

    #[test]
    fn yolo_mode_is_persistently_visible_in_dashboard_and_composer() {
        let mut app = App::with_launch_targets(
            SessionSnapshot {
                sessions: vec![],
                warnings: vec![],
            },
            true,
            Provider::Claude,
            vec![
                LaunchTarget {
                    provider: Provider::Claude,
                    supports_model: true,
                },
                LaunchTarget {
                    provider: Provider::Pi,
                    supports_model: true,
                },
            ],
        );
        app.set_yolo(true, BTreeSet::from([Provider::Claude]));
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let dashboard = buffer_text(terminal.backend().buffer());
        assert!(dashboard.contains("⚠ YOLO MODE · native permission safeguards are relaxed"));

        app.start_new_session(None);
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let composer = buffer_text(terminal.backend().buffer());
        assert!(composer.contains("⚠ YOLO"));

        app.open_harness_picker();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let picker = buffer_text(terminal.backend().buffer());
        assert!(picker.contains("Pi"));
        assert!(picker.contains("YOLO unavailable"));
    }

    #[test]
    fn header_makes_default_completed_filter_explicit() {
        let app = App::with_completed_visibility(
            SessionSnapshot {
                sessions: vec![],
                warnings: vec![],
            },
            false,
        );
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("completed hidden"));
        assert!(!rendered.contains("0 completed"));
        assert!(rendered.contains("/completed show"));
    }

    #[test]
    fn large_groups_render_a_bounded_page_and_show_more_control() {
        let sessions = (0..60)
            .map(|index| session(&format!("session-{index:02}"), SessionState::Working))
            .collect();
        let mut app = App::new(SessionSnapshot {
            sessions,
            warnings: vec![],
        });
        let backend = TestBackend::new(120, 70);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let first_page = buffer_text(terminal.backend().buffer());
        assert!(first_page.contains("session-00"));
        assert!(first_page.contains("session-24"));
        assert!(!first_page.contains("session-25"));
        assert!(first_page.contains("Show 25 more · 35 hidden"));

        app.selection = Some(SelectionKey::ShowMore("state:Working".into()));
        assert_eq!(app.activate(), crate::app::AppAction::None);
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let second_page = buffer_text(terminal.backend().buffer());
        assert!(second_page.contains("session-25"));
        assert!(second_page.contains("session-49"));
        assert!(!second_page.contains("session-50"));
        assert!(second_page.contains("Show 10 more · 10 hidden"));
        assert!(second_page.contains("enter to open"));

        app.selection = Some(SelectionKey::ShowMore("state:Working".into()));
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let selected_control = buffer_text(terminal.backend().buffer());
        assert!(selected_control.contains("enter to show more"));
    }

    #[test]
    fn row_names_provider_and_peek_shows_full_runtime() {
        let mut item = session("worker", SessionState::Working);
        item.provider = Provider::Codex;
        item.runtime = Runtime::Docker {
            container_id: "a".repeat(64),
            container_name: "long-container-name-that-must-not-shrink-the-row".into(),
            image: "example/image@sha256:digest".into(),
        };
        let mut app = App::new(SessionSnapshot {
            sessions: vec![item],
            warnings: vec![],
        });
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let row = buffer_text(terminal.backend().buffer());
        assert!(row.contains("Codex"));
        assert!(!row.contains("X@D"));
        assert!(row.contains("latest summary from worker"));
        assert!(!row.contains("long-container-name-that-must-not-shrink-the-row"));

        app.toggle_peek();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let peek = buffer_text(terminal.backend().buffer());
        assert!(peek.contains("worker · Codex · long-container-name"));
    }

    #[test]
    fn every_builtin_provider_is_named_on_its_session_row() {
        let providers = [
            (Provider::Claude, "Claude"),
            (Provider::Codex, "Codex"),
            (Provider::Pi, "Pi"),
            (Provider::OpenCode, "OpenCode"),
            (Provider::Cursor, "Cursor"),
            (Provider::GitHubCopilot, "GitHub Copilot"),
            (Provider::Antigravity, "Antigravity"),
            (Provider::MistralVibe, "Mistral Vibe"),
            (Provider::MuseCode, "Muse Code"),
            (Provider::QwenCode, "Qwen Code"),
            (Provider::KimiCode, "Kimi Code"),
            (Provider::OhMyPi, "Oh My Pi"),
            (Provider::Grok, "Grok"),
            (Provider::KiloCode, "Kilo Code"),
            (Provider::OpenHands, "OpenHands"),
            (Provider::Terminal, "Terminal"),
        ];

        for (provider, expected) in providers {
            let mut item = session("recognizable-session", SessionState::Working);
            item.provider = provider;
            let row = render_session_row(&item, ViewMode::Status, 120, true, false, true, None);
            let text: String = row.spans.iter().map(|span| span.content.as_ref()).collect();

            assert!(
                text.contains(expected),
                "session row did not name provider {expected}: {text:?}"
            );
            assert!(!text.contains('@'));
        }
    }

    #[test]
    fn truncated_session_name_keeps_a_gap_before_provider() {
        let mut item = session(
            "session-name-that-is-much-longer-than-the-column",
            SessionState::Working,
        );
        item.provider = Provider::Antigravity;
        let row = render_session_row(&item, ViewMode::Status, 120, true, false, true, None);
        let text: String = row.spans.iter().map(|span| span.content.as_ref()).collect();

        assert!(
            text.contains("…   Antigravity"),
            "provider column touched name: {text:?}"
        );
    }

    #[test]
    fn single_provider_list_drops_the_provider_column_and_widens_the_name() {
        let name = "session name that fits once provider column is gone";
        let mut item = session(name, SessionState::Working);
        item.provider = Provider::OpenCode;

        let with_provider =
            render_session_row(&item, ViewMode::Status, 120, true, false, true, None);
        let without_provider =
            render_session_row(&item, ViewMode::Status, 120, false, false, true, None);
        let with_text: String = with_provider
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        let without_text: String = without_provider
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();

        assert!(with_text.contains("OpenCode"));
        assert!(!with_text.contains(name));
        assert!(!without_text.contains("OpenCode"));
        assert!(
            without_text.contains(name),
            "name was cut off: {without_text:?}"
        );
        assert_eq!(display_width(&without_text), 120);
    }

    #[test]
    fn long_summary_never_touches_or_overwrites_the_age_column() {
        let mut item = session("database-management", SessionState::Completed);
        item.provider = Provider::GitHubCopilot;
        item.summary = "Database management is the practice of designing, storing, securing, and maintaining data so applications can use it safely".into();
        item.updated_at = Some(SystemTime::now() - Duration::from_secs(86_400));

        for width in [80, 120, 160] {
            let row = render_session_row(&item, ViewMode::Status, width, true, false, true, None);
            let text: String = row.spans.iter().map(|span| span.content.as_ref()).collect();

            assert_eq!(display_width(&text), width as usize);
            assert!(
                text.ends_with("   1d"),
                "age column was not reserved: {text:?}"
            );
            assert!(
                !text.ends_with("…1d"),
                "summary touched age column: {text:?}"
            );
        }
    }

    #[test]
    fn working_marker_blinks_without_changing_other_session_states() {
        let working = session("worker", SessionState::Working);
        let visible = render_session_row(&working, ViewMode::Status, 120, true, false, true, None);
        let dimmed = render_session_row(&working, ViewMode::Status, 120, true, false, false, None);
        assert_eq!(visible.spans[0].content, " ✳ ");
        assert_eq!(dimmed.spans[0].content, " · ");

        let completed = session("done", SessionState::Completed);
        let completed =
            render_session_row(&completed, ViewMode::Status, 120, true, false, false, None);
        assert_eq!(completed.spans[0].content, " • ");
    }

    #[test]
    fn directory_row_colors_the_state_label_and_keeps_the_harness_quiet() {
        for (state, label, color) in [
            (SessionState::NeedsInput, "Needs input", palette().attention),
            (SessionState::Completed, "Done", palette().complete),
            (SessionState::Working, "Working", palette().fg),
        ] {
            let item = session("worker", state);
            let row = render_session_row(&item, ViewMode::Directory, 120, true, false, true, None);
            let provider = row
                .spans
                .iter()
                .find(|span| span.content.contains("Claude"))
                .unwrap();
            assert_eq!(provider.style.fg, Some(palette().dim));
            let state_span = row.spans.iter().find(|span| span.content == label).unwrap();
            assert_eq!(state_span.style.fg, Some(color));
            let text: String = row.spans.iter().map(|span| span.content.as_ref()).collect();
            assert!(text.contains(&format!("{label} · latest summary from worker")));
            assert_eq!(display_width(&text), 120);
        }
    }

    #[test]
    fn provider_text_cannot_emit_terminal_controls_and_wide_text_stays_bounded() {
        let mut item = session("unsafe\u{1b}[31m\nname", SessionState::Working);
        item.summary = "deploy\u{7} summary".into();
        item.runtime = Runtime::Docker {
            container_id: "a".repeat(64),
            container_name: "container\u{1b}[2J".into(),
            image: "example/image@sha256:digest".into(),
        };
        let app = App::new(SessionSnapshot {
            sessions: vec![item],
            warnings: vec!["warning\u{1b}[2J\ncontinued".into()],
        });
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains('\u{7}'));
        assert!(rendered.contains('�'));
        assert!(rendered.contains("warning�[2J�continued"));

        let truncated = truncate("部署版本", 5);
        assert_eq!(truncated, "部署…");
        assert_eq!(display_width(&truncated), 5);
        assert_eq!(display_width(&pad_to_width(truncate("部署", 8), 8)), 8);
    }

    #[test]
    fn active_composer_keeps_contextual_keys_visible_over_history_warnings() {
        let mut app = App::new(SessionSnapshot {
            sessions: Vec::new(),
            warnings: vec!["history is bounded".into()],
        });
        app.overlay = Overlay::Composer(ComposerMode::NewSession);
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("tab harness · shift+tab model"));
        assert!(!rendered.contains("warning: history is bounded"));
    }

    #[test]
    fn footer_skips_history_cap_warnings() {
        let app = App::new(SessionSnapshot {
            sessions: Vec::new(),
            warnings: vec![
                "OpenCode (host) completed history is limited to 100 records for this refresh; increase --history-limit to load more".into(),
            ],
        });
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(!rendered.contains("warning:"));
    }

    #[test]
    fn narrow_hidden_picker_keeps_the_age_suffix_inside_the_border() {
        let mut app = App::new(SessionSnapshot::default());
        let hidden_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        app.open_hidden_picker(vec![crate::hidden::HiddenSessionRecord {
            id: "claude:host:abc".into(),
            provider: Some(Provider::Claude),
            name: Some("a very long hidden session name that cannot fit".into()),
            hidden_at_ms,
        }]);
        let backend = TestBackend::new(32, 12);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());
        let row = rendered
            .lines()
            .find(|line| line.contains('›'))
            .expect("selected picker row");
        assert!(
            row.contains(&format!("{} │", format_age(Some(Duration::ZERO)))),
            "{row}"
        );
        assert!(row.trim_end().ends_with('│'), "{row}");
    }

    #[test]
    fn tiny_terminals_render_a_clear_minimum_size_message() {
        let app = App::new(SessionSnapshot::default());
        let backend = TestBackend::new(31, 7);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("agentview needs"));
        assert!(rendered.contains("at least 32×8"));
    }

    #[test]
    fn empty_dashboard_preserves_the_reference_section_anatomy() {
        let app = App::new(SessionSnapshot::default());
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("Needs input"));
        assert!(rendered.contains("question or need your decision"));
        assert!(rendered.contains("Working"));
        assert!(rendered.contains("actively working"));
        assert!(rendered.contains("Completed"));
        assert!(rendered.contains("wait here for you to review"));
        assert!(rendered.contains("Hand off a substantial task below"));
    }

    #[test]
    fn an_empty_filter_result_does_not_show_onboarding_copy() {
        let mut app = App::new(SessionSnapshot {
            sessions: vec![session("worker", SessionState::Working)],
            warnings: vec![],
        });
        app.set_filter("no-match");
        app.replace_snapshot(app.snapshot.clone());
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("No sessions match the current filter"));
        assert!(!rendered.contains("Hand off a substantial task below"));
    }

    #[test]
    fn multiline_composer_expands_and_renders_each_input_line() {
        let mut app = App::new(SessionSnapshot::default());
        app.start_new_session(None);
        for character in "first line\nsecond line\nthird line".chars() {
            app.push_input(character);
        }
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("❯ first line"));
        assert!(rendered.contains("second line"));
        assert!(rendered.contains("third line"));
        assert!(rendered.contains("ctrl+g past sessions"));
    }

    #[test]
    fn multiline_peek_keeps_the_latest_summary_and_draft_visible() {
        let mut item = session("worker", SessionState::Working);
        item.capabilities.insert(Capability::Reply);
        let mut app = App::new(SessionSnapshot {
            sessions: vec![item],
            warnings: vec![],
        });
        app.toggle_peek();
        app.set_detail("worker".into(), "old line\nnew provider detail".into());
        for character in "first reply line\nsecond reply line".chars() {
            app.push_input(character);
        }
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("new provider detail"));
        assert!(rendered.contains("❯ first reply line"));
        assert!(rendered.contains("second reply line"));
        assert!(rendered.contains("enter to send"));
    }

    #[test]
    fn narrow_footer_keeps_the_help_affordance() {
        let app = App::new(SessionSnapshot {
            sessions: vec![session("worker", SessionState::Working)],
            warnings: vec![],
        });
        let backend = TestBackend::new(40, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("enter/right to open · ? for shortcuts"));
        assert!(!rendered.contains("space to reply"));
    }

    #[test]
    fn help_is_contextual_and_does_not_advertise_ungranted_control() {
        let mut app = App::new(SessionSnapshot {
            sessions: vec![session("worker", SessionState::Working)],
            warnings: vec![],
        });
        app.toggle_help();
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        let has_entry = |key: &str, description: &str| {
            rendered.lines().any(|line| {
                line.find(key).is_some_and(|start| {
                    line[start + key.len()..]
                        .trim_start()
                        .starts_with(description)
                })
            })
        };
        assert!(rendered.contains("shortcuts"));
        assert!(has_entry("ctrl+r", "rename"));
        assert!(has_entry("ctrl+t / ctrl+p", "pause"));
        assert!(has_entry("ctrl+m", "migrate"));
        assert!(has_entry("ctrl+s", "switch view"));
        assert!(has_entry(FOLDER_PICKER_KEYS, "pick folder"));
        assert!(has_entry("/harness name", "switch harness"));
        assert!(has_entry("/model name", "pick model"));
        assert!(has_entry("/login", "open native setup"));
        assert!(has_entry("ctrl+x", "hide locally"));
        assert!(!has_entry("ctrl+x", "stop"));
        assert_eq!(rendered.matches("? to close help").count(), 1);
        assert!(rendered.contains("describe a task · /help for commands"));
        assert!(!rendered.contains("j/k"));

        let row = |needle: &str| {
            rendered
                .lines()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("missing {needle}"))
        };
        assert!(row("describe a task") < row("shortcuts"));
        assert!(row("shortcuts") < row("dashboard"));
        assert!(row("dashboard") < row("selected session"));
        assert!(row("selected session") < row("open session"));
        assert!(
            row("new session") < row("ctrl+r"),
            "wide help lays sections out side by side"
        );
    }

    #[test]
    fn migration_target_and_name_steps_are_visually_explicit() {
        let mut item = session("worker", SessionState::Completed);
        item.name = "review API".into();
        let mut app = App::new(SessionSnapshot {
            sessions: vec![item],
            warnings: vec![],
        });
        app.start_migration();
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let picker = buffer_text(terminal.backend().buffer());
        assert!(picker.contains("migrate session · target 1/17"));
        assert!(picker.contains("from  Claude"));
        assert!(picker.contains("OpenAI Codex"));
        assert!(!picker.contains("Claude Code"));

        app.confirm_migration_target();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let naming = buffer_text(terminal.backend().buffer());
        assert!(naming.contains("migrate to Codex · choose local name"));
        assert!(naming.contains("name ❯ review API (Codex)"));
        assert!(naming.contains("migration name is local to agentview"));
    }

    #[test]
    fn narrow_help_stacks_sections_in_one_column() {
        let mut app = App::new(SessionSnapshot {
            sessions: vec![session("worker", SessionState::Working)],
            warnings: vec![],
        });
        app.toggle_help();
        let lines = help_lines(&app, 50);
        let text = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let position = |needle: &str| text.iter().position(|line| line.contains(needle)).unwrap();
        assert!(position("dashboard") < position("selected session"));
        assert!(position("selected session") < position("new session"));
        assert!(position("new session") < position("commands"));
        assert!(lines.iter().all(|line| line.width() <= 50));
    }

    #[test]
    fn completed_control_is_labeled_delete_not_stop() {
        let mut item = session("done", SessionState::Completed);
        item.capabilities.insert(Capability::Delete);
        let mut app = App::new(SessionSnapshot {
            sessions: vec![item],
            warnings: vec![],
        });
        app.toggle_help();
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("ctrl+x"));
        assert_eq!(help_description(&app, "ctrl+x"), Some("delete"));
    }

    fn help_description(app: &App, key: &str) -> Option<&'static str> {
        help_sections(app)
            .into_iter()
            .flat_map(|section| section.entries)
            .find(|(entry_key, _)| *entry_key == key)
            .map(|(_, description)| description)
    }

    #[test]
    fn completed_managed_transport_is_labeled_stop_before_delete() {
        let mut item = session("done but attached", SessionState::Completed);
        item.capabilities.insert(Capability::Interrupt);
        let mut app = App::new(SessionSnapshot {
            sessions: vec![item],
            warnings: vec![],
        });
        app.toggle_help();
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("ctrl+x"));
        assert_eq!(help_description(&app, "ctrl+x"), Some("stop"));
    }

    #[test]
    fn directory_view_header_tracks_the_selected_project() {
        let mut item = session("worker", SessionState::Working);
        let (worktree, project) = if cfg!(windows) {
            (
                r"C:\different\project\.claude\worktrees\topic",
                r"C:\different\project",
            )
        } else {
            (
                "/different/project/.claude/worktrees/topic",
                "/different/project",
            )
        };
        item.cwd = PathBuf::from(worktree);
        let mut app = App::new(SessionSnapshot {
            sessions: vec![item],
            warnings: vec![],
        });
        app.toggle_view();
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains(&format!("Claude · {project}")));
        assert!(rendered.contains(project));
        assert!(!rendered.contains(".claude/worktrees"));
    }

    #[test]
    fn denial_only_request_never_advertises_acceptance() {
        let mut item = session("blocked", SessionState::NeedsInput);
        item.capabilities.insert(Capability::Decline);
        let mut app = App::new(SessionSnapshot {
            sessions: vec![item],
            warnings: vec![],
        });
        app.toggle_peek();
        app.set_detail(
            "blocked".into(),
            "Codex requests file-change approval without a diff.".into(),
        );
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("n deny"));
        assert!(!rendered.contains("y allow once"));
    }

    #[test]
    fn new_task_composer_names_the_selected_provider_and_model() {
        let mut app = App::new(SessionSnapshot::default());
        app.launch_model = Some("opus".into());
        app.start_new_session(None);
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains(&format!(
            "{} · harness Claude · model opus",
            launch_directory_name(&app)
        )));
        assert!(!rendered.contains("new task"));
        assert!(rendered.contains("tab harness"));
        assert!(rendered.contains("shift+tab model"));
    }

    #[test]
    fn header_shows_one_usage_entry_per_provider_and_drops_extras_when_narrow() {
        let usage = |provider, percent| crate::usage::Usage {
            provider,
            windows: vec![crate::usage::Window {
                label: "5h",
                percent,
                resets_at: None,
            }],
        };
        let mut app = App::new(SessionSnapshot::default());
        app.usage = vec![usage("claude", 38.0), usage("codex", 4.0)];
        let first_row = |width| {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|frame| render(frame, &app)).unwrap();
            buffer_text(terminal.backend().buffer())
                .lines()
                .next()
                .unwrap()
                .trim_end()
                .to_owned()
        };

        let wide = first_row(100);
        assert!(wide.ends_with("claude 5h 38%  │  codex 5h 4%"), "{wide}");
        let narrow = first_row(50);
        assert!(narrow.ends_with("claude 5h 38%"), "{narrow}");
    }

    #[test]
    fn terminal_composer_and_picker_use_shell_language_and_mark_install_actions() {
        let mut app = App::with_launch_targets(
            SessionSnapshot::default(),
            false,
            Provider::Terminal,
            vec![LaunchTarget {
                provider: Provider::Terminal,
                supports_model: true,
            }],
        );
        app.start_new_session(None);
        app.open_model_picker();
        let (shell, install, install_label) = if cfg!(windows) {
            ("cmd", "install-shell:nu", "nu · install")
        } else {
            ("bash", "install-shell:fish", "fish · install")
        };
        app.set_available_models(Provider::Terminal, Ok(vec![shell.into(), install.into()]));
        app.model_selection = 2;
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains(&format!(
            "{} · harness Terminal · shell default",
            launch_directory_name(&app)
        )));
        assert!(rendered.contains("choose Terminal shell · 3 results"));
        assert!(rendered.contains("Default shell"));
        assert!(rendered.contains(install_label));
        assert!(rendered.contains("enter install in native package manager"));
        assert!(!rendered.contains("install-shell:"));
    }

    #[test]
    fn composer_cursor_is_exactly_after_the_text_without_a_phantom_left_border() {
        let mut app = App::new(SessionSnapshot::default());
        app.start_new_session(None);
        app.input = "abc".into();
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(terminal.get_cursor().unwrap(), (5, 21));
        assert_eq!(terminal.backend().buffer().get(2, 21).symbol(), "a");
        assert_eq!(terminal.backend().buffer().get(4, 21).symbol(), "c");
    }

    #[test]
    fn rename_composer_has_an_accented_mode_label_and_explicit_instruction() {
        let mut app = App::new(SessionSnapshot {
            sessions: vec![session("worker", SessionState::Completed)],
            warnings: vec![],
        });
        app.start_rename();
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = buffer_text(buffer);

        assert!(rendered.contains("rename session · current: worker"));
        assert!(!rendered.contains("name ❯ worker"));
        assert!(rendered.contains("type a new name"));
        assert!(rendered.contains("empty resets to provider name"));
        assert_eq!(buffer.get(0, 21).fg, palette().accent);
        assert_eq!(terminal.get_cursor().unwrap(), (7, 21));

        app.input = "fresh".into();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(buffer_text(buffer).contains("name ❯ fresh"));
        assert_eq!(buffer.get(7, 21).fg, palette().fg);
        assert_ne!(buffer.get(0, 21).fg, buffer.get(7, 21).fg);
    }

    #[test]
    fn model_picker_is_searchable_bounded_and_keeps_the_selected_page_visible() {
        let mut app = App::with_launch_targets(
            SessionSnapshot::default(),
            false,
            Provider::Pi,
            vec![LaunchTarget {
                provider: Provider::Pi,
                supports_model: true,
            }],
        );
        app.start_new_session(None);
        app.input = "draft survives model selection".into();
        app.open_model_picker();
        app.set_available_models(
            Provider::Pi,
            Ok((0..25)
                .map(|index| format!("provider/model-{index:02}"))
                .collect()),
        );
        app.model_selection = 13;
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let page = buffer_text(terminal.backend().buffer());
        assert!(page.contains("choose Pi model · 26 results"));
        assert!(page.contains("provider/model-09"));
        assert!(page.contains("provider/model-18"));
        assert!(!page.contains("provider/model-08"));
        assert!(!page.contains("provider/model-19"));
        assert!(page.contains("PgUp/PgDn"));
        assert!(page.contains("draft survives model selection"));

        app.push_input('2');
        app.push_input('4');
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let filtered = buffer_text(terminal.backend().buffer());
        assert!(filtered.contains("filter  24"));
        assert!(filtered.contains("choose Pi model · 1 result"));
        assert!(filtered.contains("provider/model-24"));
        assert!(!filtered.contains("provider/model-23"));
        assert_eq!(terminal.get_cursor().unwrap(), (24, 13));
    }

    #[test]
    fn model_catalog_failure_wraps_recovery_and_never_promotes_antigravity_filter_text() {
        let mut app = App::with_launch_targets(
            SessionSnapshot::default(),
            true,
            Provider::Antigravity,
            vec![LaunchTarget {
                provider: Provider::Antigravity,
                supports_model: true,
            }],
        );
        app.require_authentication(
            Provider::Antigravity,
            None,
            "keep this task".into(),
            "Antigravity's own `agy models` command timed out. Press Enter/l to open Antigravity and verify that native `/model` lists models, then return and press Ctrl+R to retry".into(),
        );
        for character in "gemini".chars() {
            app.push_input(character);
        }
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("agy models"));
        assert!(rendered.contains("/model"));
        assert!(rendered.contains("enter/l native setup"));
        assert!(rendered.contains("ctrl+r retry"));
        assert!(!rendered.contains("Use exact model ID"));
        assert_eq!(app.notice, None);
    }

    #[test]
    fn harness_picker_lists_every_available_choice_and_marks_the_current_one() {
        let mut app = App::with_launch_targets(
            SessionSnapshot::default(),
            false,
            Provider::Claude,
            vec![
                LaunchTarget {
                    provider: Provider::Claude,
                    supports_model: true,
                },
                LaunchTarget {
                    provider: Provider::Codex,
                    supports_model: true,
                },
                LaunchTarget {
                    provider: Provider::Pi,
                    supports_model: false,
                },
                LaunchTarget {
                    provider: Provider::OpenCode,
                    supports_model: false,
                },
                LaunchTarget {
                    provider: Provider::Cursor,
                    supports_model: false,
                },
                LaunchTarget {
                    provider: Provider::GitHubCopilot,
                    supports_model: false,
                },
            ],
        );
        app.start_new_session(None);
        app.input = "draft survives".into();
        app.open_harness_picker();
        app.move_harness_selection(1);
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("choose harness · 2/6"));
        assert!(rendered.contains("1  Claude"));
        assert!(rendered.contains("2  Codex"));
        assert!(rendered.contains("3  Pi"));
        assert!(rendered.contains("4  OpenCode"));
        assert!(rendered.contains("5  Cursor"));
        assert!(rendered.contains("6  GitHub Copilot"));
        assert!(rendered.contains("selectable model"));
        assert!(rendered.contains("default model"));
        assert!(rendered.contains("draft survives"));
        assert!(rendered.contains("enter select"));
        assert!(rendered.contains("esc back"));
    }

    #[test]
    fn narrow_harness_picker_pages_to_keep_the_highlighted_choice_visible() {
        let mut app = App::with_launch_targets(
            SessionSnapshot::default(),
            false,
            Provider::Claude,
            vec![
                LaunchTarget {
                    provider: Provider::Claude,
                    supports_model: true,
                },
                LaunchTarget {
                    provider: Provider::Codex,
                    supports_model: true,
                },
                LaunchTarget {
                    provider: Provider::Pi,
                    supports_model: false,
                },
                LaunchTarget {
                    provider: Provider::OpenCode,
                    supports_model: false,
                },
                LaunchTarget {
                    provider: Provider::Cursor,
                    supports_model: false,
                },
                LaunchTarget {
                    provider: Provider::GitHubCopilot,
                    supports_model: false,
                },
            ],
        );
        app.start_new_session(None);
        app.open_harness_picker();
        app.harness_selection = 5;
        let backend = TestBackend::new(32, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("choose harness · 6/6"));
        assert!(rendered.contains("6  GitHub Copilot"));
        assert!(!rendered.contains("1  Claude"));
    }

    #[test]
    fn session_row_explains_how_to_return_after_native_open() {
        let app = App::new(SessionSnapshot {
            sessions: vec![session("worker", SessionState::Working)],
            warnings: vec![],
        });
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("enter to open · back: ←/→ twice"));
    }

    #[test]
    fn light_theme_uses_a_paper_background_and_dark_selected_text() {
        let mut app = App::new(SessionSnapshot {
            sessions: vec![session("worker", SessionState::Completed)],
            warnings: vec![],
        });
        app.color_scheme = crate::theme::ColorScheme::Light;
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let light = crate::theme::Palette::light();

        assert_eq!(buffer.get(0, 0).bg, light.bg);
        assert_ne!(buffer.get(0, 0).bg, crate::theme::Palette::dark().bg);
        let mut saw_selection = false;
        let mut saw_dark_text = false;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let cell = buffer.get(x, y);
                if cell.bg == light.selected_bg {
                    saw_selection = true;
                    assert_ne!(cell.fg, Color::White);
                    if cell.fg == light.selected_fg {
                        saw_dark_text = true;
                    }
                }
            }
        }
        assert!(saw_selection);
        assert!(saw_dark_text);
    }

    fn session(name: &str, state: SessionState) -> AgentSession {
        AgentSession {
            id: name.into(),
            provider_session_id: name.into(),
            provider: Provider::Claude,
            runtime: Runtime::Host,
            kind: SessionKind::Background,
            name: name.into(),
            cwd: PathBuf::from("/work"),
            state,
            summary: format!("latest summary from {name}"),
            raw_state: None,
            pid: None,
            started_at: Some(SystemTime::now()),
            updated_at: None,
            pull_requests: None,
            capabilities: BTreeSet::from([Capability::Inspect]),
        }
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        let mut output = String::new();
        for y in buffer.area.top()..buffer.area.bottom() {
            for x in buffer.area.left()..buffer.area.right() {
                output.push_str(buffer.get(x, y).symbol());
            }
            output.push('\n');
        }
        output
    }
}
