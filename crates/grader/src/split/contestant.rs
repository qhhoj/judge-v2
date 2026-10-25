//! The contestant program of a split problem, built in the solution box per submission: the untouched
//! submission (header forced in, its `main` renamed, as the one-process grader does), the contestant glue and
//! the runtime. The entry never enters this box.

use crate::compile::CompileJob;
use crate::languages::Language;
use crate::program::SourceFile;
use crate::split::sources::SplitSources;
use crate::split::{USER_SOURCE, compiler_command};
use judge_interface::Role;
use judge_interface::glue::contestant::contestant_glue;
use judge_interface::runtime;

/// The submission's compile job; `None` when the language has no `signature` compiler.
pub fn contestant_job(
    language: &Language,
    sources: &SplitSources,
    roles: &[Role],
    source: &[u8],
) -> Option<CompileJob> {
    let cc = compiler_command(language.signature.as_ref()?);
    let header = &sources.header.name;
    let glue = contestant_glue(&sources.interface, roles, header);
    let script = format!(
        "{cc} -c {USER_SOURCE} -o qhhoj_user.o -include {header} -Dmain=qhhoj_user_main && \
         {cc} -c qhhoj_contestant.cpp -o qhhoj_contestant.o && {cc} -c {rpc} -o qhhoj_rpc.o && \
         {cc} qhhoj_user.o qhhoj_contestant.o qhhoj_rpc.o -o main -lm",
        rpc = runtime::SOURCE_NAME,
    );
    let text = |name: &str, text: &str| SourceFile { name: name.into(), bytes: text.as_bytes().to_vec() };
    let mut job = CompileJob::for_language(language, source);
    job.argv = Some(vec!["/bin/sh".into(), "-c".into(), script]);
    job.files = vec![
        SourceFile { name: USER_SOURCE.into(), bytes: source.to_vec() },
        sources.header.clone(),
        text("qhhoj_contestant.cpp", &glue),
        text(runtime::HEADER_NAME, runtime::HEADER),
        text(runtime::SOURCE_NAME, runtime::SOURCE),
    ];
    job.keep = Some(vec!["main".into()]);
    Some(job)
}
