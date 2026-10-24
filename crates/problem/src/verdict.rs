//! Case statuses as the bridge stores them (`CaseStatus` minus none):
//! AC accepted, WA wrong answer, TLE time limit, MLE memory limit, OLE output limit,
//! RTE runtime error (a signal or an uncaught exception), IR invalid return (non-zero exit),
//! SC short-circuited (skipped because an earlier case or batch decided the outcome).

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
