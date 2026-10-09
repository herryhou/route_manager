//! rm-edit: manual-edit domain and application services. Zero SQL —
//! persistence lives in rm-db; this crate orchestrates and decides.

mod rename;

pub use rename::*;
