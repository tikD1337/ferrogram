//! Scroll arithmetic independent of Slint and text rendering.
use tg_core::MessageId;
/// At most 300 (message ID, logical height, measured) rows, no layouts or pixels off screen.
/// Rows that were never drawn carry an estimated height until they are measured.
#[derive(Default)]
pub(crate) struct Geometry {
    pub rows: Vec<(MessageId, f32, bool)>,
}
impl Geometry {
    pub fn sync(&mut self, ids: impl IntoIterator<Item = MessageId>) {
        self.rows = ids
            .into_iter()
            .take(300)
            .map(|id| {
                self.rows
                    .iter()
                    .find(|row| row.0 == id)
                    .map_or((id, 80.0, false), |row| (id, row.1, row.2))
            })
            .collect();
    }
    /// Records the real height of a drawn row.
    pub fn measure(&mut self, index: usize, height: f32) {
        if let Some(row) = self.rows.get_mut(index) {
            row.1 = height;
            row.2 = true;
        }
    }
    pub fn total(&self) -> f32 {
        self.rows.iter().map(|row| row.1).sum()
    }
    /// The first measured row at or below the top edge, with the top edge's offset from that
    /// row's start (negative when the row starts lower). Anchoring to a measured row keeps what
    /// the user sees in place when a row entering at the top is drawn and changes its height.
    /// Without any measured row, the row under the top edge is used.
    pub fn anchor(&self, top: f32) -> Option<(MessageId, f32)> {
        let mut y = 0.0;
        let mut first = None;
        for &(id, height, measured) in &self.rows {
            if y + height > top {
                if measured {
                    return Some((id, top - y));
                }
                first.get_or_insert((id, (top - y).max(0.0)));
            }
            y += height;
        }
        first.or_else(|| self.rows.last().map(|row| (row.0, 0.0)))
    }
    pub fn restore(&self, anchor: Option<(MessageId, f32)>) -> f32 {
        let Some((id, offset)) = anchor else {
            return 0.0;
        };
        let mut y = 0.0;
        for &(key, height, _) in &self.rows {
            if key == id {
                return (y + offset.min((height - 1.0).max(0.0))).max(0.0);
            }
            y += height;
        }
        0.0
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn variable_height_anchor_survives_prepend_and_both_end_eviction() {
        let mut g = Geometry::default();
        g.sync([MessageId(1048576), MessageId(3145728), MessageId(7340032)]);
        g.rows[0].1 = 40.0;
        g.rows[1].1 = 170.0;
        g.rows[2].1 = 60.0;
        let anchor = g.anchor(93.0).unwrap();
        assert_eq!(anchor, (MessageId(3145728), 53.0));
        g.sync([MessageId(524288), MessageId(1048576), MessageId(3145728)]);
        assert_eq!(g.restore(Some(anchor)), 173.0);
        g.sync([MessageId(3145728), MessageId(7340032)]);
        assert_eq!(g.restore(Some(anchor)), 53.0);
    }
    #[test]
    fn exact_boundary_and_empty_window() {
        let mut g = Geometry::default();
        assert_eq!(g.anchor(0.0), None);
        assert_eq!(g.restore(None), 0.0);
        g.sync([MessageId(1048576), MessageId(2097152)]);
        g.rows[0].1 = 40.0;
        assert_eq!(g.anchor(40.0), Some((MessageId(2097152), 0.0)));
    }
    #[test]
    fn a_row_measured_while_entering_at_the_top_does_not_move_the_rows_below() {
        // Scrolling up: an unmeasured row (estimate 80) peeks in at the top above a measured one.
        let mut g = Geometry::default();
        g.sync([MessageId(1048576), MessageId(2097152)]);
        g.measure(1, 200.0);
        let top = 40.0;
        let below = |g: &Geometry, top: f32| g.rows[0].1 - top; // screen y of the measured row
        let before = below(&g, top);
        let anchor = g.anchor(top);
        // The entering row is rendered and turns out to be much taller than estimated.
        g.measure(0, 300.0);
        let restored = g.restore(anchor);
        assert_eq!(below(&g, restored), before);
    }
}

#[cfg(test)]
mod deletion_tests {
    use super::*;
    #[test]
    fn deleting_rows_above_viewport_preserves_message_and_offset() {
        let mut g = Geometry::default();
        g.sync((1..=5).map(|n| MessageId(n * 1048576)));
        let anchor = g.anchor(190.0);
        g.sync((3..=5).map(|n| MessageId(n * 1048576)));
        assert_eq!(g.anchor(g.restore(anchor)), anchor);
    }
}
