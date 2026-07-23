//! Parent-side visual rows interleaved with transcript entries.
//!
//! Decorations participate in scrollback layout, but are deliberately absent
//! from [`ScrollbackEntry`](super::ScrollbackEntry), selection reconstruction,
//! search, export, and replay.  Callers supply already-wrapped styled rows and
//! stable entry/source-line anchors for each frame.

use std::hash::{Hash, Hasher};

use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::Line;

use super::entry::EntryId;
use super::types::BlockOutput;

#[derive(Debug, Clone)]
pub struct DecorationLine {
    pub content: Line<'static>,
    pub background: Color,
    /// Optional text-selection projection for this visual-only row.
    ///
    /// The text remains decoration-owned and therefore never enters
    /// transcript selection, search, export, or replay. `col` is the display
    /// column where `text` begins within the entry content area.
    pub selectable: Option<DecorationSelectableText>,
}

impl DecorationLine {
    pub fn new(content: Line<'static>, background: Color) -> Self {
        Self {
            content,
            background,
            selectable: None,
        }
    }

    pub fn with_selectable_text(mut self, col: u16, text: impl Into<String>) -> Self {
        self.selectable = Some(DecorationSelectableText {
            col,
            text: text.into(),
        });
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DecorationSelectableText {
    pub col: u16,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecorationButton {
    /// Zero-based row within the decoration.
    pub row: usize,
    /// Display-column offset within the entry content area.
    pub col: u16,
    pub width: u16,
    /// Caller-owned opaque action identifier.
    pub action: String,
}

#[derive(Debug, Clone)]
pub struct ScrollbackDecoration {
    /// Caller-owned stable identity (for example an annotation thread UUID).
    pub id: String,
    /// Caller-owned content revision used to avoid per-frame invalidation.
    pub revision: u64,
    pub entry_id: EntryId,
    /// One-based raw message line after which this decoration should appear.
    pub after_source_line: usize,
    pub lines: Vec<DecorationLine>,
    pub buttons: Vec<DecorationButton>,
}

impl ScrollbackDecoration {
    pub fn row_count(&self) -> u16 {
        u16::try_from(self.lines.len()).unwrap_or(u16::MAX)
    }

    pub(crate) fn layout_key(&self) -> (&str, EntryId, usize, usize) {
        (
            self.id.as_str(),
            self.entry_id,
            self.after_source_line,
            self.lines.len(),
        )
    }
}

/// Screen-space placement returned by the scrollback renderer.
#[derive(Debug, Clone)]
pub struct DecorationPlacement {
    pub id: String,
    pub area: Rect,
    pub top_clipped: bool,
    pub bottom_clipped: bool,
    /// True when `after_source_line` was present in the rendered message.
    /// False means the deterministic message-boundary fallback was used.
    pub exact_anchor: bool,
    pub buttons: Vec<(String, Rect)>,
    /// Visible selectable text rows projected into screen space.
    pub selectable_lines: Vec<DecorationSelectableLinePlacement>,
    /// Stable across hover/style-only redraws; changes when selectable text,
    /// wrapping, or row positions change.
    pub selectable_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecorationSelectableLinePlacement {
    /// Zero-based row within the complete decoration, including clipped rows.
    pub row: usize,
    pub screen_x: u16,
    pub screen_y: u16,
    pub text: String,
}

pub(crate) fn selectable_revision(decoration: &ScrollbackDecoration) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (row, line) in decoration.lines.iter().enumerate() {
        if let Some(selectable) = &line.selectable {
            row.hash(&mut hasher);
            selectable.hash(&mut hasher);
        }
    }
    hasher.finish()
}

pub(crate) struct ResolvedDecoration<'a> {
    pub decoration: &'a ScrollbackDecoration,
    /// Row within the interleaved content stream (excluding entry vpad).
    pub start_row: usize,
    /// Number of original content rows before this decoration.
    insertion_boundary: usize,
    pub exact_anchor: bool,
}

/// Maps original `BlockOutput` rows into a content stream with decorations.
///
/// The map is rebuilt only for entries in the paint window. Annotation counts
/// are expected to stay small, so the direct linear construction keeps the
/// persistent scrollback cache free of caller-specific render state.
pub(crate) struct EntryDecorationLayout<'a> {
    original_rows: Vec<usize>,
    placements: Vec<ResolvedDecoration<'a>>,
    #[cfg(test)]
    total_rows: usize,
}

impl<'a> EntryDecorationLayout<'a> {
    pub(crate) fn new(output: &BlockOutput, decorations: &'a [ScrollbackDecoration]) -> Self {
        if decorations.is_empty() {
            return Self {
                original_rows: (0..output.lines.len()).collect(),
                placements: Vec::new(),
                #[cfg(test)]
                total_rows: output.lines.len(),
            };
        }

        let fallback_after = output.lines.len().saturating_sub(1);
        let resolved_after: Vec<(usize, bool)> = decorations
            .iter()
            .map(|decoration| {
                output
                    .lines
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, line)| line.source_line == Some(decoration.after_source_line))
                    .map(|(idx, _)| (idx, true))
                    .unwrap_or((fallback_after, false))
            })
            .collect();

        let mut original_rows = Vec::with_capacity(output.lines.len());
        let mut placements = Vec::with_capacity(decorations.len());
        let mut row = 0usize;

        if output.lines.is_empty() {
            for decoration in decorations {
                placements.push(ResolvedDecoration {
                    decoration,
                    start_row: row,
                    insertion_boundary: 0,
                    exact_anchor: false,
                });
                row = row.saturating_add(decoration.lines.len());
            }
        } else {
            for line_idx in 0..output.lines.len() {
                original_rows.push(row);
                row = row.saturating_add(1);
                for (decoration_idx, decoration) in decorations.iter().enumerate() {
                    let (after_idx, exact_anchor) = resolved_after[decoration_idx];
                    if after_idx == line_idx {
                        placements.push(ResolvedDecoration {
                            decoration,
                            start_row: row,
                            insertion_boundary: line_idx.saturating_add(1),
                            exact_anchor,
                        });
                        row = row.saturating_add(decoration.lines.len());
                    }
                }
            }
        }

        Self {
            original_rows,
            placements,
            #[cfg(test)]
            total_rows: row,
        }
    }

    pub(crate) fn original_row(&self, block_line_idx: usize) -> Option<usize> {
        self.original_rows.get(block_line_idx).copied()
    }

    /// Translate an undecorated content-row offset. Rows after an insertion
    /// move down by the decoration height; rows at the insertion boundary stay
    /// before the card.
    pub(crate) fn translate_content_row(&self, original_row: usize) -> usize {
        original_row.saturating_add(
            self.placements
                .iter()
                .filter(|placement| placement.insertion_boundary <= original_row)
                .map(|placement| placement.decoration.lines.len())
                .sum::<usize>(),
        )
    }

    pub(crate) fn placements(&self) -> &[ResolvedDecoration<'a>] {
        &self.placements
    }

    #[cfg(test)]
    pub(crate) fn total_rows(&self) -> usize {
        self.total_rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrollback::render::ScratchBuffer;
    use crate::scrollback::types::BlockLine;
    use crate::scrollback::{RenderBlock, ScrollbackPane, ScrollbackState};
    use ratatui::buffer::Buffer;

    fn decoration(id: &str, line: usize, rows: usize) -> ScrollbackDecoration {
        ScrollbackDecoration {
            id: id.into(),
            revision: 1,
            entry_id: EntryId::new(1),
            after_source_line: line,
            lines: (0..rows)
                .map(|idx| DecorationLine::new(Line::raw(format!("card {idx}")), Color::Reset))
                .collect(),
            buttons: Vec::new(),
        }
    }

    #[test]
    fn exact_source_line_insertion_shifts_only_following_rows() {
        let mut output = BlockOutput::default();
        output.lines = vec![
            BlockLine::text("one").with_source_line(Some(1)),
            BlockLine::text("two-a").with_source_line(Some(2)),
            BlockLine::text("two-b").with_source_line(Some(2)),
            BlockLine::text("three").with_source_line(Some(3)),
        ];
        let decorations = vec![decoration("a", 2, 3)];
        let layout = EntryDecorationLayout::new(&output, &decorations);

        assert_eq!(layout.original_row(0), Some(0));
        assert_eq!(layout.original_row(2), Some(2));
        assert_eq!(layout.placements()[0].start_row, 3);
        assert!(layout.placements()[0].exact_anchor);
        assert_eq!(layout.original_row(3), Some(6));
        assert_eq!(layout.total_rows(), 7);
    }

    #[test]
    fn missing_source_line_falls_back_to_message_boundary() {
        let mut output = BlockOutput::default();
        output.lines = vec![BlockLine::text("collapsed")];
        let decorations = vec![decoration("a", 9, 2)];
        let layout = EntryDecorationLayout::new(&output, &decorations);

        assert_eq!(layout.placements()[0].start_row, 1);
        assert!(!layout.placements()[0].exact_anchor);
        assert_eq!(layout.total_rows(), 3);
    }

    #[test]
    fn rendered_decoration_reserves_rows_between_semantic_lines() {
        let mut state = ScrollbackState::new();
        let entry_id = state.push_block(RenderBlock::user_prompt("one\ntwo\nthree"));
        state.prepare_layout(40, 20);
        let base_height = state.get_cached_entry_height(0).unwrap();
        state.set_decorations(vec![ScrollbackDecoration {
            id: "card".into(),
            revision: 1,
            entry_id,
            after_source_line: 2,
            lines: vec![
                DecorationLine::new(Line::raw("ANNOTATION CARD"), Color::Blue)
                    .with_selectable_text(3, "ANNOTATION CARD"),
            ],
            buttons: Vec::new(),
        }]);
        state.prepare_layout(40, 20);
        assert_eq!(state.get_cached_entry_height(0), Some(base_height + 1));

        let area = Rect::new(0, 0, 40, 20);
        let mut buffer = Buffer::empty(area);
        let mut scratch = ScratchBuffer::new();
        let output =
            ScrollbackPane::new().render_with_scratch(area, &mut buffer, &state, &mut scratch);
        let rows: Vec<String> = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .filter_map(|x| buffer.cell((x, y)).map(|cell| cell.symbol()))
                    .collect()
            })
            .collect();
        let row_of = |needle: &str| rows.iter().position(|row| row.contains(needle)).unwrap();
        assert!(row_of("two") < row_of("ANNOTATION CARD"));
        assert!(row_of("ANNOTATION CARD") < row_of("three"));
        assert_eq!(output.decorations.len(), 1);
        assert!(output.decorations[0].exact_anchor);
        assert_eq!(output.decorations[0].selectable_lines.len(), 1);
        assert_eq!(
            output.decorations[0].selectable_lines[0].text,
            "ANNOTATION CARD"
        );
        assert_eq!(
            output.decorations[0].selectable_lines[0].screen_x,
            output.decorations[0].area.x + 3
        );
    }

    #[test]
    fn resize_keeps_decoration_after_the_same_semantic_source_line() {
        let mut state = ScrollbackState::new();
        let entry_id = state.push_block(RenderBlock::user_prompt(
            "first\nsecond logical line wraps several times\nthird",
        ));
        let card = ScrollbackDecoration {
            id: "card".into(),
            revision: 1,
            entry_id,
            after_source_line: 2,
            lines: vec![DecorationLine::new(Line::raw("card"), Color::Reset)],
            buttons: Vec::new(),
        };

        let mut starts = Vec::new();
        for width in [12u16, 48u16] {
            let output = state
                .get(0)
                .unwrap()
                .effective_output(width, state.appearance(), false, state.cwd())
                .output()
                .clone();
            let layout = EntryDecorationLayout::new(&output, std::slice::from_ref(&card));
            let placement = &layout.placements()[0];
            let last_second = output
                .lines
                .iter()
                .rposition(|line| line.source_line == Some(2))
                .unwrap();
            let third = output
                .lines
                .iter()
                .position(|line| line.source_line == Some(3))
                .unwrap();

            assert!(placement.exact_anchor);
            assert_eq!(placement.start_row, last_second + 1);
            assert_eq!(layout.original_row(third), Some(placement.start_row + 1));
            starts.push(placement.start_row);
        }
        assert_ne!(starts[0], starts[1], "fixture must actually rewrap");
    }
}
