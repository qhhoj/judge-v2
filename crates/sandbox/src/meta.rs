//! isolate's `--meta` file: `key:value` lines.

use crate::error::SandboxError;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub status: Option<String>,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub time_s: f64,
    pub wall_s: f64,
    pub cg_mem_kib: u64,
    pub oom_killed: bool,
    pub message: String,
}

fn number<T: std::str::FromStr>(key: &str, value: &str) -> Result<T, SandboxError> {
    value.parse().map_err(|_| SandboxError::Meta(format!("{key}:{value}")))
}

fn apply(meta: &mut Meta, key: &str, value: &str) -> Result<(), SandboxError> {
    match key {
        "status" => meta.status = Some(value.to_string()),
        "exitcode" => meta.exit_code = Some(number(key, value)?),
        "exitsig" => meta.exit_signal = Some(number(key, value)?),
        "time" => meta.time_s = number(key, value)?,
        "time-wall" => meta.wall_s = number(key, value)?,
        "cg-mem" => meta.cg_mem_kib = number(key, value)?,
        "cg-oom-killed" => meta.oom_killed = value == "1",
        "message" => meta.message = value.to_string(),
        _ => {}
    }
    Ok(())
}

pub fn parse_meta(text: &str) -> Result<Meta, SandboxError> {
    let mut meta = Meta::default();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let (key, value) = line.split_once(':').ok_or_else(|| SandboxError::Meta(line.into()))?;
        apply(&mut meta, key, value)?;
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_isolate_2_6_oom_meta_skipping_unknown_keys_and_refuses_bad_numbers() {
        let meta = parse_meta(
            "time:0.014\ntime-wall:0.014\nmax-rss:14616\ncsw-voluntary:4\ncsw-forced:5\ncg-mem:8192\n\
             cg-oom-killed:1\nexitsig:9\nstatus:SG\nmessage:Caught fatal signal 9\n",
        )
        .unwrap();
        assert_eq!(meta.status.as_deref(), Some("SG"));
        assert_eq!(meta.exit_signal, Some(9));
        assert!(meta.oom_killed);
        assert_eq!(meta.cg_mem_kib, 8192);
        assert!((meta.time_s - 0.014).abs() < 1e-9);
        assert_eq!(meta.message, "Caught fatal signal 9");
        assert!(matches!(parse_meta("time:fast\n"), Err(SandboxError::Meta(_))));
    }
}
