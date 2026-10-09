//! init.yml model, case plan, verdict model, `.data-version` and directory caches.

pub mod archive;
pub mod code;
pub mod data_version;
pub mod dir_cache;
pub mod init;
pub mod plan;
pub mod resolve;
pub mod short_circuit;
pub mod verdict;

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "judge-problem");
    }
}
