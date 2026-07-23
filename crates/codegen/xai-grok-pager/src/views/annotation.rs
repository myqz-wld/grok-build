//! Standard-TUI overlays and transient interaction state for inline annotations.

use std::collections::{HashMap, HashSet};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::annotations::{AnnotationAnchor, ThreadId};
use crate::render::SafeBuf;
use crate::scrollback::decorations::DecorationPlacement;
use crate::theme::Theme;
use crate::views::prompt_widget::{PromptRenderResult, PromptStyle, PromptWidget};

pub(crate) const ANNOTATION_COMPOSER_DECORATION_ID: &str = "__annotation_composer__";

#[derive(Debug)]
pub(crate) enum AnnotationComposerTarget {
    New { anchor: AnnotationAnchor },
    FollowUp { thread_id: ThreadId },
}

impl AnnotationComposerTarget {
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
    /// Screen-space input row from the most recent render.
    pub(crate) input_area: Option<Rect>,
}

impl AnnotationComposerState {
    pub(crate) fn new(target: AnnotationComposerTarget, cwd: &std::path::Path) -> Self {
        Self {
            target,
            prompt: PromptWidget::new_with_cwd(cwd),
            input_area: None,
        }
    }

    pub(crate) fn placeholder(&self) -> &'static str {
        if self.target.thread_id().is_some() {
            "Ask a follow-up…"
        } else {
            "Ask about the selected text…"
        }
    }
}

pub(crate) struct AnnotationContextMenuState {
    pub(crate) target: AnnotationComposerTarget,
    pub(crate) column: u16,
    pub(crate) row: u16,
    pub(crate) popup_area: Option<Rect>,
    pub(crate) action_area: Option<Rect>,
    pub(crate) hovered: bool,
}

impl AnnotationContextMenuState {
    pub(crate) fn new(target: AnnotationComposerTarget, column: u16, row: u16) -> Self {
        Self {
            target,
            column,
            row,
            popup_area: None,
            action_area: None,
            hovered: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct AnnotationCardTextPoint {
    pub(crate) row: usize,
    pub(crate) col: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingAnnotationCardTextDrag {
    pub(crate) thread_id: ThreadId,
    pub(crate) selectable_revision: u64,
    pub(crate) anchor: AnnotationCardTextPoint,
    pub(crate) start_col: u16,
    pub(crate) start_row: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActiveAnnotationCardTextDrag {
    pub(crate) thread_id: ThreadId,
    pub(crate) selectable_revision: u64,
    pub(crate) anchor: AnnotationCardTextPoint,
    pub(crate) head: AnnotationCardTextPoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnnotationCardTextSelection {
    pub(crate) thread_id: ThreadId,
    pub(crate) selectable_revision: u64,
    pub(crate) anchor: AnnotationCardTextPoint,
    pub(crate) head: AnnotationCardTextPoint,
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
    pub(crate) composer_placement: Option<Rect>,
    pub(crate) context_menu: Option<AnnotationContextMenuState>,
    pub(crate) expanded_threads: HashSet<ThreadId>,
    pub(crate) card_placements: Vec<DecorationPlacement>,
    pub(crate) hovered_card_button: Option<(String, String)>,
    pub(crate) pending_card_text_drag: Option<PendingAnnotationCardTextDrag>,
    pub(crate) active_card_text_drag: Option<ActiveAnnotationCardTextDrag>,
    pub(crate) card_text_selection: Option<AnnotationCardTextSelection>,
    pub(crate) thread_card_bodies: HashMap<ThreadId, AnnotationCardBodyCache>,
    /// Cheap instrumentation used by cache regression tests and diagnostics.
    pub(crate) card_body_cache_misses: u64,
}

pub(crate) fn render_annotation_composer(
    buf: &mut Buffer,
    area: Rect,
    state: &mut AnnotationComposerState,
    theme: &Theme,
) -> Option<PromptRenderResult> {
    const INDENT: u16 = 3;
    if area.height == 0 || area.width < INDENT + 5 {
        state.input_area = None;
        return None;
    }

    let input_area = Rect {
        x: area.x + INDENT,
        y: area.y,
        width: area.width.saturating_sub(INDENT + 1),
        height: 1,
    };
    state.input_area = Some(input_area);
    let mut style = PromptStyle::inline(theme.bg_visual);
    style.show_prefix = true;
    style.prefix_override = Some(("↳ ", theme.accent_user));
    style.placeholder_override = Some(state.placeholder());
    style.image_preview = false;
    let rendered = state.prompt.draw(buf, input_area, None, &style, None, None);

    // PromptWidget intentionally hides placeholder text while focused in the
    // main composer. This compact inline editor stays focused, so paint its
    // hint explicitly without changing the shared widget's behavior.
    if state.prompt.text().is_empty() {
        let textarea = state.prompt.textarea_area();
        let placeholder =
            crate::render::line_utils::truncate_str(state.placeholder(), textarea.width as usize);
        buf.set_string_safe(
            textarea.x,
            textarea.y,
            &placeholder,
            Style::default().fg(theme.gray).bg(theme.bg_visual),
        );
    }
    Some(rendered)
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
    // A popup edge can bisect a CJK/emoji cell already present in the
    // underlying frame. Clearing one guard column on each side forces the
    // terminal diff to erase both halves of any wide glyph before painting
    // the border, avoiding intermittent missing/overwritten menu edges.
    let guard_x = popup.x.saturating_sub(u16::from(popup.x > area.x));
    let guard_right = popup
        .right()
        .saturating_add(u16::from(popup.right() < area.right()))
        .min(area.right());
    let guarded = Rect::new(
        guard_x,
        popup.y,
        guard_right.saturating_sub(guard_x),
        popup.height,
    );
    Clear.render(guarded, buf);
    buf.set_style(guarded, Style::default().bg(theme.bg_base));
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
    state.popup_area = Some(guarded);
    state.action_area = Some(Rect::new(inner.x, inner.y, inner.width, 1));
}
