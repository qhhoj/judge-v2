//! The C++ runtime both generated sides are linked with (`runtime/qhhoj_rpc.hpp`, `runtime/qhhoj_rpc.cpp`), and
//! the names the judge shares with it. The wire format and how each side ends are documented in the two files.

/// Box file name of the runtime header, which the generated sources include.
pub const HEADER_NAME: &str = "qhhoj_rpc.hpp";
pub const SOURCE_NAME: &str = "qhhoj_rpc.cpp";
pub const HEADER: &str = include_str!("../runtime/qhhoj_rpc.hpp");
pub const SOURCE: &str = include_str!("../runtime/qhhoj_rpc.cpp");

/// The file the jury writes in its box (`/box/<name>`) before it exits on a frame from the contestant that
/// breaks the protocol.
pub const PROTOCOL_FAULT_FILE: &str = ".qhhoj-fault";
/// The jury's charge record in its box: two native-endian u64, the nanoseconds of jury CPU spent serving the
/// contestant's calls so far, and the jury's process CPU clock (ns) when the call now being served began (0:
/// none). Absent when the jury served no call.
pub const CHARGE_FILE: &str = ".qhhoj-charge";
