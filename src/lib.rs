pub mod board;
pub mod perft;
pub mod positions;
pub mod uci;

pub const ENGINE_NAME: &str = "Rusty Dragon";
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ENGINE_AUTHOR: &str = "PogSmok";
