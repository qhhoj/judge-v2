//! One unpinned worker for `run` and `selftest`.

use judge_daemon::runtimes::{available, detect_versions};
use judge_grader::artefacts::{ArtefactSetup, Artefacts};
use judge_grader::generate::{Control, GenerationFailure, GenerationJob, generate_tests};
use judge_grader::judge::{GraderDefaults, JudgeEnv, Outcome, Submission, judge_submission};
use judge_grader::languages::LanguageRegistry;
use judge_grader::report::Reporter;
use judge_problem::data_version::{FsStamps, WaitPolicy};
use judge_problem::dir_cache::DirCache;
use judge_problem::resolve::Resolver;
use judge_sandbox::{IsolateBox, SandboxConfig};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Same limits the daemon's `config.example.toml` ships.
const DEFAULTS: GraderDefaults =
    GraderDefaults { default_output_limit: 25_165_824, default_output_prefix: 64, jvm_reserve_kib: 131_072 };
const CACHE_BYTES: u64 = 4 << 30;
const DATA_WAIT: Duration = Duration::from_millis(200);

pub struct LocalJudge {
    env: JudgeEnv,
    cache: Arc<DirCache>,
    pub registry: Arc<LanguageRegistry>,
    _work: tempfile::TempDir,
}

impl LocalJudge {
    /// Boxes `box_base` and `box_base + 1`; the default 900 never touches a running daemon's boxes.
    pub fn new(languages: &Path, testlib: &Path, box_base: u32) -> anyhow::Result<LocalJudge> {
        let registry = LanguageRegistry::from_toml(&std::fs::read_to_string(languages)?)?;
        let versions = detect_versions(&registry);
        let registry = Arc::new(available(registry, &versions));
        let work = tempfile::tempdir()?;
        let cache = Arc::new(DirCache::new(work.path().join("cache"), CACHE_BYTES));
        let sandbox = SandboxConfig { meta_dir: work.path().join("meta"), ..Default::default() };
        let setup = ArtefactSetup {
            cache: cache.clone(),
            registry: registry.clone(),
            build_ids: versions,
            // isolate mounts it by absolute path
            testlib_dir: testlib.canonicalize()?,
        };
        let scratch = work.path().join("scratch");
        std::fs::create_dir_all(&scratch)?;
        let env = JudgeEnv {
            solution: IsolateBox::open(&sandbox, box_base, None)?,
            helper: IsolateBox::open(&sandbox, box_base + 1, None)?,
            artefacts: Arc::new(Artefacts::new(setup)),
            stamps: Arc::new(FsStamps),
            scratch,
            defaults: DEFAULTS,
        };
        Ok(LocalJudge { env, cache, registry, _work: work })
    }

    pub fn generate(&mut self, job: &GenerationJob, control: &Control) -> Result<(), GenerationFailure> {
        generate_tests(&mut self.env, job, control)
    }

    /// `problem_dir`'s last segment is the problem code; its parent is the data root.
    pub fn judge(
        &mut self,
        problem_dir: &Path,
        submission: &Submission,
        reporter: &mut dyn Reporter,
    ) -> anyhow::Result<Outcome> {
        let code = problem_dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("bad problem dir"))?;
        let root = problem_dir.parent().ok_or_else(|| anyhow::anyhow!("problem dir has no parent"))?;
        let resolver = Resolver {
            root: root.into(),
            cache: self.cache.clone(),
            wait: WaitPolicy { timeout: DATA_WAIT, poll: Duration::from_millis(20) },
            zip: Default::default(),
            stamps: Arc::new(FsStamps),
        };
        let problem = resolver.resolve(code, 0)?;
        Ok(judge_submission(&mut self.env, &problem, submission, reporter))
    }
}
