//! Scroll arithmetic independent of Slint and text rendering.
use tg_core::MessageId;
/// At most 300 (message ID, logical height) pairs, no layouts or pixels off screen.
#[derive(Default)]
pub(crate) struct Geometry {
    pub rows: Vec<(MessageId, f32)>,
}
impl Geometry {
    pub fn sync(&mut self, ids: impl IntoIterator<Item = MessageId>) {
        self.rows = ids
            .into_iter()
            .take(300)
            .map(|id| {
                (
                    id,
                    self.rows
                        .iter()
                        .find(|row| row.0 == id)
                        .map_or(80.0, |row| row.1),
                )
            })
            .collect();
    }
    pub fn total(&self) -> f32 {
        self.rows.iter().map(|row| row.1).sum()
    }
    pub fn anchor(&self, top: f32) -> Option<(MessageId, f32)> {
        let mut y = 0.0;
        for &(id, height) in &self.rows {
            if y + height > top {
                return Some((id, (top - y).max(0.0)));
            }
            y += height;
        }
        self.rows.last().map(|row| (row.0, 0.0))
    }
    pub fn restore(&self, anchor: Option<(MessageId, f32)>) -> f32 {
        let Some((id, offset)) = anchor else {
            return 0.0;
        };
        let mut y = 0.0;
        for &(key, height) in &self.rows {
            if key == id {
                return y + offset.min((height - 1.0).max(0.0));
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
}
