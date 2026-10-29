/// A selection in char offsets. `head` is where the caret is; `anchor` is the other end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    /// Preferred column for vertical movement.
    pub goal_col: Option<usize>,
}

impl Selection {
    pub fn caret(pos: usize) -> Self {
        Self { anchor: pos, head: pos, goal_col: None }
    }
    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head, goal_col: None }
    }
    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }
    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// Sort selections and merge overlapping/touching-caret ones.
pub fn normalize(sels: &mut Vec<Selection>) {
    sels.sort_by_key(|s| (s.start(), s.end()));
    let mut out: Vec<Selection> = Vec::with_capacity(sels.len());
    for s in sels.drain(..) {
        if let Some(last) = out.last_mut() {
            let same = s.start() == last.start() && s.end() == last.end();
            if s.start() < last.end() || same {
                let start = last.start().min(s.start());
                let end = last.end().max(s.end());
                let forward = last.head >= last.anchor;
                *last = if forward { Selection::new(start, end) } else { Selection::new(end, start) };
                continue;
            }
        }
        out.push(s);
    }
    *sels = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_overlapping() {
        let mut v = vec![Selection::new(5, 10), Selection::new(0, 6), Selection::caret(20)];
        normalize(&mut v);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].start(), v[0].end()), (0, 10));
    }

    #[test]
    fn merges_duplicate_carets() {
        let mut v = vec![Selection::caret(3), Selection::caret(3)];
        normalize(&mut v);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn keeps_adjacent_ranges() {
        let mut v = vec![Selection::new(0, 3), Selection::new(3, 6)];
        normalize(&mut v);
        assert_eq!(v.len(), 2);
    }
}
