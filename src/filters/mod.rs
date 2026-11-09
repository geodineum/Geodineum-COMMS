//! Message filter module (spam detection, etc.)

mod spam;

pub use spam::{from_visitor, verdict, SpamFilter, SpamVerdict};
