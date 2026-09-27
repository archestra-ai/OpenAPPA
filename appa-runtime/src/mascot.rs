//! The OpenAPPA mascot, for the one place a terminal should show it: a
//! finished install.
//!
//! The grid is the website's, pixel for pixel — `BEAST` in
//! `website/components/Logo.tsx`, which the `<appa-mark>` custom element also
//! draws. Keeping the same bitmap means the mark a person meets in the
//! terminal is the one on the site, not a second drawing that drifts from it.
//!
//! Two pixel rows share one terminal cell as a half block, so 22 rows render
//! in 11 lines. The three tones are the website's: the body takes the
//! terminal's own text colour, the muzzle and paws a grey beside it, and the
//! eyes and nose are holes in the grid rather than a colour, so they come out
//! as the terminal's background. That is what makes the mark follow a light or
//! dark terminal the way the SVG follows a light or dark page — naming a
//! colour for the body would fix it to one of them and lose it in the other.

use crate::style::Style;

const COLS: usize = 24;

/// `1` body, `3` muzzle and paws, `2` nose, `4` eyes, `.` outside the mark.
const BEAST: [&str; 22] = [
    ".....11..........11.....",
    ".....11..........11.....",
    "....1111111111111111....",
    "...111111111111111111...",
    "...111111111111111111...",
    "...111111111111111111...",
    "...111444111111444111...",
    "...111444111111444111...",
    "...111444111111444111...",
    "...111111111111111111...",
    "...111111133331111111...",
    "...111111132231111111...",
    "...111111111111111111...",
    "....1111111111111111....",
    ".1111111111111111111111.",
    "111111111111111111111111",
    "111111111111111111111111",
    "111111111111111111111111",
    "111111111111111111111111",
    "111111111111111111111111",
    "11111..1111..1111..11111",
    "33333..3333..3333..33333",
];

/// What a pixel is made of. A hole is not a tone: it is the absence of one,
/// and the terminal's background stands in for it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Body,
    Grey,
}

fn tone(pixel: u8) -> Option<Tone> {
    match pixel {
        b'1' => Some(Tone::Body),
        b'3' => Some(Tone::Grey),
        _ => None,
    }
}

/// How one cell is painted. Grouping neighbours that share this is what keeps
/// a line to a few escapes instead of one for every block in it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Paint {
    /// The terminal's own text colour, which needs no escape at all.
    Bare,
    Grey,
    /// Grey behind a glyph whose own half stays the text colour.
    OverGrey,
}

fn cell(over: Option<Tone>, under: Option<Tone>) -> (Paint, char) {
    match (over, under) {
        (Some(Tone::Grey), Some(Tone::Grey)) => (Paint::Grey, '\u{2588}'),
        (Some(Tone::Body), Some(Tone::Body)) => (Paint::Bare, '\u{2588}'),
        // Two tones in one cell: the glyph keeps the upper half in the
        // terminal's text colour and the grey goes behind it.
        (Some(_), Some(Tone::Grey)) => (Paint::OverGrey, '\u{2580}'),
        (Some(_), Some(Tone::Body)) => (Paint::Bare, '\u{2588}'),
        (Some(tone), None) => (paint_of(tone), '\u{2580}'),
        (None, Some(tone)) => (paint_of(tone), '\u{2584}'),
        (None, None) => (Paint::Bare, ' '),
    }
}

fn paint_of(tone: Tone) -> Paint {
    match tone {
        Tone::Body => Paint::Bare,
        Tone::Grey => Paint::Grey,
    }
}

/// What the mark says.
const SAYS: &str = "Let's make this AI behave!";

/// The row the cloud starts on. Its tail then leaves the muzzle two rows
/// further down, so the words come out of the mouth rather than pointing at
/// the mark from outside it.
const SPEECH_AT: usize = 3;

/// The cloud and the tail that carries it up from the muzzle. Built rather
/// than written out, so the puffed edges cannot fall out of step with the
/// words between them.
fn speech() -> [String; 4] {
    let puff = SAYS.chars().count() + 2;
    [
        format!("      \u{256d}{}\u{256e}", "\u{25e0}".repeat(puff)),
        format!("      \u{2502} {SAYS} \u{2502}"),
        format!("   \u{25e6}  \u{2570}{}\u{256f}", "\u{25e1}".repeat(puff)),
        "  \u{b7}".to_owned(),
    ]
}

/// The mark in half blocks, indented to the receipt's margin, saying its piece.
///
/// A cell holds two pixels, so it can need two tones at once. The body is the
/// terminal's own foreground, which cannot be asked for as a background, so
/// the one pairing the grid produces — body over grey, along the paws — is
/// drawn as an upper half block on a grey background. Neighbouring cells that
/// share a paint are written as one run.
pub(crate) fn happy(style: Style) -> String {
    BEAST
        .chunks_exact(2)
        .enumerate()
        .map(|(row, pair)| {
            let (top, bottom) = (pair[0].as_bytes(), pair[1].as_bytes());
            let cells: Vec<(Paint, char)> = (0..COLS).map(|x| cell(tone(top[x]), tone(bottom[x]))).collect();
            let mut line = String::from("  ");
            let mut rest = cells.as_slice();
            while let Some((paint, _)) = rest.first().copied() {
                let run = rest.iter().take_while(|(next, _)| *next == paint).count();
                let glyphs: String = rest[..run].iter().map(|(_, glyph)| glyph).collect();
                line.push_str(&match paint {
                    Paint::Bare => glyphs,
                    Paint::Grey => style.grey(&glyphs),
                    Paint::OverGrey => style.on_grey(&glyphs),
                });
                rest = &rest[run..];
            }
            match row
                .checked_sub(SPEECH_AT)
                .and_then(|said| speech().into_iter().nth(said))
            {
                Some(said) => format!("{line}{said}"),
                None => line,
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grid is the website's: any edit that changes its shape changes this.
    #[test]
    fn the_grid_matches_the_website_mark() {
        assert_eq!(BEAST.len(), 22);
        for row in BEAST {
            assert_eq!(row.len(), COLS, "{row} is not the mark's width");
            assert!(row.bytes().all(|p| b".1234".contains(&p)), "{row} has an unknown pixel");
        }
    }

    /// The cloud puffs to the width of what it holds, and its tail leaves the
    /// muzzle: the row carrying the larger blob is the row with the mouth.
    #[test]
    fn the_cloud_fits_its_words_and_rises_from_the_muzzle() {
        let lines: Vec<String> = happy(Style::Plain).lines().map(str::to_owned).collect();
        let puff = SAYS.chars().count() + 2;

        assert_eq!(lines[SPEECH_AT].matches('\u{25e0}').count(), puff);
        assert_eq!(lines[SPEECH_AT + 2].matches('\u{25e1}').count(), puff);
        assert!(lines[SPEECH_AT + 1].contains(SAYS));
        // The muzzle is the row whose body is broken by the nose, and it is
        // the one the tail leaves from.
        let muzzle = SPEECH_AT + 2;
        assert!(lines[muzzle].contains('\u{25e6}'), "the tail does not leave the muzzle");
        assert!(lines[muzzle].contains('\u{2580}'), "row {muzzle} is not the muzzle");
        assert!(
            lines[muzzle + 1].contains('\u{b7}'),
            "the tail does not reach back to the mark"
        );
    }

    /// Eleven lines, each the mark's width, and the face is holes rather than
    /// paint: the eye rows must carry gaps inside the body.
    #[test]
    fn the_mark_renders_as_half_blocks_with_its_face_open() {
        let drawn = happy(Style::Plain);
        let lines: Vec<&str> = drawn.lines().collect();

        assert_eq!(lines.len(), 11);
        assert!(!drawn.contains('\u{1b}'));
        // Rows 6 and 7 are the eyes: the line they share has body, then a gap,
        // then body again.
        assert!(
            lines[3].contains("\u{2588}   \u{2588}"),
            "the eyes are not open: {:?}",
            lines[3]
        );
    }

    /// The rendering asks a background for one tone only. Grey has a colour to
    /// name; the body is the terminal's foreground and has none, so a cell
    /// wanting body underneath another tone could not be drawn. The grid must
    /// never produce one.
    #[test]
    fn the_only_tone_the_grid_stacks_is_grey_under_body() {
        for pair in BEAST.chunks_exact(2) {
            let (top, bottom) = (pair[0].as_bytes(), pair[1].as_bytes());
            for x in 0..COLS {
                if let (Some(over), Some(under)) = (tone(top[x]), tone(bottom[x]))
                    && over != under
                {
                    assert!(
                        over == Tone::Body && under == Tone::Grey,
                        "column {x} stacks a tone the terminal cannot paint",
                    );
                }
            }
        }
    }

    /// Colour is the only difference between the two styles: the mark keeps its
    /// shape when the escapes are gone.
    #[test]
    fn the_mark_keeps_its_shape_without_colour() {
        let plain = happy(Style::Plain);
        let colored = happy(Style::Colored);

        assert!(colored.contains('\u{1b}'));
        let stripped: String = {
            let mut out = String::new();
            let mut chars = colored.chars();
            while let Some(c) = chars.next() {
                if c == '\u{1b}' {
                    for c in chars.by_ref() {
                        if c == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        };
        assert_eq!(stripped, plain);
    }
}
