//! Static dashboard browser assets.

mod css;
mod server_script;
mod static_script;

pub(super) use css::DASHBOARD_CSS;
pub(super) use server_script::DASHBOARD_SERVER_SCRIPT;
pub(super) use static_script::DASHBOARD_SCRIPT;

pub(super) const DASHBOARD_INLINE_MARKDOWN_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/dashboard_inline_markdown.js"
));
