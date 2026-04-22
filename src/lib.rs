#![allow(
    clippy::collapsible_if,
    clippy::io_other_error,
    clippy::manual_clamp,
    clippy::manual_div_ceil,
    clippy::manual_is_multiple_of,
    clippy::needless_return,
    clippy::never_loop,
    clippy::non_octal_unix_permissions,
    clippy::suspicious_open_options,
    clippy::too_many_arguments,
    clippy::unnecessary_cast,
    clippy::useless_vec,
    clippy::while_let_on_iterator
)]

pub mod app;
pub mod boot_partition;
pub mod cli;
pub mod diagnostics;
pub mod discard;
pub mod engine;
pub mod filemap;
pub mod geometry;
pub mod health;
pub mod health_compare;
pub mod integrity;
pub mod lba_trace;
pub mod presets;
pub mod profile;
pub mod report;
pub mod storage;
pub mod system;
pub mod ui;
