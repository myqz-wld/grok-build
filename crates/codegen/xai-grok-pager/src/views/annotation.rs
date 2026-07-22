//! Standard-TUI overlays and transient interaction state for inline annotations.

use std::collections::{HashMap, HashSet};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::annotations::{AnnotationAnchor, ThreadId};
use crate::render::SafeBuf;
use crate::scrollback::decorations::DecorationPlacement;
use crate::theme::Theme;
use crate::views::modal_window::{
    ModalSizing, ModalWindowConfig, ModalWindowState, Shortcut, render_modal_window,
};
use crate::views::prompt_widget::{PromptRenderResult, PromptStyle, PromptWidget};

pub(crate) const COMPOSER_SUBMIT_ID: usize = 1;
pub(crate) const COMPOSER_CANCEL_ID: usize = 2;

pub(crate) enum AnnotationComposerTarget {
    New {
        anchor: AnnotationAnchor,
    },
    FollowUp {
        thread_id: ThreadId,
        anchor: AnnotationAnchor,
    },
}

impl AnnotationComposerTarget {
    pub(crate) fn anchor(&self) -> &AnnotationAnchor {
        match self {
            Self::New { anchor } | Self::FollowUp { anchor, .. } => anchor,
        }
    }

    pub(crate) fn thread_id(&self) -> Option<ThreadId> {
        match self {
            Self::New { .. } => None,
            Self::FollowUp { thread_id, .. } => Some(*thread_id),
        }
    }
}

pub(crate) struct AnnotationComposerState {
    pub(crate) target: AnnotationComposerTarget,
    pub(crate) prompt: PromptWidget,
    pub(crate) window: ModalWindowState,
}

impl AnnotationComposerState {
    pub(crate) fn new(target: AnnotationComposerTarget, cwd: &std::path::Path) -> Self {
        Self {
            target,
            prompt: PromptWidget::new_with_cwd(cwd),
            window: ModalWindowState::new(),
        }
    }

    pub(crate) fn title(&self) -> &'static str {
        if self.target.thread_id().is_some() {
            "Follow up on annotation"
        } else {
            "Annotate selection"
        }
    }
}

pub(crate) struct AnnotationContextMenuState {
    pub(crate) anchor: AnnotationAnchor,
    pub(crate) column: u16,
    pub(crate) row: u16,
    pub(crate) popup_area: Option<Rect>,
    pub(crate) action_area: Option<Rect>,
    pub(crate) hovered: bool,
}

impl AnnotationContextMenuState {
    pub(crate) fn new(anchor: AnnotationAnchor, column: u16, row: u16) -> Self {
        Self {
            anchor,
            column,
            row,
            popup_area: None,
            action_area: None,
            hovered: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnnotationCardAction {
    Toggle,
    Retry,
    Dismiss,
    FollowUp,
    OpenChild,
    Cancel,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AnnotationCardBodyCacheKey {
    pub(crate) content_revision: u64,
    pub(crate) width: u16,
    pub(crate) theme_revision: u64,
    pub(crate) expanded: bool,
}

pub(crate) struct AnnotationCardBodyCache {
    pub(crate) key: AnnotationCardBodyCacheKey,
    pub(crate) lines: Vec<Line<'static>>,
}

impl AnnotationCardAction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::Retry => "retry",
            Self::Dismiss => "dismiss",
            Self::FollowUp => "follow_up",
            Self::OpenChild => "open_child",
            Self::Cancel => "cancel",
            Self::Delete => "delete",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "toggle" => Some(Self::Toggle),
            "retry" => Some(Self::Retry),
            "dismiss" => Some(Self::Dismiss),
            "follow_up" => Some(Self::FollowUp),
            "open_child" => Some(Self::OpenChild),
            "cancel" => Some(Self::Cancel),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(crate) struct AnnotationUiState {
    pub(crate) composer: Option<AnnotationComposerState>,
    pub(crate) context_menu: Option<AnnotationContextMenuState>,
    pub(crate) expanded_threads: HashSet<ThreadId>,
    pub(crate) card_placements: Vec<DecorationPlacement>,
    pub(crate) hovered_card_button: Option<(String, String)>,
    pub(crate) thread_card_bodies: HashMap<ThreadId, AnnotationCardBodyCache>,
    /// Cheap instrumentation used by cache regression tests and diagnostics.
    pub(crate) card_body_cache_misses: u64,
}

pub(crate) fn render_annotation_composer(
    buf: &mut Buffer,
    area: Rect,
    state: &mut AnnotationComposerState,
    compact: bool,
    theme: &Theme,
) -> Option<PromptRenderResult> {
    let shortcuts = [
        Shortcut {
            label: "Enter submit",
            clickable: true,
            id: COMPOSER_SUBMIT_ID,
        },
        Shortcut {
            label: "Esc cancel",
            clickable: true,
            id: COMPOSER_CANCEL_ID,
        },
        Shortcut {
            label: "Shift+Enter newline",
            clickable: false,
            id: 0,
        },
    ];
    let config = ModalWindowConfig {
        title: state.title(),
        tabs: None,
        shortcuts: &shortcuts,
        sizing: ModalSizing {
            width_pct: 0.72,
            max_width: 100,
            min_width: 40,
            v_margin: 6,
            h_pad: 2,
            v_pad: 1,
            footer_lines: 2,
        }
        .with_compact(compact),
        fold_info: None,
    };
    let modal = render_modal_window(buf, area, &mut state.window, &config, theme)?;
    if modal.content.height < 4 {
        return None;
    }

    let anchor = state.target.anchor();
    let role = match anchor.entry_role {
        crate::annotations::AnnotationEntryRole::User => "User",
        crate::annotations::AnnotationEntryRole::Assistant => "Assistant",
    };
    let lines = if anchor.start_source_line == anchor.end_source_line {
        format!("L{}", anchor.start_source_line)
    } else {
        format!("L{}-L{}", anchor.start_source_line, anchor.end_source_line)
    };
    let meta = Line::from(vec![
        Span::styled(
            format!("{role} · {lines}"),
            Style::default()
                .fg(theme.accent_user)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " · inherited through this turn",
            Style::default().fg(theme.gray),
        ),
    ]);
    buf.set_line_safe(modal.content.x, modal.content.y, &meta, modal.content.width);

    let quote: String = anchor
        .selected_text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let quote_width = modal.content.width.saturating_sub(4) as usize;
    let quote = crate::render::line_utils::truncate_str(&quote, quote_width);
    let quote_line = Line::from(vec![
        Span::styled("“", Style::default().fg(theme.gray_dim)),
        Span::styled(quote, Style::default().fg(theme.gray)),
        Span::styled("”", Style::default().fg(theme.gray_dim)),
    ]);
    buf.set_line_safe(
        modal.content.x,
        modal.content.y + 1,
        &quote_line,
        modal.content.width,
    );

    let prompt_area = Rect {
        x: modal.content.x,
        y: modal.content.y + 3,
        width: modal.content.width,
        height: modal.content.height.saturating_sub(3),
    };
    let mut style = PromptStyle::inline(theme.bg_base);
    style.show_prefix = true;
    style.placeholder_override = Some(if state.target.thread_id().is_some() {
        "Ask a follow-up..."
    } else {
        "Ask about the selected text..."
    });
    Some(
        state
            .prompt
            .draw(buf, prompt_area, Some(area), &style, None, None),
    )
}

pub(crate) fn render_annotation_context_menu(
    buf: &mut Buffer,
    area: Rect,
    state: &mut AnnotationContextMenuState,
    theme: &Theme,
) {
    const LABEL: &str = "Annotate selection";
    let width = 24u16.min(area.width);
    let height = 3u16.min(area.height);
    if width < 8 || height < 3 {
        state.popup_area = None;
        state.action_area = None;
        return;
    }
    let x = state
        .column
        .min(area.right().saturating_sub(width))
        .max(area.x);
    let y = state
        .row
        .min(area.bottom().saturating_sub(height))
        .max(area.y);
    let popup = Rect::new(x, y, width, height);
    Clear.render(popup, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.gray_dim))
        .style(Style::default().bg(theme.bg_base));
    let inner = block.inner(popup);
    block.render(popup, buf);
    let style = if state.hovered {
        Style::default()
            .fg(theme.text_primary)
            .bg(theme.bg_dark)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text_primary).bg(theme.bg_base)
    };
    buf.set_string_safe(inner.x, inner.y, LABEL, style);
    state.popup_area = Some(popup);
    state.action_area = Some(Rect::new(inner.x, inner.y, inner.width, 1));
}
