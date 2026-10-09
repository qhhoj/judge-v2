//! init.yml model, case plan, verdict model, `.data-version` and directory caches.

pub mod init;
pub mod verdict;

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "judge-problem");
    }
}
