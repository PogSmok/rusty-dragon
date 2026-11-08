//! Legal move generation.
//!
//! Moves are generated legal up front rather than filtered after making them: check evasions
//! are restricted to blocking or capturing the checker, pinned pieces stay on their pin line
//! and the king never steps onto an attacked square.
//! En passant, which removes two pieces from a line at once, is the one move checked by testing the
//! resulting occupancy.

use super::Position;
use crate::board::{
    Bitboard, Castle, Color, Move, MoveKind, MoveList, PieceKind, Square, between, bishop_attacks,
    king_attacks, knight_attacks, line, pawn_attacks, rook_attacks,
};

/// Pieces a pawn can promote to: queen first, then knight, the only underpromotion that's
/// regularly better than a queen, so a search tries them in a sensible order.
const PROMOTIONS: [PieceKind; 4] =
    [PieceKind::Queen, PieceKind::Knight, PieceKind::Rook, PieceKind::Bishop];

impl Position {
    /// Every legal move for the side to move: all captures and promotions first (en passant
    /// included), then all quiet moves (castling included).
    /// No order is guaranteed within each group.
    pub fn legal_moves(&self) -> MoveList {
        // One pass over the pieces, with quiet moves held back and appended after the captures.
        let mut moves = MoveList::new();
        let mut quiets = MoveList::new();
        MoveGen::new(self).generate(&mut moves, &mut quiets);
        moves.append(&quiets);
        moves
    }

    /// Finds the legal move written in UCI notation, e.g. `e2e4`, `e1g1` or `e7e8q`.
    /// Generates every legal move to do it, so it's meant for input, not for search.
    /// To look up several moves in one position, generate once and use [`MoveList::find_uci`].
    #[must_use]
    pub fn parse_uci_move(&self, uci: &str) -> Option<Move> {
        self.legal_moves().find_uci(uci)
    }

    /// The side to move's pawns that can legally capture en passant onto `ep`.
    pub(super) fn legal_en_passant_capturers(&self, ep: Square) -> Bitboard {
        let us = self.side_to_move;
        let them = us.opposite();
        let king = self.king_square(us);
        let captured = Bitboard::from_square(ep).forward(them);

        let mut legal = Bitboard::EMPTY;
        // Our pawns attack `ep` from where an enemy pawn on `ep` would attack.
        for from in pawn_attacks(them, ep) & self.pieces(us, PieceKind::Pawn) {
            let occupied =
                (self.occupied() ^ Bitboard::from_square(from) ^ captured).with_square(ep);
            if self.attackers(king, them, occupied).difference(captured).is_empty() {
                legal = legal.with_square(from);
            }
        }
        legal
    }

    pub(super) fn has_legal_en_passant(&self, ep: Square) -> bool {
        !self.legal_en_passant_capturers(ep).is_empty()
    }
}

/// Adds a move of `kind` from `from` to each square in `targets`.
#[inline]
fn push_moves(moves: &mut MoveList, from: Square, targets: Bitboard, kind: MoveKind) {
    for to in targets {
        moves.push(Move::new(from, to, kind));
    }
}

/// Adds a move of `kind` to each square in `targets`, from the square `delta` indices before it.
#[inline]
fn push_shifted(moves: &mut MoveList, targets: Bitboard, delta: i8, kind: MoveKind) {
    for to in targets {
        moves.push(Move::new(to.wrapping_offset(-delta), to, kind));
    }
}

/// Adds every promotion onto each square in `targets`, from the square `delta` indices before it.
#[inline]
fn push_promotions(moves: &mut MoveList, targets: Bitboard, delta: i8, capture: bool) {
    for to in targets {
        let from = to.wrapping_offset(-delta);
        for piece in PROMOTIONS {
            moves.push(Move::new(from, to, MoveKind::promotion(piece, capture)));
        }
    }
}

/// The enemy attacks that matter for where our king may go.
struct KingDanger {
    /// Squares attacked by enemy pawns, knights and king.
    leapers: Bitboard,
    /// Occupancy without our king.
    occupied: Bitboard,
    /// Enemy bishops and queens.
    diagonal: Bitboard,
    /// Enemy rooks and queens.
    straight: Bitboard,
}

impl KingDanger {
    /// Whether our king would be attacked on `to`.
    /// Leapers first: one bit test that often settles it before any slider lookup.
    #[inline]
    fn attacks(&self, to: Square) -> bool {
        self.leapers.contains(to)
            || bishop_attacks(to, self.occupied).intersects(self.diagonal)
            || rook_attacks(to, self.occupied).intersects(self.straight)
    }
}

/// Everything about the position the generators share, computed once per call.
struct MoveGen<'a> {
    pos: &'a Position,
    us: Color,
    them: Color,
    king: Square,
    occupied: Bitboard,
    ours: Bitboard,
    theirs: Bitboard,
    /// Enemy pieces giving check.
    checkers: Bitboard,
    /// Our pieces that would expose our king to a slider if they left its line.
    pinned: Bitboard,
}

impl<'a> MoveGen<'a> {
    fn new(pos: &'a Position) -> Self {
        let us = pos.side_to_move();
        let them = us.opposite();
        let king = pos.king_square(us);
        let occupied = pos.occupied();
        let ours = pos.pieces_of_color(us);
        let theirs = pos.pieces_of_color(them);

        let mut checkers = (knight_attacks(king) & pos.pieces(them, PieceKind::Knight))
            | (pawn_attacks(us, king) & pos.pieces(them, PieceKind::Pawn));
        let mut pinned = Bitboard::EMPTY;

        // Enemy sliders that see the king through our pieces: with only enemy pieces as
        // blockers, the rays stop at the first enemy piece, so anything in between is ours.
        // None in between is a check, exactly one is a pin.
        let queens = pos.pieces(them, PieceKind::Queen);
        let snipers = (rook_attacks(king, theirs) & (pos.pieces(them, PieceKind::Rook) | queens))
            | (bishop_attacks(king, theirs) & (pos.pieces(them, PieceKind::Bishop) | queens));
        for sniper in snipers {
            let blockers = between(king, sniper) & occupied;
            if blockers.is_empty() {
                checkers = checkers.with_square(sniper);
            } else if blockers.popcount() == 1 {
                pinned |= blockers;
            }
        }

        Self { pos, us, them, king, occupied, ours, theirs, checkers, pinned }
    }

    /// Captures and promotions go to `captures`, all other moves to `quiets`.
    fn generate(&self, captures: &mut MoveList, quiets: &mut MoveList) {
        let danger = self.king_danger();
        let king_targets = self.king_targets(&danger);
        self.push_targets(captures, quiets, self.king, king_targets);
        if self.checkers.popcount() > 1 {
            // Double check: only the king can move.
            return;
        }

        // Where a non-king move may land: anywhere not ours, or onto the check ray when in
        // check, which either blocks it or captures the checker.
        let target = match self.checkers.lsb() {
            None => !self.ours,
            Some(checker) => between(self.king, checker).with_square(checker),
        };

        // Unpinned pawns move together; each pinned pawn is restricted to its own pin line.
        let pawns = self.pos.pieces(self.us, PieceKind::Pawn);
        self.pawn_moves(captures, quiets, pawns.difference(self.pinned), target);
        for from in pawns & self.pinned {
            let pin_target = target & line(self.king, from);
            self.pawn_moves(captures, quiets, Bitboard::from_square(from), pin_target);
        }
        self.en_passant(captures);
        self.piece_moves(captures, quiets, target);
        if self.checkers.is_empty() {
            self.castling(quiets, king_targets, &danger);
        }
    }

    /// Adds a move from `from` to each square in `targets`, splitting captures from quiet moves.
    #[inline]
    fn push_targets(
        &self,
        captures: &mut MoveList,
        quiets: &mut MoveList,
        from: Square,
        targets: Bitboard,
    ) {
        push_moves(captures, from, targets & self.theirs, MoveKind::Capture);
        push_moves(quiets, from, targets.difference(self.theirs), MoveKind::Quiet);
    }

    /// Squares the piece on `from` may move to without exposing the king.
    #[inline]
    fn pin_mask(&self, from: Square) -> Bitboard {
        if self.pinned.contains(from) { line(self.king, from) } else { Bitboard::FULL }
    }

    /// What attacks the squares our king might go to, built once for its moves and castling.
    fn king_danger(&self) -> KingDanger {
        let pos = self.pos;
        let them = self.them;

        // Pawn, knight and king attacks don't depend on occupancy, so map them all at once.
        let pawns = pos.pieces(them, PieceKind::Pawn).forward(them);
        let mut leapers = pawns.west() | pawns.east() | king_attacks(pos.king_square(them));
        for from in pos.pieces(them, PieceKind::Knight) {
            leapers |= knight_attacks(from);
        }

        let queens = pos.pieces(them, PieceKind::Queen);
        KingDanger {
            leapers,
            // Without the king, so a slider checking it also attacks the squares behind it.
            occupied: self.occupied.without_square(self.king),
            diagonal: pos.pieces(them, PieceKind::Bishop) | queens,
            straight: pos.pieces(them, PieceKind::Rook) | queens,
        }
    }

    /// Squares the king can move to without being attacked.
    fn king_targets(&self, danger: &KingDanger) -> Bitboard {
        let mut safe = Bitboard::EMPTY;
        for to in king_attacks(self.king).difference(self.ours) {
            if !danger.attacks(to) {
                safe = safe.with_square(to);
            }
        }
        safe
    }

    /// Knight, bishop, rook and queen moves landing only on `target`.
    fn piece_moves(&self, captures: &mut MoveList, quiets: &mut MoveList, target: Bitboard) {
        let pos = self.pos;
        // A pinned knight can never stay on its pin line.
        for from in pos.pieces(self.us, PieceKind::Knight).difference(self.pinned) {
            self.push_targets(captures, quiets, from, knight_attacks(from) & target);
        }

        let queens = pos.pieces(self.us, PieceKind::Queen);
        for from in pos.pieces(self.us, PieceKind::Bishop) | queens {
            let targets = bishop_attacks(from, self.occupied) & target & self.pin_mask(from);
            self.push_targets(captures, quiets, from, targets);
        }
        for from in pos.pieces(self.us, PieceKind::Rook) | queens {
            let targets = rook_attacks(from, self.occupied) & target & self.pin_mask(from);
            self.push_targets(captures, quiets, from, targets);
        }
    }

    /// Pushes, captures and promotions of all `pawns` at once, landing only on `target`.
    /// Doesn't include en passant.
    fn pawn_moves(
        &self,
        captures: &mut MoveList,
        quiets: &mut MoveList,
        pawns: Bitboard,
        target: Bitboard,
    ) {
        let us = self.us;
        let push = us.pawn_push();
        let west = push - 1;
        let east = push + 1;
        let promotion_rank = us.promotion_rank();

        let advanced = pawns.forward(us);
        let west_captures = advanced.west() & self.theirs & target;
        let east_captures = advanced.east() & self.theirs & target;
        // A double push is a single push onto the en passant rank followed by one more.
        let single = advanced.difference(self.occupied);
        let double = (single & us.en_passant_rank()).forward(us).difference(self.occupied) & target;
        let single = single & target;

        push_promotions(captures, west_captures & promotion_rank, west, true);
        push_promotions(captures, east_captures & promotion_rank, east, true);
        push_promotions(captures, single & promotion_rank, push, false);
        push_shifted(captures, west_captures.difference(promotion_rank), west, MoveKind::Capture);
        push_shifted(captures, east_captures.difference(promotion_rank), east, MoveKind::Capture);
        push_shifted(quiets, single.difference(promotion_rank), push, MoveKind::Quiet);
        push_shifted(quiets, double, 2 * push, MoveKind::DoublePush);
    }

    fn en_passant(&self, moves: &mut MoveList) {
        let Some(ep) = self.pos.en_passant() else { return };
        for from in self.pos.legal_en_passant_capturers(ep) {
            moves.push(Move::new(from, ep, MoveKind::EnPassant));
        }
    }

    /// Only called when not in check, so `danger` seeing through our king doesn't matter: a
    /// slider attacking past it would be giving check.
    /// `king_targets` already says whether the square the king passes over (next to it) is safe;
    /// only the landing square needs checking.
    fn castling(&self, moves: &mut MoveList, king_targets: Bitboard, danger: &KingDanger) {
        let rights = self.pos.castling_rights();
        for castle in Castle::of(self.us) {
            if !rights.contains(castle.right) {
                continue;
            }
            debug_assert_eq!(self.king, castle.king_from, "castling right without the king");
            debug_assert!(
                self.pos.pieces(self.us, PieceKind::Rook).contains(castle.rook_from),
                "castling right without the rook on {}",
                castle.rook_from
            );
            if between(castle.king_from, castle.rook_from).intersects(self.occupied) {
                continue;
            }
            // The king may not pass through or land on an attacked square.
            // The square it passes over is empty, so `king_targets` holds it exactly when it's
            // safe.
            let passed = between(castle.king_from, castle.king_to);
            if !passed.difference(king_targets).is_empty() || danger.attacks(castle.king_to) {
                continue;
            }
            moves.push(Move::new(castle.king_from, castle.king_to, castle.kind));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::positions::{FOOLS_MATE, KIWIPETE, MOST_LEGAL_MOVES};

    fn moves(fen: &str) -> Vec<String> {
        let pos = Position::from_fen(fen).expect("valid FEN");
        let mut moves: Vec<String> = pos.legal_moves().iter().map(Move::to_string).collect();
        moves.sort();
        moves
    }

    fn has(fen: &str, uci: &str) -> bool {
        moves(fen).iter().any(|mv| mv == uci)
    }

    #[test]
    fn startpos_has_twenty_moves() {
        assert_eq!(Position::startpos().legal_moves().len(), 20);
    }

    #[test]
    fn most_moves_known_fit_in_the_list() {
        let pos = Position::from_fen(MOST_LEGAL_MOVES).expect("valid FEN");
        assert_eq!(pos.legal_moves().len(), 218);
    }

    #[test]
    fn captures_and_promotions_come_before_quiet_moves() {
        let fens = [
            // Captures by every piece kind, castling both sides.
            KIWIPETE,
            // En passant next to quiet pawn pushes.
            "rnbqkbnr/ppp1p1pp/8/3pPp2/8/8/PPPP1PPP/RNBQKBNR w KQkq f6 0 3",
            // Promotions, both by push and by capture.
            "1n2k3/P7/8/8/8/8/8/4K3 w - - 0 1",
            // Black: bxa1, bxc1 and b1 promotions, Rxa1, then quiet moves.
            "r3k3/8/8/8/8/8/1p6/R1N1K3 b - - 0 1",
            // Double check by the rook on e8 and knight on d3: only Kxd2 and Kd1.
            "4r1k1/8/8/8/8/3n4/3n4/4K3 w - - 0 1",
        ];
        for fen in fens {
            let pos = Position::from_fen(fen).expect("valid FEN");
            let moves = pos.legal_moves();
            let tactical = |mv: &Move| mv.is_capture() || mv.promotion().is_some();
            let quiet_start = moves.iter().position(|mv| !tactical(mv)).unwrap_or(moves.len());
            assert!(quiet_start > 0, "{fen}: expected some captures");
            let late: Vec<_> =
                moves.as_slice()[quiet_start..].iter().filter(|mv| tactical(mv)).collect();
            assert!(late.is_empty(), "{fen}: {late:?} after the first quiet move");
        }
    }

    #[test]
    fn double_check_allows_only_king_moves() {
        // Rook on e8 and knight on f3 both check the king on e1.
        let fen = "4r1k1/8/8/8/8/5n2/3P4/R3K2R w KQ - 0 1";
        let moves = moves(fen);
        assert!(moves.iter().all(|mv| mv.starts_with("e1")), "{moves:?}");
        assert_eq!(moves, ["e1d1", "e1f1", "e1f2"]);
    }

    #[test]
    fn single_check_is_blocked_or_captured() {
        // Bishop on b4 checks the king on e1; Nc3, Nd2, c3 and Bd2 block, a3 doesn't help.
        let fen = "4k3/8/8/8/1b6/8/2P5/1NB1K3 w - - 0 1";
        let moves = moves(fen);
        assert!(moves.contains(&"c2c3".to_string()) && moves.contains(&"b1c3".to_string()));
        assert!(moves.contains(&"c1d2".to_string()) && moves.contains(&"b1d2".to_string()));
        assert!(!moves.contains(&"c2c4".to_string()));
    }

    #[test]
    fn king_does_not_step_back_along_check_ray() {
        // Rook on a1 checks along rank 1; f1 is still attacked once the king leaves e1.
        let fen = "4k3/8/8/8/8/8/8/r3K3 w - - 0 1";
        assert!(!has(fen, "e1f1") && !has(fen, "e1d1"));
        assert!(has(fen, "e1e2") && has(fen, "e1d2"));
    }

    #[test]
    fn pinned_pieces_stay_on_the_pin_line() {
        // Bishop on e2 is pinned by the rook on e8 and can't move; rook on d1 isn't pinned.
        let fen = "4r1k1/8/8/8/8/8/4B3/3RK3 w - - 0 1";
        assert!(moves(fen).iter().all(|mv| !mv.starts_with("e2")));
        // A rook pinned on a file can slide along it and capture the pinner.
        let fen = "4r1k1/8/8/8/8/8/4R3/4K3 w - - 0 1";
        let rook: Vec<_> = moves(fen).into_iter().filter(|mv| mv.starts_with("e2")).collect();
        assert_eq!(rook, ["e2e3", "e2e4", "e2e5", "e2e6", "e2e7", "e2e8"]);
    }

    #[test]
    fn en_passant_is_generated() {
        let fen = "rnbqkbnr/ppp1p1pp/8/3pPp2/8/8/PPPP1PPP/RNBQKBNR w KQkq f6 0 3";
        assert!(has(fen, "e5f6"));
        assert!(!has(fen, "e5d6"), "d6 is not the en passant square");
    }

    #[test]
    fn en_passant_that_exposes_king_along_rank_is_illegal() {
        // Capturing d6 would remove both pawns from rank 5, exposing the king to the rook.
        let fen = "8/8/8/K2pP2r/8/8/8/7k w - d6 0 1";
        assert!(!has(fen, "e5d6"));
    }

    #[test]
    fn en_passant_capturing_checker_is_legal() {
        // The pawn that just double-pushed to d5 checks the king on e4.
        let fen = "8/8/8/3pP3/4K3/8/8/7k w - d6 0 1";
        assert!(has(fen, "e5d6"));
    }

    #[test]
    fn castling_both_sides() {
        let fen = "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1";
        assert!(has(fen, "e1g1") && has(fen, "e1c1"));
        let fen = "r3k2r/8/8/8/8/8/8/R3K2R b KQkq - 0 1";
        assert!(has(fen, "e8g8") && has(fen, "e8c8"));
    }

    #[test]
    fn castling_needs_rights_empty_path_and_safe_squares() {
        // No rights.
        assert!(!has("r3k2r/8/8/8/8/8/8/R3K2R w - - 0 1", "e1g1"));
        // Knight on b1 blocks queenside but not kingside.
        let fen = "r3k2r/8/8/8/8/8/8/RN2K2R w KQkq - 0 1";
        assert!(!has(fen, "e1c1") && has(fen, "e1g1"));
        // Rook on f8 attacks f1, which the king passes through.
        assert!(!has("4kr2/8/8/8/8/8/8/R3K2R w KQ - 0 1", "e1g1"));
        // Rook on b8 attacks b1, which only the rook passes: queenside is still legal.
        assert!(has("1r2k3/8/8/8/8/8/8/R3K2R w KQ - 0 1", "e1c1"));
        // In check from the rook on e8, no castling at all.
        let fen = "4r1k1/8/8/8/8/8/8/R3K2R w KQ - 0 1";
        assert!(!has(fen, "e1g1") && !has(fen, "e1c1"));
    }

    #[test]
    fn rook_returning_home_does_not_restore_castling() {
        let mut pos = Position::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1").expect("valid");
        for uci in ["h1h2", "a8a7", "h2h1", "a7a8"] {
            let mv = pos.parse_uci_move(uci).expect("legal");
            pos.make_move(mv);
        }
        // Both rooks are home again, but each side lost the right on the side it moved.
        assert_eq!(pos.parse_uci_move("e1g1"), None);
        assert!(pos.parse_uci_move("e1c1").is_some());
        pos.make_move(pos.parse_uci_move("e1d1").expect("legal"));
        assert_eq!(pos.parse_uci_move("e8c8"), None);
        assert!(pos.parse_uci_move("e8g8").is_some());
    }

    #[test]
    fn promotions_include_every_piece() {
        let fen = "1n2k3/P7/8/8/8/8/8/4K3 w - - 0 1";
        for uci in ["a7a8q", "a7a8r", "a7a8b", "a7a8n", "a7b8q", "a7b8n"] {
            assert!(has(fen, uci), "{uci}");
        }
    }

    #[test]
    fn checkmate_and_stalemate_have_no_moves() {
        assert_eq!(moves(FOOLS_MATE), Vec::<String>::new());
        let stalemate = "7k/5Q2/6K1/8/8/8/8/8 b - - 0 1";
        assert_eq!(moves(stalemate), Vec::<String>::new());
    }

    #[test]
    fn pinned_pawns_move_only_along_the_pin() {
        let pawn = |fen: &str, from: &str| -> Vec<String> {
            moves(fen).into_iter().filter(|mv| mv.starts_with(from)).collect()
        };
        // Pinned on its file by the rook on e8: both pushes stay on the line.
        assert_eq!(pawn("4r1k1/8/8/8/8/8/4P3/4K3 w - - 0 1", "e2"), ["e2e3", "e2e4"]);
        // Pinned on a diagonal by the bishop on f3: only capturing it is legal.
        assert_eq!(pawn("6k1/8/8/8/8/5b2/4P3/3K4 w - - 0 1", "e2"), ["e2f3"]);
        // Pinned on its rank by the rook on h2: nothing.
        assert_eq!(pawn("6k1/8/8/8/8/3p4/K3P2r/8 w - - 0 1", "e2"), Vec::<String>::new());
        // Black pawn pinned on a diagonal, capturing towards White's side.
        assert_eq!(pawn("8/8/2k5/3p4/4B3/8/8/K7 b - - 0 1", "d5"), ["d5e4"]);
    }

    #[test]
    fn parse_uci_move_finds_legal_moves_only() {
        let pos = Position::startpos();
        let mv = pos.parse_uci_move("e2e4").expect("legal");
        assert!(mv.is_double_push());
        for bad in ["e2e5", "", "e2", "e2e", "e2e4q", "nonsense", "é2e4", "e2e4 "] {
            assert_eq!(pos.parse_uci_move(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn parse_uci_move_selects_the_promotion() {
        let pos = Position::from_fen("1n2k3/P7/8/8/8/8/8/4K3 w - - 0 1").expect("valid FEN");
        let mv = pos.parse_uci_move("a7b8r").expect("legal");
        assert_eq!(mv.promotion(), Some(PieceKind::Rook));
        assert!(mv.is_capture());
        assert_eq!(pos.parse_uci_move("a7a8"), None, "promotion piece is required");
        assert_eq!(pos.parse_uci_move("a7a8k"), None);
        assert_eq!(pos.parse_uci_move("a7b8R"), Some(mv), "uppercase is accepted");
        assert_eq!(pos.parse_uci_move("A7B8R"), Some(mv), "uppercase is accepted");
    }
}
