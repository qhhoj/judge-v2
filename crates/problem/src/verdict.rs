//! Case statuses as the bridge stores them (`CaseStatus` minus none).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ac,
    Wa,
    Tle,
    Mle,
    Ole,
    Rte,
    Ir,
    Sc,
}

impl Status {
    pub fn code(self) -> &'static str {
        match self {
            Status::Ac => "AC",
            Status::Wa => "WA",
            Status::Tle => "TLE",
            Status::Mle => "MLE",
            Status::Ole => "OLE",
            Status::Rte => "RTE",
            Status::Ir => "IR",
            Status::Sc => "SC",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Status;

    #[test]
    fn codes_match_the_bridge_case_statuses() {
        let all = [
            Status::Ac,
            Status::Wa,
            Status::Tle,
            Status::Mle,
            Status::Ole,
            Status::Rte,
            Status::Ir,
            Status::Sc,
        ];
        let codes: Vec<_> = all.iter().map(|s| s.code()).collect();
        assert_eq!(codes, ["AC", "WA", "TLE", "MLE", "OLE", "RTE", "IR", "SC"]);
    }
}
