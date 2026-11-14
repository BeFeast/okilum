//! Exact replay of native LTR advances, gated by the complete native layout.
//! A global delta is insufficient: f32 pen accumulation at the far end of a
//! huge line must run in the same order as the native shaper, not shift old Xs.
//!
//! An edit (#2803) keeps everything before its first changed word: glyph
//! positions, word ends and the running width are a prefix of the old ones.
//! Word ends are re-derived only between two UAX #14 safe pairs around the
//! edit (`unicode_linebreak::split_at_safe`: breaks after one do not depend on
//! the text before it); past the second one the old ends are shifted. Without
//! such a pair nearby, the whole line is analysed as before.
use super::*;

/// Bytes searched for a safe pair on each side of an edit.
const SAFE_SCAN: usize = 4096;

#[derive(Clone)]
pub(super) struct NativeAscii {
    pub(super) boundaries: Vec<usize>,
    /// The line width after each word, summed in native order: `widths[k]`
    /// covers the words ending at `boundaries[1..=k]`; `widths[0]` is zero.
    widths: Vec<f32>,
    advances: Arc<[f32]>,
    pairs: Arc<HashMap<[u8; 2], bool>>,
}

#[cfg(test)]
thread_local! {
    /// Bytes given to line-break analysis on this thread (#2803 regression).
    pub(super) static WORD_SCAN_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn count_scan(_bytes: usize) {
    #[cfg(test)]
    WORD_SCAN_BYTES.with(|n| n.set(n.get() + _bytes));
}

impl NativeAscii {
    pub(super) fn prepare(
        text: &str,
        size: Pixels,
        font: FontId,
        layout: &LineLayout,
        previous: Option<&Self>,
        shaper: &dyn PlatformTextSystem,
    ) -> Option<Self> {
        let advances = advances(
            size,
            font,
            layout.runs.iter().flat_map(|r| &r.glyphs),
            previous,
            shaper,
        )?;
        let mut pairs = Pairs::new(previous, shaper, size, font);
        count_scan(text.len());
        let boundaries = word_ends(text, &mut pairs)?;
        let mut copy = copy_layout(layout);
        let mut widths = Vec::with_capacity(boundaries.len());
        widths.push(0.);
        let width = replay(
            &advances,
            &boundaries,
            0,
            (0., 0.),
            copy.runs.iter_mut().flat_map(|r| &mut r.glyphs),
            &mut widths,
        );
        copy.width = gpui::px(width);
        // This admission check covers font-specific kerning, offsets, glyph
        // ordering, native word boundaries and the platform's accumulation.
        exact_geometry(&copy, layout).then(|| Self {
            boundaries,
            widths,
            advances: advances.into(),
            pairs: pairs.known,
        })
    }

    /// Plans an edit of this line, whose previous layout is `old`, into
    /// `text`: its changed words are the admitted `middle` shape. `prefix` and
    /// `suffix` are the unchanged byte counts; `left_at` indexes the first
    /// reshaped word start in `self.boundaries`. Reads only.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn plan_edit(
        &self,
        old: &LineLayout,
        text: &str,
        (prefix, suffix, left_at): (usize, usize, usize),
        middle: &Self,
        shaper: &dyn PlatformTextSystem,
        size: Pixels,
        font: FontId,
    ) -> Option<Edit> {
        let mut pairs = Pairs {
            known: middle.pairs.clone(),
            shaper,
            size,
            font,
        };
        let spliced = splice_word_ends(&self.boundaries, text, old.len, prefix, suffix, &mut pairs);
        let (ends, from, pen) = match spliced {
            // Words before both the first reshaped and the first re-derived
            // end keep their glyphs, pen and running width.
            Some(splice) => {
                let from = (splice.shared - 1).min(left_at);
                match first_glyph_at(old, self.boundaries[from]) {
                    Some(glyph) => (NewEnds::Splice(splice), from, f32::from(glyph.position.x)),
                    None => (NewEnds::Splice(splice), 0, 0.),
                }
            }
            None => {
                count_scan(text.len());
                (NewEnds::Full(word_ends(text, &mut pairs)?), 0, 0.)
            }
        };
        Some(Edit {
            ends,
            from,
            pen,
            pairs: pairs.known,
        })
    }

    /// Applies a planned edit: `layout` holds the edited glyphs (positions
    /// before the first changed word unchanged). Sets word ends, positions from
    /// the planned word on, running widths and the line width, in place.
    pub(super) fn apply_edit(&mut self, edit: Edit, layout: &mut LineLayout, middle: &Self) {
        match edit.ends {
            NewEnds::Splice(Splice {
                shared,
                derived,
                resume,
                delta,
            }) => {
                let count = derived.len();
                self.boundaries
                    .splice(shared..resume, derived)
                    .for_each(drop);
                for end in &mut self.boundaries[shared + count..] {
                    *end = end.wrapping_add_signed(delta);
                }
            }
            NewEnds::Full(ends) => self.boundaries = ends,
        }
        let from = edit.from;
        let width = self.widths[from];
        self.widths.truncate(from + 1);
        let start = self.boundaries[from];
        let width = if let [run] = layout.runs.as_mut_slice() {
            let at = run.glyphs.partition_point(|g| g.index < start);
            replay(
                &middle.advances,
                &self.boundaries,
                from,
                (edit.pen, width),
                run.glyphs[at..].iter_mut(),
                &mut self.widths,
            )
        } else {
            let glyphs = layout.runs.iter_mut().flat_map(|r| &mut r.glyphs);
            replay(
                &middle.advances,
                &self.boundaries,
                from,
                (edit.pen, width),
                glyphs.skip_while(|g| g.index < start),
                &mut self.widths,
            )
        };
        layout.width = gpui::px(width);
        self.advances = middle.advances.clone();
        self.pairs = edit.pairs;
    }

    /// Every test edit is checked against the complete derivation.
    #[cfg(test)]
    pub(super) fn assert_matches_full_replay(
        &self,
        text: &str,
        layout: &LineLayout,
        shaper: &dyn PlatformTextSystem,
        size: Pixels,
        font: FontId,
    ) {
        let mut pairs = Pairs {
            known: self.pairs.clone(),
            shaper,
            size,
            font,
        };
        let full = word_ends(text, &mut pairs).expect("complete word ends");
        assert_eq!(&*self.boundaries, full.as_slice(), "spliced word ends");
        let mut copy = copy_layout(layout);
        let mut widths = vec![0.];
        let width = replay(
            &self.advances,
            &full,
            0,
            (0., 0.),
            copy.runs.iter_mut().flat_map(|r| &mut r.glyphs),
            &mut widths,
        );
        copy.width = gpui::px(width);
        assert!(exact_geometry(&copy, layout), "partial replay geometry");
        assert_eq!(&*self.widths, widths.as_slice(), "running word widths");
    }

    pub(super) fn bytes(&self) -> usize {
        size_of::<Self>()
            + size_of_val(self.boundaries.as_slice())
            + size_of_val(self.widths.as_slice())
            + size_of_val(self.advances.as_ref())
            + self.pairs.capacity() * (size_of::<([u8; 2], bool)>() + size_of::<usize>() * 2)
    }
}

/// A planned edit of a line's word ends and positions.
pub(super) struct Edit {
    ends: NewEnds,
    /// The word end from which positions and widths are replayed.
    from: usize,
    /// The pen at that end.
    pen: f32,
    pairs: Arc<HashMap<[u8; 2], bool>>,
}

enum NewEnds {
    Splice(Splice),
    Full(Vec<usize>),
}

/// Old ends `[..shared]` stay, `[shared..resume]` become `derived`, and the
/// rest move by `delta` bytes.
struct Splice {
    shared: usize,
    derived: Vec<usize>,
    resume: usize,
    delta: isize,
}

/// The first glyph at or after byte `index` of an LTR layout, by bisection.
fn first_glyph_at(layout: &LineLayout, index: usize) -> Option<&ShapedGlyph> {
    layout.runs.iter().find_map(|run| {
        let at = run.glyphs.partition_point(|g| g.index < index);
        run.glyphs.get(at)
    })
}

/// Font advances by glyph id, extending `previous` with these glyphs.
fn advances<'a>(
    size: Pixels,
    font: FontId,
    glyphs: impl Iterator<Item = &'a ShapedGlyph> + Clone,
    previous: Option<&NativeAscii>,
    shaper: &dyn PlatformTextSystem,
) -> Option<Vec<f32>> {
    let em = shaper.font_metrics(font).units_per_em as f32;
    if em == 0. {
        return None;
    }
    let maximum = glyphs.clone().map(|g| g.id.0 as usize).max().unwrap_or(0);
    if maximum > u16::MAX as usize {
        return None;
    }
    let mut advances = previous.map_or_else(Vec::new, |p| p.advances.to_vec());
    advances.resize(advances.len().max(maximum + 1), f32::NAN);
    for glyph in glyphs {
        let slot = &mut advances[glyph.id.0 as usize];
        if slot.is_nan() {
            *slot = f32::from(size).mul_add(shaper.advance(font, glyph.id).ok()?.width / em, 0.);
        }
        if !slot.is_finite() || *slot < 0. {
            return None;
        }
    }
    Some(advances)
}

/// Adjacent ASCII punctuation the native engine shapes as one word.
struct Pairs<'a> {
    known: Arc<HashMap<[u8; 2], bool>>,
    shaper: &'a dyn PlatformTextSystem,
    size: Pixels,
    font: FontId,
}

impl<'a> Pairs<'a> {
    fn new(
        previous: Option<&NativeAscii>,
        shaper: &'a dyn PlatformTextSystem,
        size: Pixels,
        font: FontId,
    ) -> Self {
        Self {
            known: previous.map_or_else(Default::default, |p| p.pairs.clone()),
            shaper,
            size,
            font,
        }
    }

    /// Whether the line-break opportunity at `end` lies inside a native word.
    fn joined(&mut self, bytes: &[u8], end: usize) -> Option<bool> {
        if end == 0
            || end >= bytes.len()
            || !bytes[end - 1].is_ascii_punctuation()
            || !bytes[end].is_ascii_punctuation()
        {
            return Some(false);
        }
        let pair = [bytes[end - 1], bytes[end]];
        if let Some(joined) = self.known.get(&pair) {
            return Some(*joined);
        }
        let probe = self.shaper.layout_line(
            std::str::from_utf8(&pair).ok()?,
            self.size,
            &[FontRun {
                len: 2,
                font_id: self.font,
            }],
        );
        if probe.runs.iter().any(|r| r.font_id != self.font) {
            return None;
        }
        let ids: Vec<_> = probe
            .runs
            .iter()
            .flat_map(|r| &r.glyphs)
            .map(|g| g.id)
            .collect();
        let standard = [
            self.shaper.glyph_for_char(self.font, pair[0] as char)?,
            self.shaper.glyph_for_char(self.font, pair[1] as char)?,
        ];
        let joined = ids.len() < 2 || (ids.len() == 2 && ids.as_slice() != standard);
        Arc::make_mut(&mut self.known).insert(pair, joined);
        Some(joined)
    }
}

/// Native word ends, built from line-break opportunities that are not joined.
struct Ends {
    ends: Vec<usize>,
    /// The previous opportunity that ended a word.
    start: usize,
}

impl Ends {
    fn push(&mut self, bytes: &[u8], end: usize) {
        let mut nonblank_end = end;
        while nonblank_end > self.start && bytes[nonblank_end - 1] == b' ' {
            nonblank_end -= 1;
        }
        if self.start < nonblank_end {
            self.ends.push(nonblank_end);
        }
        // The native engine treats each trailing ASCII space as a word.
        self.ends.extend(nonblank_end + 1..=end);
        self.start = end;
    }
}

/// The complete native word ends of `text`, starting with zero.
fn word_ends(text: &str, pairs: &mut Pairs) -> Option<Vec<usize>> {
    let bytes = text.as_bytes();
    let mut ends = Ends {
        ends: vec![0],
        start: 0,
    };
    for (end, _) in unicode_linebreak::linebreaks(text) {
        if !pairs.joined(bytes, end)? {
            ends.push(bytes, end);
        }
    }
    (ends.ends.last().copied() == Some(text.len())).then_some(ends.ends)
}

/// The word ends of `text` after an edit of a line of `old_len` bytes whose
/// word ends were `old`; `prefix`/`suffix` bytes are unchanged. None without a
/// safe pair in reach: the caller then analyses the whole line.
fn splice_word_ends(
    old: &[usize],
    text: &str,
    old_len: usize,
    prefix: usize,
    suffix: usize,
    pairs: &mut Pairs,
) -> Option<Splice> {
    let bytes = text.as_bytes();
    // A few unchanged words before the edit, so an opportunity that ends a
    // word lies between the safe pair and the first changed byte.
    let near = old[old.partition_point(|end| *end <= prefix).saturating_sub(4)];
    let low = near.saturating_sub(SAFE_SCAN);
    let start = if low == 0 {
        0
    } else {
        let (before, _) = unicode_linebreak::split_at_safe(text.get(low..near)?);
        if before.is_empty() {
            return None;
        }
        low + before.len()
    };
    // `split_at_safe` keeps one character before the pair; skip its own
    // position and the next one as well.
    let first = if start == 0 { 0 } else { start + 2 };
    let mut events = unicode_linebreak::linebreaks(text.get(start..)?)
        .map(|(at, _)| start + at)
        .filter(|end| *end >= first);
    let anchor = loop {
        let end = events.next()?;
        if end >= prefix {
            return None;
        }
        if !pairs.joined(bytes, end)? {
            break end;
        }
    };
    // An unjoined opportunity before the edit ends a word in both texts.
    let shared = old.binary_search(&anchor).ok()? + 1;
    let mut ends = Ends {
        ends: Vec::new(),
        start: anchor,
    };
    // A safe pair inside the unchanged suffix makes every later opportunity
    // the old one, shifted.
    let tail = text.len() - suffix;
    let mut safe = None;
    for span in [64, 512, SAFE_SCAN] {
        let high = (tail + span).min(text.len());
        let (before, _) = unicode_linebreak::split_at_safe(text.get(tail..high)?);
        if !before.is_empty() {
            safe = Some(tail + before.len());
            break;
        }
        if high == text.len() {
            // Close to the end: derive the remaining words.
            safe = Some(usize::MAX);
            break;
        }
    }
    let safe = safe?;
    let delta = text.len() as isize - old_len as isize;
    for end in events {
        if pairs.joined(bytes, end)? {
            continue;
        }
        ends.push(bytes, end);
        if end >= safe.saturating_add(2) {
            count_scan(end - start);
            let old_end = usize::try_from(end as isize - delta).ok()?;
            let resume = old.binary_search(&old_end).ok()? + 1;
            return Some(Splice {
                shared,
                derived: ends.ends,
                resume,
                delta,
            });
        }
    }
    count_scan(text.len() - start);
    (ends.ends.last().copied() == Some(text.len())).then_some(Splice {
        shared,
        derived: ends.ends,
        resume: old.len(),
        delta,
    })
}

/// Replays native advances for the words after `ends[from]`: positions of
/// `glyphs` (those from that end on, in order), each running width pushed to
/// `widths`, and the final width. `pen_width` holds both sums at that end.
fn replay<'a>(
    advances: &[f32],
    ends: &[usize],
    from: usize,
    (mut pen, mut width): (f32, f32),
    glyphs: impl Iterator<Item = &'a mut ShapedGlyph>,
    widths: &mut Vec<f32>,
) -> f32 {
    let mut glyphs = glyphs.peekable();
    for &end in &ends[from + 1..] {
        let mut word_width = 0f32;
        while glyphs.peek().is_some_and(|g| g.index < end) {
            let glyph = glyphs.next().unwrap();
            glyph.position.x = gpui::px(pen);
            let advance = advances[glyph.id.0 as usize];
            pen += advance;
            word_width += advance;
        }
        width += word_width;
        widths.push(width);
    }
    width
}

pub(super) fn exact_geometry(a: &LineLayout, b: &LineLayout) -> bool {
    a.font_size == b.font_size
        && a.width == b.width
        && a.ascent == b.ascent
        && a.descent == b.descent
        && a.len == b.len
        && a.runs.len() == b.runs.len()
        && a.runs.iter().zip(&b.runs).all(|(a, b)| {
            a.font_id == b.font_id
                && a.glyphs.len() == b.glyphs.len()
                && a.glyphs.iter().zip(&b.glyphs).all(|(a, b)| {
                    a.id == b.id
                        && a.index == b.index
                        && a.position == b.position
                        && a.is_emoji == b.is_emoji
                })
        })
}
