//! Standard-TUI interaction and card projection for inline annotations.

use std::hash::{Hash, Hasher};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::AgentView;
use crate::annotations::{
    AnnotationAnchor, AnnotationEntryRole, AnnotationExchangePhase, AnnotationExchangeStatus,
    AnnotationOrphanReason, AnnotationThread, AnnotationThreadAttachment, PendingAnnotationFork,
    ThreadId, resolve_transcript_entry, resolve_transcript_key_with_index,
    validate_annotation_anchor,
};
use crate::app::actions::Action;
use crate::app::app_view::InputOutcome;
use crate::scrollback::{
    DecorationButton, DecorationLine, EntryId, HorizontalLayout, ScrollbackDecoration,
};
use crate::theme::Theme;
use crate::views::annotation::{
    ANNOTATION_COMPOSER_DECORATION_ID, ANNOTATION_COMPOSER_INPUT_ACTION,
    ActiveAnnotationCardTextDrag, AnnotationCardAction, AnnotationCardBodyCache,
    AnnotationCardBodyCacheKey, AnnotationCardTextPoint, AnnotationCardTextSelection,
    AnnotationComposerState, AnnotationComposerTarget, AnnotationContextMenuState,
    PendingAnnotationCardTextDrag,
};
use crate::views::prompt_widget::PromptEvent;

const CARD_PREFIX_WIDTH: usize = 3;
const CARD_LABEL_WIDTH: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AnnotationCardTextHit {
    thread_id: ThreadId,
    selectable_revision: u64,
    point: AnnotationCardTextPoint,
}

impl AgentView {
    pub(crate) fn annotation_overlay_open(&self) -> bool {
        self.annotation_ui.composer.is_some() || self.annotation_ui.context_menu.is_some()
    }

    pub(crate) fn open_annotation_composer_from_selection(&mut self) -> InputOutcome {
        if crate::app::minimal_mode_active() {
            return InputOutcome::Unchanged;
        }
        if let Some((thread_id, after_card_row)) = self
            .annotation_ui
            .card_text_selection
            .as_ref()
            .map(|selection| {
                let (_, end) = ordered_annotation_card_points(selection.anchor, selection.head);
                (selection.thread_id, end.row)
            })
        {
            return self.open_annotation_follow_up_at(thread_id, Some(after_card_row));
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
        self.annotation_ui.composer_placement = None;
        self.annotation_ui.composer = Some(AnnotationComposerState::new(target, &self.session.cwd));
    }

    fn open_annotation_follow_up(&mut self, thread_id: ThreadId) -> InputOutcome {
        self.open_annotation_follow_up_at(thread_id, None)
    }

    fn open_annotation_follow_up_at(
        &mut self,
        thread_id: ThreadId,
        after_card_row: Option<usize>,
    ) -> InputOutcome {
        if let Some(message) = self.annotation_storage_unavailable() {
            self.show_toast(&message);
            return InputOutcome::Changed;
        }
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
        // The generic follow-up action belongs below the complete expanded
        // card. A text-selection follow-up must preserve the card's current
        // shape so its captured row continues to identify the selected line.
        if after_card_row.is_none() {
            self.annotation_ui.expanded_threads.insert(thread_id);
        }
        self.open_annotation_composer(AnnotationComposerTarget::FollowUp {
            thread_id,
            after_card_row,
        });
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
                self.annotation_ui.context_menu = Some(AnnotationContextMenuState::new(
                    AnnotationComposerTarget::New { anchor },
                    column,
                    row,
                ));
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
                        composer.prompt.handle_paste(text);
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
            self.annotation_ui.composer_placement = None;
            return InputOutcome::Changed;
        }
        let enter_submits = self
            .annotation_ui
            .composer
            .as_ref()
            .is_some_and(|composer| {
                key.code == KeyCode::Enter && !composer.prompt.file_search_visible()
            });
        if enter_submits {
            return self.submit_annotation_composer();
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
        let inside = composer
            .input_area
            .is_some_and(|area| area.contains((mouse.column, mouse.row).into()));
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if !inside => {
                self.annotation_ui.composer = None;
                self.annotation_ui.composer_placement = None;
                InputOutcome::Changed
            }
            MouseEventKind::Down(MouseButton::Left)
            | MouseEventKind::Drag(MouseButton::Left)
            | MouseEventKind::Up(MouseButton::Left) => {
                composer.prompt.handle_mouse(mouse);
                InputOutcome::Changed
            }
            _ => InputOutcome::Changed,
        }
    }

    fn submit_annotation_composer(&mut self) -> InputOutcome {
        let Some(composer) = self.annotation_ui.composer.as_ref() else {
            return InputOutcome::Changed;
        };
        if composer.prompt.text().trim().is_empty() {
            self.show_toast("Enter a question for the selected text");
            return InputOutcome::Changed;
        }
        let question = composer.prompt.text().trim().to_string();
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
        match menu.target {
            AnnotationComposerTarget::New { anchor } => {
                self.open_annotation_composer(AnnotationComposerTarget::New { anchor });
                InputOutcome::Changed
            }
            AnnotationComposerTarget::FollowUp {
                thread_id,
                after_card_row,
            } => self.open_annotation_follow_up_at(thread_id, after_card_row),
        }
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
        match mouse.kind {
            MouseEventKind::Moved => {
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
                None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let button = self
                    .annotation_ui
                    .card_placements
                    .iter()
                    .find_map(|placement| {
                        placement
                            .buttons
                            .iter()
                            .find(|(_, area)| area.contains((mouse.column, mouse.row).into()))
                            .map(|(action, _)| (placement.id.clone(), action.clone()))
                    });
                if let Some((id, action)) = button {
                    self.persistent_text_selection = None;
                    self.table_selection_geometry = None;
                    self.clear_annotation_card_text_selection();
                    let thread_id = uuid::Uuid::parse_str(&id).ok()?;
                    let action = AnnotationCardAction::parse(&action)?;
                    return Some(self.activate_annotation_card_action(thread_id, action));
                }
                if let Some(hit) = self.annotation_card_text_hit_exact(mouse.column, mouse.row) {
                    self.persistent_text_selection = None;
                    self.table_selection_geometry = None;
                    self.selection_created_at = None;
                    self.annotation_ui.card_text_selection = None;
                    self.annotation_ui.active_card_text_drag = None;
                    self.annotation_ui.pending_card_text_drag =
                        Some(PendingAnnotationCardTextDrag {
                            thread_id: hit.thread_id,
                            selectable_revision: hit.selectable_revision,
                            anchor: hit.point,
                            start_col: mouse.column,
                            start_row: mouse.row,
                        });
                    return Some(InputOutcome::Changed);
                }
                if self.annotation_card_contains_point(mouse.column, mouse.row) {
                    self.persistent_text_selection = None;
                    self.table_selection_geometry = None;
                    self.clear_annotation_card_text_selection();
                    return Some(InputOutcome::Changed);
                }
                None
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(mut drag) = self.annotation_ui.active_card_text_drag {
                    if let Some(head) = self.annotation_card_text_hit_nearest(
                        drag.thread_id,
                        drag.selectable_revision,
                        mouse.column,
                        mouse.row,
                    ) {
                        drag.head = head;
                        self.annotation_ui.active_card_text_drag = Some(drag);
                    }
                    return Some(InputOutcome::Changed);
                }
                let pending = self.annotation_ui.pending_card_text_drag?;
                if pending.start_col.abs_diff(mouse.column) >= 1
                    || pending.start_row.abs_diff(mouse.row) >= 1
                {
                    let head = self
                        .annotation_card_text_hit_nearest(
                            pending.thread_id,
                            pending.selectable_revision,
                            mouse.column,
                            mouse.row,
                        )
                        .unwrap_or(pending.anchor);
                    self.annotation_ui.active_card_text_drag = Some(ActiveAnnotationCardTextDrag {
                        thread_id: pending.thread_id,
                        selectable_revision: pending.selectable_revision,
                        anchor: pending.anchor,
                        head,
                    });
                    self.annotation_ui.pending_card_text_drag = None;
                    return Some(InputOutcome::Changed);
                }
                Some(InputOutcome::Unchanged)
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(drag) = self.annotation_ui.active_card_text_drag.take() {
                    self.annotation_ui.pending_card_text_drag = None;
                    if let Some(text) = self.reconstruct_annotation_card_selection(&drag)
                        && !text.is_empty()
                    {
                        self.annotation_ui.card_text_selection =
                            Some(AnnotationCardTextSelection {
                                thread_id: drag.thread_id,
                                selectable_revision: drag.selectable_revision,
                                anchor: drag.anchor,
                                head: drag.head,
                            });
                        self.selection_created_at = Some(Instant::now());
                        self.copy_to_clipboard(&text);
                    }
                    return Some(InputOutcome::Changed);
                }
                if self.annotation_ui.pending_card_text_drag.take().is_some() {
                    return Some(InputOutcome::Changed);
                }
                None
            }
            MouseEventKind::Up(MouseButton::Right) => {
                if self.annotation_card_selection_contains_point(mouse.column, mouse.row) {
                    return Some(self.open_annotation_card_context_menu(mouse.column, mouse.row));
                }
                self.annotation_card_contains_point(mouse.column, mouse.row)
                    .then_some(InputOutcome::Unchanged)
            }
            _ => None,
        }
    }

    fn annotation_card_contains_point(&self, column: u16, row: u16) -> bool {
        self.annotation_ui
            .card_placements
            .iter()
            .any(|placement| placement.area.contains((column, row).into()))
    }

    fn annotation_card_text_hit_exact(
        &self,
        column: u16,
        row: u16,
    ) -> Option<AnnotationCardTextHit> {
        self.annotation_ui
            .card_placements
            .iter()
            .find_map(|placement| {
                let thread_id = uuid::Uuid::parse_str(&placement.id).ok()?;
                placement.selectable_lines.iter().find_map(|line| {
                    if line.screen_y != row {
                        return None;
                    }
                    let width = UnicodeWidthStr::width(line.text.as_str()) as u16;
                    let right = line.screen_x.saturating_add(width);
                    (width > 0 && column >= line.screen_x && column < right).then_some(
                        AnnotationCardTextHit {
                            thread_id,
                            selectable_revision: placement.selectable_revision,
                            point: AnnotationCardTextPoint {
                                row: line.row,
                                col: column.saturating_sub(line.screen_x),
                            },
                        },
                    )
                })
            })
    }

    fn annotation_card_text_hit_nearest(
        &self,
        thread_id: ThreadId,
        selectable_revision: u64,
        column: u16,
        row: u16,
    ) -> Option<AnnotationCardTextPoint> {
        let placement = self
            .annotation_ui
            .card_placements
            .iter()
            .find(|placement| {
                placement.id == thread_id.to_string()
                    && placement.selectable_revision == selectable_revision
            })?;
        placement
            .selectable_lines
            .iter()
            .filter_map(|line| {
                let width = UnicodeWidthStr::width(line.text.as_str()) as u16;
                if width == 0 {
                    return None;
                }
                let right = line.screen_x.saturating_add(width);
                let (col_distance, col) = if column < line.screen_x {
                    (line.screen_x - column, 0)
                } else if column >= right {
                    (column - right.saturating_sub(1), width.saturating_sub(1))
                } else {
                    (0, column - line.screen_x)
                };
                Some((
                    (line.screen_y.abs_diff(row), col_distance),
                    AnnotationCardTextPoint { row: line.row, col },
                ))
            })
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, point)| point)
    }

    fn reconstruct_annotation_card_selection(
        &self,
        drag: &ActiveAnnotationCardTextDrag,
    ) -> Option<String> {
        let placement = self
            .annotation_ui
            .card_placements
            .iter()
            .find(|placement| {
                placement.id == drag.thread_id.to_string()
                    && placement.selectable_revision == drag.selectable_revision
            })?;
        let (start, end) = ordered_annotation_card_points(drag.anchor, drag.head);
        let mut text = String::new();
        let mut found = false;
        for line in &placement.selectable_lines {
            let Some(cols) = annotation_card_selected_cols(start, end, line.row, &line.text) else {
                continue;
            };
            if found {
                text.push('\n');
            }
            found = true;
            text.push_str(&crate::scrollback::types::slice_display_cols(
                &line.text, cols.start, cols.end,
            ));
        }
        found.then_some(text)
    }

    pub(crate) fn render_annotation_card_text_selection(&self, buf: &mut ratatui::buffer::Buffer) {
        let (thread_id, selectable_revision, anchor, head) =
            if let Some(drag) = self.annotation_ui.active_card_text_drag {
                (
                    drag.thread_id,
                    drag.selectable_revision,
                    drag.anchor,
                    drag.head,
                )
            } else if let Some(selection) = &self.annotation_ui.card_text_selection {
                (
                    selection.thread_id,
                    selection.selectable_revision,
                    selection.anchor,
                    selection.head,
                )
            } else {
                return;
            };
        let Some(placement) = self.annotation_ui.card_placements.iter().find(|placement| {
            placement.id == thread_id.to_string()
                && placement.selectable_revision == selectable_revision
        }) else {
            return;
        };
        let (start, end) = ordered_annotation_card_points(anchor, head);
        let theme = Theme::current();
        for line in &placement.selectable_lines {
            let Some(cols) = annotation_card_selected_cols(start, end, line.row, &line.text) else {
                continue;
            };
            for col in cols {
                if let Some(cell) = buf.cell_mut((line.screen_x.saturating_add(col), line.screen_y))
                {
                    crate::scrollback::text_selection::apply_selection_highlight(&theme, cell);
                }
            }
        }
    }

    pub(crate) fn clear_annotation_card_text_selection(&mut self) {
        self.annotation_ui.pending_card_text_drag = None;
        self.annotation_ui.active_card_text_drag = None;
        self.annotation_ui.card_text_selection = None;
        self.selection_created_at = None;
    }

    fn annotation_card_selection_contains_point(&self, column: u16, row: u16) -> bool {
        let Some(selection) = &self.annotation_ui.card_text_selection else {
            return false;
        };
        let Some(hit) = self.annotation_card_text_hit_exact(column, row) else {
            return false;
        };
        if hit.thread_id != selection.thread_id
            || hit.selectable_revision != selection.selectable_revision
        {
            return false;
        }
        let (start, end) = ordered_annotation_card_points(selection.anchor, selection.head);
        hit.point >= start && hit.point <= end
    }

    fn open_annotation_card_context_menu(&mut self, column: u16, row: u16) -> InputOutcome {
        let Some(selection) = &self.annotation_ui.card_text_selection else {
            return InputOutcome::Unchanged;
        };
        if !self
            .annotation_runtime
            .state
            .threads
            .contains_key(&selection.thread_id)
        {
            return InputOutcome::Unchanged;
        }
        self.annotation_ui.context_menu = Some(AnnotationContextMenuState::new(
            AnnotationComposerTarget::FollowUp {
                thread_id: selection.thread_id,
                after_card_row: Some(
                    ordered_annotation_card_points(selection.anchor, selection.head)
                        .1
                        .row,
                ),
            },
            column,
            row,
        ));
        InputOutcome::Changed
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
                if let Some(message) = self.annotation_runtime.last_error.as_ref() {
                    self.show_toast(&format!("Annotation storage is unavailable: {message}"));
                    return InputOutcome::Changed;
                }
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
        let theme_revision = annotation_theme_revision(&theme);
        let hovered = self.annotation_ui.hovered_card_button.as_ref();
        let new_composer_anchor =
            self.annotation_ui
                .composer
                .as_ref()
                .and_then(|composer| match &composer.target {
                    AnnotationComposerTarget::New { anchor } => Some((
                        anchor.transcript_key.clone(),
                        anchor.selected_text_hash.clone(),
                        anchor.end_source_line,
                    )),
                    AnnotationComposerTarget::FollowUp { .. } => None,
                });
        let follow_up_composer =
            self.annotation_ui
                .composer
                .as_ref()
                .and_then(|composer| match &composer.target {
                    AnnotationComposerTarget::FollowUp {
                        thread_id,
                        after_card_row,
                    } => Some((*thread_id, *after_card_row)),
                    AnnotationComposerTarget::New { .. } => None,
                });
        let mut decorations = Vec::new();

        // A new annotation editor is the first decoration after the selected
        // source line, so it stays visually attached to that line even when
        // other annotation cards share the same anchor.
        if let Some((transcript_key, selected_text_hash, after_source_line)) =
            new_composer_anchor.as_ref()
            && let Some((entry_idx, _)) =
                resolve_transcript_key_with_index(&self.scrollback, transcript_key)
            && let Some(entry_id) = self.scrollback.entry(entry_idx).map(|entry| entry.id)
        {
            decorations.push(build_inline_composer_decoration(
                selected_text_hash,
                entry_id,
                *after_source_line,
                content_width,
                &theme,
            ));
        }

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
            let in_flight = self.annotation_runtime.in_flight.get(thread_id);
            let expanded = self.annotation_ui.expanded_threads.contains(thread_id);
            let active = in_flight.is_some_and(|in_flight| {
                !matches!(
                    in_flight.phase,
                    AnnotationExchangePhase::DrainingAfterStorageFailure
                )
            });
            let content_revision = self.annotation_runtime.thread_revision(*thread_id);
            let cache_key = AnnotationCardBodyCacheKey {
                content_revision,
                width: content_width,
                theme_revision,
                expanded,
            };
            let body = if let Some(cache) = self
                .annotation_ui
                .thread_card_bodies
                .get(thread_id)
                .filter(|cache| cache.key == cache_key)
            {
                cache.lines.clone()
            } else {
                let lines = build_thread_card_body(thread, content_width, expanded, &theme);
                self.annotation_ui.thread_card_bodies.insert(
                    *thread_id,
                    AnnotationCardBodyCache {
                        key: cache_key,
                        lines: lines.clone(),
                    },
                );
                self.annotation_ui.card_body_cache_misses =
                    self.annotation_ui.card_body_cache_misses.saturating_add(1);
                lines
            };
            let after_source_line = if attached {
                thread.anchor.end_source_line
            } else {
                usize::MAX
            };
            let composer_after_card_row =
                follow_up_composer.and_then(|(composer_thread_id, after_card_row)| {
                    (composer_thread_id == *thread_id)
                        .then_some(after_card_row)
                        .flatten()
                });
            decorations.push(build_thread_card_with_body(
                thread,
                entry_id,
                after_source_line,
                content_width,
                expanded,
                active,
                hovered,
                body,
                content_revision,
                theme_revision,
                composer_after_card_row,
                &theme,
            ));
            // The generic follow-up action belongs immediately below its
            // owning card. Selection-created editors are embedded inside the
            // card directly after the selected row.
            if follow_up_composer == Some((*thread_id, None)) {
                decorations.push(build_inline_composer_decoration(
                    &thread.anchor.selected_text_hash,
                    entry_id,
                    after_source_line,
                    content_width,
                    &theme,
                ));
            }
        }
        self.annotation_ui
            .thread_card_bodies
            .retain(|thread_id, _| {
                self.annotation_runtime
                    .state
                    .threads
                    .get(thread_id)
                    .is_some_and(|thread| !thread.deleted)
            });
        let selection_is_valid = |thread_id: ThreadId, selectable_revision: u64| {
            decorations.iter().any(|decoration| {
                decoration.id == thread_id.to_string()
                    && crate::scrollback::decorations::selectable_revision(decoration)
                        == selectable_revision
            })
        };
        if self
            .annotation_ui
            .pending_card_text_drag
            .is_some_and(|selection| {
                !selection_is_valid(selection.thread_id, selection.selectable_revision)
            })
        {
            self.annotation_ui.pending_card_text_drag = None;
        }
        if self
            .annotation_ui
            .active_card_text_drag
            .is_some_and(|selection| {
                !selection_is_valid(selection.thread_id, selection.selectable_revision)
            })
        {
            self.annotation_ui.active_card_text_drag = None;
        }
        if self
            .annotation_ui
            .card_text_selection
            .as_ref()
            .is_some_and(|selection| {
                !selection_is_valid(selection.thread_id, selection.selectable_revision)
            })
        {
            self.annotation_ui.card_text_selection = None;
            self.selection_created_at = None;
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
                if thread.attachment != attachment {
                    thread.attachment = attachment;
                    self.annotation_runtime.bump_thread_revision(thread_id);
                }
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

fn ordered_annotation_card_points(
    first: AnnotationCardTextPoint,
    second: AnnotationCardTextPoint,
) -> (AnnotationCardTextPoint, AnnotationCardTextPoint) {
    if first <= second {
        (first, second)
    } else {
        (second, first)
    }
}

fn annotation_card_selected_cols(
    start: AnnotationCardTextPoint,
    end: AnnotationCardTextPoint,
    row: usize,
    text: &str,
) -> Option<std::ops::Range<u16>> {
    if row < start.row || row > end.row {
        return None;
    }
    let width = UnicodeWidthStr::width(text) as u16;
    if width == 0 {
        return None;
    }
    let col_start = if row == start.row {
        start.col.min(width.saturating_sub(1))
    } else {
        0
    };
    let col_end = if row == end.row {
        end.col.saturating_add(1).min(width)
    } else {
        width
    };
    (col_start < col_end).then_some(col_start..col_end)
}

fn build_inline_composer_decoration(
    selected_text_hash: &str,
    entry_id: EntryId,
    after_source_line: usize,
    width: u16,
    theme: &Theme,
) -> ScrollbackDecoration {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ANNOTATION_COMPOSER_DECORATION_ID.hash(&mut hasher);
    selected_text_hash.hash(&mut hasher);
    after_source_line.hash(&mut hasher);
    width.hash(&mut hasher);
    theme.bg_visual.hash(&mut hasher);

    ScrollbackDecoration {
        id: ANNOTATION_COMPOSER_DECORATION_ID.into(),
        revision: hasher.finish(),
        entry_id,
        after_source_line,
        lines: vec![DecorationLine::new(
            Line::from(Span::styled(
                "│  ",
                Style::default().fg(theme.accent_user).bg(theme.bg_visual),
            )),
            theme.bg_visual,
        )],
        buttons: Vec::new(),
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
        revision_for_pending(thread_id, pending, width, hovered),
        theme,
    )
}

#[cfg(test)]
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
    let body = build_thread_card_body(thread, width, expanded, theme);
    build_thread_card_with_body(
        thread,
        entry_id,
        after_source_line,
        width,
        expanded,
        active,
        hovered,
        body,
        0,
        annotation_theme_revision(theme),
        None,
        theme,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_thread_card_with_body(
    thread: &AnnotationThread,
    entry_id: EntryId,
    after_source_line: usize,
    width: u16,
    expanded: bool,
    active: bool,
    hovered: Option<&(String, String)>,
    body: Vec<Line<'static>>,
    content_revision: u64,
    theme_revision: u64,
    composer_after_card_row: Option<usize>,
    theme: &Theme,
) -> ScrollbackDecoration {
    let status = thread_status(thread, active);
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
    let mut card = build_card(
        id,
        &thread.anchor,
        entry_id,
        after_source_line,
        status,
        body,
        action_rows,
        revision_for_thread(
            thread.thread_id,
            content_revision,
            theme_revision,
            width,
            expanded,
            active,
            hovered,
            composer_after_card_row,
        ),
        theme,
    );
    if let Some(after_card_row) = composer_after_card_row {
        insert_inline_composer_row(&mut card, after_card_row, width, theme);
    }
    card
}

fn insert_inline_composer_row(
    card: &mut ScrollbackDecoration,
    after_card_row: usize,
    width: u16,
    theme: &Theme,
) {
    let fallback_row = card
        .lines
        .iter()
        .rposition(|line| line.selectable.is_some())
        .unwrap_or(0);
    let anchored_row = card
        .lines
        .get(after_card_row)
        .is_some_and(|line| line.selectable.is_some())
        .then_some(after_card_row)
        .unwrap_or(fallback_row);
    let insertion_row = anchored_row.saturating_add(1).min(card.lines.len());
    for button in &mut card.buttons {
        if button.row >= insertion_row {
            button.row = button.row.saturating_add(1);
        }
    }
    card.lines.insert(
        insertion_row,
        DecorationLine::new(
            Line::from(Span::styled(
                "│  ",
                Style::default().fg(theme.accent_user).bg(theme.bg_visual),
            )),
            theme.bg_visual,
        ),
    );
    card.buttons.push(DecorationButton {
        row: insertion_row,
        col: 0,
        width,
        action: ANNOTATION_COMPOSER_INPUT_ACTION.into(),
    });
}

fn build_thread_card_body(
    thread: &AnnotationThread,
    width: u16,
    expanded: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
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

    body
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
        let selectable_text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let mut spans = vec![Span::styled("│  ", Style::default().fg(theme.gray_dim))];
        spans.extend(line.spans);
        decoration_lines.push(
            DecorationLine::new(Line::from(spans), background)
                .with_selectable_text(CARD_PREFIX_WIDTH as u16, selectable_text),
        );
    }
    // Expand/collapse is intentionally available only through the explicit
    // action chip. Treating the whole header/card as a toggle made ordinary
    // clicks and text-selection starts too easy to misfire.
    let mut buttons = Vec::new();
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
    thread_id: ThreadId,
    content_revision: u64,
    theme_revision: u64,
    width: u16,
    expanded: bool,
    active: bool,
    hovered: Option<&(String, String)>,
    composer_after_card_row: Option<usize>,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    thread_id.hash(&mut hasher);
    content_revision.hash(&mut hasher);
    theme_revision.hash(&mut hasher);
    width.hash(&mut hasher);
    expanded.hash(&mut hasher);
    active.hash(&mut hasher);
    hovered.hash(&mut hasher);
    composer_after_card_row.hash(&mut hasher);
    hasher.finish()
}

fn annotation_theme_revision(theme: &Theme) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // The cached body uses these label/status colors plus the complete
    // Markdown palette. Hashing fixed-size style values avoids walking any
    // annotation text while still invalidating live theme changes.
    macro_rules! hash_styles {
        ($($value:expr),+ $(,)?) => {
            $($value.hash(&mut hasher);)+
        };
    }
    hash_styles!(
        theme.accent_user,
        theme.accent_error,
        theme.gray,
        theme.warning,
        theme.md_heading_h1,
        theme.md_heading_h1_mod,
        theme.md_heading_h2,
        theme.md_heading_h2_mod,
        theme.md_heading_h3,
        theme.md_heading_h3_mod,
        theme.md_heading_h4,
        theme.md_heading_h4_mod,
        theme.md_heading_h5,
        theme.md_heading_h5_mod,
        theme.md_heading_h6,
        theme.md_heading_h6_mod,
        theme.md_code,
        theme.md_task_checked,
        theme.md_task_unchecked,
        theme.md_muted,
        theme.md_code_bg,
        theme.md_text,
        theme.link_fg,
    );
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotations::{AnnotationExchange, TranscriptKey};
    use crate::app::agent_view::test_agent_view;
    use crate::scrollback::blocks::{AgentMessageBlock, UserPromptBlock};
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

    fn agent_with_thread_card(mut thread: AnnotationThread) -> AgentView {
        let selected = "selected text";
        thread.anchor.start_source_line = 1;
        thread.anchor.end_source_line = 1;
        thread.anchor.selected_text = selected.into();
        thread.anchor.selected_text_hash = blake3::hash(selected.as_bytes()).to_hex().to_string();
        thread.anchor.surrounding_text_hash =
            blake3::hash(selected.as_bytes()).to_hex().to_string();
        thread.attachment = AnnotationThreadAttachment::Attached;
        let thread_id = thread.thread_id;

        let mut agent = test_agent_view(Some("parent"), "/tmp/project".into());
        let mut user = UserPromptBlock::new("question");
        user.prompt_index = Some(1);
        agent.scrollback.push_block(RenderBlock::UserPrompt(user));
        agent
            .scrollback
            .push_block(RenderBlock::AgentMessage(AgentMessageBlock::new(selected)));
        agent
            .annotation_runtime
            .state
            .threads
            .insert(thread_id, thread);
        agent
            .annotation_runtime
            .thread_revisions
            .insert(thread_id, 1);
        agent.annotation_ui.expanded_threads.insert(thread_id);
        agent
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
        assert!(
            card.buttons
                .iter()
                .filter(|button| button.action == "toggle")
                .all(|button| button.row > 0),
            "only the explicit action chip may toggle the card"
        );
        assert!(card.lines.len() > 5, "expanded card must include body rows");
    }

    #[test]
    fn card_body_rows_expose_copyable_text_without_card_chrome() {
        let thread = completed_thread();
        let card = build_thread_card(
            &thread,
            EntryId::new(1),
            4,
            48,
            true,
            false,
            None,
            &Theme::current(),
        );

        let selectable: Vec<_> = card
            .lines
            .iter()
            .filter_map(|line| line.selectable.as_ref().map(|text| text.text.as_str()))
            .collect();
        assert!(selectable.iter().any(|text| text.contains("Q  Why?")));
        assert!(selectable.iter().any(|text| text.contains("A  Because.")));
        assert!(
            selectable.iter().all(|text| !text.starts_with('│')),
            "copied text must omit visual card borders"
        );
    }

    #[test]
    fn clicking_card_content_does_not_toggle_expansion() {
        let thread_id = uuid::Uuid::from_u128(80);
        let mut agent = test_agent_view(Some("parent"), "/tmp/project".into());
        agent.annotation_ui.expanded_threads.insert(thread_id);
        agent.annotation_ui.card_placements = vec![crate::scrollback::DecorationPlacement {
            id: thread_id.to_string(),
            area: Rect::new(2, 3, 30, 5),
            top_clipped: false,
            bottom_clipped: false,
            exact_anchor: true,
            buttons: Vec::new(),
            selectable_lines: Vec::new(),
            selectable_revision: 0,
        }];

        let outcome = agent
            .handle_annotation_card_mouse(&MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 8,
                row: 5,
                modifiers: KeyModifiers::NONE,
            })
            .expect("card click must be consumed");

        assert!(matches!(outcome, InputOutcome::Changed));
        assert!(
            agent.annotation_ui.expanded_threads.contains(&thread_id),
            "passive card content must not collapse the thread"
        );
    }

    #[test]
    fn annotation_card_selection_reconstructs_copy_text_and_routes_menu_to_follow_up() {
        let thread = completed_thread();
        let thread_id = thread.thread_id;
        let mut agent = test_agent_view(Some("parent"), "/tmp/project".into());
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
            buttons: Vec::new(),
            selectable_lines: vec![
                crate::scrollback::DecorationSelectableLinePlacement {
                    row: 1,
                    screen_x: 5,
                    screen_y: 4,
                    text: "Q  first".into(),
                },
                crate::scrollback::DecorationSelectableLinePlacement {
                    row: 2,
                    screen_x: 5,
                    screen_y: 5,
                    text: "A  second".into(),
                },
            ],
            selectable_revision: 7,
        }];
        let drag = ActiveAnnotationCardTextDrag {
            thread_id,
            selectable_revision: 7,
            anchor: AnnotationCardTextPoint { row: 1, col: 3 },
            head: AnnotationCardTextPoint { row: 2, col: 4 },
        };

        assert_eq!(
            agent
                .reconstruct_annotation_card_selection(&drag)
                .as_deref(),
            Some("first\nA  se")
        );
        agent.annotation_ui.card_text_selection = Some(AnnotationCardTextSelection {
            thread_id,
            selectable_revision: 7,
            anchor: drag.anchor,
            head: drag.head,
        });
        assert!(matches!(
            agent.open_annotation_card_context_menu(6, 5),
            InputOutcome::Changed
        ));
        assert!(matches!(
            agent
                .annotation_ui
                .context_menu
                .as_ref()
                .map(|menu| &menu.target),
            Some(AnnotationComposerTarget::FollowUp {
                thread_id: id,
                after_card_row: Some(2),
            }) if *id == thread_id
        ));
        assert!(matches!(
            agent.activate_annotation_context_menu(),
            InputOutcome::Changed
        ));
        assert!(matches!(
            agent
                .annotation_ui
                .composer
                .as_ref()
                .map(|composer| &composer.target),
            Some(AnnotationComposerTarget::FollowUp {
                thread_id: id,
                after_card_row: Some(2),
            }) if *id == thread_id
        ));

        agent.annotation_ui.composer = None;
        assert!(matches!(
            agent.open_annotation_composer_from_selection(),
            InputOutcome::Changed
        ));
        assert!(matches!(
            agent
                .annotation_ui
                .composer
                .as_ref()
                .map(|composer| &composer.target),
            Some(AnnotationComposerTarget::FollowUp {
                thread_id: id,
                after_card_row: Some(2),
            }) if *id == thread_id
        ));
    }

    #[test]
    fn new_annotation_composer_is_a_one_row_decoration_after_the_source_line() {
        let mut agent = agent_with_annotatable_selection();
        assert!(matches!(
            agent.open_annotation_composer_from_selection(),
            InputOutcome::Changed
        ));
        agent.sync_annotation_decorations(80);

        let composer = agent
            .scrollback
            .decoration_map()
            .values()
            .flatten()
            .find(|decoration| decoration.id == ANNOTATION_COMPOSER_DECORATION_ID)
            .expect("inline composer decoration");
        assert_eq!(composer.after_source_line, 1);
        assert_eq!(composer.row_count(), 1);
    }

    #[test]
    fn follow_up_composer_is_immediately_below_its_annotation_card() {
        let thread = completed_thread();
        let thread_id = thread.thread_id;
        let mut agent = agent_with_thread_card(thread);
        assert!(matches!(
            agent.open_annotation_follow_up(thread_id),
            InputOutcome::Changed
        ));
        agent.sync_annotation_decorations(80);

        let decorations = agent
            .scrollback
            .decoration_map()
            .values()
            .find(|decorations| {
                decorations
                    .iter()
                    .any(|decoration| decoration.id == thread_id.to_string())
            })
            .expect("annotation decoration group");
        let card_index = decorations
            .iter()
            .position(|decoration| decoration.id == thread_id.to_string())
            .expect("thread card");
        assert_eq!(
            decorations
                .get(card_index + 1)
                .map(|decoration| decoration.id.as_str()),
            Some(ANNOTATION_COMPOSER_DECORATION_ID)
        );
        assert_eq!(decorations[card_index + 1].row_count(), 1);
    }

    #[test]
    fn selection_follow_up_composer_is_directly_below_selected_card_row() {
        let thread = completed_thread();
        let thread_id = thread.thread_id;
        let mut agent = agent_with_thread_card(thread);
        agent.sync_annotation_decorations(80);
        let (selected_row, selectable_revision) = {
            let card = agent
                .scrollback
                .decoration_map()
                .values()
                .flatten()
                .find(|decoration| decoration.id == thread_id.to_string())
                .expect("thread card");
            let selected_row = card
                .lines
                .iter()
                .position(|line| line.selectable.is_some())
                .expect("selectable card body row");
            (
                selected_row,
                crate::scrollback::decorations::selectable_revision(card),
            )
        };
        agent.annotation_ui.card_text_selection = Some(AnnotationCardTextSelection {
            thread_id,
            selectable_revision,
            anchor: AnnotationCardTextPoint {
                row: selected_row,
                col: 0,
            },
            head: AnnotationCardTextPoint {
                row: selected_row,
                col: 2,
            },
        });

        assert!(matches!(
            agent.open_annotation_composer_from_selection(),
            InputOutcome::Changed
        ));
        assert!(matches!(
            agent
                .annotation_ui
                .composer
                .as_ref()
                .map(|composer| &composer.target),
            Some(AnnotationComposerTarget::FollowUp {
                thread_id: id,
                after_card_row: Some(row),
            }) if *id == thread_id && *row == selected_row
        ));
        agent.sync_annotation_decorations(80);

        let decorations: Vec<_> = agent
            .scrollback
            .decoration_map()
            .values()
            .flatten()
            .collect();
        assert!(
            decorations
                .iter()
                .all(|decoration| decoration.id != ANNOTATION_COMPOSER_DECORATION_ID),
            "selection follow-up must not create a standalone editor below the card"
        );
        let card = decorations
            .iter()
            .find(|decoration| decoration.id == thread_id.to_string())
            .expect("thread card");
        let marker = card
            .buttons
            .iter()
            .find(|button| button.action == ANNOTATION_COMPOSER_INPUT_ACTION)
            .expect("embedded composer placement marker");
        assert_eq!(marker.row, selected_row + 1);
        assert_eq!(
            card.lines[marker.row].background,
            Theme::current().bg_visual
        );
    }

    #[test]
    fn inline_composer_renders_on_one_indented_row() {
        let theme = Theme::current();
        let mut state = AnnotationComposerState::new(
            AnnotationComposerTarget::New { anchor: anchor() },
            std::path::Path::new("/tmp"),
        );
        let screen = Rect::new(0, 0, 50, 8);
        let row = Rect::new(2, 4, 40, 1);
        let mut buffer = ratatui::buffer::Buffer::empty(screen);

        let rendered = crate::views::annotation::render_annotation_composer(
            &mut buffer,
            row,
            &mut state,
            &theme,
        )
        .expect("one-row composer");

        assert_eq!(state.input_area, Some(Rect::new(5, 4, 36, 1)));
        assert_eq!(rendered.cursor_pos.map(|(_, y)| y), Some(4));
        let rendered_row = (row.x..row.right())
            .filter_map(|x| buffer.cell((x, row.y)).map(|cell| cell.symbol()))
            .collect::<String>();
        assert!(rendered_row.contains("Ask about the selected text"));
    }

    #[test]
    fn context_menu_clears_guard_cells_around_wide_underlay_glyphs() {
        let screen = Rect::new(0, 0, 40, 8);
        let mut buffer = ratatui::buffer::Buffer::empty(screen);
        buffer.set_string(4, 2, "如", Style::default());
        buffer.set_string(28, 2, "界", Style::default());
        let mut menu = AnnotationContextMenuState::new(
            AnnotationComposerTarget::New { anchor: anchor() },
            5,
            2,
        );

        crate::views::annotation::render_annotation_context_menu(
            &mut buffer,
            screen,
            &mut menu,
            &Theme::current(),
        );

        assert_eq!(buffer.cell((4, 2)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((5, 2)).unwrap().symbol(), "┌");
        assert_eq!(buffer.cell((28, 2)).unwrap().symbol(), "┐");
        assert_eq!(buffer.cell((29, 2)).unwrap().symbol(), " ");
    }

    #[test]
    fn long_answer_card_cache_hits_on_idle_and_hover_redraws_and_invalidates_precisely() {
        let mut thread = completed_thread();
        thread.exchanges[0].answer_markdown =
            "A paragraph with **formatting** and `code`.\n\n".repeat(2_000);
        let thread_id = thread.thread_id;
        let mut agent = agent_with_thread_card(thread);

        agent.sync_annotation_decorations(80);
        assert_eq!(agent.annotation_ui.card_body_cache_misses, 1);
        assert_eq!(agent.annotation_ui.thread_card_bodies.len(), 1);

        for redraw in 0..64 {
            agent.annotation_ui.hovered_card_button =
                (redraw % 2 == 0).then(|| (thread_id.to_string(), "toggle".into()));
            agent.sync_annotation_decorations(80);
        }
        assert_eq!(
            agent.annotation_ui.card_body_cache_misses, 1,
            "hover-only and idle redraws must not reparse a long Markdown answer"
        );

        agent.sync_annotation_decorations(72);
        assert_eq!(agent.annotation_ui.card_body_cache_misses, 2);

        agent.annotation_ui.expanded_threads.remove(&thread_id);
        agent.sync_annotation_decorations(72);
        assert_eq!(agent.annotation_ui.card_body_cache_misses, 3);

        agent.annotation_ui.expanded_threads.insert(thread_id);
        agent
            .annotation_runtime
            .state
            .threads
            .get_mut(&thread_id)
            .unwrap()
            .exchanges[0]
            .answer_markdown
            .push_str("\nnew streamed content");
        agent.annotation_runtime.bump_thread_revision(thread_id);
        agent.sync_annotation_decorations(72);
        assert_eq!(agent.annotation_ui.card_body_cache_misses, 4);
        assert_eq!(
            agent.annotation_ui.thread_card_bodies.len(),
            1,
            "one-entry-per-thread replacement keeps the cache bounded"
        );
        assert_ne!(
            annotation_theme_revision(&Theme::groknight()),
            annotation_theme_revision(&Theme::grokday()),
            "theme changes must invalidate cached styled Markdown"
        );
    }

    #[test]
    fn in_flight_annotation_respects_collapse_and_stays_collapsed_on_completion() {
        let mut thread = completed_thread();
        thread.exchanges[0].answer_markdown = "streamed answer\n".repeat(100);
        let thread_id = thread.thread_id;
        let exchange_id = thread.exchanges[0].exchange_id;
        let mut agent = agent_with_thread_card(thread);
        agent.annotation_runtime.in_flight.insert(
            thread_id,
            crate::annotations::AnnotationInFlight::new(
                exchange_id,
                "Why?".into(),
                AnnotationExchangePhase::Prompting,
            ),
        );

        assert!(matches!(
            agent.activate_annotation_card_action(thread_id, AnnotationCardAction::Toggle),
            InputOutcome::Changed
        ));
        assert!(!agent.annotation_ui.expanded_threads.contains(&thread_id));
        agent.sync_annotation_decorations(80);
        assert!(
            !agent
                .annotation_ui
                .thread_card_bodies
                .get(&thread_id)
                .unwrap()
                .key
                .expanded,
            "an active response must not override the user's collapsed state"
        );
        let active_rows = agent
            .scrollback
            .decoration_map()
            .values()
            .flatten()
            .find(|decoration| decoration.id == thread_id.to_string())
            .unwrap()
            .row_count();

        agent.annotation_runtime.in_flight.remove(&thread_id);
        agent.sync_annotation_decorations(80);
        assert!(
            !agent
                .annotation_ui
                .thread_card_bodies
                .get(&thread_id)
                .unwrap()
                .key
                .expanded,
            "completion must not trigger a delayed collapse transition"
        );
        let completed_rows = agent
            .scrollback
            .decoration_map()
            .values()
            .flatten()
            .find(|decoration| decoration.id == thread_id.to_string())
            .unwrap()
            .row_count();
        assert_eq!(
            active_rows, completed_rows,
            "completion should not abruptly change a collapsed card's height"
        );
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
    fn single_line_composer_submits_trailing_backslash_literally() {
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

        let outcome = agent.handle_annotation_overlay_input(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::SHIFT,
        )));
        assert!(matches!(
            outcome,
            Some(InputOutcome::Action(Action::BeginInlineAnnotation { question, .. }))
                if question == "explain this\\"
        ));
        assert!(agent.annotation_ui.composer.is_none());
    }

    #[test]
    fn single_line_composer_collapses_multiline_paste_without_losing_line_breaks() {
        let mut agent = test_agent_view(Some("parent"), "/tmp/project".into());
        agent.open_annotation_composer(AnnotationComposerTarget::New { anchor: anchor() });

        let outcome =
            agent.handle_annotation_overlay_input(&Event::Paste("one\ntwo\nthree".into()));

        assert!(matches!(outcome, Some(InputOutcome::Changed)));
        let prompt = &agent.annotation_ui.composer.as_ref().unwrap().prompt;
        assert_eq!(prompt.text(), "one\ntwo\nthree");
        assert_eq!(
            prompt.textarea.elements().len(),
            1,
            "multiline content should render as one atomic paste chip"
        );

        let outcome = agent.handle_annotation_overlay_input(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(matches!(
            outcome,
            Some(InputOutcome::Action(Action::BeginInlineAnnotation { question, .. }))
                if question == "one\ntwo\nthree"
        ));
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
            selectable_lines: Vec::new(),
            selectable_revision: 0,
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
