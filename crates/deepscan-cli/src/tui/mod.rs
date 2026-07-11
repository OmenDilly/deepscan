//! Interactive terminal views. `tree` is the explore browser; `widget`,
//! `action`, and `render` (added in later tasks) back the shared sized-list.

mod tree;
mod widget;

pub use tree::run;
