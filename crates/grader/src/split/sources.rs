//! The problem files a split build reads: the entry and the header, under checked names, and the header parsed.

use crate::artefacts::read_data_source;
use crate::error::GradeError;
use crate::program::SourceFile;
use crate::signature::safe_name;
use crate::split::USER_SOURCE;
use judge_interface::{Interface, parse_header};
use judge_problem::init::SignatureSpec;
use judge_problem::resolve::ResolvedProblem;

/// Every file the judge writes beside the header starts with this, so a header name cannot shadow one.
pub const OWN_PREFIX: &str = "qhhoj";

pub struct SplitSources {
    pub entry: SourceFile,
    pub header: SourceFile,
    pub interface: Interface,
}

/// The header keeps its init.yml name (the sources include it by name); it must be plain and not shadow a file
/// the judge writes. A header outside grammar v1 is a problem fault (IE), named with line, column and construct.
pub fn split_sources(problem: &ResolvedProblem, spec: &SignatureSpec) -> Result<SplitSources, GradeError> {
    for name in [&spec.entry, &spec.header] {
        if !safe_name(name) || name.starts_with(OWN_PREFIX) || [USER_SOURCE, "main"].contains(&name.as_str())
        {
            return Err(GradeError::Checker(format!("bad signature file name {name:?}")));
        }
    }
    let entry = read_data_source(problem, &spec.entry)?;
    let header = read_data_source(problem, &spec.header)?;
    let text = std::str::from_utf8(&header.bytes)
        .map_err(|_| GradeError::Checker(format!("{} is not UTF-8", spec.header)))?;
    let interface =
        parse_header(&spec.header, text).map_err(|refusal| GradeError::Checker(refusal.to_string()))?;
    Ok(SplitSources { entry, header, interface })
}
