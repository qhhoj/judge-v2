use super::text::{split_lines, tokens};
use super::{CheckError, CheckInput, CheckOutcome, Checker, read_file};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitOn {
    Lines,
    Whitespace,
}

/// Same lines (each a token list), or same tokens, in any order. Also serves `unordered`.
pub struct Sorted {
    pub split_on: SplitOn,
}

fn sorted_lines(data: &[u8]) -> Vec<Vec<&[u8]>> {
    let mut lines: Vec<_> =
        split_lines(data).into_iter().map(tokens).filter(|line| !line.is_empty()).collect();
    lines.sort_unstable();
    lines
}

fn sorted_tokens(data: &[u8]) -> Vec<&[u8]> {
    let mut all = tokens(data);
    all.sort_unstable();
    all
}

impl Checker for Sorted {
    fn check(&self, io: &CheckInput) -> Result<CheckOutcome, CheckError> {
        let (got, want) = (read_file(io.output)?, read_file(io.answer)?);
        let same = match self.split_on {
            SplitOn::Lines => sorted_lines(&got) == sorted_lines(&want),
            SplitOn::Whitespace => sorted_tokens(&got) == sorted_tokens(&want),
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
