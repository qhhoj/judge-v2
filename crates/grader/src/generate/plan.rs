//! The generation job as the grader sees it. `daemon::wire` converts the protocol's `GenerateTests`.

use judge_problem::init::{FileIo, is_plain_box_name};
use std::collections::HashSet;
use std::path::{Component, Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Unspecified,
    Generator,
    Validator,
    Solution,
    /// Compiled only, in a compile check.
    Checker,
    /// Compiled only, in a compile check.
    Interactor,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanProgram {
    pub name: String,
    pub role: Role,
    pub language_key: String,
    /// Inside the bundle; its directory's files are copied beside it when compiling.
    pub source_path: String,
}

/// One test: a manual input of the bundle, or a generator run. Exactly one of the two is set.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlanTest {
    pub index: u32,
    pub input_name: String,
    pub answer_name: String,
    pub group: String,
    pub manual_path: String,
    pub generator: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TestSource<'a> {
    Manual(&'a str),
    Generated { generator: &'a str, args: &'a [String] },
}

impl PlanTest {
    /// None when the test names both a manual file and a generator, or neither.
    pub fn source(&self) -> Option<TestSource<'_>> {
        match (self.manual_path.is_empty(), self.generator.is_empty()) {
            (false, true) => Some(TestSource::Manual(&self.manual_path)),
            (true, false) => Some(TestSource::Generated { generator: &self.generator, args: &self.args }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ProgramLimits {
    pub time_s: f64,
    pub wall_s: f64,
    pub memory_kib: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PlanLimits {
    pub generator: ProgramLimits,
    pub validator: ProgramLimits,
    pub solution: ProgramLimits,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_upload_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct GenerationPlan {
    pub id: String,
    pub bundle_size: u64,
    pub deadline_s: u32,
    pub limits: PlanLimits,
    pub programs: Vec<PlanProgram>,
    pub tests: Vec<PlanTest>,
    /// The solution's input and output files instead of stdin and stdout.
    pub file_io: Option<FileIo>,
}

impl GenerationPlan {
    pub fn program(&self, name: &str) -> Option<&PlanProgram> {
        self.programs.iter().find(|program| program.name == name)
    }

    pub fn validator(&self) -> Option<&PlanProgram> {
        self.programs.iter().find(|program| program.role == Role::Validator)
    }

    pub fn solution(&self) -> Option<&PlanProgram> {
        self.programs.iter().find(|program| program.role == Role::Solution)
    }
}

/// A relative path with only plain segments: nothing absolute, no `..`, no `.`.
pub fn is_safe_relative(path: &str) -> bool {
    !path.is_empty() && Path::new(path).components().all(|part| matches!(part, Component::Normal(_)))
}

/// A generation without tests only compiles its programs, which are checkers and interactors.
pub fn is_compile_check(plan: &GenerationPlan) -> bool {
    plan.tests.is_empty()
}

fn check_unique_named_roles(plan: &GenerationPlan) -> Result<(), String> {
    let mut names = HashSet::new();
    for program in &plan.programs {
        if program.name.is_empty() || !names.insert(program.name.as_str()) {
            return Err(format!("program name {:?} is empty or repeated", program.name));
        }
        if program.role == Role::Unspecified {
            return Err(format!("program {} has no role", program.name));
        }
    }
    Ok(())
}

fn is_compile_only(role: Role) -> bool {
    matches!(role, Role::Checker | Role::Interactor)
}

fn check_compile_programs(plan: &GenerationPlan) -> Result<(), String> {
    check_unique_named_roles(plan)?;
    if plan.programs.is_empty() {
        return Err("no tests and no programs".into());
    }
    match plan.programs.iter().find(|program| !is_compile_only(program.role)) {
        Some(program) => {
            Err(format!("compile check: program {} is not a checker or an interactor", program.name))
        }
        None => Ok(()),
    }
}

fn check_programs(plan: &GenerationPlan) -> Result<(), String> {
    check_unique_named_roles(plan)?;
    if let Some(program) = plan.programs.iter().find(|program| is_compile_only(program.role)) {
        return Err(format!(
            "program {} is a checker or an interactor, which only a compile check takes",
            program.name
        ));
    }
    let count = |role| plan.programs.iter().filter(|program| program.role == role).count();
    if count(Role::Solution) != 1 {
        return Err("exactly one solution program is required".into());
    }
    if count(Role::Validator) > 1 {
        return Err("at most one validator program is allowed".into());
    }
    Ok(())
}

fn check_test(plan: &GenerationPlan, test: &PlanTest) -> Result<(), String> {
    let index = test.index;
    match test.source() {
        None => return Err(format!("test {index} needs exactly one of manual_path and generator")),
        Some(TestSource::Manual(path)) if !is_safe_relative(path) => {
            return Err(format!("test {index}: unsafe manual path {path:?}"));
        }
        Some(TestSource::Generated { generator, .. })
            if plan.program(generator).is_none_or(|program| program.role != Role::Generator) =>
        {
            return Err(format!("test {index}: {generator:?} is not a generator program"));
        }
        Some(_) => {}
    }
    Ok(())
}

fn check_tests(plan: &GenerationPlan) -> Result<(), String> {
    let mut entries = HashSet::new();
    for test in &plan.tests {
        check_test(plan, test)?;
        for name in [&test.input_name, &test.answer_name] {
            if !is_safe_relative(name) || !entries.insert(name.as_str()) {
                return Err(format!("test {}: entry name {name:?} is unsafe or repeated", test.index));
            }
        }
    }
    Ok(())
}

/// The reason the plan cannot be run, if any. Nothing is compiled or run for an invalid plan.
pub fn check_plan(plan: &GenerationPlan) -> Result<(), String> {
    if plan.deadline_s == 0 {
        return Err("deadline_s is 0".into());
    }
    if let Some(io) = &plan.file_io
        && !(is_plain_box_name(&io.input) && is_plain_box_name(&io.output))
    {
        return Err("file names must be plain box file names".into());
    }
    if is_compile_check(plan) {
        return check_compile_programs(plan);
    }
    check_programs(plan)?;
    check_tests(plan)
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn program(name: &str, role: Role) -> PlanProgram {
        PlanProgram {
            name: name.into(),
            role,
            language_key: "CPP17".into(),
            source_path: format!("files/{name}.cpp"),
        }
    }

    pub fn generated(index: u32, generator: &str) -> PlanTest {
        PlanTest {
            index,
            input_name: format!("{index:02}.inp"),
            answer_name: format!("{index:02}.out"),
            generator: generator.into(),
            args: vec!["5".into()],
            ..Default::default()
        }
    }

    pub fn plan() -> GenerationPlan {
        GenerationPlan {
            id: "g1".into(),
            bundle_size: 1,
            deadline_s: 60,
            limits: PlanLimits::default(),
            programs: vec![program("gen", Role::Generator), program("sol", Role::Solution)],
            tests: vec![generated(1, "gen"), generated(2, "gen")],
            file_io: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    fn refused(edit: impl FnOnce(&mut GenerationPlan)) -> String {
        let mut plan = plan();
        edit(&mut plan);
        check_plan(&plan).unwrap_err()
    }

    #[test]
    fn a_complete_plan_is_accepted() {
        assert!(check_plan(&plan()).is_ok());
    }

    #[test]
    fn a_test_has_exactly_one_source() {
        assert!(refused(|p| p.tests[0].manual_path = "tests/01".into()).contains("exactly one"));
        assert!(refused(|p| p.tests[0].generator.clear()).contains("exactly one"));
        assert!(refused(|p| p.tests[0].generator = "sol".into()).contains("not a generator"));
        assert!(refused(|p| p.tests[0].generator = "nope".into()).contains("not a generator"));
    }

    #[test]
    fn programs_need_one_solution_at_most_one_validator_and_unique_names() {
        assert!(refused(|p| p.programs.retain(|x| x.role != Role::Solution)).contains("solution"));
        assert!(refused(|p| p.programs.push(program("sol2", Role::Solution))).contains("solution"));
        let two_validators = |p: &mut GenerationPlan| {
            p.programs.push(program("v1", Role::Validator));
            p.programs.push(program("v2", Role::Validator));
        };
        assert!(refused(two_validators).contains("validator"));
        assert!(refused(|p| p.programs.push(program("gen", Role::Generator))).contains("repeated"));
        assert!(refused(|p| p.programs[0].role = Role::Unspecified).contains("no role"));
    }

    #[test]
    fn entry_names_and_manual_paths_stay_inside_their_directory() {
        assert!(refused(|p| p.tests[0].input_name = "../x".into()).contains("unsafe"));
        assert!(refused(|p| p.tests[1].answer_name = "01.inp".into()).contains("repeated"));
        assert!(refused(|p| p.tests[0] = manual("/etc/passwd")).contains("unsafe manual path"));
        assert!(refused(|p| p.tests[0] = manual("tests/../../x")).contains("unsafe manual path"));
        assert!(
            refused(|p| p.file_io = Some(FileIo { input: "a/b".into(), output: "o".into() }))
                .contains("plain")
        );
        assert!(refused(|p| p.deadline_s = 0).contains("deadline"));
    }

    fn compile_check() -> GenerationPlan {
        GenerationPlan {
            tests: vec![],
            programs: vec![program("check", Role::Checker), program("talk", Role::Interactor)],
            ..plan()
        }
    }

    #[test]
    fn a_plan_without_tests_is_a_compile_check_of_checkers_and_interactors_only() {
        assert!(check_plan(&compile_check()).is_ok());
        let refused = |edit: fn(&mut GenerationPlan)| {
            let mut plan = compile_check();
            edit(&mut plan);
            check_plan(&plan).unwrap_err()
        };
        assert!(refused(|p| p.programs.clear()).contains("no programs"));
        assert!(refused(|p| p.programs.push(program("sol", Role::Solution))).contains("not a checker"));
        assert!(refused(|p| p.programs.push(program("gen", Role::Generator))).contains("not a checker"));
        assert!(refused(|p| p.programs.push(program("check", Role::Checker))).contains("repeated"));
        assert!(refused(|p| p.deadline_s = 0).contains("deadline"));
    }

    #[test]
    fn a_plan_with_tests_refuses_checker_and_interactor_programs() {
        assert!(refused(|p| p.programs.push(program("check", Role::Checker))).contains("compile check"));
        assert!(refused(|p| p.programs.push(program("talk", Role::Interactor))).contains("compile check"));
    }

    fn manual(path: &str) -> PlanTest {
        PlanTest { manual_path: path.into(), ..generated(1, "") }
    }
}
