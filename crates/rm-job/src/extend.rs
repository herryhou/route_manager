//! The extend walk lives in `rm_osm_match::extend`; this shim keeps
//! `rm_job::extend::plan_extension` (the CLI's dispatch and tests) working.

pub use rm_osm_match::extend::{plan_extension, Extension};
