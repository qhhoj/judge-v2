use super::text::{lines, tokens};
use super::{CheckError, CheckInput, CheckOutcome, Checker, read_file};
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitOn {
    Lines,
    Whitespace,
}

/// Same lines (each a token list), or same tokens, in any order. Also serves `unordered`.
pub struct Sorted {
    pub split_on: SplitOn,
}

/// The lines that hold a token, each kept whole: it is compared by its tokens.
fn token_lines(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    lines(data).filter(|line| tokens(line).next().is_some())
}

/// `units` sorted with `order`, in a vector of exactly `count`, the number the caller counted beforehand.
fn sorted_units<'a>(
    units: impl Iterator<Item = &'a [u8]>,
    count: usize,
    order: impl Fn(&&[u8], &&[u8]) -> Ordering,
) -> Vec<&'a [u8]> {
    let mut all = Vec::with_capacity(count);
    all.extend(units);
    all.sort_unstable_by(order);
    all
}

/// Same lines, as token lists, in any order; lines without a token are ignored. The counts are compared first,
/// so an output with more lines than the answer is never collected.
fn same_lines(got: &[u8], want: &[u8]) -> bool {
    let count = token_lines(want).count();
    if token_lines(got).count() != count {
        return false;
    }
    let by_tokens = |a: &&[u8], b: &&[u8]| tokens(a).cmp(tokens(b));
    let got = sorted_units(token_lines(got), count, by_tokens);
    let want = sorted_units(token_lines(want), count, by_tokens);
    got.iter().zip(&want).all(|(g, w)| tokens(g).eq(tokens(w)))
}

/// Same tokens in any order; the counts are compared first, as for lines.
fn same_tokens(got: &[u8], want: &[u8]) -> bool {
    let count = tokens(want).count();
    if tokens(got).count() != count {
        return false;
    }
    let by_bytes = |a: &&[u8], b: &&[u8]| a.cmp(b);
    sorted_units(tokens(got), count, by_bytes) == sorted_units(tokens(want), count, by_bytes)
}

impl Checker for Sorted {
    fn check(&self, io: &CheckInput) -> Result<CheckOutcome, CheckError> {
        let (got, want) = (read_file(io.output)?, read_file(io.answer)?);
        let same = match self.split_on {
            SplitOn::Lines => same_lines(&got, &want),
            SplitOn::Whitespace => same_tokens(&got, &want),
        };
        Ok(if same { CheckOutcome::pass() } else { CheckOutcome::fail("") })
    }
}

#[cfg(test)]
mod tests {
    use super::{Sorted, SplitOn};
    use crate::checkers::testing::run;

    #[test]
    fn lines_mode_reorders_lines_but_not_tokens_within_a_line() {
        let c = Sorted { split_on: SplitOn::Lines };
        assert!(run(&c, b"2 1\n3\n", b"3\n2   1\n\n").passed);
        assert!(!run(&c, b"1 2\n3\n", b"2 1\n3\n").passed, "token order inside a line matters");
        assert!(!run(&c, b"1\n1\n", b"1\n").passed, "line counts must match");
        assert!(!run(&c, b"1 2\n", b"1\n2\n").passed, "line boundaries matter");
    }

    #[test]
    fn vertical_tab_separates_tokens_in_both_modes() {
        for split_on in [SplitOn::Lines, SplitOn::Whitespace] {
            assert!(run(&Sorted { split_on }, b"1\x0b2\n", b"1 2\n").passed, "{split_on:?}");
        }
    }

    #[test]
    fn whitespace_mode_compares_the_token_multiset() {
        let c = Sorted { split_on: SplitOn::Whitespace };
        assert!(run(&c, b"3 1\n2", b"1 2 3\n").passed);
        assert!(!run(&c, b"1 1 2", b"1 2 2").passed, "multiplicity matters");
        assert!(!run(&c, b"1 2", b"1 2 3").passed);
    }
}
