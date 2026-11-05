//! Board representation.

mod attacks;
mod bitboard;
mod castling_rights;
mod chess_move;
mod color;
mod file;
mod move_list;
mod piece;
mod piece_kind;
mod position;
mod rank;
mod square;

pub use attacks::{
    between, bishop_attacks, king_attacks, knight_attacks, line, pawn_attacks, queen_attacks,
    rook_attacks,
};
pub use bitboard::{Bitboard, BitboardIter};
pub use castling_rights::{Castle, CastlingRights};
pub use chess_move::{Move, MoveKind};
pub use color::Color;
pub use file::File;
pub use move_list::MoveList;
pub use piece::Piece;
pub use piece_kind::PieceKind;
pub use position::{FenError, Position};
pub use rank::Rank;
pub use square::{ParseSquareError, Square};
