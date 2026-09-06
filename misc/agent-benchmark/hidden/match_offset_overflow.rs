use aho_corasick::{Match, PatternID};

#[test]
#[should_panic]
fn hidden_match_offset_checks_overflow_like_span_offset() {
    let m = Match::new(PatternID::ZERO, (usize::MAX - 1)..usize::MAX);
    let _ = m.offset(1);
}
