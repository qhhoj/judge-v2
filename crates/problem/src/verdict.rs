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
