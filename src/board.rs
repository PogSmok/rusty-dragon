//! Board representation.

mod attacks;
mod bitboard;
mod castling_rights;
mod color;
mod file;
mod piece;
mod piece_kind;
mod position;
mod rank;
mod square;

pub use attacks::{
    bishop_attacks, king_attacks, knight_attacks, pawn_attacks, queen_attacks, rook_attacks,
};
pub use bitboard::{Bitboard, BitboardIter};
pub use castling_rights::CastlingRights;
pub use color::Color;
pub use file::File;
pub use piece::Piece;
pub use piece_kind::PieceKind;
pub use position::{FenError, Position};
pub use rank::Rank;
pub use square::{ParseSquareError, Square};
