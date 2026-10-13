//! Which change of a batch broke CI. Each change of a batch is one commit on the change
//! before it, so prefix i is the tree after the changes 0..=i. When the tip (prefix n-1)
//! fails and main (the base) passes, some change i has a passing tree under it (prefix i-1,
//! or main) and a failing tree with it (prefix i). That change is the culprit.
//!
//! The search halves the range between the last prefix known to pass and the first prefix
//! known to fail, so it checks at most ceil(log2(n)) prefixes. The scan it replaces checked
//! the prefixes in order, up to n-1 of them. When a change only adds an error (the usual
//! case), every prefix after it fails too, and both find the same change: the first that
//! fails. When a later change fixes an earlier error, the search can find a later culprit.
//! That change also turned a passing tree into a failing one, and the changes before it
//! land on a tree that passed.

/// The state of one search: `lo` passes (-1 is the base), `hi` fails.
#[derive(Clone, Debug)]
pub struct Search {
    lo: isize,
    hi: usize,
}

impl Search {
    /// A batch of `n` changes (n >= 1) whose base passes and whose tip fails.
    pub fn new(n: usize) -> Self {
        assert!(n >= 1, "a batch has at least one change");
        Search { lo: -1, hi: n - 1 }
    }

    /// The next prefix to check, or None when the culprit is known.
    pub fn next(&self) -> Option<usize> {
        let span = self.hi as isize - self.lo;
        (span > 1).then(|| (self.lo + span / 2) as usize)
    }

    /// The result of the check of prefix `i` (a value from `next`).
    pub fn report(&mut self, i: usize, passed: bool) {
        debug_assert!(i as isize > self.lo && i < self.hi, "prefix {i} is not between {} and {}", self.lo, self.hi);
        if passed {
            self.lo = i as isize;
        } else {
            self.hi = i;
        }
    }

    /// The change that broke CI: its prefix fails, and the prefix before it passes.
    pub fn culprit(&self) -> usize {
        debug_assert!(self.next().is_none(), "the search is not done");
        self.hi
    }
}

#[cfg(test)]
mod tests {
    use super::Search;

    /// Runs a search on a batch where `fails(i)` says whether prefix i fails. The tip must
    /// fail. Returns the culprit and the prefixes checked.
    fn run(n: usize, fails: impl Fn(usize) -> bool) -> (usize, Vec<usize>) {
        assert!(fails(n - 1));
        let mut s = Search::new(n);
        let mut checked = vec![];
        while let Some(i) = s.next() {
            assert!(!checked.contains(&i), "prefix {i} checked twice");
            checked.push(i);
            s.report(i, !fails(i));
        }
        (s.culprit(), checked)
    }

    /// The scan of the old code: the first prefix that fails, or the last change.
    fn linear(n: usize, fails: impl Fn(usize) -> bool) -> usize {
        (0..n - 1).find(|&i| fails(i)).unwrap_or(n - 1)
    }

    fn log2_ceil(n: usize) -> usize {
        (usize::BITS - (n.max(1) - 1).leading_zeros()) as usize
    }

    #[test]
    fn one_change() {
        assert_eq!(run(1, |_| true), (0, vec![]));
    }

    #[test]
    fn two_changes() {
        assert_eq!(run(2, |_| true), (0, vec![0]));
        assert_eq!(run(2, |i| i == 1), (1, vec![0]));
    }

    #[test]
    fn first_and_last() {
        for n in 1..=64 {
            assert_eq!(run(n, |_| true).0, 0, "n={n}: the first change breaks it");
            assert_eq!(run(n, |i| i == n - 1).0, n - 1, "n={n}: the last change breaks it");
        }
    }

    /// One bad change (or several): every prefix from the first bad change on fails. The
    /// search finds the same change as the scan, with at most ceil(log2(n)) checks.
    #[test]
    fn same_as_the_scan_when_errors_stay() {
        for n in 1..=64 {
            for first in 0..n {
                let fails = |i: usize| i >= first;
                let (c, checked) = run(n, fails);
                assert_eq!(c, linear(n, fails), "n={n} first={first}");
                assert!(checked.len() <= log2_ceil(n), "n={n} first={first}: {} checks", checked.len());
            }
        }
    }

    /// A later change fixes an earlier error, and another breaks it again: whatever the
    /// pattern, the culprit's prefix fails, and the tree under it passes (main, or a
    /// prefix that was checked and passed).
    #[test]
    fn culprit_has_a_passing_tree_under_it() {
        for n in 1..=12usize {
            for mask in 0..(1u32 << n) {
                let fails = |i: usize| mask & (1 << i) != 0;
                if !fails(n - 1) {
                    continue;
                }
                let mut s = Search::new(n);
                let mut passed = vec![];
                while let Some(i) = s.next() {
                    let ok = !fails(i);
                    if ok {
                        passed.push(i);
                    }
                    s.report(i, ok);
                }
                let c = s.culprit();
                assert!(fails(c), "n={n} mask={mask:b}: culprit {c} passes");
                assert!(c == 0 || passed.contains(&(c - 1)), "n={n} mask={mask:b}: nothing checked passes under {c}");
            }
        }
    }

    /// The batch sizes of the measurements (cf/scale/ci.md): checks for a culprit.
    #[test]
    fn checks_at_the_batch_caps() {
        assert_eq!(run(16, |i| i >= 5).1.len(), 4);
        assert_eq!(run(64, |i| i >= 20).1.len(), 6);
        assert_eq!(run(64, |_| true).1.len(), 6);
    }
}
