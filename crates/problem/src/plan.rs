//! Turns init.yml into the ordered list of cases one run grades.

use crate::init::{CaseEntry, CheckerSpec, InitError, InitYaml};

pub const MAX_SUBMISSION_CASES: usize = 10000;
/// Largest `output_limit_length` an init.yml may set. The grader reads up to this much program output into memory.
pub const MAX_OUTPUT_LIMIT: u64 = 64 << 20;

#[derive(Debug, Clone)]
pub struct PlanOptions {
    pub pretests_only: bool,
    pub default_output_limit: u64,
    pub default_output_prefix: u64,
}

#[derive(Debug, Clone)]
pub struct PlannedCase {
    pub position: u32,
    pub input: String,
    pub output: String,
    pub total: f64,
    pub batch: Option<u32>,
    pub checker: CheckerSpec,
    pub output_limit: u64,
    pub output_prefix: u64,
}

#[derive(Debug, Clone)]
pub struct CasePlan {
    pub pretested: bool,
    pub cases: Vec<PlannedCase>,
}

/// What a case inherits from its batch or the problem.
#[derive(Clone)]
struct Inherited {
    checker: CheckerSpec,
    output_limit: u64,
    output_prefix: u64,
}

impl Inherited {
    fn apply(&self, entry: &CaseEntry) -> Inherited {
        Inherited {
            checker: entry.checker.clone().unwrap_or_else(|| self.checker.clone()),
            output_limit: entry.output_limit_length.unwrap_or(self.output_limit),
            output_prefix: entry.output_prefix_length.unwrap_or(self.output_prefix),
        }
    }
}

fn invalid(message: impl Into<String>) -> InitError {
    InitError::Invalid(message.into())
}

pub fn plan_cases(init: &InitYaml, options: &PlanOptions) -> Result<CasePlan, InitError> {
    let pretested = options.pretests_only && !init.pretest_test_cases.is_empty();
    let problem = Inherited {
        checker: init.checker.clone().unwrap_or_else(|| CheckerSpec::Name("standard".into())),
        output_limit: init.output_limit_length.unwrap_or(options.default_output_limit),
        output_prefix: init.output_prefix_length.unwrap_or(options.default_output_prefix),
    };
    let ceiling = MAX_OUTPUT_LIMIT.max(options.default_output_limit);
    let mut builder = Builder { cases: Vec::new(), batches: 0, ceiling };
    if pretested {
        builder.add_entries(&init.pretest_test_cases, &problem, Pass::AsWritten)?;
    } else {
        // DMOJ's full run (pinned by the backend parity run): the pretests first, then the tests.
        builder.add_entries(&init.pretest_test_cases, &problem, Pass::FullRunPretest)?;
        builder.add_entries(&init.test_cases, &problem, Pass::AsWritten)?;
    }
    Ok(CasePlan { pretested, cases: builder.cases })
}

/// How an entry's points count in this run.
#[derive(Clone, Copy, PartialEq)]
enum Pass {
    AsWritten,
    /// A pretest in a full run: a plain pretest case is worth 0, so its failure ends the run (`ShortCircuit`);
    /// a pretest batch keeps its points (DMOJ's observed behaviour).
    FullRunPretest,
}

/// What a case is worth and which batch it belongs to.
#[derive(Clone, Copy)]
struct Scoring {
    total: f64,
    batch: Option<u32>,
}

struct Builder {
    cases: Vec<PlannedCase>,
    batches: u32,
    /// Highest output limit a case may resolve to.
    ceiling: u64,
}

impl Builder {
    fn add_entries(
        &mut self,
        entries: &[CaseEntry],
        problem: &Inherited,
        pass: Pass,
    ) -> Result<(), InitError> {
        entries.iter().try_for_each(|entry| self.add_entry(entry, problem, pass))
    }

    fn add_entry(&mut self, entry: &CaseEntry, problem: &Inherited, pass: Pass) -> Result<(), InitError> {
        let points = entry.points.ok_or_else(|| invalid("a top-level case or batch has no points"))?;
        let Some(batched) = &entry.batched else {
            let total = if pass == Pass::FullRunPretest { 0.0 } else { points };
            return self.push(entry, &problem.apply(entry), Scoring { total, batch: None });
        };
        if batched.is_empty() {
            return Err(invalid("empty batch"));
        }
        self.batches += 1;
        let batch_options = problem.apply(entry);
        for case in batched {
            self.push(
                case,
                &batch_options.apply(case),
                Scoring { total: points, batch: Some(self.batches) },
            )?;
        }
        Ok(())
    }

    fn push(&mut self, entry: &CaseEntry, options: &Inherited, scoring: Scoring) -> Result<(), InitError> {
        if self.cases.len() >= MAX_SUBMISSION_CASES {
            return Err(invalid(format!("more than {MAX_SUBMISSION_CASES} cases")));
        }
        if options.output_limit > self.ceiling {
            return Err(invalid(format!("output_limit_length above {} bytes", self.ceiling)));
        }
        let (Some(input), Some(output)) = (&entry.input, &entry.out) else {
            return Err(invalid("a case needs both `in` and `out`"));
        };
        self.cases.push(PlannedCase {
            position: self.cases.len() as u32 + 1,
            input: input.clone(),
            output: output.clone(),
            total: scoring.total,
            batch: scoring.batch,
            checker: options.checker.clone(),
            output_limit: options.output_limit,
            output_prefix: options.output_prefix,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::parse_init;

    fn options(pretests_only: bool) -> PlanOptions {
        PlanOptions { pretests_only, default_output_limit: 25_165_824, default_output_prefix: 64 }
    }

    fn plan(yaml: &str, pretests_only: bool) -> Result<CasePlan, InitError> {
        plan_cases(&parse_init(yaml).unwrap(), &options(pretests_only))
    }

    #[test]
    fn pretest_run_uses_only_pretests_with_their_points() {
        let yaml = "pretest_test_cases:\n- {in: p, out: q, points: 1}\ntest_cases:\n- {in: a, out: b, points: 2}\n- {in: c, out: d, points: 3}\n";
        let pre = plan(yaml, true).unwrap();
        assert!(pre.pretested);
        let shape: Vec<_> = pre.cases.iter().map(|c| (c.input.as_str(), c.total)).collect();
        assert_eq!(shape, [("p", 1.0)]);
    }

    #[test]
    fn full_run_takes_the_pretests_first_plain_ones_worth_nothing_then_the_tests() {
        let yaml = "pretest_test_cases:\n- {in: p, out: q, points: 10}\n- points: 30\n  batched:\n  - {in: r, out: s}\n  - {in: t, out: u}\n\
                    test_cases:\n- {in: a, out: b, points: 30}\n- points: 5\n  batched:\n  - {in: c, out: d}\n";
        let full = plan(yaml, false).unwrap();
        assert!(!full.pretested);
        let shape: Vec<_> =
            full.cases.iter().map(|c| (c.position, c.input.as_str(), c.total, c.batch)).collect();
        assert_eq!(
            shape,
            [
                (1, "p", 0.0, None),
                (2, "r", 30.0, Some(1)),
                (3, "t", 30.0, Some(1)),
                (4, "a", 30.0, None),
                (5, "c", 5.0, Some(2))
            ]
        );
    }

    #[test]
    fn pretests_only_without_pretests_runs_the_tests_unflagged() {
        let p = plan("test_cases:\n- {in: a, out: b, points: 2}\n", true).unwrap();
        assert!(!p.pretested);
        assert_eq!(p.cases.len(), 1);
    }

    #[test]
    fn limits_and_checker_resolve_case_then_batch_then_problem() {
        let p = plan(
            "output_limit_length: 1000\noutput_prefix_length: 9\nchecker: identical\ntest_cases:\n\
             - points: 10\n  output_limit_length: 500\n  checker: linecount\n  batched:\n  \
             - {in: a, out: b}\n  - {in: c, out: d, output_limit_length: 7, checker: floats}\n\
             - {in: e, out: f, points: 1}\n",
            false,
        )
        .unwrap();
        let shape: Vec<_> =
            p.cases.iter().map(|c| (c.output_limit, c.output_prefix, c.checker.name().to_string())).collect();
        assert_eq!(
            shape,
            [(500, 9, "linecount".into()), (7, 9, "floats".into()), (1000, 9, "identical".into())]
        );
    }

    #[test]
    fn malformed_plans_are_invalid() {
        for yaml in [
            "test_cases:\n- {in: a, out: b}\n",            // plain case without points
            "test_cases:\n- batched: [{in: a, out: b}]\n", // batch without points
            "test_cases:\n- {points: 3, batched: []}\n",   // empty batch
            "test_cases:\n- {out: b, points: 1}\n",        // missing input
        ] {
            assert!(matches!(plan(yaml, false), Err(InitError::Invalid(_))), "{yaml}");
        }
    }

    #[test]
    fn output_limit_above_the_ceiling_is_invalid_at_any_level() {
        for yaml in [
            "output_limit_length: 99999999999\ntest_cases:\n- {in: a, out: b, points: 1}\n",
            "test_cases:\n- {in: a, out: b, points: 1, output_limit_length: 99999999999}\n",
            "test_cases:\n- points: 1\n  output_limit_length: 99999999999\n  batched: [{in: a, out: b}]\n",
        ] {
            assert!(matches!(plan(yaml, false), Err(InitError::Invalid(_))), "{yaml}");
        }
        let at_ceiling =
            format!("output_limit_length: {MAX_OUTPUT_LIMIT}\ntest_cases:\n- {{in: a, out: b, points: 1}}\n");
        assert!(plan(&at_ceiling, false).is_ok());
    }

    #[test]
    fn more_than_ten_thousand_cases_is_invalid() {
        let mut yaml = String::from("test_cases:\n");
        for i in 0..=MAX_SUBMISSION_CASES {
            yaml.push_str(&format!("- {{in: {i}.in, out: {i}.out, points: 1}}\n"));
        }
        assert!(matches!(plan(&yaml, false), Err(InitError::Invalid(_))));
    }
}
