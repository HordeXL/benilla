//! Reading the client's own TTFs, and what has to come off the raw bytes: one metric and the
//! charmap.

use cosmic_text::skrifa::raw::{tables::cmap::PlatformId, FontRef, TableProvider};
use cosmic_text::FontSystem;

/// The four Latin client TTFs, plain TTF in `fonts.MPQ`, read through the patch chain. Index 0,
/// Friz Quadrata, is the fallback face and is required.
///
/// A localized install ships a different set under the same directory: a zhCN one carries
/// `FZBWJW`/`FZJZJW`/`FZLBJW`/`FZXHJW`/`FZXHLJW` instead of `MORPHEUS`/`SKURRI`, and its
/// `FRIZQT__` covers no CJK at all. So this list alone cannot draw a localized client's own text;
/// [`cjk_fallback_face`] finds the face that can, by reading the install rather than guessing a
/// name that varies by publisher.
pub(super) const CLIENT_FONTS: &[&str] = &[
    "Fonts\\FRIZQT__.TTF",
    "Fonts\\ARIALN.TTF",
    "Fonts\\MORPHEUS.TTF",
    "Fonts\\SKURRI.TTF",
];

/// The code points [`cjk_fallback_face`] scores a face against: the block a localized client's UI
/// actually draws, which is the range `Fonts.xml` localizations pick a face for. A Latin face
/// covers none of them and is never chosen.
const CJK_PROBE: &[char] = &[
    '中', '文', '魔', '兽', '世', '界', '任', '务', '背', '包', '，', '。', '：', '！', '（', '）',
];

/// The face's coverage of [`CJK_PROBE`], for ranking candidates.
fn cjk_coverage(bytes: &[u8]) -> usize {
    CJK_PROBE
        .iter()
        .filter(|&&c| cmap_lookup(bytes, u32::from(c)).is_some())
        .count()
}

/// The best face among `candidates` for characters the requested face cannot draw: the `.ttf`
/// whose `cmap` covers the most of [`CJK_PROBE`], with ties broken by path so the pick is
/// deterministic. `None` when none covers any, and every caller then behaves as before. `read` is
/// the chain read and `candidates` the chain's listed names ([`Chain::list`]), both injected so
/// this is testable without an install.
///
/// The reference picks its face per locale from the shipped `Fonts.xml`, which names a file the
/// publisher chose; reading the install's own archive is the same fact without the name table.
pub(super) fn cjk_fallback_face(
    candidates: &[String],
    read: impl Fn(&str) -> Option<Vec<u8>>,
) -> Option<(String, Vec<u8>)> {
    let mut best: Option<(usize, String, Vec<u8>)> = None;
    for path in candidates {
        if !path.to_ascii_lowercase().ends_with(".ttf") {
            continue;
        }
        let Some(bytes) = read(path) else {
            continue;
        };
        let score = cjk_coverage(&bytes);
        if score == 0 {
            continue;
        }
        let key = path.to_ascii_lowercase();
        match &best {
            Some((s, k, _)) if (*s, k) >= (score, &key) => {}
            _ => best = Some((score, key, bytes)),
        }
    }
    best.map(|(_, path, bytes)| (path, bytes))
}

/// The glyph id `cp` maps to in the sfnt's `cmap`, off the best format-4 (BMP) or format-12 (full)
/// subtable: the highest-ranked platform present, then the largest format, so a platform-3 table
/// wins over its Unicode-platform duplicate. `None` when no subtable covers `cp`.
pub(super) fn cmap_lookup(bytes: &[u8], cp: u32) -> Option<u16> {
    let num = u16::from_be_bytes(bytes.get(4..6)?.try_into().ok()?) as usize;
    let mut cmap = None;
    for i in 0..num {
        let rec = bytes.get(12 + 16 * i..12 + 16 * i + 16)?;
        if &rec[0..4] == b"cmap" {
            cmap = Some(u32::from_be_bytes(rec[8..12].try_into().ok()?) as usize);
        }
    }
    let cmap = cmap?;
    let tables = u16::from_be_bytes(bytes.get(cmap + 2..cmap + 4)?.try_into().ok()?) as usize;
    let mut best: Option<(u32, usize, u16)> = None;
    for i in 0..tables {
        let rec = bytes.get(cmap + 4 + 8 * i..cmap + 4 + 8 * i + 8)?;
        let plat = u16::from_be_bytes(rec[0..2].try_into().ok()?);
        let enc = u16::from_be_bytes(rec[2..4].try_into().ok()?);
        let off = u32::from_be_bytes(rec[4..8].try_into().ok()?) as usize;
        let sub = cmap.checked_add(off)?;
        let format = u16::from_be_bytes(bytes.get(sub..sub + 2)?.try_into().ok()?);
        let rank: u32 = match (plat, enc, format) {
            (3, 10, 12) | (3, 1, 4) => 3,
            (0, _, 12) | (0, _, 4) => 2,
            (_, _, 12) => 1,
            (_, _, 4) => 0,
            _ => continue,
        };
        if best.is_none_or(|(r, _, _)| rank > r) {
            best = Some((rank, sub, format));
        }
    }
    let (_, sub, format) = best?;
    match format {
        4 => cmap4(bytes, sub, cp),
        12 => cmap12(bytes, sub, cp),
        _ => None,
    }
}

/// A format-4 (segment-mapped BMP) subtable lookup; the arithmetics are the spec's, and a zero
/// glyph id is "no glyph", never the `.notdef` slot.
fn cmap4(d: &[u8], sub: usize, cp: u32) -> Option<u16> {
    let c = u16::try_from(cp).ok()?;
    let seg_x2 = usize::from(u16::from_be_bytes(
        d.get(sub + 6..sub + 8)?.try_into().ok()?,
    ));
    let segs = seg_x2 / 2;
    let end_at = sub + 14;
    let start_at = end_at + seg_x2 + 2;
    let delta_at = start_at + seg_x2;
    let range_at = delta_at + seg_x2;
    for seg in 0..segs {
        let end = u16::from_be_bytes(
            d.get(end_at + 2 * seg..end_at + 2 * seg + 2)?
                .try_into()
                .ok()?,
        );
        if c > end {
            continue;
        }
        let start = u16::from_be_bytes(
            d.get(start_at + 2 * seg..start_at + 2 * seg + 2)?
                .try_into()
                .ok()?,
        );
        if c < start {
            return None;
        }
        let delta = i16::from_be_bytes(
            d.get(delta_at + 2 * seg..delta_at + 2 * seg + 2)?
                .try_into()
                .ok()?,
        );
        let range = u16::from_be_bytes(
            d.get(range_at + 2 * seg..range_at + 2 * seg + 2)?
                .try_into()
                .ok()?,
        );
        let gid = if range == 0 {
            (c as i32 + i32::from(delta)) as u16
        } else {
            let at = range_at + 2 * seg + usize::from(range) + 2 * usize::from(c - start);
            let g = u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?);
            if g == 0 {
                return None;
            }
            (g as i32 + i32::from(delta)) as u16
        };
        return (gid != 0).then_some(gid);
    }
    None
}

/// A format-12 (segmented coverage) subtable lookup, by binary search over the group starts.
fn cmap12(d: &[u8], sub: usize, cp: u32) -> Option<u16> {
    let groups = u32::from_be_bytes(d.get(sub + 12..sub + 16)?.try_into().ok()?) as usize;
    let (mut lo, mut hi) = (0usize, groups);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let at = sub + 16 + 12 * mid;
        let start = u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?);
        let end = u32::from_be_bytes(d.get(at + 4..at + 8)?.try_into().ok()?);
        if cp < start {
            hi = mid;
        } else if cp > end {
            lo = mid + 1;
        } else {
            let gid = u32::from_be_bytes(d.get(at + 8..at + 12)?.try_into().ok()?);
            return u16::try_from(gid + (cp - start)).ok();
        }
    }
    None
}

/// The face's ascender as a fraction of the em, `hhea.asc / (hhea.asc + |hhea.desc|)`, off the raw
/// sfnt bytes. The reference puts the baseline at `cellTop + round(em · ratio)` (`[CGxFont+0x17c]`,
/// set at `0x5ca030`, reaching the placement kernel `0x5d1360` through `0x5ca160` and `0x5d1120`);
/// FreeType's scaled ascender (`asc/upem`: 0.965 for Friz, where this ratio is 0.794) is not on
/// that path.
pub(super) fn hhea_ascent_ratio(bytes: &[u8]) -> Option<f32> {
    let num = u16::from_be_bytes(bytes.get(4..6)?.try_into().ok()?) as usize;
    let (mut asc, mut desc) = (None, None);
    for i in 0..num {
        let rec = bytes.get(12 + 16 * i..12 + 16 * i + 16)?;
        let toff = u32::from_be_bytes(rec[8..12].try_into().ok()?) as usize;
        if &rec[0..4] == b"hhea" {
            asc = Some(i16::from_be_bytes(bytes.get(toff + 4..toff + 6)?.try_into().ok()?) as f32);
            desc = Some(i16::from_be_bytes(bytes.get(toff + 6..toff + 8)?.try_into().ok()?) as f32);
        }
    }
    let (asc, desc) = (asc?, desc?);
    // The denominator is narrowed to f32 as the reference does (`fstp m32` at `0x5ca0be`); the
    // callers floor `em · ratio + 0.5`.
    let denom = asc + desc.abs();
    (denom > 0.0 && asc > 0.0).then_some(asc / denom)
}

/// The index of the cmap encoding record the reference reads characters through. The face open
/// selects the Unicode charmap (`FT_Select_Charmap(face, 'unic')`, `0x5d015d` → `0x7ce8d0`): the
/// first record whose platform and encoding FreeType's table (`0x7d8e00`, rows `0x81eb70` to
/// `0x81ebe8`) names Unicode, platform 2 or 0 with any encoding or platform 3 encoding 1; that
/// table has no platform 3 encoding 10. `None` fails the open (`0x5d0169`).
pub(super) fn unicode_charmap(bytes: &[u8]) -> Option<u16> {
    let cmap = FontRef::from_index(bytes, 0).ok()?.cmap().ok()?;
    let i = cmap.encoding_records().iter().position(|r| {
        matches!(
            (r.platform_id(), r.encoding_id()),
            (PlatformId::ISO | PlatformId::Unicode, _) | (PlatformId::Windows, 1)
        )
    })?;
    u16::try_from(i).ok()
}

/// The glyph `ch` maps to through encoding record `record`, `FT_Get_Char_Index` (`0x7ce960`):
/// `None` for a code point the charmap lacks, which FreeType answers with glyph 0.
pub(super) fn charmap_glyph(bytes: &[u8], index: u32, record: u16, ch: char) -> Option<u16> {
    let gid = FontRef::from_index(bytes, index)
        .ok()?
        .cmap()
        .ok()?
        .subtable(record)
        .ok()?
        .map_codepoint(ch)?;
    u16::try_from(gid.to_u32()).ok().filter(|&g| g != 0)
}

/// What names a registered face to the shaper: its id, family and three CSS axes, read off the
/// face. `cosmic-text` 0.16 matches on all three, so attrs naming only the family ask for a
/// normal-weight, normal-style face and silently get another one when this face is bold or italic.
pub(super) struct Registered {
    pub(super) id: fontdb::ID,
    pub(super) family: String,
    pub(super) weight: fontdb::Weight,
    pub(super) style: fontdb::Style,
    pub(super) stretch: fontdb::Stretch,
}

/// Register a raw TTF into `font_system`'s database, named by what the loaded face says.
pub(super) fn register_font(
    font_system: &mut FontSystem,
    bytes: Vec<u8>,
) -> anyhow::Result<Registered> {
    let source = fontdb::Source::Binary(
        std::sync::Arc::new(bytes) as std::sync::Arc<dyn AsRef<[u8]> + Sync + Send>
    );
    let ids = font_system.db_mut().load_font_source(source);
    let id = *ids
        .first()
        .ok_or_else(|| anyhow::anyhow!("font source produced no faces (not a valid TTF?)"))?;
    let info = font_system
        .db_mut()
        .face(id)
        .ok_or_else(|| anyhow::anyhow!("face {id:?} vanished right after loading"))?;
    let family = info
        .families
        .first()
        .map(|(name, _)| name.clone())
        .ok_or_else(|| anyhow::anyhow!("face {id:?} carries no family name"))?;
    Ok(Registered {
        id,
        family,
        weight: info.weight,
        style: info.style,
        stretch: info.stretch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal sfnt with one format-4 `cmap` subtable and nothing else, mapping `codes`
    /// to glyph ids 1..: enough for [`cmap_lookup`], which reads only the table directory and the
    /// `cmap`. The format-4 this writes is one segment per code, delta = `gid - code`, no glyph
    /// id array, and the required 0xFFFF terminator.
    fn sfnt_with_cmap4(codes: &[char]) -> Vec<u8> {
        let mut segs = codes
            .iter()
            .map(|&c| u16::try_from(u32::from(c)).expect("BMP only"))
            .collect::<Vec<_>>();
        segs.sort_unstable();
        segs.dedup();
        let n = segs.len() + 1; // + the 0xFFFF terminator
        let seg_x2 = 2 * n;
        // cmap subtable format 4: a 14-byte header, then `n` u16 endCodes, a 2-byte reserved pad,
        // and `n` u16 each of startCodes, idDeltas and idRangeOffsets.
        let sub_len = 14 + 4 * seg_x2 + 2;
        let mut sub = Vec::with_capacity(sub_len);
        sub.extend_from_slice(&4u16.to_be_bytes()); // format
        sub.extend_from_slice(&(sub_len as u16).to_be_bytes()); // length
        sub.extend_from_slice(&0u16.to_be_bytes()); // language
        sub.extend_from_slice(&(seg_x2 as u16).to_be_bytes()); // segCountX2
        sub.extend_from_slice(&0u16.to_be_bytes()); // searchRange (unused by us)
        sub.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
        sub.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
        for &c in &segs {
            sub.extend_from_slice(&c.to_be_bytes());
        }
        sub.extend_from_slice(&0xFFFFu16.to_be_bytes());
        sub.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
        for &c in &segs {
            sub.extend_from_slice(&c.to_be_bytes());
        }
        sub.extend_from_slice(&0xFFFFu16.to_be_bytes());
        for (i, &c) in segs.iter().enumerate() {
            let gid = (i + 1) as u16;
            sub.extend_from_slice(&gid.wrapping_sub(c).to_be_bytes()); // delta
        }
        // The terminator segment: 0xFFFF -> 0xFFFF + 1 = 0 (wraps), the conventional "no mapping",
        // so a lookup for U+FFFF answers None rather than a bogus 0xFFFF glyph.
        sub.extend_from_slice(&1u16.to_be_bytes()); // terminator delta
        for _ in 0..n {
            sub.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset: all delta-mapped
        }

        // cmap: version(4) + numTables(2) + one encoding record(8) + the subtable.
        let cmap_off = 12 + 16; // right after the single table record
        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes()); // version
        cmap.extend_from_slice(&1u16.to_be_bytes()); // numTables
        cmap.extend_from_slice(&3u16.to_be_bytes()); // platform 3
        cmap.extend_from_slice(&1u16.to_be_bytes()); // encoding 1 (BMP)
        cmap.extend_from_slice(&12u32.to_be_bytes()); // offset to sub
        cmap.extend_from_slice(&sub);

        // sfnt header + one table record for `cmap`.
        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // numTables
        out.extend_from_slice(&0u16.to_be_bytes()); // searchRange
        out.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
        out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
        out.extend_from_slice(b"cmap");
        out.extend_from_slice(&0u32.to_be_bytes()); // checksum
        out.extend_from_slice(&(cmap_off as u32).to_be_bytes());
        out.extend_from_slice(&(cmap.len() as u32).to_be_bytes());
        out.extend_from_slice(&cmap);
        out
    }

    #[test]
    fn cmap_lookup_reads_a_format_4_subtable() {
        let f = sfnt_with_cmap4(&['A', 'B', '中']);
        assert_eq!(cmap_lookup(&f, u32::from('A')), Some(1));
        assert_eq!(cmap_lookup(&f, u32::from('B')), Some(2));
        assert_eq!(cmap_lookup(&f, u32::from('中')), Some(3));
        assert_eq!(cmap_lookup(&f, u32::from('Z')), None, "unmapped");
        assert_eq!(cmap_lookup(&f, 0x2_0000), None, "past the BMP");
        assert_eq!(
            cmap_lookup(&f, 0xFFFF),
            None,
            "the terminator segment maps to glyph 0, i.e. no glyph"
        );
        assert_eq!(cmap_lookup(&[], 0), None, "no header");
        assert_eq!(cmap_lookup(&f[..10], 0), None, "truncated directory");
    }

    #[test]
    fn cjk_coverage_counts_the_probe_a_face_can_draw() {
        let latin = sfnt_with_cmap4(&['A', 'B', 'c']);
        assert_eq!(cjk_coverage(&latin), 0, "a Latin face covers no probe");
        let cjk = sfnt_with_cmap4(&CJK_PROBE[..4]);
        assert_eq!(cjk_coverage(&cjk), 4);
    }

    #[test]
    fn cjk_fallback_face_scores_its_candidates() {
        let latin = sfnt_with_cmap4(&['A', 'B']);
        let few = sfnt_with_cmap4(&CJK_PROBE[..3]);
        let many = sfnt_with_cmap4(CJK_PROBE);
        // The best CJK coverage wins, not the first or last candidate.
        let by_name: std::collections::HashMap<&str, Vec<u8>> = [
            ("Fonts\\FRIZQT__.TTF", latin.clone()),
            ("Fonts\\A.TTF", few.clone()),
            ("Fonts\\ZZ.TTF", many.clone()),
        ]
        .into_iter()
        .collect();
        let names = by_name.keys().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let read = |p: &str| by_name.get(p).cloned();
        let (path, bytes) = cjk_fallback_face(&names, read).expect("ZZ wins");
        assert_eq!(path, "fonts\\zz.ttf", "lowercased key");
        assert_eq!(cjk_coverage(&bytes), CJK_PROBE.len());
    }

    #[test]
    fn cjk_fallback_face_is_none_when_nothing_covers_cjk() {
        let latin = sfnt_with_cmap4(&['A', 'B']);
        let read = |p: &str| (p == "Fonts\\FRIZQT__.TTF").then(|| latin.clone());
        let names = vec!["Fonts\\FRIZQT__.TTF".to_string(), "README.txt".to_string()];
        assert!(
            cjk_fallback_face(&names, read).is_none(),
            "a non-ttf and a Latin face must not be chosen"
        );
    }

    /// A one-table sfnt whose `cmap` holds `records` in order, each `(platform, encoding)` over
    /// its own format 12 subtable mapping `ch` to `glyph`.
    fn sfnt_with_cmap(records: &[(u16, u16, char, u32)]) -> Vec<u8> {
        let mut cmap = Vec::new();
        cmap.extend(0u16.to_be_bytes());
        cmap.extend((records.len() as u16).to_be_bytes());
        let first = 4 + 8 * records.len();
        for (i, &(pid, eid, ..)) in records.iter().enumerate() {
            cmap.extend(pid.to_be_bytes());
            cmap.extend(eid.to_be_bytes());
            cmap.extend(((first + 28 * i) as u32).to_be_bytes());
        }
        for &(.., ch, glyph) in records {
            for half in [12u16, 0] {
                cmap.extend(half.to_be_bytes());
            }
            for word in [28u32, 0, 1, ch as u32, ch as u32, glyph] {
                cmap.extend(word.to_be_bytes());
            }
        }
        let mut font = Vec::new();
        for half in [1u16, 0, 1, 16, 0, 0] {
            font.extend(half.to_be_bytes());
        }
        font.extend(b"cmap");
        for word in [0u32, 28, cmap.len() as u32] {
            font.extend(word.to_be_bytes());
        }
        font.extend(cmap);
        font
    }

    /// FreeType's table names platform 3 encoding 1 Unicode and not encoding 10 (`0x81eb70`), and
    /// the open takes the first Unicode record, whatever the later ones map.
    #[test]
    fn the_charmap_is_the_first_record_freetype_calls_unicode() {
        let font = sfnt_with_cmap(&[
            (3, 0, 'A', 4),
            (3, 10, 'A', 5),
            (3, 1, 'B', 6),
            (0, 3, 'A', 7),
        ]);
        assert_eq!(unicode_charmap(&font), Some(2));
        assert_eq!(charmap_glyph(&font, 0, 2, 'B'), Some(6));
        assert_eq!(
            charmap_glyph(&font, 0, 2, 'A'),
            None,
            "a later record does not answer"
        );
        let iso = sfnt_with_cmap(&[(1, 0, 'A', 4), (2, 1, 'A', 5)]);
        assert_eq!(unicode_charmap(&iso), Some(1));
        let symbol = sfnt_with_cmap(&[(3, 0, 'A', 4), (3, 10, 'A', 5)]);
        assert_eq!(
            unicode_charmap(&symbol),
            None,
            "no Unicode record: the face does not open"
        );
    }
}
