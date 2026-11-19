//! Legal move generation.
//!
//! Moves are generated legal up front rather than filtered after making them: check evasions
//! are restricted to blocking or capturing the checker, pinned pieces stay on their pin line
//! and the king never steps onto an attacked square.
//! En passant, which removes two pieces from a line at once, is the one move checked by testing the
//! resulting occupancy.

use std::cell::OnceCell;

use super::Position;
use crate::board::{
    Bitboard, Castle, Color, Move, MoveKind, MoveList, PieceKind, Square, between, bishop_attacks,
    king_attacks, knight_attacks, line, pawn_attacks, queen_attacks, rook_attacks,
};

/// Pieces a pawn can promote to: queen first, then knight, the only underpromotion that's
/// regularly better than a queen, so a search tries them in a sensible order.
const PROMOTIONS: [PieceKind; 4] =
    [PieceKind::Queen, PieceKind::Knight, PieceKind::Rook, PieceKind::Bishop];

/// What determines whether a move gives check, computed once per position rather than per move.
#[derive(Clone, Copy, Debug)]
pub struct CheckInfo {
    their_king: Square,
    /// By `PieceKind`: the squares from which a piece of ours of that kind attacks their king.
    squares: [Bitboard; 6],
    /// Our pieces that alone stand between one of our sliders and their king: moving one off
    /// that line uncovers a check.
    discoverers: Bitboard,
}

impl Position {
    /// Every legal move for the side to move: all captures and promotions first (en passant
    /// included), then all quiet moves (castling included).
    ///
    /// No order is guaranteed within each group.
    pub fn legal_moves(&self) -> MoveList {
        let mut moves = MoveList::new();
        self.push_legal_moves(&mut moves);
        moves
    }

    /// Appends every legal move to `moves`, in the order of [`Position::legal_moves`].
    pub fn push_legal_moves(&self, moves: &mut MoveList) {
        MoveGenerator::new(self).push_all(moves);
    }

    /// The legal captures and promotions, en passant included.
    #[cfg(test)]
    pub(crate) fn legal_tactical_moves(&self) -> MoveList {
        let mut moves = MoveList::new();
        MoveGenerator::new(self).push_tactical(&mut moves);
        moves
    }

    /// Whether `mv` is legal here.
    ///
    /// `mv` may be any move, even another position's.
    /// See [`MoveGenerator::is_legal`].
    #[must_use]
    pub fn is_legal(&self, mv: Move) -> bool {
        MoveGenerator::new(self).is_legal(mv)
    }

    /// Finds the legal move written in UCI notation, e.g. `e2e4`, `e1g1` or `e7e8q`.
    ///
    /// Generates every legal move to do it, so it's meant for input, not for search.
    /// To look up several moves in one position, generate once and use [`MoveList::find_uci`].
    #[must_use]
    pub fn parse_uci_move(&self, uci: &str) -> Option<Move> {
        self.legal_moves().find_uci(uci)
    }

    /// The side to move's pawns that can legally capture en passant onto `ep` (the en passant
    /// square).
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

    /// What [`Position::gives_check_with`] needs, for every move of this position.
    #[must_use]
    pub fn check_info(&self) -> CheckInfo {
        let us = self.side_to_move;
        let them = us.opposite();
        let king = self.king_square(them);
        let occupied = self.occupied();
        let diagonal = bishop_attacks(king, occupied);
        let straight = rook_attacks(king, occupied);
        let squares = [
            // Where our pawn attacks the king from is where their pawn on it would attack.
            pawn_attacks(them, king),
            knight_attacks(king),
            diagonal,
            straight,
            diagonal | straight,
            Bitboard::EMPTY,
        ];

        // A lone piece of ours between one of our snipers and the king uncovers check.
        let mut discoverers = Bitboard::EMPTY;
        for sniper in self.snipers(king, us, us) {
            let blockers = between(king, sniper) & occupied;
            if blockers.popcount() == 1 {
                discoverers |= blockers;
            }
        }
        CheckInfo { their_king: king, squares, discoverers }
    }

    /// `attacker`'s sliders that would see `king` if `through`'s pieces were not there.
    ///
    /// Only the other side's pieces block the rays, so
    /// anything between a sniper and the king is `through`'s.
    /// When `through` is the king's side, one piece between is pinned.
    /// When it is the attacker's own, one piece between uncovers check by moving.
    #[inline]
    fn snipers(&self, king: Square, attacker: Color, through: Color) -> Bitboard {
        let stoppers = self.pieces_of_color(through.opposite());
        ((rook_attacks(king, stoppers) & self.straight_sliders())
            | (bishop_attacks(king, stoppers) & self.diagonal_sliders()))
            & self.pieces_of_color(attacker)
    }

    /// Whether `mv`, legal here, checks the opponent's king.
    ///
    /// Builds the [`CheckInfo`] each call; [`Position::gives_check_with`] reuses one across moves.
    #[cfg(test)]
    pub(crate) fn gives_check(&self, mv: Move) -> bool {
        self.gives_check_with(mv, &self.check_info())
    }

    /// Whether `mv`, legal here, checks the opponent's king.
    ///
    /// Found without making the move: the moved piece attacks the king from its new square,
    /// or it uncovers a slider that does.
    /// `info` is this position's [`Position::check_info`].
    ///
    /// # Panics
    ///
    /// May panic if `mv` is not legal here.
    #[must_use]
    #[inline]
    pub fn gives_check_with(&self, mv: Move, info: &CheckInfo) -> bool {
        let (from, to) = (mv.from(), mv.to());
        // Both move or take a second piece: checked against the occupancy after the move, the
        // rook from its new square, and our sliders through the squares the move empties.
        if mv.is_castle() || mv.is_en_passant() {
            let us = self.side_to_move;
            let ours = self.pieces_of_color(us);
            let (mut occupied, mut straight) = (self.occupied(), self.straight_sliders() & ours);
            occupied = occupied.without_square(from).with_square(to);
            if mv.is_castle() {
                let castle = Castle::landing_on(us, to);
                occupied = occupied.without_square(castle.rook_from).with_square(castle.rook_to);
                straight = straight.without_square(castle.rook_from).with_square(castle.rook_to);
            } else {
                occupied = occupied.without_square(mv.en_passant_victim());
                if info.squares[PieceKind::Pawn.index()].contains(to) {
                    return true;
                }
            }
            let diagonal = self.diagonal_sliders() & ours;
            return bishop_attacks(info.their_king, occupied).intersects(diagonal)
                || rook_attacks(info.their_king, occupied).intersects(straight);
        }
        let direct = match mv.promotion() {
            None => {
                let kind =
                    self.piece_at(from).expect("the source square should hold a piece").kind();
                info.squares[kind.index()].contains(to)
            }
            // The pawn leaving may open the promoted piece's line to the king.
            Some(kind) => {
                let occupied = self.occupied().without_square(from);
                piece_attacks(kind, to, occupied).contains(info.their_king)
            }
        };
        direct || (info.discoverers.contains(from) && !line(info.their_king, from).contains(to))
    }
}

/// Squares a knight, bishop, rook or queen on `from` attacks, with `occupied` as the blockers.
#[inline]
fn piece_attacks(kind: PieceKind, from: Square, occupied: Bitboard) -> Bitboard {
    match kind {
        PieceKind::Knight => knight_attacks(from),
        PieceKind::Bishop => bishop_attacks(from, occupied),
        PieceKind::Rook => rook_attacks(from, occupied),
        PieceKind::Queen => queen_attacks(from, occupied),
        PieceKind::Pawn | PieceKind::King => panic!("{kind:?} is neither a knight nor a slider"),
    }
}

/// Where `us`'s `pawns` can push to among `occupied`: one square ahead, and two.
///
/// A double push is a single push onto the en passant rank followed by one more.
#[inline]
fn pawn_pushes(pawns: Bitboard, us: Color, occupied: Bitboard) -> (Bitboard, Bitboard) {
    let single = pawns.forward(us).difference(occupied);
    let double = (single & us.en_passant_rank()).forward(us).difference(occupied);
    (single, double)
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
#[derive(Clone, Copy, Debug)]
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
    /// The squares of `candidates`, next to the king, it can move to without being attacked.
    fn safe(&self, candidates: Bitboard) -> Bitboard {
        let mut safe = Bitboard::EMPTY;
        for to in candidates {
            if !self.attacks(to) {
                safe = safe.with_square(to);
            }
        }
        safe
    }

    /// Whether our king would be attacked on `to`.
    ///
    /// Leapers first: one bit test that often settles it before any slider lookup.
    #[inline]
    fn attacks(&self, to: Square) -> bool {
        self.leapers.contains(to)
            || bishop_attacks(to, self.occupied).intersects(self.diagonal)
            || rook_attacks(to, self.occupied).intersects(self.straight)
    }
}

/// Generates the legal moves of a position in parts, on demand, sharing what the parts have in
/// common: the checkers and pins are found once, and the map of where the king may go when
/// first needed.
#[derive(Debug)]
pub struct MoveGenerator<'a> {
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
    /// What attacks the squares the king may go to: the costliest part, built when first
    /// needed.
    danger: OnceCell<KingDanger>,
}

impl<'a> MoveGenerator<'a> {
    #[must_use]
    pub fn new(pos: &'a Position) -> Self {
        let us = pos.side_to_move();
        let them = us.opposite();
        let king = pos.king_square(us);
        let occupied = pos.occupied();
        let ours = pos.pieces_of_color(us);
        let theirs = pos.pieces_of_color(them);

        let mut checkers = (knight_attacks(king) & pos.pieces(them, PieceKind::Knight))
            | (pawn_attacks(us, king) & pos.pieces(them, PieceKind::Pawn));
        let mut pinned = Bitboard::EMPTY;

        // None between an enemy sniper and the king is a check, exactly one is a pin.
        for sniper in pos.snipers(king, them, us) {
            let blockers = between(king, sniper) & occupied;
            if blockers.is_empty() {
                checkers = checkers.with_square(sniper);
            } else if blockers.popcount() == 1 {
                pinned |= blockers;
            }
        }

        let danger = OnceCell::new();
        Self { pos, us, them, king, occupied, ours, theirs, checkers, pinned, danger }
    }

    /// The king's danger map, built on first use.
    #[inline]
    fn danger(&self) -> &KingDanger {
        self.danger.get_or_init(|| self.king_danger())
    }

    /// Appends every legal move to `moves`: the captures and promotions, then the others.
    pub fn push_all(&self, moves: &mut MoveList) {
        // One pass over the pieces, with quiet moves held back and appended after the captures.
        let mut quiets = MoveList::new();
        self.generate::<true, true>(moves, &mut quiets);
        moves.append(&quiets);
    }

    /// Appends the legal captures and promotions, en passant included, to `moves`.
    pub fn push_tactical(&self, moves: &mut MoveList) {
        let mut unused = MoveList::new();
        self.generate::<true, false>(moves, &mut unused);
        debug_assert!(unused.is_empty(), "tactical generation must not produce quiet moves");
    }

    /// Appends the legal moves that are neither captures nor promotions, castling included,
    /// to `moves`.
    pub fn push_quiet(&self, moves: &mut MoveList) {
        let mut unused = MoveList::new();
        self.generate::<false, true>(&mut unused, moves);
        debug_assert!(unused.is_empty(), "quiet generation must not produce tactical moves");
    }

    /// Whether `mv` is legal. `mv` may be any move, even another position's.
    ///
    /// A few bitboard tests: where the piece reaches, where check and pins
    /// let it go, and whether the move's kind is the one it would be here.
    #[must_use]
    #[expect(clippy::missing_panics_doc, reason = "only our pieces are looked up")]
    pub fn is_legal(&self, mv: Move) -> bool {
        let (from, to) = (mv.from(), mv.to());
        if !self.ours.contains(from) {
            return false;
        }
        // Both move or take a second piece, as the generator checks them.
        if mv.is_castle() {
            // A foreign castle gets the queenside fallback, which the comparison then rejects.
            let castle = Castle::landing_on(self.us, to);
            return mv == Move::new(castle.king_from, castle.king_to, castle.kind)
                && self.checkers.is_empty()
                && self.castle_path_clear(castle)
                && {
                    let danger = self.danger();
                    let passed = between(castle.king_from, castle.king_to);
                    danger.safe(passed) == passed && !danger.attacks(castle.king_to)
                };
        }
        if mv.is_en_passant() {
            return self.pos.en_passant() == Some(to)
                && mv == Move::new(from, to, MoveKind::EnPassant)
                && self.pos.legal_en_passant_capturers(to).contains(from);
        }
        let us = self.us;
        let occupied = self.occupied;
        let capture = self.theirs.contains(to);
        let plain = if capture { MoveKind::Capture } else { MoveKind::Quiet };
        let kind = self.pos.piece_at(from).expect("the source square should hold our piece").kind();
        let (reach, expected) = match kind {
            PieceKind::King => {
                let reach = king_attacks(from).difference(self.ours);
                if !reach.contains(to) {
                    return false;
                }
                return !self.danger().attacks(to) && mv == Move::new(from, to, plain);
            }
            PieceKind::Pawn => {
                let (single, double) = pawn_pushes(Bitboard::from_square(from), us, occupied);
                let reach = (pawn_attacks(us, from) & self.theirs) | single | double;
                let expected = if us.promotion_rank().contains(to) {
                    let Some(promotion) = mv.promotion() else { return false };
                    MoveKind::promotion(promotion, capture)
                } else if double.contains(to) {
                    MoveKind::DoublePush
                } else {
                    plain
                };
                (reach, expected)
            }
            // A pinned knight can never stay on its pin line.
            PieceKind::Knight if self.pinned.contains(from) => return false,
            PieceKind::Knight | PieceKind::Bishop | PieceKind::Rook | PieceKind::Queen => {
                (piece_attacks(kind, from, occupied), plain)
            }
        };
        // In double check only the king moves; in check, a move must block it or take the
        // checker.
        let Some(target) = self.target() else { return false };
        (reach & target & self.pin_mask(from)).contains(to) && mv == Move::new(from, to, expected)
    }

    /// With `TACTICAL`, captures and promotions go to `captures`.
    ///
    /// With `QUIETS`, all other moves go to `quiets`. A group left out is not generated.
    fn generate<const TACTICAL: bool, const QUIETS: bool>(
        &self,
        captures: &mut MoveList,
        quiets: &mut MoveList,
    ) {
        // Only the king's squares of the groups asked for are worth checking for safety.
        let candidates = match (TACTICAL, QUIETS) {
            (true, true) => king_attacks(self.king).difference(self.ours),
            (true, false) => king_attacks(self.king) & self.theirs,
            (false, _) => king_attacks(self.king).difference(self.occupied),
        };
        // The danger map is the costliest part: skipped
        // when the king cannot move or has no square to try.
        // Castling needs it too, but without an empty square next to
        // the king, the one it would pass over is not empty either.
        let king = (!candidates.is_empty()).then(|| {
            let danger = self.danger();
            (danger, danger.safe(candidates))
        });
        let king_targets = king.map_or(Bitboard::EMPTY, |(_, targets)| targets);
        self.push_targets::<TACTICAL, QUIETS>(captures, quiets, self.king, king_targets);
        // Double check: only the king can move.
        let Some(target) = self.target() else { return };

        // Unpinned pawns move together; each pinned pawn is restricted to its own pin line.
        let pawns = self.pos.pieces(self.us, PieceKind::Pawn);
        let unpinned = pawns.difference(self.pinned);
        self.pawn_moves::<TACTICAL, QUIETS>(captures, quiets, unpinned, target);
        for from in pawns & self.pinned {
            let pin_target = target & line(self.king, from);
            let pawn = Bitboard::from_square(from);
            self.pawn_moves::<TACTICAL, QUIETS>(captures, quiets, pawn, pin_target);
        }
        if TACTICAL {
            self.en_passant(captures);
        }
        self.piece_moves::<TACTICAL, QUIETS>(captures, quiets, target);
        if let Some((danger, _)) = king.filter(|_| QUIETS && self.checkers.is_empty()) {
            self.castling(quiets, king_targets, danger);
        }
    }

    /// Adds a move from `from` to each square in `targets`, splitting captures from quiet moves.
    #[inline]
    fn push_targets<const TACTICAL: bool, const QUIETS: bool>(
        &self,
        captures: &mut MoveList,
        quiets: &mut MoveList,
        from: Square,
        targets: Bitboard,
    ) {
        if TACTICAL {
            push_moves(captures, from, targets & self.theirs, MoveKind::Capture);
        }
        if QUIETS {
            push_moves(quiets, from, targets.difference(self.theirs), MoveKind::Quiet);
        }
    }

    /// Where a non-king move may land: anywhere not ours, or onto the check
    /// ray when in check, which either blocks it or captures the checker.
    ///
    /// `None` in double check, when only the king moves.
    #[inline]
    fn target(&self) -> Option<Bitboard> {
        match self.checkers.lsb() {
            _ if self.checkers.popcount() > 1 => None,
            None => Some(!self.ours),
            Some(checker) => Some(between(self.king, checker).with_square(checker)),
        }
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

        KingDanger {
            leapers,
            // Without the king, so a slider checking it also attacks the squares behind it.
            occupied: self.occupied.without_square(self.king),
            diagonal: pos.diagonal_sliders() & self.theirs,
            straight: pos.straight_sliders() & self.theirs,
        }
    }

    /// Knight, bishop, rook and queen moves landing only on `target`.
    fn piece_moves<const TACTICAL: bool, const QUIETS: bool>(
        &self,
        captures: &mut MoveList,
        quiets: &mut MoveList,
        target: Bitboard,
    ) {
        let pos = self.pos;
        // Only the squares of the groups asked for are worth looking up.
        let target = match (TACTICAL, QUIETS) {
            (true, true) => target,
            (true, false) => target & self.theirs,
            (false, _) => target.difference(self.theirs),
        };
        let ours = self.ours;
        // A pinned knight can never stay on its pin line.
        for from in (pos.pieces_of_kind(PieceKind::Knight) & ours).difference(self.pinned) {
            let targets = knight_attacks(from) & target;
            self.push_targets::<TACTICAL, QUIETS>(captures, quiets, from, targets);
        }

        for from in pos.diagonal_sliders() & ours {
            let targets = bishop_attacks(from, self.occupied) & target & self.pin_mask(from);
            self.push_targets::<TACTICAL, QUIETS>(captures, quiets, from, targets);
        }
        for from in pos.straight_sliders() & ours {
            let targets = rook_attacks(from, self.occupied) & target & self.pin_mask(from);
            self.push_targets::<TACTICAL, QUIETS>(captures, quiets, from, targets);
        }
    }

    /// Pushes, captures and promotions of all `pawns` at once, landing only on `target`.
    ///
    /// Doesn't include en passant.
    /// Captures and promotions only with `TACTICAL`, the other pushes only with `QUIETS`.
    fn pawn_moves<const TACTICAL: bool, const QUIETS: bool>(
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
        let (single, double) = pawn_pushes(pawns, us, self.occupied);
        let (single, double) = (single & target, double & target);

        if TACTICAL {
            push_promotions(captures, west_captures & promotion_rank, west, true);
            push_promotions(captures, east_captures & promotion_rank, east, true);
            push_promotions(captures, single & promotion_rank, push, false);
            let (west_rest, east_rest) = (
                west_captures.difference(promotion_rank),
                east_captures.difference(promotion_rank),
            );
            push_shifted(captures, west_rest, west, MoveKind::Capture);
            push_shifted(captures, east_rest, east, MoveKind::Capture);
        }
        if QUIETS {
            push_shifted(quiets, single.difference(promotion_rank), push, MoveKind::Quiet);
            push_shifted(quiets, double, 2 * push, MoveKind::DoublePush);
        }
    }

    fn en_passant(&self, moves: &mut MoveList) {
        let Some(ep) = self.pos.en_passant() else { return };
        for from in self.pos.legal_en_passant_capturers(ep) {
            moves.push(Move::new(from, ep, MoveKind::EnPassant));
        }
    }

    /// Only called when not in check, so `danger` seeing through our king doesn't matter: a
    /// slider attacking past it would be giving check.
    ///
    /// `king_targets` already says whether the square the king passes over (next to it) is safe;
    /// only the landing square needs checking.
    fn castling(&self, moves: &mut MoveList, king_targets: Bitboard, danger: &KingDanger) {
        for castle in Castle::of(self.us) {
            if !self.castle_path_clear(castle) {
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

    /// Whether `castle`'s right is held and nothing stands between its king and rook.
    ///
    /// The squares the king crosses must still be checked.
    fn castle_path_clear(&self, castle: Castle) -> bool {
        if !self.pos.castling_rights().contains(castle.right) {
            return false;
        }
        debug_assert_eq!(self.king, castle.king_from, "castling right must have its king in place");
        debug_assert!(
            self.pos.pieces(self.us, PieceKind::Rook).contains(castle.rook_from),
            "castling right must have its rook on {}",
            castle.rook_from
        );
        !between(castle.king_from, castle.rook_from).intersects(self.occupied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::positions::{FOOLS_MATE, KIWIPETE, MOST_LEGAL_MOVES};

    fn moves(fen: &str) -> Vec<String> {
        let pos = Position::from_fen(fen).expect("FEN should be valid");
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
        let pos = Position::from_fen(MOST_LEGAL_MOVES).expect("FEN should be valid");
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
            let pos = Position::from_fen(fen).expect("FEN should be valid");
            let moves = pos.legal_moves();
            let tactical = |mv: &Move| mv.is_capture() || mv.promotion().is_some();
            let quiet_start = moves.iter().position(|mv| !tactical(mv)).unwrap_or(moves.len());
            assert!(quiet_start > 0, "{fen}: expected some captures");
            let late: Vec<_> =
                moves.as_slice()[quiet_start..].iter().filter(|mv| tactical(mv)).collect();
            assert!(late.is_empty(), "{fen}: {late:?} after the first quiet move");
        }
    }

    /// Checks at every node `depth` plies below `pos` that the tactical moves are exactly the
    /// captures and promotions of the full list, in the same order.
    fn check_tactical_moves(pos: &Position, depth: u32) {
        let all = pos.legal_moves();
        let tactical: Vec<Move> =
            all.iter().copied().filter(|mv| mv.is_capture() || mv.promotion().is_some()).collect();
        assert_eq!(pos.legal_tactical_moves().as_slice(), tactical, "{}", pos.to_fen());
        // One generator, its king map shared with a legality check, generates the same moves
        // in parts.
        let generator = MoveGenerator::new(pos);
        if let Some(&last) = all.last() {
            assert!(generator.is_legal(last));
        }
        let mut staged = MoveList::new();
        generator.push_tactical(&mut staged);
        generator.push_quiet(&mut staged);
        assert_eq!(staged.as_slice(), all.as_slice(), "{}", pos.to_fen());
        if depth > 0 {
            for &mv in &all {
                let mut child = pos.clone();
                child.make_move(mv);
                check_tactical_moves(&child, depth - 1);
            }
        }
    }

    /// Checks at every node `depth` plies below `pos` that `gives_check` agrees with making
    /// each move.
    fn check_gives_check(pos: &Position, depth: u32) {
        for &mv in &pos.legal_moves() {
            let mut child = pos.clone();
            child.make_move(mv);
            assert_eq!(pos.gives_check(mv), child.in_check(), "{mv} in {}", pos.to_fen());
            if depth > 0 {
                check_gives_check(&child, depth - 1);
            }
        }
    }

    #[test]
    fn gives_check_matches_making_the_move() {
        use crate::positions::{POSITION_3, POSITION_4, POSITION_5, POSITION_6};
        for fen in [KIWIPETE, POSITION_3, POSITION_4, POSITION_5, POSITION_6] {
            check_gives_check(&Position::from_fen(fen).expect("FEN should be valid"), 2);
        }
        // Discovered check by a rook behind a moving knight, and by promotion.
        let pos =
            Position::from_fen("4k3/8/8/4N3/8/8/8/4RK2 w - - 0 1").expect("FEN should be valid");
        assert!(pos.gives_check(pos.parse_uci_move("e5c4").expect("move should be legal")));
        let pos =
            Position::from_fen("1k6/4P3/8/8/8/8/8/4K3 w - - 0 1").expect("FEN should be valid");
        assert!(pos.gives_check(pos.parse_uci_move("e7e8q").expect("move should be legal")));
        assert!(!pos.gives_check(pos.parse_uci_move("e7e8n").expect("move should be legal")));
        // Castling checks with the rook, on either side, and en passant directly or by
        // uncovering the rook behind both pawns.
        for (fen, uci, check) in [
            ("5k2/8/8/8/8/8/8/4K2R w K - 0 1", "e1g1", true),
            ("3k4/8/8/8/8/8/8/R3K3 w Q - 0 1", "e1c1", true),
            ("6k1/8/8/8/8/8/8/4K2R w K - 0 1", "e1g1", false),
            ("8/2k5/8/3pP3/8/8/8/4K3 w - d6 0 1", "e5d6", true),
            ("8/8/8/R2pP2k/8/8/8/4K3 w - d6 0 1", "e5d6", true),
            ("8/8/8/3pP3/8/8/8/k3K3 w - d6 0 1", "e5d6", false),
        ] {
            let pos = Position::from_fen(fen).expect("FEN should be valid");
            let mv = pos.parse_uci_move(uci).expect("move should be legal");
            let mut child = pos.clone();
            child.make_move(mv);
            assert_eq!(child.in_check(), check, "{uci} in {fen}");
            assert_eq!(pos.gives_check(mv), check, "{uci} in {fen}");
        }
    }

    #[test]
    fn tactical_moves_are_the_captures_and_promotions() {
        use crate::positions::{POSITION_3, POSITION_4, POSITION_5};
        for fen in [KIWIPETE, POSITION_3, POSITION_4, POSITION_5] {
            check_tactical_moves(&Position::from_fen(fen).expect("FEN should be valid"), 2);
        }
    }

    #[test]
    fn is_legal_agrees_with_the_legal_moves() {
        use crate::positions::{POSITION_3, POSITION_4, POSITION_5};
        for fen in [KIWIPETE, POSITION_3, POSITION_4, POSITION_5] {
            let pos = Position::from_fen(fen).expect("FEN should be valid");
            let legal = pos.legal_moves();
            // The same side's moves two plies on: some legal here, most not.
            let mut candidates = Vec::new();
            for &mv in &legal {
                let mut child = pos.clone();
                child.make_move(mv);
                for &reply in &child.legal_moves() {
                    let mut grandchild = child.clone();
                    grandchild.make_move(reply);
                    candidates.extend(grandchild.legal_moves().iter().copied());
                }
            }
            candidates.push(Move::NULL);
            for mv in candidates {
                assert_eq!(pos.is_legal(mv), legal.contains(&mv), "{mv} in {fen}");
            }
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
        let mut pos = Position::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1")
            .expect("FEN should be valid");
        for uci in ["h1h2", "a8a7", "h2h1", "a7a8"] {
            let mv = pos.parse_uci_move(uci).expect("move should be legal");
            pos.make_move(mv);
        }
        // Both rooks are home again, but each side lost the right on the side it moved.
        assert_eq!(pos.parse_uci_move("e1g1"), None);
        assert!(pos.parse_uci_move("e1c1").is_some());
        pos.make_move(pos.parse_uci_move("e1d1").expect("move should be legal"));
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
        let mv = pos.parse_uci_move("e2e4").expect("move should be legal");
        assert!(mv.is_double_push());
        for bad in ["e2e5", "", "e2", "e2e", "e2e4q", "nonsense", "é2e4", "e2e4 "] {
            assert_eq!(pos.parse_uci_move(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn parse_uci_move_selects_the_promotion() {
        let pos =
            Position::from_fen("1n2k3/P7/8/8/8/8/8/4K3 w - - 0 1").expect("FEN should be valid");
        let mv = pos.parse_uci_move("a7b8r").expect("move should be legal");
        assert_eq!(mv.promotion(), Some(PieceKind::Rook));
        assert!(mv.is_capture());
        assert_eq!(pos.parse_uci_move("a7a8"), None, "promotion piece is required");
        assert_eq!(pos.parse_uci_move("a7a8k"), None);
        assert_eq!(pos.parse_uci_move("a7b8R"), Some(mv), "uppercase is accepted");
        assert_eq!(pos.parse_uci_move("A7B8R"), Some(mv), "uppercase is accepted");
    }
}
