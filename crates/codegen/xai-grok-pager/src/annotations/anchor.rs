use std::fmt;

use serde::{Deserialize, Serialize};

use crate::scrollback::RenderBlock;
use crate::scrollback::state::ScrollbackState;
use crate::scrollback::text_selection::{ActiveTextDrag, SelectionKind};
use crate::scrollback::types::{BlockLine, block_line_selectable_width, selectable_cols};

/// Transcript roles eligible for V1 inline annotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationEntryRole {
    User,
    Assistant,
}

impl fmt::Display for AnnotationEntryRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Assistant => f.write_str("assistant"),
        }
    }
}

/// Replay-stable identity for one annotatable transcript message.
///
/// `EntryId` and rendered indices are deliberately excluded: both are local
/// layout identities. Prompt index + role + within-turn ordinal can be rebuilt
/// from persisted replay updates and is validated against quote/context hashes
/// before an annotation is attached.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TranscriptKey {
    pub prompt_index: usize,
    pub role: AnnotationEntryRole,
    pub ordinal: usize,
}

impl fmt::Display for TranscriptKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "prompt:{}:{}:{}",
            self.prompt_index, self.role, self.ordinal
        )
    }
}

/// Durable identity plus source text for one resolved transcript entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub key: TranscriptKey,
    pub role: AnnotationEntryRole,
    pub target_prompt_index: usize,
    pub raw_text: String,
}

/// Stable parent-owned anchor written into the annotation event log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationAnchor {
    pub parent_session_id: String,
    pub transcript_key: TranscriptKey,
    pub entry_role: AnnotationEntryRole,
    pub target_prompt_index: usize,
    /// 1-based, inclusive raw-message line.
    pub start_source_line: usize,
    /// 1-based, inclusive raw-message line.
    pub end_source_line: usize,
    pub selected_text: String,
    pub selected_text_hash: String,
    pub surrounding_text_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnotationSelectionError {
    MissingParentSession,
    EntryOutOfBounds,
    UnsupportedEntry,
    StreamingEntry,
    MissingPromptTurn,
    CrossMessage,
    CrossSelectionRange,
    EmptySelection,
    InvalidRenderedRange,
    MissingSemanticLine,
    SourceLineOutOfBounds,
}

impl fmt::Display for AnnotationSelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingParentSession => "the parent session has no persistent id yet",
            Self::EntryOutOfBounds => "the selected transcript entry is no longer available",
            Self::UnsupportedEntry => {
                "only completed User or Assistant message text can be annotated"
            }
            Self::StreamingEntry => "wait for the selected message to finish streaming",
            Self::MissingPromptTurn => "the selected message has no containing prompt turn",
            Self::CrossMessage => "annotations cannot span multiple messages",
            Self::CrossSelectionRange => "annotations cannot span unrelated content regions",
            Self::EmptySelection => "select some message text before adding an annotation",
            Self::InvalidRenderedRange => "the rendered selection is no longer valid",
            Self::MissingSemanticLine => {
                "the selection includes content without a stable source line"
            }
            Self::SourceLineOutOfBounds => {
                "the selected source lines no longer match the raw message"
            }
        };
        f.write_str(message)
    }
}

impl std::error::Error for AnnotationSelectionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorValidationError {
    TranscriptKey,
    Role,
    PromptIndex,
    SelectedTextHash,
    SourceLineRange,
    SurroundingTextHash,
}

fn role_of(block: &RenderBlock) -> Option<AnnotationEntryRole> {
    match block {
        RenderBlock::UserPrompt(_) => Some(AnnotationEntryRole::User),
        RenderBlock::AgentMessage(_) => Some(AnnotationEntryRole::Assistant),
        _ => None,
    }
}

fn is_turn_start(block: &RenderBlock) -> bool {
    matches!(block, RenderBlock::UserPrompt(user) if !user.is_interjection)
}

/// Resolve an annotatable entry to its replay-stable transcript identity.
pub fn resolve_transcript_entry(
    scrollback: &ScrollbackState,
    entry_idx: usize,
) -> Result<TranscriptEntry, AnnotationSelectionError> {
    let entry = scrollback
        .get(entry_idx)
        .ok_or(AnnotationSelectionError::EntryOutOfBounds)?;
    let role = role_of(&entry.block).ok_or(AnnotationSelectionError::UnsupportedEntry)?;
    if entry.is_running {
        return Err(AnnotationSelectionError::StreamingEntry);
    }

    let turn_start_idx = (0..=entry_idx)
        .rev()
        .find(|idx| {
            scrollback
                .get(*idx)
                .is_some_and(|candidate| is_turn_start(&candidate.block))
        })
        .ok_or(AnnotationSelectionError::MissingPromptTurn)?;

    let turn_prompt = scrollback
        .get(turn_start_idx)
        .and_then(|entry| match &entry.block {
            RenderBlock::UserPrompt(user) => Some(user),
            _ => None,
        })
        .ok_or(AnnotationSelectionError::MissingPromptTurn)?;
    let target_prompt_index = turn_prompt.prompt_index.unwrap_or_else(|| {
        (0..=turn_start_idx)
            .filter(|idx| {
                scrollback
                    .get(*idx)
                    .is_some_and(|candidate| is_turn_start(&candidate.block))
            })
            .count()
            .saturating_sub(1)
    });

    let ordinal = (turn_start_idx..=entry_idx)
        .filter(|idx| {
            scrollback
                .get(*idx)
                .and_then(|candidate| role_of(&candidate.block))
                == Some(role)
        })
        .count()
        .saturating_sub(1);

    let raw_text = match &entry.block {
        RenderBlock::UserPrompt(user) => user.text.clone(),
        RenderBlock::AgentMessage(assistant) => assistant.text(),
        _ => return Err(AnnotationSelectionError::UnsupportedEntry),
    };

    Ok(TranscriptEntry {
        key: TranscriptKey {
            prompt_index: target_prompt_index,
            role,
            ordinal,
        },
        role,
        target_prompt_index,
        raw_text,
    })
}

/// Rebuild an annotatable transcript entry from its durable key after replay.
pub fn resolve_transcript_key(
    scrollback: &ScrollbackState,
    key: &TranscriptKey,
) -> Option<TranscriptEntry> {
    resolve_transcript_key_with_index(scrollback, key).map(|(_, entry)| entry)
}

/// Resolve a durable transcript key and retain its current in-memory entry
/// index for UI-only decoration placement.
pub(crate) fn resolve_transcript_key_with_index(
    scrollback: &ScrollbackState,
    key: &TranscriptKey,
) -> Option<(usize, TranscriptEntry)> {
    let mut turn_count = 0usize;
    let mut current_prompt_index = None;
    let mut user_ordinal = 0usize;
    let mut assistant_ordinal = 0usize;

    for entry_idx in 0..scrollback.len() {
        let entry = scrollback.get(entry_idx)?;
        if is_turn_start(&entry.block) {
            current_prompt_index = match &entry.block {
                RenderBlock::UserPrompt(user) => Some(user.prompt_index.unwrap_or(turn_count)),
                _ => unreachable!("is_turn_start accepts only UserPrompt"),
            };
            turn_count = turn_count.saturating_add(1);
            user_ordinal = 0;
            assistant_ordinal = 0;
        }

        let Some(role) = role_of(&entry.block) else {
            continue;
        };
        let ordinal = match role {
            AnnotationEntryRole::User => user_ordinal,
            AnnotationEntryRole::Assistant => assistant_ordinal,
        };
        match role {
            AnnotationEntryRole::User => user_ordinal = user_ordinal.saturating_add(1),
            AnnotationEntryRole::Assistant => {
                assistant_ordinal = assistant_ordinal.saturating_add(1)
            }
        }

        let Some(target_prompt_index) = current_prompt_index else {
            continue;
        };
        if entry.is_running
            || target_prompt_index != key.prompt_index
            || role != key.role
            || ordinal != key.ordinal
        {
            continue;
        }
        let raw_text = match &entry.block {
            RenderBlock::UserPrompt(user) => user.text.clone(),
            RenderBlock::AgentMessage(assistant) => assistant.text(),
            _ => unreachable!("role_of accepted a non-message block"),
        };
        return Some((
            entry_idx,
            TranscriptEntry {
                key: key.clone(),
                role,
                target_prompt_index,
                raw_text,
            },
        ));
    }
    None
}

/// Convert transient rendered selection coordinates into a durable semantic
/// anchor. The selected text is supplied by the existing copy reconstruction
/// path so table-shaped selections and Unicode display columns stay identical
/// to what the user highlighted.
pub fn build_annotation_anchor(
    parent_session_id: &str,
    transcript: &TranscriptEntry,
    block_lines: &[BlockLine],
    selection: &ActiveTextDrag,
    selected_text: &str,
) -> Result<AnnotationAnchor, AnnotationSelectionError> {
    if parent_session_id.is_empty() {
        return Err(AnnotationSelectionError::MissingParentSession);
    }
    if selection.anchor.entry_idx != selection.head.entry_idx {
        return Err(AnnotationSelectionError::CrossMessage);
    }
    if selection.anchor.range_id != selection.head.range_id {
        return Err(AnnotationSelectionError::CrossSelectionRange);
    }
    if selected_text.is_empty() {
        return Err(AnnotationSelectionError::EmptySelection);
    }

    let start = selection
        .anchor
        .block_line_idx
        .min(selection.head.block_line_idx);
    let end = selection
        .anchor
        .block_line_idx
        .max(selection.head.block_line_idx);
    if start >= block_lines.len() || end >= block_lines.len() {
        return Err(AnnotationSelectionError::InvalidRenderedRange);
    }

    let source_lines = selected_source_lines(block_lines, selection)?;
    let mut source_lines = source_lines.into_iter();
    let first_source = source_lines
        .next()
        .ok_or(AnnotationSelectionError::MissingSemanticLine)?;
    let (mut min_source, mut max_source) = (first_source, first_source);
    for source in source_lines {
        min_source = min_source.min(source);
        max_source = max_source.max(source);
    }

    let surrounding = surrounding_text(&transcript.raw_text, min_source, max_source)
        .ok_or(AnnotationSelectionError::SourceLineOutOfBounds)?;

    Ok(AnnotationAnchor {
        parent_session_id: parent_session_id.to_string(),
        transcript_key: transcript.key.clone(),
        entry_role: transcript.role,
        target_prompt_index: transcript.target_prompt_index,
        start_source_line: min_source,
        end_source_line: max_source,
        selected_text: selected_text.to_string(),
        selected_text_hash: blake3::hash(selected_text.as_bytes()).to_hex().to_string(),
        surrounding_text_hash: blake3::hash(surrounding.as_bytes()).to_hex().to_string(),
    })
}

fn selected_source_lines(
    block_lines: &[BlockLine],
    selection: &ActiveTextDrag,
) -> Result<Vec<usize>, AnnotationSelectionError> {
    let anchor = (
        selection.anchor.block_line_idx,
        selection.anchor.col_within_range,
    );
    let head = (
        selection.head.block_line_idx,
        selection.head.col_within_range,
    );
    let ((start_line, start_col), (end_line, end_col)) = if anchor <= head {
        (anchor, head)
    } else {
        (head, anchor)
    };

    let mut result = Vec::new();
    for (line_idx, line) in block_lines
        .iter()
        .enumerate()
        .take(end_line + 1)
        .skip(start_line)
        .filter(|(_, line)| line.selection_range == Some(selection.anchor.range_id))
    {
        // Table-shaped selections are not a linear column sweep. Their rows
        // already carry an unambiguous scalar source line, so retain that
        // established path.
        if selection.kind != SelectionKind::Linear || line.source_spans.is_empty() {
            result.push(
                line.source_line
                    .ok_or(AnnotationSelectionError::MissingSemanticLine)?,
            );
            continue;
        }

        let selectable_width = block_line_selectable_width(line);
        let selected = if start_line == end_line {
            start_col.min(end_col)..start_col.max(end_col).saturating_add(1)
        } else if line_idx == start_line {
            start_col..selectable_width
        } else if line_idx == end_line {
            0..end_col.saturating_add(1)
        } else {
            0..selectable_width
        };
        let selected = selected.start.min(selectable_width)..selected.end.min(selectable_width);
        let selectable_start = selectable_cols(&line.content, &line.selectable)
            .ok_or(AnnotationSelectionError::MissingSemanticLine)?
            .start as usize;
        let absolute = (selectable_start + selected.start as usize)
            ..(selectable_start + selected.end as usize);
        let before = result.len();
        result.extend(
            line.source_spans
                .iter()
                .filter(|span| {
                    span.column_range.start < absolute.end && span.column_range.end > absolute.start
                })
                .map(|span| span.source_line),
        );
        if result.len() == before {
            return Err(AnnotationSelectionError::MissingSemanticLine);
        }
    }
    Ok(result)
}

fn surrounding_text(raw_text: &str, start_line: usize, end_line: usize) -> Option<String> {
    let raw_lines: Vec<&str> = raw_text.split('\n').collect();
    if start_line == 0 || start_line > end_line || end_line > raw_lines.len() {
        return None;
    }
    let context_start = start_line.saturating_sub(3);
    let context_end = (end_line + 2).min(raw_lines.len());
    Some(raw_lines[context_start..context_end].join("\n"))
}

/// Validate a persisted anchor against the transcript entry resolved from the
/// current replay. Any mismatch or invalid bound must orphan the annotation;
/// line numbers alone are never enough to reattach it.
pub fn validate_annotation_anchor(
    anchor: &AnnotationAnchor,
    transcript: &TranscriptEntry,
) -> Result<(), AnchorValidationError> {
    if anchor.transcript_key != transcript.key {
        return Err(AnchorValidationError::TranscriptKey);
    }
    if anchor.entry_role != transcript.role {
        return Err(AnchorValidationError::Role);
    }
    if anchor.target_prompt_index != transcript.target_prompt_index {
        return Err(AnchorValidationError::PromptIndex);
    }
    if blake3::hash(anchor.selected_text.as_bytes())
        .to_hex()
        .as_str()
        != anchor.selected_text_hash
    {
        return Err(AnchorValidationError::SelectedTextHash);
    }
    let surrounding = surrounding_text(
        &transcript.raw_text,
        anchor.start_source_line,
        anchor.end_source_line,
    )
    .ok_or(AnchorValidationError::SourceLineRange)?;
    if blake3::hash(surrounding.as_bytes()).to_hex().as_str() != anchor.surrounding_text_hash {
        return Err(AnchorValidationError::SurroundingTextHash);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::AppearanceConfig;
    use crate::scrollback::block::BlockContent;
    use crate::scrollback::blocks::UserPromptBlock;
    use crate::scrollback::blocks::markdown_content::MarkdownContent;
    use crate::scrollback::text_selection::{RangeHit, SelectionKind};
    use crate::scrollback::types::{BlockContext, DisplayMode, derive_selection_text};
    use unicode_width::UnicodeWidthStr;

    fn context(width: u16) -> BlockContext {
        BlockContext {
            mode: DisplayMode::Expanded,
            is_running: false,
            width,
            raw: false,
            max_lines: None,
            appearance: AppearanceConfig::default(),
            is_selected: false,
            cwd: None,
        }
    }

    fn selection(entry: usize, start_line: usize, end_line: usize) -> ActiveTextDrag {
        ActiveTextDrag {
            anchor: RangeHit {
                entry_idx: entry,
                range_id: 0,
                block_line_idx: start_line,
                col_within_range: 0,
            },
            head: RangeHit {
                entry_idx: entry,
                range_id: 0,
                block_line_idx: end_line,
                col_within_range: 0,
            },
            kind: SelectionKind::Linear,
            anchor_content_width: Some(12),
        }
    }

    fn selection_for_text(lines: &[BlockLine], needle: &str) -> ActiveTextDrag {
        for (line_idx, line) in lines.iter().enumerate() {
            let text = derive_selection_text(line);
            let Some(byte_start) = text.find(needle) else {
                continue;
            };
            let start_col = UnicodeWidthStr::width(&text[..byte_start]) as u16;
            let end_col = start_col
                .saturating_add(UnicodeWidthStr::width(needle) as u16)
                .saturating_sub(1);
            return ActiveTextDrag {
                anchor: RangeHit {
                    entry_idx: 0,
                    range_id: 0,
                    block_line_idx: line_idx,
                    col_within_range: start_col,
                },
                head: RangeHit {
                    entry_idx: 0,
                    range_id: 0,
                    block_line_idx: line_idx,
                    col_within_range: end_col,
                },
                kind: SelectionKind::Linear,
                anchor_content_width: Some(80),
            };
        }
        panic!("no rendered line contains {needle:?}");
    }

    #[test]
    fn replay_key_uses_prompt_role_and_within_turn_ordinal() {
        let mut scrollback = ScrollbackState::new();
        let mut user = UserPromptBlock::new("question");
        user.prompt_index = Some(7);
        scrollback.push_block(RenderBlock::UserPrompt(user));
        scrollback.push_block(RenderBlock::agent_message("first answer"));
        scrollback.push_block(RenderBlock::agent_message("second answer"));

        let user = resolve_transcript_entry(&scrollback, 0).unwrap();
        let first = resolve_transcript_entry(&scrollback, 1).unwrap();
        let second = resolve_transcript_entry(&scrollback, 2).unwrap();

        assert_eq!(user.key.to_string(), "prompt:7:user:0");
        assert_eq!(first.key.to_string(), "prompt:7:assistant:0");
        assert_eq!(second.key.to_string(), "prompt:7:assistant:1");
        assert_eq!(second.target_prompt_index, 7);
    }

    #[test]
    fn linear_key_resolution_matches_per_entry_identity() {
        let mut scrollback = ScrollbackState::new();
        let mut first = UserPromptBlock::new("first");
        first.prompt_index = Some(7);
        scrollback.push_block(RenderBlock::UserPrompt(first));
        scrollback.push_block(RenderBlock::agent_message("first answer"));
        scrollback.push_block(RenderBlock::UserPrompt(UserPromptBlock::interjection(
            "clarify",
        )));
        scrollback.push_block(RenderBlock::agent_message("second answer"));
        scrollback.push_block(RenderBlock::system("chrome"));
        scrollback.push_block(RenderBlock::user_prompt("fallback turn"));
        scrollback.push_block(RenderBlock::agent_message("fallback answer"));

        for entry_idx in 0..scrollback.len() {
            let Ok(expected) = resolve_transcript_entry(&scrollback, entry_idx) else {
                continue;
            };
            assert_eq!(
                resolve_transcript_key_with_index(&scrollback, &expected.key),
                Some((entry_idx, expected))
            );
        }
    }

    #[test]
    fn fallback_prompt_index_and_interjection_ordinal_are_deterministic() {
        let mut scrollback = ScrollbackState::new();
        scrollback.push_block(RenderBlock::user_prompt("first"));
        scrollback.push_block(RenderBlock::agent_message("answer"));
        scrollback.push_block(RenderBlock::UserPrompt(UserPromptBlock::interjection(
            "clarification",
        )));

        let interjection = resolve_transcript_entry(&scrollback, 2).unwrap();
        assert_eq!(interjection.key.to_string(), "prompt:0:user:1");
        assert_eq!(interjection.target_prompt_index, 0);
    }

    #[test]
    fn wrapped_fragments_collapse_to_one_semantic_source_line() {
        let block = crate::scrollback::blocks::AgentMessageBlock::new(
            "one source line with enough words to wrap narrowly",
        );
        let output = block.output(&context(12));
        assert!(output.lines.len() > 1);
        let transcript = TranscriptEntry {
            key: TranscriptKey {
                prompt_index: 2,
                role: AnnotationEntryRole::Assistant,
                ordinal: 0,
            },
            role: AnnotationEntryRole::Assistant,
            target_prompt_index: 2,
            raw_text: block.text(),
        };

        let anchor = build_annotation_anchor(
            "parent",
            &transcript,
            &output.lines,
            &selection(0, 0, output.lines.len() - 1),
            "one source line",
        )
        .unwrap();

        assert_eq!((anchor.start_source_line, anchor.end_source_line), (1, 1));
        assert_eq!(anchor.target_prompt_index, 2);
        assert_eq!(anchor.selected_text, "one source line");
        assert_eq!(anchor.selected_text_hash.len(), 64);
        assert_eq!(anchor.surrounding_text_hash.len(), 64);
    }

    #[test]
    fn collapsed_soft_break_anchors_each_selected_side_at_multiple_widths() {
        for (raw, first, second, widths) in [
            ("**alpha**\n_beta_", "alpha", "beta", [80usize, 6]),
            ("**甲乙**\n_丙丁_", "甲乙", "丙丁", [80usize, 5]),
        ] {
            let transcript = TranscriptEntry {
                key: TranscriptKey {
                    prompt_index: 2,
                    role: AnnotationEntryRole::Assistant,
                    ordinal: 0,
                },
                role: AnnotationEntryRole::Assistant,
                target_prompt_index: 2,
                raw_text: raw.to_string(),
            };
            for width in widths {
                let output = MarkdownContent::new(raw).output(width);
                let first_anchor = build_annotation_anchor(
                    "parent",
                    &transcript,
                    &output.lines,
                    &selection_for_text(&output.lines, first),
                    first,
                )
                .unwrap();
                let second_anchor = build_annotation_anchor(
                    "parent",
                    &transcript,
                    &output.lines,
                    &selection_for_text(&output.lines, second),
                    second,
                )
                .unwrap();
                assert_eq!(
                    (first_anchor.start_source_line, first_anchor.end_source_line),
                    (1, 1),
                    "raw={raw:?}, width={width}, first={first:?}",
                );
                assert_eq!(
                    (
                        second_anchor.start_source_line,
                        second_anchor.end_source_line
                    ),
                    (2, 2),
                    "raw={raw:?}, width={width}, second={second:?}",
                );
            }
        }
    }

    #[test]
    fn cross_message_selection_is_rejected() {
        let transcript = TranscriptEntry {
            key: TranscriptKey {
                prompt_index: 0,
                role: AnnotationEntryRole::User,
                ordinal: 0,
            },
            role: AnnotationEntryRole::User,
            target_prompt_index: 0,
            raw_text: "hello".to_string(),
        };
        let output = UserPromptBlock::new("hello").output(&context(80));
        let mut cross = selection(0, 0, 0);
        cross.head.entry_idx = 1;

        assert_eq!(
            build_annotation_anchor("parent", &transcript, &output.lines, &cross, "hello"),
            Err(AnnotationSelectionError::CrossMessage)
        );
    }

    #[test]
    fn non_message_and_running_entries_are_ineligible() {
        let mut scrollback = ScrollbackState::new();
        scrollback.push_block(RenderBlock::user_prompt("question"));
        scrollback.push_block(RenderBlock::system("chrome"));
        let running = scrollback.start_streaming_agent();

        assert_eq!(
            resolve_transcript_entry(&scrollback, 1),
            Err(AnnotationSelectionError::UnsupportedEntry)
        );
        let running_idx = scrollback.index_of_id(running).unwrap();
        assert_eq!(
            resolve_transcript_entry(&scrollback, running_idx),
            Err(AnnotationSelectionError::StreamingEntry)
        );
    }

    #[test]
    fn replay_validation_orphans_changed_source_context() {
        let transcript = TranscriptEntry {
            key: TranscriptKey {
                prompt_index: 0,
                role: AnnotationEntryRole::User,
                ordinal: 0,
            },
            role: AnnotationEntryRole::User,
            target_prompt_index: 0,
            raw_text: "before\nselected\nafter".to_string(),
        };
        let output = UserPromptBlock::new(&transcript.raw_text).output(&context(80));
        let anchor = build_annotation_anchor(
            "parent",
            &transcript,
            &output.lines,
            &selection(0, 1, 1),
            "selected",
        )
        .unwrap();
        assert_eq!(validate_annotation_anchor(&anchor, &transcript), Ok(()));

        let changed = TranscriptEntry {
            raw_text: "before\nchanged\nafter".to_string(),
            ..transcript
        };
        assert_eq!(
            validate_annotation_anchor(&anchor, &changed),
            Err(AnchorValidationError::SurroundingTextHash)
        );
    }
}
