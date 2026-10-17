//! Source inventory for the AOT port of Code-OSS.
//!
//! `w3cos port analyze <tree>` writes a JSON clearance list: Electron APIs,
//! Node built-ins, native modules from the roadmap table, and dynamic code
//! sites (`eval`, `new Function`, dynamic `require` / `import`, `vm`).

mod report;
mod scan;

pub use report::{Report, SCHEMA, analyze, parse_npmrc};
