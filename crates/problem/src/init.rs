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

/// `signature_grader {entry, header, allow_main}`: the contestant writes functions, `entry` holds `main` (Task 34).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureSpec {
    pub entry: String,
    pub header: String,
    #[serde(default)]
    pub allow_main: bool,
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

const GRADED_CUSTOM_CHECKERS: [&str; 2] = ["testlib", "default"];

impl InitYaml {
    pub fn unsupported_feature(&self) -> Option<String> {
        if self.generator.is_some() {
            return Some("generator".into());
        }
        if self.hints.iter().any(|hint| hint == "nobigmath") {
            return Some("hints.nobigmath".into());
        }
        self.unsupported_custom_checker()
    }

    fn unsupported_custom_checker(&self) -> Option<String> {
        let checker = self.checker.as_ref().filter(|c| c.name() == "bridged")?;
        let kind = checker.args().get("type").and_then(Value::as_str).unwrap_or("testlib");
        (!GRADED_CUSTOM_CHECKERS.contains(&kind)).then(|| format!("checker.{kind}"))
    }
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
        let themis = parse_init(
            "test_cases: []\nchecker: {name: bridged, args: {type: themis, lang: CPP17, files: c.cpp}}\n",
        )
        .unwrap();
        assert_eq!(themis.unsupported_feature().as_deref(), Some("checker.themis"));
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
}
