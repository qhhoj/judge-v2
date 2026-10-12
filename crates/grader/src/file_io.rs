//! File I/O names in any letter case, as the old judge allowed.
//!
//! Teachers write the names on Windows, where `Number.inp` and `NUMBER.INP` are the same file, and the old
//! judge accepted any spelling: a program configured with `Number.inp` may open `NUMBER.INP`, `number.inp` or
//! `PathLib.inp`. The box is case-sensitive, so the case input is laid out under each spelling a program may
//! use (the usual letter cases, plus the spellings of the name's stem and extension found in its source), and
//! the output is whichever file matches the configured name ignoring case.

use judge_problem::init::{FileIo, is_plain_box_name};
use std::collections::BTreeSet;

/// Spellings of one word taken from a source at most.
const MAX_SOURCE_SPELLINGS: usize = 8;

/// `name` with its stem as written, lower, upper and capitalised, and its extension lower and upper.
pub fn letter_cases(name: &str) -> BTreeSet<String> {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return [name.to_string(), name.to_ascii_lowercase(), name.to_ascii_uppercase()].into();
    };
    let exts = [ext.to_ascii_lowercase(), ext.to_ascii_uppercase()];
    let mut names = BTreeSet::new();
    for stem in stem_cases(stem) {
        names.extend(exts.iter().map(|ext| format!("{stem}.{ext}")));
    }
    names
}

fn stem_cases(stem: &str) -> [String; 4] {
    let lower = stem.to_ascii_lowercase();
    let mut capitalised = lower.clone();
    if let Some(first) = capitalised.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    [stem.to_string(), lower, stem.to_ascii_uppercase(), capitalised]
}

/// Every spelling of `word` in `source`, ignoring ASCII case (at most `MAX_SOURCE_SPELLINGS`).
fn source_spellings(source: &[u8], word: &str) -> BTreeSet<String> {
    let lower = source.to_ascii_lowercase();
    let needle = word.to_ascii_lowercase().into_bytes();
    let mut found = BTreeSet::new();
    if needle.is_empty() {
        return found;
    }
    for (at, window) in lower.windows(needle.len()).enumerate() {
        if window != needle.as_slice() {
            continue;
        }
        if let Ok(spelling) = std::str::from_utf8(&source[at..at + needle.len()]) {
            found.insert(spelling.to_string());
        }
        if found.len() >= MAX_SOURCE_SPELLINGS {
            break;
        }
    }
    found
}

/// Each spelling of `name` a program with this source may open: `letter_cases`, and every combination of
/// the stem and `.ext` spellings in the source (so `#define fn "PathLib"` + `fn".INP"` is found too).
fn spellings_for(name: &str, source: &[u8]) -> BTreeSet<String> {
    let mut names = letter_cases(name);
    let Some((stem, ext)) = name.rsplit_once('.') else {
        names.extend(source_spellings(source, name));
        return names;
    };
    let mut stems: BTreeSet<String> = stem_cases(stem).into();
    stems.extend(source_spellings(source, stem));
    let mut exts: BTreeSet<String> = [ext.to_string()].into();
    let dotted = source_spellings(source, &format!(".{ext}"));
    exts.extend(dotted.iter().map(|spelling| spelling[1..].to_string()));
    for stem in &stems {
        names.extend(exts.iter().map(|ext| format!("{stem}.{ext}")));
    }
    names
}

/// A solution's file I/O names: the case input goes in under `input` and every alias, and the output is
/// read from the file named `output` in any letter case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolutionFileIo {
    pub input: String,
    pub aliases: Vec<String>,
    pub output: String,
}

impl SolutionFileIo {
    /// `source` is the program's source; it only adds spellings. An alias is a plain box name that differs
    /// from the input and never matches the output name, whatever the case.
    pub fn new(io: &FileIo, source: &[u8]) -> SolutionFileIo {
        let aliases = spellings_for(&io.input, source)
            .into_iter()
            .filter(|name| *name != io.input && is_plain_box_name(name))
            .filter(|name| !name.eq_ignore_ascii_case(&io.output))
            .collect();
        SolutionFileIo { input: io.input.clone(), aliases, output: io.output.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn io(input: &str, output: &str) -> FileIo {
        FileIo { input: input.into(), output: output.into() }
    }

    fn opens(names: &SolutionFileIo, name: &str) -> bool {
        names.input == name || names.aliases.iter().any(|alias| alias == name)
    }

    #[test]
    fn the_usual_letter_cases_are_inputs_without_any_source() {
        let names = SolutionFileIo::new(&io("Number.inp", "Number.out"), b"");
        for name in ["Number.inp", "NUMBER.INP", "number.inp", "NUMBER.inp", "Number.INP", "number.INP"] {
            assert!(opens(&names, name), "{name}: {names:?}");
        }
        let names = SolutionFileIo::new(&io("divseq.inp", "divseq.out"), b"");
        assert!(opens(&names, "Divseq.inp"), "{names:?}");
    }

    #[test]
    fn spellings_in_the_source_are_inputs_even_when_split_by_a_macro() {
        let names =
            SolutionFileIo::new(&io("pathlib.inp", "pathlib.out"), br#"freopen("PathLib.inp","r",stdin);"#);
        assert!(opens(&names, "PathLib.inp"), "{names:?}");
        let split = br#"#define fn "NumBer"
            freopen(fn".Inp", "r", stdin);"#;
        let names = SolutionFileIo::new(&io("number.inp", "number.out"), split);
        assert!(opens(&names, "NumBer.Inp"), "{names:?}");
    }

    #[test]
    fn an_alias_never_leaves_the_box_shadows_a_box_file_or_matches_the_output() {
        let names = SolutionFileIo::new(&io("INPUT", "data.out"), b"");
        assert!(!opens(&names, "input"), "reserved box name: {names:?}");
        let names = SolutionFileIo::new(&io("data.inp", "DATA.INP"), b"");
        assert!(names.aliases.iter().all(|alias| !alias.eq_ignore_ascii_case("data.inp")), "{names:?}");
        let names = SolutionFileIo::new(&io("a.inp", "a.out"), "\"../A.inp\" \u{1F600}a.INP".as_bytes());
        assert!(names.aliases.iter().all(|alias| is_plain_box_name(alias)), "{names:?}");
    }

    #[test]
    fn letter_cases_of_a_name_without_an_extension() {
        assert_eq!(letter_cases("Data"), ["DATA", "Data", "data"].map(String::from).into());
    }
}
