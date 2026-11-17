//! The board screen's header shows the session key the board is bound to (docs/tui.md
//! section 4.1, docs/board-v2.md section 5.1): `workspace › title › key`, the key dimmed;
//! when the text is too wide, the key is shortened to 8 characters and `…`, then left
//! out, and only then is the title cut. The right-hand indicators are never cut.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use super::v2_state::v2_snapshot;
use super::*;
use crate::run::render_text;
use crate::text::width;
use crate::view;

const UUID: &str = "3f2a9c1e-7b4d-4e2a-9c51-0d8e6f1a2b3c";
const SHORT: &str = "3f2a9c1e…";
/// The right-hand side of `v2_snapshot`'s header.
const RIGHT: &str = "提問 3  討論 1  ● 已連線";
const HEIGHT: u16 = 30;

fn keyed(key: &str) -> BoardSnapshot {
    let mut snap = v2_snapshot();
    snap.board.session_key = key.into();
    snap
}

fn header(snap: BoardSnapshot, w: u16) -> String {
    let app = app_with(snap, (w, HEIGHT));
    render_text(&app, w, HEIGHT)
        .lines()
        .next()
        .unwrap()
        .to_string()
}

fn buffer(snap: BoardSnapshot, w: u16) -> Buffer {
    let app = app_with(snap, (w, HEIGHT));
    let area = Rect::new(0, 0, w, HEIGHT);
    let mut buf = Buffer::empty(area);
    view::render(&app, area, &mut buf);
    buf
}

/// The cell where `text` starts on row 0, by display column.
fn find(buf: &Buffer, text: &str) -> Option<u16> {
    let want: Vec<char> = text.chars().collect();
    (0..buf.area.width).find(|&x| {
        let mut cx = x;
        for ch in &want {
            if cx >= buf.area.width || buf[(cx, 0)].symbol() != ch.to_string() {
                return false;
            }
            cx += width(buf[(cx, 0)].symbol()).max(1) as u16;
        }
        true
    })
}

/// The narrowest screen on which `left` (with its two-cell gap) and the right-hand side
/// fit side by side.
fn needed(left: &str) -> u16 {
    (width(left) + 2 + width(RIGHT) + 1) as u16
}

#[test]
fn a_wide_header_shows_the_full_key_after_the_title() {
    let full = format!(" Acme_DevOps › 修正登入逾時 › {UUID}");
    let h = header(keyed(UUID), 120);
    assert!(h.starts_with(&format!("{full}  ")), "{h}");
    assert!(h.ends_with(RIGHT), "{h}");
    // Exactly at the edge, the full key still fits.
    let w = needed(&full);
    let h = header(keyed(UUID), w);
    assert!(h.starts_with(&full), "{h}");
    assert!(h.ends_with(RIGHT), "{h}");
}

#[test]
fn a_medium_header_shortens_the_key() {
    let full = format!(" Acme_DevOps › 修正登入逾時 › {UUID}");
    let short = format!(" Acme_DevOps › 修正登入逾時 › {SHORT}");
    for w in [needed(&full) - 1, 80, needed(&short)] {
        let h = header(keyed(UUID), w);
        assert!(h.starts_with(&format!("{short}  ")), "{w}: {h}");
        assert!(!h.contains(UUID), "{w}: {h}");
        assert!(h.ends_with(RIGHT), "{w}: {h}");
    }
}

#[test]
fn a_narrow_header_leaves_the_key_out_and_keeps_the_title() {
    let short = format!(" Acme_DevOps › 修正登入逾時 › {SHORT}");
    for w in [needed(&short) - 1, 62] {
        let h = header(keyed(UUID), w);
        assert!(h.starts_with(" Acme_DevOps › 修正登入逾時  "), "{w}: {h}");
        assert!(!h.contains("3f2a9c1e"), "{w}: {h}");
        assert_eq!(h.matches('›').count(), 1, "{w}: {h}");
        assert!(!h.contains('…'), "{w}: {h}");
        assert!(h.ends_with(RIGHT), "{w}: {h}");
    }
}

#[test]
fn the_title_is_cut_as_before_once_the_key_is_gone() {
    let long = |key: &str| {
        let mut snap = keyed(key);
        snap.board.title = "修正登入逾時並補上整合測試與文件".into();
        snap
    };
    let h = header(long(UUID), 60);
    assert!(h.starts_with(" Acme_DevOps › 修正登入"), "{h}");
    assert!(h.contains('…'), "{h}");
    assert!(!h.contains("3f2a9c1e"), "{h}");
    assert!(h.ends_with(RIGHT), "{h}");
    // The same cut as a board without a key.
    assert_eq!(h, header(long(""), 60));
}

#[test]
fn an_untitled_board_shows_the_key_once() {
    let mut snap = keyed(UUID);
    snap.board.title = "  ".into();
    let h = header(snap, 120);
    assert!(h.starts_with(&format!(" Acme_DevOps › {UUID}  ")), "{h}");
    assert_eq!(h.matches(UUID).count(), 1, "{h}");
    assert_eq!(h.matches('›').count(), 1, "{h}");
    assert!(h.ends_with(RIGHT), "{h}");
}

#[test]
fn a_short_key_is_shown_as_it_is() {
    let h = header(keyed("demo-p0"), 80);
    assert!(h.starts_with(" Acme_DevOps › 修正登入逾時 › demo-p0  "), "{h}");
    assert!(!h.contains('…'), "{h}");
    // Nine characters are never shortened: the key fits whole or is left out.
    let nine = " Acme_DevOps › 修正登入逾時 › abcdefghi";
    let h = header(keyed("abcdefghi"), needed(nine));
    assert!(h.starts_with(nine), "{h}");
    let h = header(keyed("abcdefghi"), needed(nine) - 1);
    assert!(h.starts_with(" Acme_DevOps › 修正登入逾時  "), "{h}");
    assert!(!h.contains("abcdefgh"), "{h}");
    // Ten characters are.
    let ten = " Acme_DevOps › 修正登入逾時 › abcdefghij";
    let h = header(keyed("abcdefghij"), needed(ten) - 1);
    assert!(
        h.starts_with(" Acme_DevOps › 修正登入逾時 › abcdefgh…  "),
        "{h}"
    );
}

#[test]
fn the_key_is_dimmed_and_the_rest_keeps_its_style() {
    let buf = buffer(keyed(UUID), 120);
    let k = find(&buf, UUID).unwrap();
    for x in k..k + UUID.len() as u16 {
        assert_eq!(buf[(x, 0)].fg, Color::DarkGray, "column {x}");
        assert!(!buf[(x, 0)].modifier.contains(Modifier::BOLD), "column {x}");
    }
    // The second separator, before the key, is plain like the first.
    assert_eq!(buf[(k - 2, 0)].symbol(), "›");
    assert_eq!(buf[(k - 2, 0)].fg, Color::Reset);
    let t = find(&buf, "修正登入逾時").unwrap();
    assert!(buf[(t, 0)].modifier.contains(Modifier::BOLD));
    assert_eq!(buf[(t, 0)].fg, Color::Reset);
    let ws = find(&buf, "Acme_DevOps").unwrap();
    assert_eq!(buf[(ws, 0)].fg, Color::Reset);
    // The shortened key is dimmed too.
    let buf = buffer(keyed(UUID), 80);
    let k = find(&buf, SHORT).unwrap();
    assert_eq!(buf[(k, 0)].fg, Color::DarkGray);
    assert_eq!(buf[(k + 8, 0)].fg, Color::DarkGray, "the ellipsis");
}

#[test]
fn the_right_hand_indicators_are_unchanged() {
    for w in [150, 93, 80, 66, 62, 60] {
        let with_key = buffer(keyed(UUID), w);
        let without = buffer(keyed(""), w);
        let from = w - (width(RIGHT) as u16 + 1);
        for x in from..w {
            assert_eq!(with_key[(x, 0)], without[(x, 0)], "width {w}, column {x}");
        }
        assert_eq!(find(&with_key, "提問 3"), Some(from), "width {w}");
    }
}

#[test]
fn other_screens_do_not_show_the_key() {
    // The card screen.
    let mut app = app_with(keyed(UUID), (120, HEIGHT));
    press(&mut app, &[Key::Enter]);
    deliver_card(&mut app, detail(1, "內容", vec![], vec![]));
    assert!(app.card.is_some());
    let text = render_text(&app, 120, HEIGHT);
    assert!(!text.contains("3f2a9c1e"), "{text}");
    // The epic screen.
    let mut app = app_with(keyed(UUID), (120, HEIGHT));
    press(&mut app, &[Key::Left, Key::Enter]);
    assert!(app.epic.is_some());
    let text = render_text(&app, 120, HEIGHT);
    assert!(!text.contains("3f2a9c1e"), "{text}");
}
