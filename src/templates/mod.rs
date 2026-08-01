//! Template rendering module

mod renderer;
pub mod tg_escape;

pub use renderer::TemplateRenderer;
pub use tg_escape::escape_markdown_v2;
