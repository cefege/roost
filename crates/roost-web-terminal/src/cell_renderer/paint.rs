//! Painting the live grid: the viewport row diff, the cursor, the grid width and
//! the find highlights. The diff is O(dirty rows): each row element carries the
//! hash of everything the painter sets, so a frame repainting the same cells
//! touches no DOM. Ports `renderDelta`, `renderViewportRepair`, `updateCursor`,
//! `setGridWidth`, `setFindHighlights` and `_repaintScrollbackRow` of
//! `apps/web/src/renderer/cellRenderer.ts`.

use std::collections::BTreeMap;

use crate::cell_renderer::CellGridRenderer;
use crate::cell_renderer::history_page::to_row_index;
use crate::cell_renderer::scrollback::BLOCK_CLASS;
use crate::cell_renderer_dom::{
    DomResult, is_placed_in, paint_cell_grid_width, sync_alternate_screen,
};
use crate::cell_row::dom::render_row;
use crate::cell_row::{FindHit, row_hash};
use crate::render_element::RenderElement;
use roost_protocol::cell::CellRow;

impl<E: RenderElement> CellGridRenderer<E> {
    /// The find hits for one row, and the active match's column on that row.
    ///
    /// Find hits are keyed in the worker's ABSOLUTE row space, so a viewport row
    /// at `i` is looked up at `scrollback_total + i` and a history row is looked
    /// up at its own index.
    fn hits_for(&self, absolute_row: u32) -> (Option<&[FindHit]>, Option<u32>) {
        let hits = self.find_hits.get(&absolute_row).map(Vec::as_slice);
        let active_col = self
            .active_hit
            .filter(|(row, _)| *row == absolute_row)
            .map(|(_, column)| column);
        (hits, active_col)
    }

    /// Repaint every viewport row whose hash changed, adding or removing row
    /// elements until the list is exactly the grid's height.
    pub(crate) fn render_viewport_repair(&mut self) -> DomResult<()> {
        let Some(frame) = self.frame.clone() else {
            return Ok(());
        };
        let viewport_base = to_row_index(frame.scrollback_total);
        for (index, row) in frame.viewport_rows.iter().enumerate() {
            let (hits, active_col) = self.hits_for(viewport_base + index as u32);
            let hash = row_hash(row, hits, active_col);
            if index < self.row_elements.len() {
                if self.row_hashes[index] == hash {
                    continue;
                }
                let element = render_row(
                    row,
                    &self.container,
                    hits,
                    active_col,
                    &mut self.styles.borrow_mut(),
                )?;
                self.row_elements[index].replace_with(&element);
                self.row_elements[index] = element;
                self.row_hashes[index] = hash;
            } else {
                let element = render_row(
                    row,
                    &self.container,
                    hits,
                    active_col,
                    &mut self.styles.borrow_mut(),
                )?;
                self.insert_viewport_row(&element);
                self.row_elements.push(element);
                self.row_hashes.push(hash);
            }
        }
        while self.row_elements.len() > frame.viewport_rows.len() {
            if let Some(element) = self.row_elements.pop() {
                element.remove();
            }
            self.row_hashes.pop();
        }
        self.attach_viewport_overlays();
        Ok(())
    }
    /// Rebuild every live row after the text-run shaping preference changes.
    pub fn refresh_cell_text_runs(&mut self) -> bool {
        for element in self.row_elements.drain(..) {
            element.remove();
        }
        self.row_hashes.clear();
        let history_refreshed = self.repaint_scrollback_rows();
        self.render_viewport_repair().is_ok() && history_refreshed
    }

    /// Patch only the rows a delta changed, after discarding the rows a proven
    /// viewport shift rotated away.
    pub(crate) fn render_delta(&mut self, dirty_rows: &[CellRow], scrolled: u32) -> DomResult<()> {
        let Some(frame) = self.frame.clone() else {
            return Ok(());
        };
        let viewport_base = to_row_index(frame.scrollback_total);
        let shifted = usize::try_from(scrolled.min(self.row_elements.len() as u32)).unwrap_or(0);
        for element in self.row_elements.iter().take(shifted) {
            element.remove();
        }
        if shifted > 0 {
            self.row_elements.drain(..shifted);
            self.row_hashes.drain(..shifted);
        }
        for row in dirty_rows {
            let index = row.index as usize;
            if index >= self.row_elements.len() {
                continue;
            }
            let (hits, active_col) = self.hits_for(viewport_base + index as u32);
            let hash = row_hash(row, hits, active_col);
            if self.row_hashes[index] == hash {
                continue;
            }
            let element = render_row(
                row,
                &self.container,
                hits,
                active_col,
                &mut self.styles.borrow_mut(),
            )?;
            self.row_elements[index].replace_with(&element);
            self.row_elements[index] = element;
            self.row_hashes[index] = hash;
        }
        while self.row_elements.len() < frame.viewport_rows.len() {
            let index = self.row_elements.len();
            let row = &frame.viewport_rows[index];
            let (hits, active_col) = self.hits_for(viewport_base + index as u32);
            let hash = row_hash(row, hits, active_col);
            let element = render_row(
                row,
                &self.container,
                hits,
                active_col,
                &mut self.styles.borrow_mut(),
            )?;
            self.insert_viewport_row(&element);
            self.row_elements.push(element);
            self.row_hashes.push(hash);
        }
        self.attach_viewport_overlays();
        Ok(())
    }

    /// Append one row element BELOW the overlays, so a row never lands between
    /// the cursor and the rows it points at.
    fn insert_viewport_row(&self, element: &E) {
        let anchor = is_placed_in(&self.cursor, &self.viewport).then_some(&self.cursor);
        self.viewport.insert_before(element, anchor);
    }

    /// Keep the cursor and the ghost host inside the viewport, then place the
    /// cursor. The overlays must be the LAST children so a row appended above
    /// them does not cover the cursor.
    fn attach_viewport_overlays(&mut self) {
        if !is_placed_in(&self.cursor, &self.viewport) {
            self.viewport.append_child(&self.cursor);
        }
        if !is_placed_in(&self.ghosts, &self.viewport) {
            self.viewport.append_child(&self.ghosts);
        }
        self.update_cursor();
    }

    /// Publish a leading cursor column, or withdraw it. A prediction is drawn
    /// because the local echo arrives before the frame that confirms it.
    pub fn set_predicted_cursor(&mut self, column: Option<u32>) {
        if self.predicted_col == column {
            return;
        }
        self.predicted_col = column;
        self.update_cursor();
        self.mark_reconciled_if_current();
    }

    /// Place the cursor element from the accepted frame.
    pub(crate) fn update_cursor(&mut self) {
        let Some(frame) = self.frame.as_ref() else {
            return;
        };
        let visible = frame.cursor_visible;
        if self.painted_cursor_visible != Some(visible) {
            self.painted_cursor_visible = Some(visible);
            self.cursor
                .set_attribute("data-visible", if visible { "true" } else { "false" });
            self.cursor
                .set_style("display", if visible { "block" } else { "none" });
        }
        if !visible {
            return;
        }
        if self.painted_cursor_row != i64::from(frame.cursor_row) {
            self.painted_cursor_row = i64::from(frame.cursor_row);
            self.cursor
                .set_attribute("data-row", &frame.cursor_row.to_string());
            self.cursor
                .set_style("top", &format!("{}lh", frame.cursor_row));
        }
        let column = self.predicted_col.unwrap_or(frame.cursor_col);
        if self.painted_cursor_col != i64::from(column) {
            self.painted_cursor_col = i64::from(column);
            self.cursor
                .set_attribute("data-column", &column.to_string());
            self.cursor.set_style("left", &format!("{column}ch"));
        }
    }

    /// Publish the grid's column count so the CSS paints exactly `cols` wide.
    pub(crate) fn set_grid_width(&mut self) {
        self.painted_cols =
            paint_cell_grid_width(&self.container, self.frame.as_deref(), self.painted_cols);
    }

    /// Toggle the alternate-screen class for the accepted frame.
    pub(crate) fn sync_alt_screen(&mut self) {
        self.painted_alt_screen = Some(sync_alternate_screen(
            &self.container,
            self.frame.as_deref(),
            self.painted_alt_screen,
        ));
    }

    /// Republish the find highlights and repaint exactly the rows whose
    /// highlighting changed.
    pub fn set_find_highlights(
        &mut self,
        hits: BTreeMap<u32, Vec<FindHit>>,
        active: Option<(u32, u32)>,
    ) -> bool {
        let mut affected: Vec<u32> = self.find_hits.keys().copied().collect();
        affected.extend(hits.keys().copied());
        if let Some((row, _)) = self.active_hit {
            affected.push(row);
        }
        if let Some((row, _)) = active {
            affected.push(row);
        }
        self.find_hits = hits;
        self.active_hit = active;
        if self.frame.is_none() {
            return false;
        }
        let total = self.frame_total();
        for row in affected {
            if u64::from(row) < total {
                self.repaint_scrollback_row(row);
            }
        }
        self.render_viewport_repair().is_ok()
    }

    /// Repaint one history row in place, so a highlight change costs one row
    /// element rather than the whole sheet.
    fn repaint_scrollback_row(&mut self, absolute_index: u32) {
        let Some(row) = self.painted.row_at(absolute_index).cloned() else {
            return;
        };
        let wanted = absolute_index.to_string();
        for block_index in 0..self.child_count() {
            let Some(block) = self.child_at(block_index) else {
                continue;
            };
            if block.class_name() != BLOCK_CLASS {
                continue;
            }
            for child_index in 0..block.child_count() {
                let Some(child) = block.child_at(child_index) else {
                    continue;
                };
                if child.attribute("data-row-index").as_deref() != Some(wanted.as_str()) {
                    continue;
                }
                if let Ok(replacement) = self.render_scrollback_row(&row) {
                    child.replace_with(&replacement);
                }
                return;
            }
        }
    }
    fn repaint_scrollback_rows(&self) -> bool {
        let mut repainted = true;
        for block_index in 0..self.child_count() {
            let Some(block) = self.child_at(block_index) else {
                continue;
            };
            if block.class_name() != BLOCK_CLASS {
                continue;
            }
            for child_index in 0..block.child_count() {
                let Some(child) = block.child_at(child_index) else {
                    continue;
                };
                let Some(index) = child
                    .attribute("data-row-index")
                    .and_then(|value| value.parse::<u32>().ok())
                else {
                    continue;
                };
                let Some(row) = self.painted.row_at(index) else {
                    continue;
                };
                match self.render_scrollback_row(row) {
                    Ok(replacement) => child.replace_with(&replacement),
                    Err(_) => repainted = false,
                }
            }
        }
        repainted
    }

    /// Paint one immutable history row, stamping the absolute index the
    /// backfill and the find overlay both address it by.
    pub(crate) fn render_scrollback_row(&self, row: &CellRow) -> DomResult<E> {
        let (hits, active_col) = self.hits_for(row.index);
        let element = render_row(
            row,
            &self.container,
            hits,
            active_col,
            &mut self.styles.borrow_mut(),
        )?;
        element.set_attribute("data-row-index", &row.index.to_string());
        Ok(element)
    }
}
