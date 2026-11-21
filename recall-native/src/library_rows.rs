//! Library groups as list rows: a header and rows of tiles for a group that fills a row, and
//! small groups (fewer clips than a row holds) packed side by side under their own slim
//! labels, so a library of a few clips is not one header and one tile per line. Also the
//! "Group by" choice, collapsed groups, and the date a tile shows.
use super::LibraryState;
use crate::services::library::{ClipRecord, GroupBy};

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// Group index; `first` adds less top padding.
    Header { group: usize, first: bool },
    /// Clips of group `group`, a row's worth.
    Tiles { group: usize, items: Vec<usize> },
    /// Small groups in one row: (group index, clip count) in order, their clips in `items`.
    Packed { segments: Vec<(usize, usize)>, items: Vec<usize> },
}

impl Row {
    /// The clips this row shows; none for a header.
    pub fn items(&self) -> &[usize] {
        match self {
            Row::Tiles { items, .. } | Row::Packed { items, .. } => items,
            Row::Header { .. } => &[],
        }
    }
}

/// Rows for `groups` (each its clips and whether it is collapsed) at `columns` tiles a row.
/// `headers` false (no grouping): rows of tiles only.
pub(super) fn layout(groups: &[(&[usize], bool)], columns: usize, headers: bool) -> Vec<Row> {
    let columns = columns.max(1);
    let mut rows = Vec::new();
    if !headers {
        for (index, &(items, _)) in groups.iter().enumerate() {
            rows.extend(items.chunks(columns).map(|chunk| Row::Tiles { group: index, items: chunk.to_vec() }));
        }
        return rows;
    }
    let mut segments: Vec<(usize, usize)> = Vec::new();
    let mut packed: Vec<usize> = Vec::new();
    let flush = |rows: &mut Vec<Row>, segments: &mut Vec<(usize, usize)>, packed: &mut Vec<usize>| {
        if !segments.is_empty() {
            rows.push(Row::Packed { segments: std::mem::take(segments), items: std::mem::take(packed) });
        }
    };
    for (index, &(items, collapsed)) in groups.iter().enumerate() {
        if !collapsed && items.len() < columns {
            if packed.len() + items.len() > columns {
                flush(&mut rows, &mut segments, &mut packed);
            }
            segments.push((index, items.len()));
            packed.extend_from_slice(items);
            continue;
        }
        flush(&mut rows, &mut segments, &mut packed);
        rows.push(Row::Header { group: index, first: rows.is_empty() });
        if !collapsed {
            rows.extend(items.chunks(columns).map(|chunk| Row::Tiles { group: index, items: chunk.to_vec() }));
        }
    }
    flush(&mut rows, &mut segments, &mut packed);
    rows
}

impl LibraryState {
    pub(super) fn rebuild_rows(&mut self) {
        let groups: Vec<(&[usize], bool)> = self.groups.iter().map(|g| (g.items.as_slice(), self.folded(&g.key))).collect();
        let rows = layout(&groups, self.columns, self.group != GroupBy::None);
        if rows != self.rows {
            self.rows = rows;
            self.list.reset(self.rows.len());
        }
    }

    /// Clips on screen (or one scroll away): every group that is not collapsed, in order.
    pub fn shown(&self) -> impl Iterator<Item = usize> + '_ {
        self.groups.iter().filter(|g| !self.folded(&g.key)).flat_map(|g| g.items.iter().copied())
    }

    /// A search shows every hit: folded groups open while a query is typed.
    fn folded(&self, key: &str) -> bool {
        self.filter.query.is_empty() && self.collapsed.contains(key)
    }

    /// A new "Group by": regrouped from the clips in memory, remembered for the next start.
    pub fn set_group(&mut self, group: GroupBy) {
        if self.group == group {
            return;
        }
        self.group = group;
        self.refilter();
        self.list.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: gpui::px(0.) });
        if !crate::TESTING && !cfg!(test) {
            let mut prefs = crate::services::editor_prefs::EditorPrefs::load();
            prefs.library_group = group;
            if let Err(error) = prefs.save() {
                self.notice = Some(format!("Couldn't save the grouping: {error}"));
            }
        }
    }

    pub fn is_collapsed(&self, group: usize) -> bool {
        self.groups.get(group).is_some_and(|g| self.folded(&g.key))
    }

    /// Folds a group to its header or opens it again. Folded clips leave the selection, as
    /// clips a filter hides do.
    pub fn toggle_collapsed(&mut self, group: usize) {
        let Some(g) = self.groups.get(group) else { return };
        // Folding during a search would hide hits the search was typed to find.
        if !self.filter.query.is_empty() {
            return;
        }
        if !self.collapsed.remove(&g.key) {
            self.collapsed.insert(g.key.clone());
            for &i in &g.items {
                self.selection.remove(&self.records[i].path);
            }
            self.confirm_delete &= !self.selection.is_empty();
        }
        self.rebuild_rows();
    }

    /// The time beside a tile's title: "22:34" in a one-day group, else "17 Oct 22:34", or
    /// "17 Oct 2025" for another year.
    pub fn tile_when(&self, record: &ClipRecord, single_day: bool) -> String {
        let s = record.started;
        if single_day {
            return format!("{:02}:{:02}", s.hour, s.minute);
        }
        let month = crate::services::library::short_month(s.month);
        if s.year == self.today.year { format!("{} {month} {:02}:{:02}", s.day, s.hour, s.minute) } else { format!("{} {month} {}", s.day, s.year) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(sizes: &[(usize, bool)]) -> (Vec<Vec<usize>>, Vec<bool>) {
        let mut next = 0;
        let items = sizes
            .iter()
            .map(|&(n, _)| {
                let v: Vec<usize> = (next..next + n).collect();
                next += n;
                v
            })
            .collect();
        (items, sizes.iter().map(|&(_, c)| c).collect())
    }

    fn rows(sizes: &[(usize, bool)], columns: usize) -> Vec<Row> {
        let (items, collapsed) = groups(sizes);
        let input: Vec<(&[usize], bool)> = items.iter().map(Vec::as_slice).zip(collapsed).collect();
        layout(&input, columns, true)
    }

    #[test]
    fn no_grouping_is_tiles_only() {
        let items: Vec<usize> = (0..5).collect();
        assert_eq!(layout(&[(&items, false)], 4, false), vec![Row::Tiles { group: 0, items: vec![0, 1, 2, 3] }, Row::Tiles { group: 0, items: vec![4] }]);
    }

    #[test]
    fn a_few_clips_share_rows_instead_of_a_header_each() {
        // Today 1, Yesterday 1, Last week 2, October 1 at 4 a row: 2 rows, not 4 headers + 4 rows.
        let r = rows(&[(1, false), (1, false), (2, false), (1, false)], 4);
        assert_eq!(r, vec![
            Row::Packed { segments: vec![(0, 1), (1, 1), (2, 2)], items: vec![0, 1, 2, 3] },
            Row::Packed { segments: vec![(3, 1)], items: vec![4] },
        ]);
    }

    #[test]
    fn a_full_group_keeps_its_header_and_rows() {
        let r = rows(&[(2, false), (5, false), (1, false)], 4);
        assert_eq!(r, vec![
            Row::Packed { segments: vec![(0, 2)], items: vec![0, 1] },
            Row::Header { group: 1, first: false },
            Row::Tiles { group: 1, items: vec![2, 3, 4, 5] },
            Row::Tiles { group: 1, items: vec![6] },
            Row::Packed { segments: vec![(2, 1)], items: vec![7] },
        ]);
        assert_eq!(rows(&[(4, false)], 4)[0], Row::Header { group: 0, first: true });
    }

    #[test]
    fn a_collapsed_group_is_its_header_only() {
        let r = rows(&[(6, true), (1, true), (1, false)], 4);
        assert_eq!(r, vec![
            Row::Header { group: 0, first: true },
            Row::Header { group: 1, first: false },
            Row::Packed { segments: vec![(2, 1)], items: vec![7] },
        ]);
        assert!(rows(&[], 4).is_empty());
    }

    #[test]
    fn every_clip_appears_once_in_order() {
        let sizes = [(3, false), (1, false), (9, false), (2, false), (2, false), (1, false)];
        for columns in 2..=8 {
            let flat: Vec<usize> = rows(&sizes, columns).iter().flat_map(|r| r.items().to_vec()).collect();
            assert_eq!(flat, (0..18).collect::<Vec<_>>(), "{columns} columns");
            assert!(rows(&sizes, columns).iter().all(|r| r.items().len() <= columns));
        }
    }
}
