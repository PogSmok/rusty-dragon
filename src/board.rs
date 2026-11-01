//! Board representation.

mod bitboard;
mod castling_rights;
mod color;
mod file;
mod piece;
mod piece_kind;
mod position;
mod rank;
mod square;

pub use bitboard::{Bitboard, BitboardIter};
pub use castling_rights::CastlingRights;
pub use color::Color;
pub use file::File;
pub use piece::Piece;
pub use piece_kind::PieceKind;
pub use position::{FenError, Position};
pub use rank::Rank;
pub use square::{ParseSquareError, Square};
