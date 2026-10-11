//! init.yml checker → a checker (spec §2 "Checkers").

use super::custom::PartialPoints;
use super::easy::Easy;
use super::floats::{ErrorMode, Floats};
use super::identical::Identical;
use super::linecount::LineCount;
use super::linematches::LineMatches;
use super::rstripped::RStripped;
use super::sorted::{Sorted, SplitOn};
use super::standard::Standard;
use super::{CheckError, Checker};
use judge_problem::init::CheckerSpec;
use serde_norway::{Mapping, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomKind {
    Testlib,
    Default,
    Themis,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CustomSpec {
    pub source: String,
    pub lang: String,
    pub kind: CustomKind,
    pub partial: PartialPoints,
}

pub enum CheckerChoice {
    Builtin(Box<dyn Checker>),
    Custom(CustomSpec),
}

fn arg_str<'a>(args: &'a Mapping, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn error_mode(args: &Mapping, default: ErrorMode) -> Result<ErrorMode, CheckError> {
    match arg_str(args, "error_mode") {
        None => Ok(default),
        Some("default") => Ok(ErrorMode::Default),
        Some("absolute") => Ok(ErrorMode::Absolute),
        Some("relative") => Ok(ErrorMode::Relative),
        Some(other) => Err(CheckError(format!("unknown error_mode {other:?}"))),
    }
}

fn precision(args: &Mapping) -> Result<u32, CheckError> {
    match args.get("precision") {
        None => Ok(6),
        Some(value) => value.as_u64().and_then(|p| u32::try_from(p).ok()).ok_or_else(|| {
            CheckError(format!("precision must be a small non-negative integer, got {value:?}"))
        }),
    }
}

fn floats(args: &Mapping, default: ErrorMode) -> Result<CheckerChoice, CheckError> {
    let checker = Floats { precision: precision(args)?, mode: error_mode(args, default)? };
    Ok(CheckerChoice::Builtin(Box::new(checker)))
}

fn custom(args: &Mapping) -> Result<CheckerChoice, CheckError> {
    let missing = |key: &str| CheckError(format!("bridged checker needs `{key}`"));
    let kind = match arg_str(args, "type").unwrap_or("testlib") {
        "testlib" => CustomKind::Testlib,
        "default" => CustomKind::Default,
        "themis" => CustomKind::Themis,
        other => return Err(CheckError(format!("unsupported custom checker type {other:?}"))),
    };
    let source = arg_str(args, "files").ok_or_else(|| missing("files"))?.to_string();
    let lang = arg_str(args, "lang").ok_or_else(|| missing("lang"))?.to_string();
    let partial = match args.get("treat_checker_points_as_percentage") {
        None | Some(Value::Bool(false)) => PartialPoints::Absolute,
        Some(Value::Bool(true)) => PartialPoints::Percentage,
        Some(other) => {
            return Err(CheckError(format!(
                "treat_checker_points_as_percentage must be a boolean, got {other:?}"
            )));
        }
    };
    Ok(CheckerChoice::Custom(CustomSpec { source, lang, kind, partial }))
}

fn identical(args: &Mapping, unicode: bool) -> CheckerChoice {
    let pe_allowed = args.get("pe_allowed").and_then(Value::as_bool).unwrap_or(true);
    CheckerChoice::Builtin(Box::new(Identical { pe_allowed, unicode }))
}

fn bool_arg(args: &Mapping, key: &str, default: bool) -> Result<bool, CheckError> {
    match args.get(key) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(CheckError(format!("{key} must be a boolean, got {other:?}"))),
    }
}

fn sorted(args: &Mapping, split_on: Option<SplitOn>) -> Result<CheckerChoice, CheckError> {
    let split_on = match (split_on, arg_str(args, "split_on")) {
        (Some(fixed), _) => fixed,
        (None, None | Some("lines")) => SplitOn::Lines,
        (None, Some("whitespace")) => SplitOn::Whitespace,
        (None, Some(other)) => return Err(CheckError(format!("unknown split_on {other:?}"))),
    };
    Ok(CheckerChoice::Builtin(Box::new(Sorted { split_on })))
}

fn point_distribution(args: &Mapping) -> Result<Vec<u64>, CheckError> {
    let bad = |what: String| {
        CheckError(format!("point_distribution must be a list of non-negative integers, got {what}"))
    };
    match args.get("point_distribution") {
        None => Ok(vec![1]),
        Some(Value::Sequence(items)) => {
            items.iter().map(|v| v.as_u64().ok_or_else(|| bad(format!("{v:?}")))).collect()
        }
        Some(other) => Err(bad(format!("{other:?}"))),
    }
}

fn linematches(args: &Mapping) -> Result<CheckerChoice, CheckError> {
    let checker = LineMatches {
        point_distribution: point_distribution(args)?,
        filler_lines_required: bool_arg(args, "filler_lines_required", true)?,
    };
    Ok(CheckerChoice::Builtin(Box::new(checker)))
}

/// `unicode` is the problem's `unicode` hint: it decides which characters are whitespace (standard,
/// identical's presentation-error test) and is not read by the other checkers.
pub fn choose(spec: &CheckerSpec, unicode: bool) -> Result<CheckerChoice, CheckError> {
    let args = spec.args();
    match spec.name() {
        "standard" => Ok(CheckerChoice::Builtin(Box::new(Standard { unicode }))),
        "identical" => Ok(identical(args, unicode)),
        "rstripped" => {
            let filter_new_line = bool_arg(args, "filter_new_line", false)?;
            Ok(CheckerChoice::Builtin(Box::new(RStripped { filter_new_line })))
        }
        "sorted" => sorted(args, None),
        "unordered" => sorted(args, Some(SplitOn::Whitespace)),
        "easy" => Ok(CheckerChoice::Builtin(Box::new(Easy))),
        "linematches" => linematches(args),
        "linecount" => Ok(CheckerChoice::Builtin(Box::new(LineCount))),
        "floats" => floats(args, ErrorMode::Default),
        "floatsabs" => floats(args, ErrorMode::Absolute),
        "floatsrel" => floats(args, ErrorMode::Relative),
        "bridged" => custom(args),
        other => Err(CheckError(format!("unknown checker {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkers::identical::PRESENTATION_ERROR;
    use crate::checkers::testing::run;
    use judge_problem::init::parse_init;

    fn spec(yaml: &str) -> CheckerSpec {
        parse_init(&format!("test_cases: []\nchecker: {yaml}\n")).unwrap().checker.unwrap()
    }

    fn builtin_with(yaml: &str, unicode: bool) -> Box<dyn Checker> {
        match choose(&spec(yaml), unicode).unwrap() {
            CheckerChoice::Builtin(c) => c,
            CheckerChoice::Custom(_) => panic!("custom"),
        }
    }

    fn builtin(yaml: &str) -> Box<dyn Checker> {
        builtin_with(yaml, false)
    }

    #[test]
    fn names_map_to_behaviour() {
        assert!(run(builtin("standard").as_ref(), b"1  2", b"1 2").passed);
        assert!(!run(builtin("identical").as_ref(), b"1  2", b"1 2").passed);
        assert!(run(builtin("floats").as_ref(), b"1.0000001", b"1").passed);
        assert!(!run(builtin("floatsabs").as_ref(), b"1000000.01", b"1000000").passed);
        assert!(run(builtin("floatsrel").as_ref(), b"1000000.0001", b"1000000").passed);
        assert!(run(builtin("{name: floats, args: {precision: 1}}").as_ref(), b"1.04", b"1").passed);
        let strict = "{name: floats, args: {precision: 1, error_mode: absolute}}";
        assert!(!run(builtin(strict).as_ref(), b"1.2", b"1").passed);
        let no_pe = "{name: identical, args: {pe_allowed: false}}";
        assert_eq!(run(builtin(no_pe).as_ref(), b"1  2", b"1 2").feedback, "");
        assert!(run(builtin("linecount").as_ref(), b"a\n", b"a\n").passed);
    }

    #[test]
    fn text_checker_names_map_to_behaviour() {
        assert!(run(builtin("rstripped").as_ref(), b"a \n", b"a\n").passed);
        assert!(run(builtin("sorted").as_ref(), b"b\na\n", b"a\nb\n").passed);
        assert!(!run(builtin("sorted").as_ref(), b"b a\n", b"a b\n").passed);
        assert!(run(builtin("{name: sorted, args: {split_on: whitespace}}").as_ref(), b"b a", b"a b").passed);
        assert!(run(builtin("unordered").as_ref(), b"b\na", b"a b").passed);
        assert!(run(builtin("easy").as_ref(), b"AB", b"b a").passed);
        assert!(run(builtin("linematches").as_ref(), b"a\n", b"a\n").passed);
    }

    #[test]
    fn the_unicode_hint_reaches_standard_and_identical() {
        // U+3000 is whitespace only when the hint is on.
        let (got, want) = ("a\u{3000}b".as_bytes(), b"a b".as_slice());
        assert!(run(builtin_with("standard", true).as_ref(), got, want).passed);
        assert!(!run(builtin_with("standard", false).as_ref(), got, want).passed);
        let pe = run(builtin_with("identical", true).as_ref(), got, want);
        assert_eq!(pe.feedback, PRESENTATION_ERROR);
        assert_eq!(run(builtin_with("identical", false).as_ref(), got, want).feedback, "");
    }

    #[test]
    fn bridged_becomes_a_custom_spec() {
        let choice =
            choose(&spec("{name: bridged, args: {type: default, lang: PAS, files: chk.pas}}"), false);
        let Ok(CheckerChoice::Custom(custom)) = choice else { panic!("expected a custom spec") };
        assert_eq!((custom.source.as_str(), custom.lang.as_str()), ("chk.pas", "PAS"));
        assert_eq!(custom.kind, CustomKind::Default);
        let testlib = choose(&spec("{name: bridged, args: {lang: CPP17, files: c.cpp}}"), false);
        assert!(matches!(testlib, Ok(CheckerChoice::Custom(CustomSpec { kind: CustomKind::Testlib, .. }))));
    }

    #[test]
    fn the_percentage_flag_picks_how_partial_points_are_read() {
        let partial = |args: &str| match choose(
            &spec(&format!("{{name: bridged, args: {{lang: CPP17, files: c.cpp{args}}}}}")),
            false,
        ) {
            Ok(CheckerChoice::Custom(custom)) => Ok(custom.partial),
            Ok(CheckerChoice::Builtin(_)) => panic!("builtin"),
            Err(error) => Err(error),
        };
        assert_eq!(partial(""), Ok(PartialPoints::Absolute));
        assert_eq!(partial(", treat_checker_points_as_percentage: false"), Ok(PartialPoints::Absolute));
        assert_eq!(partial(", treat_checker_points_as_percentage: true"), Ok(PartialPoints::Percentage));
        assert!(partial(", treat_checker_points_as_percentage: yes").is_err());
    }

    #[test]
    fn unknown_names_and_broken_args_are_errors() {
        let broken = |yaml: &str| choose(&spec(yaml), false).is_err();
        assert!(broken("rot13"));
        assert!(broken("{name: bridged, args: {lang: CPP17}}"));
        assert!(broken("{name: bridged, args: {files: c.cpp}}"));
        assert!(broken("{name: bridged, args: {type: cms, lang: CPP17, files: c.cpp}}"));
        assert!(broken("{name: floats, args: {error_mode: sideways}}"));
        assert!(broken("{name: floats, args: {precision: -1}}"));
        assert!(broken("{name: floats, args: {precision: 99999999999}}"));
    }

    #[test]
    fn text_checker_args_are_validated() {
        let broken = |yaml: &str| choose(&spec(yaml), false).is_err();
        assert!(broken("{name: sorted, args: {split_on: words}}"));
        assert!(broken("{name: rstripped, args: {filter_new_line: 1}}"));
        assert!(broken("{name: linematches, args: {filler_lines_required: maybe}}"));
        assert!(broken("{name: linematches, args: {point_distribution: [1, -1]}}"));
        assert!(broken("{name: linematches, args: {point_distribution: 3}}"));
    }
}
