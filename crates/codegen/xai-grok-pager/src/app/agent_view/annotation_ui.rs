//! Standard-TUI interaction and card projection for inline annotations.

use std::hash::{Hash, Hasher};

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::AgentView;
use crate::annotations::{
    AnnotationAnchor, AnnotationEntryRole, AnnotationExchangeStatus, AnnotationOrphanReason,
    AnnotationThread, AnnotationThreadAttachment, PendingAnnotationFork, ThreadId,
    resolve_transcript_entry, resolve_transcript_key_with_index, validate_annotation_anchor,
};
use crate::app::actions::Action;
use crate::app::app_view::InputOutcome;
use crate::scrollback::{
    DecorationButton, DecorationLine, EntryId, HorizontalLayout, ScrollbackDecoration,
};
use crate::theme::Theme;
use crate::views::annotation::{
    AnnotationCardAction, AnnotationComposerState, AnnotationComposerTarget,
    AnnotationContextMenuState, COMPOSER_CANCEL_ID, COMPOSER_SUBMIT_ID,
};
use crate::views::modal_window::{ModalWindowOutcome, handle_modal_mouse};
use crate::views::prompt_widget::{EnterOutcome, PromptEvent};

const CARD_PREFIX_WIDTH: usize = 3;
const CARD_LABEL_WIDTH: usize = 3;

impl AgentView {
    pub(crate) fn annotation_overlay_open(&self) -> bool {
        self.annotation_ui.composer.is_some() || self.annotation_ui.context_menu.is_some()
    }

    pub(crate) fn open_annotation_composer_from_selection(&mut self) -> InputOutcome {
        if crate::app::minimal_mode_active() {
            return InputOutcome::Unchanged;
        }
        match self.selected_annotation_anchor() {
            Ok(anchor) => {
                self.open_annotation_composer(AnnotationComposerTarget::New { anchor });
                InputOutcome::Changed
            }
            Err(error) => {
                self.show_toast(&error.to_string());
                InputOutcome::Changed
            }
        }
    }

    fn open_annotation_composer(&mut self, target: AnnotationComposerTarget) {
        self.annotation_ui.context_menu = None;
        self.annotation_ui.composer = Some(AnnotationComposerState::new(target, &self.session.cwd));
    }

    fn open_annotation_follow_up(&mut self, thread_id: ThreadId) -> InputOutcome {
        let Some(thread) = self.annotation_runtime.state.threads.get(&thread_id) else {
            self.show_toast("Annotation thread not found");
            return InputOutcome::Changed;
        };
        if thread.deleted {
            self.show_toast("This annotation was deleted");
            return InputOutcome::Changed;
        }
        if !matches!(thread.attachment, AnnotationThreadAttachment::Attached) {
            self.show_toast("This annotation is detached from its original text");
            return InputOutcome::Changed;
        }
        if self.annotation_runtime.in_flight.contains_key(&thread_id) {
            self.show_toast("This annotation is already answering a question");
            return InputOutcome::Changed;
        }
        let anchor = thread.anchor.clone();
        self.annotation_ui.expanded_threads.insert(thread_id);
        self.open_annotation_composer(AnnotationComposerTarget::FollowUp { thread_id, anchor });
        InputOutcome::Changed
    }

    pub(crate) fn open_annotation_context_menu(&mut self, column: u16, row: u16) -> InputOutcome {
        if crate::app::minimal_mode_active()
            || !self.annotation_selection_contains_point(column, row)
        {
            return InputOutcome::Unchanged;
        }
        match self.selected_annotation_anchor() {
            Ok(anchor) => {
                self.annotation_ui.context_menu =
                    Some(AnnotationContextMenuState::new(anchor, column, row));
                InputOutcome::Changed
            }
            Err(error) => {
                self.show_toast(&error.to_string());
                InputOutcome::Changed
            }
        }
    }

    pub(crate) fn handle_annotation_overlay_input(
        &mut self,
        event: &crossterm::event::Event,
    ) -> Option<InputOutcome> {
        if self.annotation_ui.composer.is_some() {
            return Some(match event {
                crossterm::event::Event::Key(key)
                    if key.kind != crossterm::event::KeyEventKind::Release =>
                {
                    self.handle_annotation_composer_key(key)
                }
                crossterm::event::Event::Mouse(mouse) => {
                    self.handle_annotation_composer_mouse(mouse)
                }
                crossterm::event::Event::Paste(text) => {
                    if let Some(composer) = self.annotation_ui.composer.as_mut() {
                        composer.prompt.textarea.insert_str(text);
                    }
                    InputOutcome::Changed
                }
                _ => InputOutcome::Changed,
            });
        }
        if self.annotation_ui.context_menu.is_some() {
            return Some(match event {
                crossterm::event::Event::Key(key)
                    if key.kind != crossterm::event::KeyEventKind::Release =>
                {
                    match key.code {
                        KeyCode::Esc => {
                            self.annotation_ui.context_menu = None;
                            InputOutcome::Changed
                        }
                        KeyCode::Enter | KeyCode::Char('a') => {
                            self.activate_annotation_context_menu()
                        }
                        _ => InputOutcome::Changed,
                    }
                }
                crossterm::event::Event::Mouse(mouse) => {
                    self.handle_annotation_context_menu_mouse(mouse)
                }
                _ => InputOutcome::Changed,
            });
        }
        None
    }

    fn handle_annotation_composer_key(&mut self, key: &KeyEvent) -> InputOutcome {
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            self.annotation_ui.composer = None;
            return InputOutcome::Changed;
        }
        let enter = self
            .annotation_ui
            .composer
            .as_mut()
            .map(|composer| composer.prompt.route_enter(key))
            .unwrap_or(EnterOutcome::PassThrough);
        match enter {
            EnterOutcome::NewlineInserted => return InputOutcome::Changed,
            EnterOutcome::Submit => return self.submit_annotation_composer(),
            EnterOutcome::PassThrough => {}
        }
        let event = self
            .annotation_ui
            .composer
            .as_mut()
            .map(|composer| composer.prompt.handle_key(key))
            .unwrap_or(PromptEvent::Ignored);
        match event {
            PromptEvent::Edited => InputOutcome::Changed,
            PromptEvent::Ignored => InputOutcome::Changed,
        }
    }

    fn handle_annotation_composer_mouse(&mut self, mouse: &MouseEvent) -> InputOutcome {
        let Some(composer) = self.annotation_ui.composer.as_mut() else {
            return InputOutcome::Unchanged;
        };
        match handle_modal_mouse(&mut composer.window, mouse.kind, mouse.column, mouse.row) {
            ModalWindowOutcome::CloseRequested
            | ModalWindowOutcome::ShortcutActivated(COMPOSER_CANCEL_ID) => {
                self.annotation_ui.composer = None;
                InputOutcome::Changed
            }
            ModalWindowOutcome::ShortcutActivated(COMPOSER_SUBMIT_ID) => {
                self.submit_annotation_composer()
            }
            ModalWindowOutcome::Unhandled => {
                composer.prompt.handle_mouse(mouse);
                InputOutcome::Changed
            }
            _ => InputOutcome::Changed,
        }
    }

    fn submit_annotation_composer(&mut self) -> InputOutcome {
        let Some(composer) = self.annotation_ui.composer.as_mut() else {
            return InputOutcome::Changed;
        };
        if composer.prompt.text().trim().is_empty() {
            self.show_toast("Enter a question for the selected text");
            return InputOutcome::Changed;
        }
        let Some(question) = composer.prompt.try_send() else {
            // A trailing backslash is the prompt widget's portable newline
            // gesture. Keep the composer and its edited draft open.
            return InputOutcome::Changed;
        };
        let question = question.trim().to_string();
        let composer = self
            .annotation_ui
            .composer
            .take()
            .expect("composer remained present after reading its prompt");
        match composer.target {
            AnnotationComposerTarget::New { anchor } => {
                InputOutcome::Action(Action::BeginInlineAnnotation { anchor, question })
            }
            AnnotationComposerTarget::FollowUp { thread_id, .. } => {
                InputOutcome::Action(Action::FollowUpInlineAnnotation {
                    thread_id,
                    question,
                })
            }
        }
    }

    fn activate_annotation_context_menu(&mut self) -> InputOutcome {
        let Some(menu) = self.annotation_ui.context_menu.take() else {
            return InputOutcome::Changed;
        };
        self.open_annotation_composer(AnnotationComposerTarget::New {
            anchor: menu.anchor,
        });
        InputOutcome::Changed
    }

    fn handle_annotation_context_menu_mouse(&mut self, mouse: &MouseEvent) -> InputOutcome {
        let Some(menu) = self.annotation_ui.context_menu.as_mut() else {
            return InputOutcome::Unchanged;
        };
        match mouse.kind {
            MouseEventKind::Moved => {
                let hovered = menu
                    .action_area
                    .is_some_and(|area| area.contains((mouse.column, mouse.row).into()));
                menu.hovered = hovered;
                InputOutcome::Changed
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if menu
                    .action_area
                    .is_some_and(|area| area.contains((mouse.column, mouse.row).into()))
                {
                    self.activate_annotation_context_menu()
                } else {
                    self.annotation_ui.context_menu = None;
                    InputOutcome::Changed
                }
            }
            _ => InputOutcome::Changed,
        }
    }

    pub(crate) fn handle_annotation_card_mouse(
        &mut self,
        mouse: &MouseEvent,
    ) -> Option<InputOutcome> {
        if crate::app::minimal_mode_active() || self.annotation_overlay_open() {
            return None;
        }
        if mouse.kind == MouseEventKind::Moved {
            let hovered = self
                .annotation_ui
                .card_placements
                .iter()
                .find_map(|placement| {
                    placement.buttons.iter().find_map(|(action, area)| {
                        area.contains((mouse.column, mouse.row).into())
                            .then(|| (placement.id.clone(), action.clone()))
                    })
                });
            if hovered != self.annotation_ui.hovered_card_button {
                self.annotation_ui.hovered_card_button = hovered;
                return Some(InputOutcome::Changed);
            }
            return None;
        }
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return None;
        }

        let hit = self
            .annotation_ui
            .card_placements
            .iter()
            .find_map(|placement| {
                placement
                    .buttons
                    .iter()
                    .find(|(_, area)| area.contains((mouse.column, mouse.row).into()))
                    .map(|(action, _)| (placement.id.clone(), action.clone()))
                    .or_else(|| {
                        placement
                            .area
                            .contains((mouse.column, mouse.row).into())
                            .then(|| {
                                (
                                    placement.id.clone(),
                                    AnnotationCardAction::Toggle.as_str().to_string(),
                                )
                            })
                    })
            });
        let (id, action) = hit?;
        let thread_id = uuid::Uuid::parse_str(&id).ok()?;
        let action = AnnotationCardAction::parse(&action)?;
        Some(self.activate_annotation_card_action(thread_id, action))
    }

    fn activate_annotation_card_action(
        &mut self,
        thread_id: ThreadId,
        action: AnnotationCardAction,
    ) -> InputOutcome {
        match action {
            AnnotationCardAction::Toggle => {
                if !self.annotation_ui.expanded_threads.remove(&thread_id) {
                    self.annotation_ui.expanded_threads.insert(thread_id);
                }
                InputOutcome::Changed
            }
            AnnotationCardAction::Retry => {
                let Some(PendingAnnotationFork::Failed {
                    anchor, question, ..
                }) = self.annotation_runtime.pending_forks.remove(&thread_id)
                else {
                    self.show_toast("Annotation draft is no longer retryable");
                    return InputOutcome::Changed;
                };
                InputOutcome::Action(Action::BeginInlineAnnotation { anchor, question })
            }
            AnnotationCardAction::Dismiss => {
                self.annotation_runtime.pending_forks.remove(&thread_id);
                if self
                    .annotation_ui
                    .hovered_card_button
                    .as_ref()
                    .is_some_and(|(id, _)| id == &thread_id.to_string())
                {
                    self.annotation_ui.hovered_card_button = None;
                }
                InputOutcome::Changed
            }
            AnnotationCardAction::FollowUp => self.open_annotation_follow_up(thread_id),
            AnnotationCardAction::OpenChild => {
                let Some(child_session_id) = self
                    .annotation_runtime
                    .state
                    .threads
                    .get(&thread_id)
                    .map(|thread| thread.child_session_id.clone())
                else {
                    self.show_toast("Annotation thread not found");
                    return InputOutcome::Changed;
                };
                InputOutcome::Action(Action::LoadSession(
                    child_session_id,
                    Some(self.session.cwd.clone()),
                    false,
                ))
            }
            AnnotationCardAction::Cancel => {
                InputOutcome::Action(Action::CancelInlineAnnotation(thread_id))
            }
            AnnotationCardAction::Delete => {
                InputOutcome::Action(Action::DeleteInlineAnnotation(thread_id))
            }
        }
    }

    /// Rebuild width-specific cards before `ScrollbackState::prepare_layout`.
    pub(crate) fn sync_annotation_decorations(&mut self, scrollback_width: u16) {
        if crate::app::minimal_mode_active() || scrollback_width == 0 {
            self.scrollback.set_decorations(Vec::new());
            return;
        }
        let appearance = self.scrollback.appearance();
        let content_width = HorizontalLayout::new(
            ratatui::layout::Rect::new(0, 0, scrollback_width, 1),
            &appearance.scrollback.layout,
        )
        .content_width();
        if content_width < 12 {
            self.scrollback.set_decorations(Vec::new());
            return;
        }

        self.refresh_annotation_transcript_attachments();

        let fallback_entry = self.orphan_annotation_fallback_entry();
        let theme = Theme::current();
        let hovered = self.annotation_ui.hovered_card_button.as_ref();
        let mut decorations = Vec::new();

        for pending in &self.annotation_runtime.pending_forks {
            let (thread_id, pending) = pending;
            let entry_id = self
                .annotation_entry_id(pending.anchor())
                .or(fallback_entry);
            let Some(entry_id) = entry_id else { continue };
            decorations.push(build_pending_card(
                *thread_id,
                pending,
                entry_id,
                content_width,
                hovered,
                &theme,
            ));
        }

        for (thread_id, thread) in &self.annotation_runtime.state.threads {
            if thread.deleted {
                continue;
            }
            let attached = matches!(thread.attachment, AnnotationThreadAttachment::Attached);
            let entry_id = if attached {
                self.annotation_entry_id(&thread.anchor)
            } else {
                fallback_entry
            };
            let Some(entry_id) = entry_id else { continue };
            let expanded = self.annotation_ui.expanded_threads.contains(thread_id)
                || self.annotation_runtime.in_flight.contains_key(thread_id);
            decorations.push(build_thread_card(
                thread,
                entry_id,
                if attached {
                    thread.anchor.end_source_line
                } else {
                    usize::MAX
                },
                content_width,
                expanded,
                self.annotation_runtime.in_flight.contains_key(thread_id),
                hovered,
                &theme,
            ));
        }
        self.scrollback.set_decorations(decorations);
    }

    fn annotation_entry_id(&self, anchor: &AnnotationAnchor) -> Option<EntryId> {
        let (idx, _) = resolve_transcript_key_with_index(&self.scrollback, &anchor.transcript_key)?;
        self.scrollback.entry(idx).map(|entry| entry.id)
    }

    /// Rewind and replay can change the parent transcript after annotations
    /// were loaded. Revalidate before projecting cards so a stale key or
    /// changed source becomes an explicit orphan instead of silently
    /// disappearing or accepting a follow-up against the wrong text.
    fn refresh_annotation_transcript_attachments(&mut self) {
        let updates: Vec<_> = self
            .annotation_runtime
            .state
            .threads
            .iter()
            .filter(|(_, thread)| {
                !thread.deleted
                    && !matches!(
                        thread.attachment,
                        AnnotationThreadAttachment::Orphaned(
                            AnnotationOrphanReason::MissingChildSession
                        )
                    )
            })
            .map(|(thread_id, thread)| {
                let attachment = match crate::annotations::resolve_transcript_key(
                    &self.scrollback,
                    &thread.anchor.transcript_key,
                ) {
                    Some(transcript)
                        if validate_annotation_anchor(&thread.anchor, &transcript).is_ok() =>
                    {
                        AnnotationThreadAttachment::Attached
                    }
                    Some(_) => {
                        AnnotationThreadAttachment::Orphaned(AnnotationOrphanReason::AnchorMismatch)
                    }
                    None => AnnotationThreadAttachment::Orphaned(
                        AnnotationOrphanReason::MissingTranscriptEntry,
                    ),
                };
                (*thread_id, attachment)
            })
            .collect();
        for (thread_id, attachment) in updates {
            if let Some(thread) = self.annotation_runtime.state.threads.get_mut(&thread_id) {
                thread.attachment = attachment;
            }
        }
    }

    fn orphan_annotation_fallback_entry(&self) -> Option<EntryId> {
        (0..self.scrollback.len())
            .rev()
            .find_map(|idx| {
                resolve_transcript_entry(&self.scrollback, idx)
                    .ok()
                    .and_then(|_| self.scrollback.entry(idx).map(|entry| entry.id))
            })
            .or_else(|| {
                self.scrollback
                    .len()
                    .checked_sub(1)
                    .and_then(|idx| self.scrollback.entry(idx).map(|entry| entry.id))
            })
    }
}

fn build_pending_card(
    thread_id: ThreadId,
    pending: &PendingAnnotationFork,
    entry_id: EntryId,
    width: u16,
    hovered: Option<&(String, String)>,
    theme: &Theme,
) -> ScrollbackDecoration {
    let status = match pending {
        PendingAnnotationFork::Forking { .. } => "creating child…",
        PendingAnnotationFork::Failed { .. } => "creation failed",
    };
    let mut body = wrapped_text_lines(
        pending.question(),
        labeled_body_width(width),
        "Q  ",
        theme.text_primary,
    );
    if let PendingAnnotationFork::Failed { message, .. } = pending {
        body.extend(wrapped_text_lines(
            &format!("Error: {message}"),
            labeled_body_width(width),
            "!  ",
            theme.accent_error,
        ));
    }
    let id = thread_id.to_string();
    let action_rows = if matches!(pending, PendingAnnotationFork::Failed { .. }) {
        card_action_rows(
            &id,
            &[AnnotationCardAction::Retry, AnnotationCardAction::Dismiss],
            width,
            true,
            hovered,
            theme,
        )
    } else {
        Vec::new()
    };
    build_card(
        id,
        pending.anchor(),
        entry_id,
        pending.anchor().end_source_line,
        status,
        body,
        action_rows,
        width,
        revision_for_pending(thread_id, pending, width, hovered),
        theme,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_thread_card(
    thread: &AnnotationThread,
    entry_id: EntryId,
    after_source_line: usize,
    width: u16,
    expanded: bool,
    active: bool,
    hovered: Option<&(String, String)>,
    theme: &Theme,
) -> ScrollbackDecoration {
    let status = thread_status(thread, active);
    let mut body = Vec::new();
    if expanded {
        for exchange in &thread.exchanges {
            body.extend(wrapped_text_lines(
                &exchange.question,
                labeled_body_width(width),
                "Q  ",
                theme.accent_user,
            ));
            if exchange.answer_markdown.is_empty() {
                body.push(styled_text_line(
                    if matches!(exchange.status, AnnotationExchangeStatus::Streaming) {
                        "A  Waiting for answer…"
                    } else {
                        "A  (no answer)"
                    },
                    theme.gray,
                ));
            } else {
                let markdown = crate::scrollback::blocks::markdown_content::MarkdownContent::new(
                    &exchange.answer_markdown,
                );
                for (idx, line) in markdown
                    .output(labeled_body_width(width))
                    .lines
                    .into_iter()
                    .enumerate()
                {
                    let prefix = if idx == 0 { "A  " } else { "   " };
                    let mut spans = vec![Span::styled(prefix, Style::default().fg(theme.gray))];
                    spans.extend(line.content.spans);
                    body.push(Line::from(spans));
                }
            }
            if let AnnotationExchangeStatus::Failed { message } = &exchange.status {
                body.extend(wrapped_text_lines(
                    &format!("Failed: {message}"),
                    labeled_body_width(width),
                    "!  ",
                    theme.accent_error,
                ));
            }
        }
        if thread.exchanges.is_empty() {
            body.extend(wrapped_text_lines(
                &thread.first_question,
                labeled_body_width(width),
                "Q  ",
                theme.accent_user,
            ));
        }
        if let AnnotationThreadAttachment::Orphaned(reason) = thread.attachment {
            body.push(styled_text_line(orphan_message(reason), theme.warning));
        }
    } else {
        let preview = thread
            .exchanges
            .last()
            .filter(|exchange| !exchange.answer_markdown.is_empty())
            .map(|exchange| exchange.answer_markdown.as_str())
            .unwrap_or(thread.first_question.as_str());
        let preview = preview.split_whitespace().collect::<Vec<_>>().join(" ");
        body.push(styled_text_line(
            crate::render::line_utils::truncate_str(
                &preview,
                width.saturating_sub(CARD_PREFIX_WIDTH as u16) as usize,
            ),
            theme.gray,
        ));
    }

    let mut actions = vec![AnnotationCardAction::Toggle];
    if active {
        actions.push(AnnotationCardAction::Cancel);
    } else {
        if matches!(thread.attachment, AnnotationThreadAttachment::Attached) {
            actions.push(AnnotationCardAction::FollowUp);
            actions.push(AnnotationCardAction::OpenChild);
        } else if !matches!(
            thread.attachment,
            AnnotationThreadAttachment::Orphaned(AnnotationOrphanReason::MissingChildSession)
        ) {
            actions.push(AnnotationCardAction::OpenChild);
        }
        actions.push(AnnotationCardAction::Delete);
    }
    let id = thread.thread_id.to_string();
    let action_rows = card_action_rows(&id, &actions, width, expanded, hovered, theme);
    build_card(
        id,
        &thread.anchor,
        entry_id,
        after_source_line,
        status,
        body,
        action_rows,
        width,
        revision_for_thread(thread, width, expanded, active, hovered),
        theme,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_card(
    id: String,
    anchor: &AnnotationAnchor,
    entry_id: EntryId,
    after_source_line: usize,
    status: &str,
    body: Vec<Line<'static>>,
    action_rows: Vec<(Line<'static>, Vec<DecorationButton>)>,
    width: u16,
    revision: u64,
    theme: &Theme,
) -> ScrollbackDecoration {
    let background = theme.bg_dark;
    let role = match anchor.entry_role {
        AnnotationEntryRole::User => "User",
        AnnotationEntryRole::Assistant => "Assistant",
    };
    let lines = if anchor.start_source_line == anchor.end_source_line {
        format!("L{}", anchor.start_source_line)
    } else {
        format!("L{}-L{}", anchor.start_source_line, anchor.end_source_line)
    };
    let status = (!status.is_empty())
        .then(|| format!(" · {status}"))
        .unwrap_or_default();
    let header = Line::from(vec![
        Span::styled("╭─ ", Style::default().fg(theme.accent_user)),
        Span::styled(
            format!("Annotation · {role} · {lines}"),
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(status, Style::default().fg(theme.gray)),
    ]);
    let mut decoration_lines = vec![DecorationLine::new(header, background)];
    for line in body {
        let mut spans = vec![Span::styled("│  ", Style::default().fg(theme.gray_dim))];
        spans.extend(line.spans);
        decoration_lines.push(DecorationLine::new(Line::from(spans), background));
    }
    let mut buttons = vec![DecorationButton {
        row: 0,
        col: 0,
        width,
        action: AnnotationCardAction::Toggle.as_str().into(),
    }];
    for (line, mut row_buttons) in action_rows {
        let row = decoration_lines.len();
        for button in &mut row_buttons {
            button.row = row;
        }
        buttons.extend(row_buttons);
        decoration_lines.push(DecorationLine::new(line, background));
    }
    decoration_lines.push(DecorationLine::new(
        Line::from(Span::styled("╰─", Style::default().fg(theme.gray_dim))),
        background,
    ));

    ScrollbackDecoration {
        id,
        revision,
        entry_id,
        after_source_line,
        lines: decoration_lines,
        buttons,
    }
}

fn card_action_rows(
    id: &str,
    actions: &[AnnotationCardAction],
    width: u16,
    expanded: bool,
    hovered: Option<&(String, String)>,
    theme: &Theme,
) -> Vec<(Line<'static>, Vec<DecorationButton>)> {
    let mut rows = Vec::new();
    let mut spans = vec![Span::styled("│  ", Style::default().fg(theme.gray_dim))];
    let mut buttons = Vec::new();
    let mut col = CARD_PREFIX_WIDTH as u16;
    for action in actions {
        let label = match action {
            AnnotationCardAction::Toggle if expanded => "[collapse]",
            AnnotationCardAction::Toggle => "[expand]",
            AnnotationCardAction::Retry => "[retry]",
            AnnotationCardAction::Dismiss => "[dismiss]",
            AnnotationCardAction::FollowUp => "[follow up]",
            AnnotationCardAction::OpenChild => "[open child]",
            AnnotationCardAction::Cancel => "[cancel]",
            AnnotationCardAction::Delete => "[delete]",
        };
        let label_width = UnicodeWidthStr::width(label) as u16;
        let required = label_width + u16::from(col > CARD_PREFIX_WIDTH as u16);
        if col.saturating_add(required) > width && !buttons.is_empty() {
            rows.push((Line::from(spans), buttons));
            spans = vec![Span::styled("│  ", Style::default().fg(theme.gray_dim))];
            buttons = Vec::new();
            col = CARD_PREFIX_WIDTH as u16;
        }
        if col > CARD_PREFIX_WIDTH as u16 {
            spans.push(Span::raw(" "));
            col += 1;
        }
        let hovered = hovered.is_some_and(|(hovered_id, hovered_action)| {
            hovered_id == id && hovered_action == action.as_str()
        });
        spans.push(Span::styled(
            label.to_string(),
            Style::default()
                .fg(if hovered {
                    theme.text_primary
                } else {
                    theme.gray_bright
                })
                .add_modifier(if hovered {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ));
        buttons.push(DecorationButton {
            row: 0,
            col,
            width: label_width,
            action: action.as_str().into(),
        });
        col = col.saturating_add(label_width);
    }
    if !buttons.is_empty() {
        rows.push((Line::from(spans), buttons));
    }
    rows
}

fn wrapped_text_lines(
    text: &str,
    width: usize,
    first_prefix: &'static str,
    color: ratatui::style::Color,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    textwrap::wrap(text, width)
        .into_iter()
        .enumerate()
        .map(|(idx, text)| {
            let prefix = if idx == 0 { first_prefix } else { "   " };
            Line::from(vec![
                Span::styled(prefix, Style::default().fg(color)),
                Span::styled(text.into_owned(), Style::default().fg(color)),
            ])
        })
        .collect()
}

fn labeled_body_width(width: u16) -> usize {
    width.saturating_sub((CARD_PREFIX_WIDTH + CARD_LABEL_WIDTH) as u16) as usize
}

fn styled_text_line(text: impl Into<String>, color: ratatui::style::Color) -> Line<'static> {
    Line::from(Span::styled(text.into(), Style::default().fg(color)))
}

fn thread_status(thread: &AnnotationThread, active: bool) -> &'static str {
    if let AnnotationThreadAttachment::Orphaned(_) = thread.attachment {
        return "detached";
    }
    if active {
        return "answering…";
    }
    match thread.exchanges.last().map(|exchange| &exchange.status) {
        Some(AnnotationExchangeStatus::Failed { .. }) => "failed",
        Some(AnnotationExchangeStatus::Streaming) => "interrupted",
        _ => "",
    }
}

fn orphan_message(reason: AnnotationOrphanReason) -> &'static str {
    match reason {
        AnnotationOrphanReason::MissingChildSession => "Detached: child session is missing",
        AnnotationOrphanReason::MissingTranscriptEntry => "Detached: source message is missing",
        AnnotationOrphanReason::AnchorMismatch => "Detached: source text changed",
    }
}

fn revision_for_pending(
    thread_id: ThreadId,
    pending: &PendingAnnotationFork,
    width: u16,
    hovered: Option<&(String, String)>,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    thread_id.hash(&mut hasher);
    width.hash(&mut hasher);
    pending.question().hash(&mut hasher);
    match pending {
        PendingAnnotationFork::Forking { .. } => 0u8.hash(&mut hasher),
        PendingAnnotationFork::Failed { message, .. } => {
            1u8.hash(&mut hasher);
            message.hash(&mut hasher);
        }
    }
    hovered.hash(&mut hasher);
    hasher.finish()
}

fn revision_for_thread(
    thread: &AnnotationThread,
    width: u16,
    expanded: bool,
    active: bool,
    hovered: Option<&(String, String)>,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    thread.thread_id.hash(&mut hasher);
    width.hash(&mut hasher);
    expanded.hash(&mut hasher);
    active.hash(&mut hasher);
    format!("{:?}", thread.attachment).hash(&mut hasher);
    for exchange in &thread.exchanges {
        exchange.exchange_id.hash(&mut hasher);
        exchange.question.hash(&mut hasher);
        exchange.answer_markdown.hash(&mut hasher);
        format!("{:?}", exchange.status).hash(&mut hasher);
    }
    hovered.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotations::{AnnotationExchange, TranscriptKey};
    use crate::app::agent_view::test_agent_view;
    use crate::scrollback::blocks::UserPromptBlock;
    use crate::scrollback::text_selection::{
        PersistentTextSelection, ResolvedSelectableLine, SelectionEndpoint, SelectionKind,
        SelectionOrigin, VisibleBlockGeometry,
    };
    use crate::scrollback::types::{block_line_selectable_width, derive_selection_text};
    use crate::scrollback::{RenderBlock, ResolvedSelectionModel};
    use crate::views::annotation::AnnotationComposerTarget;
    use chrono::Utc;
    use crossterm::event::{Event, KeyEvent, KeyModifiers, MouseEvent};
    use ratatui::layout::Rect;

    fn anchor() -> AnnotationAnchor {
        AnnotationAnchor {
            parent_session_id: "parent".into(),
            transcript_key: TranscriptKey {
                prompt_index: 1,
                role: AnnotationEntryRole::Assistant,
                ordinal: 0,
            },
            entry_role: AnnotationEntryRole::Assistant,
            target_prompt_index: 1,
            start_source_line: 2,
            end_source_line: 4,
            selected_text: "selected text".into(),
            selected_text_hash: "hash".into(),
            surrounding_text_hash: "context".into(),
        }
    }

    fn completed_thread() -> AnnotationThread {
        let now = Utc::now();
        AnnotationThread {
            thread_id: uuid::Uuid::from_u128(1),
            anchor: anchor(),
            child_session_id: "child".into(),
            first_question: "Why?".into(),
            exchanges: vec![AnnotationExchange {
                exchange_id: uuid::Uuid::from_u128(2),
                question: "Why?".into(),
                answer_markdown: "Because.".into(),
                status: AnnotationExchangeStatus::Completed,
                started_at: now,
                updated_at: now,
            }],
            deleted: false,
            attachment: AnnotationThreadAttachment::Attached,
            created_at: now,
            updated_at: now,
        }
    }

    fn agent_with_annotatable_selection() -> AgentView {
        let mut agent = test_agent_view(Some("parent"), "/tmp/project".into());
        let mut user = UserPromptBlock::new("selected source text");
        user.prompt_index = Some(1);
        agent.scrollback.push_block(RenderBlock::UserPrompt(user));

        let content_width = 40u16;
        let output = {
            let entry = agent.scrollback.get(0).unwrap();
            entry.ensure_cached(
                content_width,
                agent.scrollback.appearance(),
                false,
                agent.scrollback.cwd(),
            );
            entry.cached_rendered_output_ref().output.clone()
        };
        let block_line_idx = output
            .lines
            .iter()
            .position(|line| line.selection_range == Some(0))
            .expect("user text must be selectable");
        let line = &output.lines[block_line_idx];
        let selectable_width = block_line_selectable_width(line);
        let mut model = ResolvedSelectionModel::default();
        model.push_line(ResolvedSelectableLine {
            entry_idx: 0,
            range_id: 0,
            block_line_idx,
            screen_y: 6,
            screen_x: 4,
            selectable_cols: 0..selectable_width,
            text: derive_selection_text(line),
            joiner_to_previous: None,
        });
        model.visible_blocks.push(VisibleBlockGeometry {
            entry_idx: 0,
            area: Rect::new(0, 4, content_width, output.lines.len() as u16),
            content_area: Rect::new(0, 4, content_width, output.lines.len() as u16),
            selection_area: Rect::new(0, 4, content_width, output.lines.len() as u16),
            content_width,
            top_clipped: false,
            bottom_clipped: false,
            drag_startable: true,
        });
        agent.update_scrollback_selection_state(model, Default::default());
        agent.persistent_text_selection = Some(PersistentTextSelection {
            entry_idx: 0,
            range_id: 0,
            anchor: SelectionEndpoint {
                block_line_idx,
                col_within_range: 0,
            },
            head: SelectionEndpoint {
                block_line_idx,
                col_within_range: 7,
            },
            origin: SelectionOrigin::Drag,
            kind: SelectionKind::Linear,
        });
        agent
    }

    #[test]
    fn narrow_card_wraps_actions_and_preserves_each_hit_target() {
        let thread = completed_thread();
        let card = build_thread_card(
            &thread,
            EntryId::new(1),
            4,
            24,
            true,
            false,
            None,
            &Theme::current(),
        );
        let actions: std::collections::HashSet<_> = card
            .buttons
            .iter()
            .map(|button| button.action.as_str())
            .collect();
        assert!(actions.contains("toggle"));
        assert!(actions.contains("follow_up"));
        assert!(actions.contains("open_child"));
        assert!(actions.contains("delete"));
        assert!(card.lines.len() > 5, "expanded card must include body rows");
    }

    #[test]
    fn pending_card_wraps_labeled_body_inside_available_width() {
        let pending = PendingAnnotationFork::Forking {
            anchor: anchor(),
            question: "Why does this selected historical passage need a detailed explanation?"
                .into(),
            exchange_id: uuid::Uuid::from_u128(3),
        };
        let card = build_pending_card(
            uuid::Uuid::from_u128(4),
            &pending,
            EntryId::new(1),
            24,
            None,
            &Theme::current(),
        );

        assert!(card.lines.len() > 3, "long question should wrap");
        for line in &card.lines[1..card.lines.len() - 1] {
            assert!(
                line.content.width() <= 24,
                "card body row overflowed: {:?}",
                line.content
            );
        }
    }

    #[test]
    fn transcript_change_orphans_card_then_valid_replay_reattaches_it() {
        let mut agent = test_agent_view(Some("parent"), "/tmp/project".into());
        let mut user = UserPromptBlock::new("replayed source");
        user.prompt_index = Some(9);
        let fallback_id = agent.scrollback.push_block(RenderBlock::UserPrompt(user));
        let mut thread = completed_thread();
        let thread_id = thread.thread_id;
        agent
            .annotation_runtime
            .state
            .threads
            .insert(thread_id, thread.clone());

        agent.sync_annotation_decorations(80);
        assert_eq!(
            agent.annotation_runtime.state.threads[&thread_id].attachment,
            AnnotationThreadAttachment::Orphaned(AnnotationOrphanReason::MissingTranscriptEntry)
        );
        let detached = &agent.scrollback.decoration_map()[&fallback_id][0];
        assert_eq!(detached.after_source_line, usize::MAX);

        let selected = "replayed source";
        thread.anchor = AnnotationAnchor {
            parent_session_id: "parent".into(),
            transcript_key: TranscriptKey {
                prompt_index: 9,
                role: AnnotationEntryRole::User,
                ordinal: 0,
            },
            entry_role: AnnotationEntryRole::User,
            target_prompt_index: 9,
            start_source_line: 1,
            end_source_line: 1,
            selected_text: selected.into(),
            selected_text_hash: blake3::hash(selected.as_bytes()).to_hex().to_string(),
            surrounding_text_hash: blake3::hash(selected.as_bytes()).to_hex().to_string(),
        };
        thread.attachment =
            AnnotationThreadAttachment::Orphaned(AnnotationOrphanReason::MissingTranscriptEntry);
        agent
            .annotation_runtime
            .state
            .threads
            .insert(thread_id, thread);

        agent.sync_annotation_decorations(80);
        assert_eq!(
            agent.annotation_runtime.state.threads[&thread_id].attachment,
            AnnotationThreadAttachment::Attached
        );
        assert_eq!(
            agent.scrollback.decoration_map()[&fallback_id][0].after_source_line,
            1
        );
    }

    #[test]
    fn failed_draft_can_retry_or_be_dismissed() {
        let thread_id = uuid::Uuid::from_u128(70);
        let failed = || PendingAnnotationFork::Failed {
            anchor: anchor(),
            question: "Try again?".into(),
            exchange_id: uuid::Uuid::from_u128(71),
            message: "fork failed".into(),
        };
        let mut agent = test_agent_view(Some("parent"), "/tmp/project".into());
        agent
            .annotation_runtime
            .pending_forks
            .insert(thread_id, failed());

        let outcome = agent.activate_annotation_card_action(thread_id, AnnotationCardAction::Retry);
        assert!(matches!(
            outcome,
            InputOutcome::Action(Action::BeginInlineAnnotation { question, .. })
                if question == "Try again?"
        ));
        assert!(
            !agent
                .annotation_runtime
                .pending_forks
                .contains_key(&thread_id)
        );

        agent
            .annotation_runtime
            .pending_forks
            .insert(thread_id, failed());
        assert!(matches!(
            agent.activate_annotation_card_action(thread_id, AnnotationCardAction::Dismiss),
            InputOutcome::Changed
        ));
        assert!(
            !agent
                .annotation_runtime
                .pending_forks
                .contains_key(&thread_id)
        );
    }

    #[test]
    fn composer_escape_cancels_without_creating_a_child() {
        let mut agent = test_agent_view(Some("parent"), "/tmp/project".into());
        agent.open_annotation_composer(AnnotationComposerTarget::New { anchor: anchor() });
        let outcome = agent.handle_annotation_overlay_input(&Event::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert!(matches!(outcome, Some(InputOutcome::Changed)));
        assert!(agent.annotation_ui.composer.is_none());
        assert!(agent.annotation_runtime.pending_forks.is_empty());
    }

    #[test]
    fn composer_enter_emits_typed_begin_action() {
        let mut agent = test_agent_view(Some("parent"), "/tmp/project".into());
        agent.open_annotation_composer(AnnotationComposerTarget::New { anchor: anchor() });
        agent
            .annotation_ui
            .composer
            .as_mut()
            .unwrap()
            .prompt
            .set_text("Why this line?");
        let outcome = agent.handle_annotation_overlay_input(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(matches!(
            outcome,
            Some(InputOutcome::Action(Action::BeginInlineAnnotation { question, .. }))
                if question == "Why this line?"
        ));
        assert!(agent.annotation_ui.composer.is_none());
    }

    #[test]
    fn composer_submit_keeps_backslash_continuation_draft_open() {
        let mut agent = test_agent_view(Some("parent"), "/tmp/project".into());
        agent.open_annotation_composer(AnnotationComposerTarget::New { anchor: anchor() });
        agent
            .annotation_ui
            .composer
            .as_mut()
            .unwrap()
            .prompt
            .textarea
            .insert_str("explain this\\");

        assert!(matches!(
            agent.submit_annotation_composer(),
            InputOutcome::Changed
        ));
        let composer = agent.annotation_ui.composer.as_ref().unwrap();
        assert_eq!(composer.prompt.text(), "explain this\n");
        assert!(agent.annotation_runtime.pending_forks.is_empty());
    }

    #[test]
    fn alt_a_is_the_registered_annotation_shortcut() {
        let registry = crate::actions::ActionRegistry::defaults();
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT);
        assert!(registry.matches_id(crate::actions::ActionId::AnnotateSelection, &key));
    }

    #[test]
    fn right_click_opens_context_menu_only_inside_held_selection() {
        let mut inside = agent_with_annotatable_selection();
        assert!(matches!(
            inside.open_annotation_context_menu(6, 6),
            InputOutcome::Changed
        ));
        assert!(inside.annotation_ui.context_menu.is_some());

        let mut outside = agent_with_annotatable_selection();
        assert!(matches!(
            outside.open_annotation_context_menu(30, 6),
            InputOutcome::Unchanged
        ));
        assert!(outside.annotation_ui.context_menu.is_none());
    }

    #[test]
    fn card_hit_targets_route_to_typed_actions() {
        let mut agent = test_agent_view(Some("parent"), "/tmp/project".into());
        let thread = completed_thread();
        let thread_id = thread.thread_id;
        agent
            .annotation_runtime
            .sessions
            .insert(thread.child_session_id.clone(), thread_id);
        agent
            .annotation_runtime
            .state
            .threads
            .insert(thread_id, thread);
        agent.annotation_ui.card_placements = vec![crate::scrollback::DecorationPlacement {
            id: thread_id.to_string(),
            area: Rect::new(2, 3, 30, 5),
            top_clipped: false,
            bottom_clipped: false,
            exact_anchor: true,
            buttons: vec![("follow_up".into(), Rect::new(5, 6, 11, 1))],
        }];

        let outcome = agent
            .handle_annotation_card_mouse(&MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 8,
                row: 6,
                modifiers: KeyModifiers::NONE,
            })
            .expect("button must be consumed");
        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(matches!(
            agent.annotation_ui.composer.as_ref().map(|composer| &composer.target),
            Some(AnnotationComposerTarget::FollowUp { thread_id: id, .. }) if *id == thread_id
        ));

        agent.annotation_ui.composer = None;
        assert!(matches!(
            agent.activate_annotation_card_action(thread_id, AnnotationCardAction::OpenChild),
            InputOutcome::Action(Action::LoadSession(id, _, false)) if id == "child"
        ));
        assert!(matches!(
            agent.activate_annotation_card_action(thread_id, AnnotationCardAction::Cancel),
            InputOutcome::Action(Action::CancelInlineAnnotation(id)) if id == thread_id
        ));
        assert!(matches!(
            agent.activate_annotation_card_action(thread_id, AnnotationCardAction::Delete),
            InputOutcome::Action(Action::DeleteInlineAnnotation(id)) if id == thread_id
        ));
    }
}
