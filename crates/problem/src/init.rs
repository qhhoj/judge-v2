//! The init.yml subset `compileInitYaml` emits (spec §1). Unknown keys are refused by name.

use serde::Deserialize;
use serde_norway::{Mapping, Value};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InitError {
    #[error("unsupported init.yml key: {0}")]
    Unsupported(String),
    #[error("invalid init.yml: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitYaml {
    #[serde(default)]
    pub archive: Option<String>,
    #[serde(default)]
    pub test_cases: Vec<CaseEntry>,
    #[serde(default)]
    pub pretest_test_cases: Vec<CaseEntry>,
    #[serde(default)]
    pub output_limit_length: Option<u64>,
    #[serde(default)]
    pub output_prefix_length: Option<u64>,
    #[serde(default)]
    pub checker: Option<CheckerSpec>,
    #[serde(default)]
    pub file_io: Option<FileIo>,
    #[serde(default)]
    pub output_only: bool,
    #[serde(default)]
    pub interactive: Option<InteractiveSpec>,
    #[serde(default)]
    pub hints: Vec<String>,
    /// Parsed only to answer `UNSUPPORTED`: no legacy problem uses a generator (legacy-judge-usage.md).
    #[serde(default)]
    pub generator: Option<Value>,
    #[serde(default)]
    pub signature_grader: Option<SignatureSpec>,
}

/// `signature_grader {entry, header, allow_main, split, cores}`: the contestant writes functions, `entry` holds
/// `main` (Task 34). `split` runs the contestant in its own box behind generated glue and `cores: 2` asks for a
/// paired lane (docs/split-and-communication.md §4, §6). The backend writes both only when set, so a legacy
/// init.yml is byte-identical.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureSpec {
    pub entry: String,
    pub header: String,
    #[serde(default)]
    pub allow_main: bool,
    #[serde(default)]
    pub split: bool,
    #[serde(default)]
    pub cores: Option<u8>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseEntry {
    #[serde(rename = "in", default)]
    pub input: Option<String>,
    #[serde(default)]
    pub out: Option<String>,
    #[serde(default)]
    pub points: Option<f64>,
    #[serde(default)]
    pub batched: Option<Vec<CaseEntry>>,
    #[serde(default)]
    pub output_limit_length: Option<u64>,
    #[serde(default)]
    pub output_prefix_length: Option<u64>,
    #[serde(default)]
    pub checker: Option<CheckerSpec>,
    #[serde(default)]
    pub generator_args: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum CheckerSpec {
    Name(String),
    WithArgs {
        name: String,
        #[serde(default)]
        args: Mapping,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIo {
    pub input: String,
    pub output: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractiveSpec {
    pub files: FilesSpec,
    #[serde(rename = "type")]
    pub kind: String,
    pub lang: String,
    /// Polygon's checker of an interactive problem: run on the interactor's `tout` once the interactor
    /// accepts, and its verdict decides the case. Absent, the interactor alone decides (every legacy
    /// problem; their top-level and per-case checkers are never run).
    #[serde(default)]
    pub checker: Option<CheckerSpec>,
    /// The interactor's testlib `points N` is N percent of the case, as a bridged checker's flag of the same
    /// name (Polygon's percentage flag). Absent, N is points of the case.
    #[serde(default)]
    pub treat_checker_points_as_percentage: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FilesSpec {
    One(String),
    Many(Vec<String>),
}

impl FilesSpec {
    /// The source to compile: the only file, or the first of the list.
    pub fn first(&self) -> Option<&str> {
        match self {
            FilesSpec::One(name) => Some(name),
            FilesSpec::Many(names) => names.first().map(String::as_str),
        }
    }
}

static EMPTY_ARGS: std::sync::OnceLock<Mapping> = std::sync::OnceLock::new();

impl CheckerSpec {
    pub fn name(&self) -> &str {
        match self {
            CheckerSpec::Name(name) | CheckerSpec::WithArgs { name, .. } => name,
        }
    }

    pub fn args(&self) -> &Mapping {
        match self {
            CheckerSpec::Name(_) => EMPTY_ARGS.get_or_init(Mapping::new),
            CheckerSpec::WithArgs { args, .. } => args,
        }
    }
}

const GRADED_CUSTOM_CHECKERS: [&str; 3] = ["testlib", "default", "themis"];

/// Hello features of split signature (docs/split-and-communication.md §3.3): one-core lane and paired lane.
pub const SPLIT_FEATURE: &str = "signature.split";
pub const SPLIT_PAIRED_FEATURE: &str = "signature.split.paired";

/// A mode an init.yml selects by setting its key. `SignatureSplit` is `signature_grader` with `split: true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeKey {
    Interactive,
    Signature,
    SignatureSplit,
    OutputOnly,
    FileIo,
}

impl ModeKey {
    pub fn name(self) -> &'static str {
        match self {
            ModeKey::Interactive => "interactive",
            ModeKey::Signature => "signature_grader",
            ModeKey::SignatureSplit => "signature_grader.split",
            ModeKey::OutputOnly => "output_only",
            ModeKey::FileIo => "file_io",
        }
    }

    /// The session modes (§2) must be the only mode of their file. Legacy modes keep every combination that
    /// parsed before them (§3.1): refusing those waits for a query proving no stored problem has one.
    fn is_exclusive(self) -> bool {
        matches!(self, ModeKey::SignatureSplit)
    }
}

impl InitYaml {
    pub fn unsupported_feature(&self) -> Option<String> {
        if self.generator.is_some() {
            return Some("generator".into());
        }
        if self.hints.iter().any(|hint| hint == "nobigmath") {
            return Some("hints.nobigmath".into());
        }
        if let Some(feature) = self.split_feature() {
            return Some(feature.into());
        }
        self.unsupported_custom_checker()
    }

    /// Every mode key this file sets, in a fixed order.
    pub fn mode_keys(&self) -> Vec<ModeKey> {
        let signature = self.signature_grader.as_ref().map(|spec| match spec.split {
            true => ModeKey::SignatureSplit,
            false => ModeKey::Signature,
        });
        let interactive = self.interactive.as_ref().map(|_| ModeKey::Interactive);
        let output_only = self.output_only.then_some(ModeKey::OutputOnly);
        let file_io = self.file_io.as_ref().map(|_| ModeKey::FileIo);
        [interactive, signature, output_only, file_io].into_iter().flatten().collect()
    }

    /// A split problem until this judge grades split signature (plan 05 removes the one-core case, plan 12 the
    /// paired one): requeued, never graded by the one-process path.
    fn split_feature(&self) -> Option<&'static str> {
        let spec = self.signature_grader.as_ref().filter(|spec| spec.split)?;
        Some(if spec.cores == Some(2) { SPLIT_PAIRED_FEATURE } else { SPLIT_FEATURE })
    }

    /// The problem-wide checker and the interactive problem's own checker.
    fn unsupported_custom_checker(&self) -> Option<String> {
        let interactive = self.interactive.as_ref().and_then(|spec| spec.checker.as_ref());
        [self.checker.as_ref(), interactive].into_iter().flatten().find_map(unsupported_custom_kind)
    }
}

fn unsupported_custom_kind(checker: &CheckerSpec) -> Option<String> {
    if checker.name() != "bridged" {
        return None;
    }
    let kind = checker.args().get("type").and_then(Value::as_str).unwrap_or("testlib");
    (!GRADED_CUSTOM_CHECKERS.contains(&kind)).then(|| format!("checker.{kind}"))
}

/// serde reports an unknown field as "unknown field `name`, expected ...".
fn unknown_field(message: &str) -> Option<String> {
    let rest = message.split("unknown field `").nth(1)?;
    rest.split('`').next().map(str::to_string)
}

/// Names the box itself uses; a `file_io` name must not shadow them (the `case_file` rule).
const RESERVED_BOX_NAMES: [&str; 4] = ["input", "output", "main", ".stderr"];

/// A `file_io` name is a plain file name inside the box: not empty, no leading `.`, no `/` or `\`,
/// not a reserved box name. `Path::join` with an absolute or `../` name would leave the box (preflight C4).
pub fn is_plain_box_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && !RESERVED_BOX_NAMES.contains(&name)
}

fn check_file_io(file_io: &FileIo) -> Result<(), InitError> {
    for name in [&file_io.input, &file_io.output] {
        if !is_plain_box_name(name) {
            return Err(InitError::Invalid(format!("file_io name {name:?} is not a plain file name")));
        }
    }
    Ok(())
}

/// The split options' own rules (§3.1): two `main`s cannot be linked, `cores` only means something with `split`.
fn check_signature(spec: &SignatureSpec) -> Result<(), InitError> {
    let refused = |what: String| Err(InitError::Unsupported(what));
    if spec.split && spec.allow_main {
        return refused("signature_grader.split with allow_main".into());
    }
    match spec.cores {
        Some(_) if !spec.split => refused("signature_grader.cores without split".into()),
        Some(cores) if !(1..=2).contains(&cores) => {
            refused(format!("signature_grader.cores {cores} (1 or 2)"))
        }
        _ => Ok(()),
    }
}

/// An exclusive mode with any other mode key is refused, naming both keys.
fn check_modes(init: &InitYaml) -> Result<(), InitError> {
    let modes = init.mode_keys();
    let Some(exclusive) = modes.iter().copied().find(|mode| mode.is_exclusive()) else { return Ok(()) };
    match modes.iter().find(|mode| **mode != exclusive) {
        Some(other) => Err(InitError::Unsupported(format!("{} with {}", exclusive.name(), other.name()))),
        None => Ok(()),
    }
}

pub fn parse_init(text: &str) -> Result<InitYaml, InitError> {
    let init: InitYaml = serde_norway::from_str(text).map_err(|error| {
        let message = error.to_string();
        match unknown_field(&message) {
            Some(key) => InitError::Unsupported(key),
            None => InitError::Invalid(message),
        }
    })?;
    if let Some(file_io) = &init.file_io {
        check_file_io(file_io)?;
    }
    if let Some(signature) = &init.signature_grader {
        check_signature(signature)?;
    }
    check_modes(&init)?;
    Ok(init)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_io_names_that_leave_the_box_are_invalid() {
        let error = parse_init("test_cases: []\nfile_io: {input: ../x, output: /etc/passwd}\n").unwrap_err();
        assert!(matches!(error, InitError::Invalid(_)));
        for bad in ["\"\"", ".hidden", "a\\\\b", "input", "output", "main", ".stderr", "sub/x"] {
            let text = format!("test_cases: []\nfile_io: {{input: {bad}, output: ok.out}}\n");
            assert!(matches!(parse_init(&text), Err(InitError::Invalid(_))), "{bad} accepted");
        }
        assert!(parse_init("test_cases: []\nfile_io: {input: sum.inp, output: sum.out}\n").is_ok());
    }

    #[test]
    fn capability_gated_features_are_reported() {
        let generator = parse_init("generator: gen.cpp\ntest_cases: []\n").unwrap();
        assert_eq!(generator.unsupported_feature().as_deref(), Some("generator"));
        let cms = parse_init(
            "test_cases: []\nchecker: {name: bridged, args: {type: cms, lang: CPP17, files: c.cpp}}\n",
        )
        .unwrap();
        assert_eq!(cms.unsupported_feature().as_deref(), Some("checker.cms"));
        let testlib = parse_init(
            "test_cases: []\nchecker: {name: bridged, args: {type: testlib, lang: CPP17, files: c.cpp}}\n",
        )
        .unwrap();
        assert_eq!(testlib.unsupported_feature(), None);
        let nobigmath = parse_init("test_cases: []\nhints: [nobigmath]\n").unwrap();
        assert_eq!(nobigmath.unsupported_feature().as_deref(), Some("hints.nobigmath"));
        let signature =
            parse_init("test_cases: []\nsignature_grader: {entry: g.cpp, header: s.h}\n").unwrap();
        assert_eq!(signature.unsupported_feature(), None);
    }

    #[test]
    fn interactive_files_accepts_string_or_list() {
        let one =
            parse_init("test_cases: []\ninteractive: {files: i.cpp, type: testlib, lang: CPP17}\n").unwrap();
        assert_eq!(one.interactive.unwrap().files.first(), Some("i.cpp"));
        let many =
            parse_init("test_cases: []\ninteractive: {files: [i.cpp, h.h], type: testlib, lang: CPP17}\n")
                .unwrap();
        assert_eq!(many.interactive.unwrap().files.first(), Some("i.cpp"));
    }

    #[test]
    fn an_interactive_checker_is_parsed_and_its_custom_type_is_gated() {
        let interactive = |checker: &str| {
            parse_init(&format!(
                "test_cases: []\ninteractive: {{files: i.cpp, type: testlib, lang: CPP17, checker: {checker}}}\n"
            ))
            .unwrap()
        };
        let named = interactive("{name: bridged, args: {files: [c.cpp, testlib.h], lang: CPP17}}");
        let checker = named.interactive.as_ref().and_then(|spec| spec.checker.as_ref()).unwrap();
        assert_eq!(checker.name(), "bridged");
        assert_eq!(named.unsupported_feature(), None);
        let cms = interactive("{name: bridged, args: {files: c.cpp, lang: CPP17, type: cms}}");
        assert_eq!(cms.unsupported_feature().as_deref(), Some("checker.cms"));
        let legacy =
            parse_init("test_cases: []\ninteractive: {files: i.cpp, type: testlib, lang: CPP17}\n").unwrap();
        assert!(legacy.interactive.unwrap().checker.is_none(), "no key, no checker");
    }

    #[test]
    fn the_interactors_percentage_flag_is_optional_and_boolean() {
        let interactive = |tail: &str| {
            parse_init(&format!(
                "test_cases: []\ninteractive: {{files: i.cpp, type: testlib, lang: CPP17{tail}}}\n"
            ))
        };
        let flag =
            |tail: &str| interactive(tail).unwrap().interactive.unwrap().treat_checker_points_as_percentage;
        assert!(flag(", treat_checker_points_as_percentage: true"));
        assert!(!flag(", treat_checker_points_as_percentage: false"));
        assert!(!flag(""), "absent, points stay absolute");
        assert!(interactive(", treat_checker_points_as_percentage: 50").is_err());
    }

    #[test]
    fn unknown_keys_are_refused_at_every_nesting_level() {
        let top = parse_init("test_cases: []\nsymlinks: true\n").unwrap_err();
        assert_eq!(top, InitError::Unsupported("symlinks".into()));
        let case = parse_init("test_cases:\n- {in: a, out: b, points: 1, weird: 2}\n").unwrap_err();
        assert_eq!(case, InitError::Unsupported("weird".into()));
        let batched = "test_cases:\n- points: 1\n  batched:\n  - {in: a, out: b, weird: 1}\n";
        assert_eq!(parse_init(batched).unwrap_err(), InitError::Unsupported("weird".into()));
        let file_io = parse_init("test_cases: []\nfile_io: {input: a, output: b, mode: x}\n").unwrap_err();
        assert_eq!(file_io, InitError::Unsupported("mode".into()));
        let interactive =
            "test_cases: []\ninteractive: {files: i.cpp, type: testlib, lang: CPP17, extra: 1}\n";
        assert_eq!(parse_init(interactive).unwrap_err(), InitError::Unsupported("extra".into()));
        let signature = "test_cases: []\nsignature_grader: {entry: g.cpp, header: s.h, nope: 1}\n";
        assert_eq!(parse_init(signature).unwrap_err(), InitError::Unsupported("nope".into()));
    }

    #[test]
    fn unknown_checker_keys_are_not_accepted_silently() {
        let error = parse_init("test_cases: []\nchecker: {name: floats, typo: 1}\n").unwrap_err();
        assert!(matches!(error, InitError::Invalid(_) | InitError::Unsupported(_)), "{error:?}");
    }

    fn split(tail: &str) -> Result<InitYaml, InitError> {
        parse_init(&format!("test_cases: []\nsignature_grader: {{entry: g.cpp, header: s.h{tail}}}\n"))
    }

    fn signature_of(init: &InitYaml) -> &SignatureSpec {
        init.signature_grader.as_ref().unwrap()
    }

    #[test]
    fn a_legacy_signature_problem_reads_as_one_process() {
        for tail in ["", ", split: false", ", allow_main: true"] {
            let init = split(tail).unwrap();
            assert!(!signature_of(&init).split, "{tail}");
            assert_eq!(signature_of(&init).cores, None, "{tail}");
            assert_eq!(init.unsupported_feature(), None, "{tail}");
            assert_eq!(init.mode_keys(), vec![ModeKey::Signature], "{tail}");
        }
    }

    #[test]
    fn a_split_problem_is_requeued_until_this_judge_grades_it() {
        let feature = |tail: &str| split(tail).unwrap().unsupported_feature();
        assert_eq!(feature(", split: true").as_deref(), Some(SPLIT_FEATURE));
        assert_eq!(feature(", split: true, cores: 1").as_deref(), Some(SPLIT_FEATURE));
        assert_eq!(feature(", split: true, cores: 2").as_deref(), Some(SPLIT_PAIRED_FEATURE));
        assert_eq!(split(", split: true").unwrap().mode_keys(), vec![ModeKey::SignatureSplit]);
    }

    #[test]
    fn split_options_that_contradict_each_other_are_refused() {
        let refused = |tail: &str| match split(tail) {
            Err(InitError::Unsupported(message)) => message,
            other => panic!("{tail}: {other:?}"),
        };
        assert_eq!(refused(", split: true, allow_main: true"), "signature_grader.split with allow_main");
        assert_eq!(refused(", cores: 2"), "signature_grader.cores without split");
        assert_eq!(refused(", cores: 1"), "signature_grader.cores without split");
        assert_eq!(refused(", split: true, cores: 0"), "signature_grader.cores 0 (1 or 2)");
        assert_eq!(refused(", split: true, cores: 3"), "signature_grader.cores 3 (1 or 2)");
    }

    #[test]
    fn malformed_split_values_are_invalid_not_a_panic() {
        for tail in [
            ", split: yes please",
            ", split: true, cores: -1",
            ", split: true, cores: \"2\"",
            ", split: true, cores: 300",
        ] {
            assert!(matches!(split(tail), Err(InitError::Invalid(_))), "{tail}");
        }
    }

    #[test]
    fn a_split_problem_is_the_only_mode_of_its_file() {
        let others = [
            ("interactive: {files: i.cpp, type: testlib, lang: CPP17}", "interactive"),
            ("output_only: true", "output_only"),
            ("file_io: {input: a.inp, output: a.out}", "file_io"),
        ];
        for (line, name) in others {
            let text = format!(
                "test_cases: []\n{line}\nsignature_grader: {{entry: g.cpp, header: s.h, split: true}}\n"
            );
            assert_eq!(
                parse_init(&text).unwrap_err(),
                InitError::Unsupported(format!("signature_grader.split with {name}")),
            );
        }
    }

    #[test]
    fn every_legacy_combination_of_mode_keys_still_parses() {
        let lines = [
            "interactive: {files: i.cpp, type: testlib, lang: CPP17}",
            "signature_grader: {entry: g.cpp, header: s.h}",
            "output_only: true",
            "file_io: {input: a.inp, output: a.out}",
        ];
        for (index, first) in lines.iter().enumerate() {
            for second in &lines[index + 1..] {
                let text = format!("test_cases: []\n{first}\n{second}\n");
                let init = parse_init(&text).unwrap_or_else(|e| panic!("{first} + {second}: {e}"));
                assert_eq!(init.mode_keys().len(), 2, "{first} + {second}");
            }
        }
    }
}
