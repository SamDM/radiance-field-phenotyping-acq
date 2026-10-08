//! Resolve configured `family: ids` selectors into what the detector runs and looks for.
//!
//! # Whole families, filtered afterwards
//!
//! `detect_markers` takes a list of dictionaries and runs the expensive part of the
//! pipeline — adaptive threshold, contour finding, polygon approximation, perspective warp
//! per candidate — once, then identifies each candidate against every dictionary. So the
//! detector is handed each configured family whole, in one call, and detections whose id
//! was not configured are dropped here.
//!
//! Passing the whole family rather than just the configured codewords matters for false
//! triggers. An unconfigured marker from the same family is identified as *itself* and then
//! dropped, instead of being error-corrected onto the nearest configured codeword. And the
//! cost is negligible: identification compares a handful of candidates against at most a
//! thousand codes.
//!
//! # When two configured markers are the same marker
//!
//! The predefined dictionaries overlap. `DICT_4X4_50` is a prefix of `DICT_4X4_100`, which
//! is a prefix of `DICT_4X4_250`, and so on — so `DICT_4X4_50:4` and `DICT_4X4_250:4` are
//! one physical card with two names. Across every predefined family there is exactly one
//! overlap where the *ids* also differ: `DICT_4X4_1000:854` is the same pattern as
//! `DICT_APRILTAG_16H5:18`. (`no_unexpected_cross_family_collisions` pins that, so a
//! library update which changes it fails loudly rather than quietly.)
//!
//! When several dictionaries identify one candidate, the detector reports it under one of
//! them only: the most confident, then the first in the list. That name need not be the one
//! the operator configured — with `DICT_4X4_250:100` and `DICT_4X4_50:4` configured, a card
//! showing id 4 comes back as `DICT_4X4_250:4`. So detections are matched on the
//! *codeword*, not on the reported name. A codeword configured under two names is reported
//! under the first, and the other name is kept in [`MarkerSet::aliases`] for the startup
//! log. Either way the trigger behaves the same: a configured marker was seen.

use std::collections::HashMap;

use arucotools::dictionaries::{predefined, MarkerFamily};

use super::{MarkerCfgError, MarkerSelector};

/// A configured marker, identified the way the operator wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub family: &'static str,
    pub id: usize,
}

/// Two configured markers that turned out to be the same codeword.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alias {
    /// The identity detections of this codeword will be reported under.
    pub reported: Hit,
    /// The other name the operator gave the same physical marker.
    pub also: Hit,
}

/// The dictionaries to detect with, and which of their markers count.
#[derive(Debug)]
pub struct MarkerSet {
    /// Each configured family once, in the order first configured.
    families: Vec<&'static MarkerFamily>,
    /// Configured codewords, keyed by grid size and code, so a detection reported under an
    /// overlapping dictionary still finds the name the operator gave it.
    wanted: HashMap<(usize, u64), Hit>,
    aliases: Vec<Alias>,
}

impl MarkerSet {
    /// Validate selectors and collect the families and codewords they name.
    pub fn new(selectors: &[MarkerSelector]) -> Result<Self, MarkerCfgError> {
        let mut set = MarkerSet { families: Vec::new(), wanted: HashMap::new(), aliases: Vec::new() };

        for selector in selectors {
            let (name, family) = predefined::ALL
                .iter()
                .find(|(name, _)| *name == selector.family)
                .copied()
                .ok_or_else(|| MarkerCfgError::UnknownFamily {
                    name: selector.family.clone(),
                    known: known_family_names(),
                })?;

            for &id in &selector.ids {
                if id >= family.n_codes() {
                    return Err(MarkerCfgError::IdOutOfRange {
                        family: name,
                        id,
                        n_codes: family.n_codes(),
                    });
                }
                let hit = Hit { family: name, id };
                // Dedup on the codeword, not on the name: overlapping dictionaries mean two
                // different names can denote the same physical marker.
                let key = (family.nbits_pattern_side(), family.codes()[id]);
                match set.wanted.get(&key) {
                    // `reported == hit` is the plain duplicate case, which needs no note.
                    Some(reported) if *reported != hit => {
                        if !set.aliases.iter().any(|a| a.also == hit) {
                            set.aliases.push(Alias { reported: reported.clone(), also: hit });
                        }
                    }
                    Some(_) => {}
                    None => {
                        set.wanted.insert(key, hit);
                    }
                }
            }

            if !set.families.iter().any(|f| std::ptr::eq(*f, family)) {
                set.families.push(family);
            }
        }

        Ok(set)
    }

    /// The dictionaries to pass to `detect_markers`, in one call.
    pub fn families(&self) -> &[&'static MarkerFamily] {
        &self.families
    }

    /// Map a detection back to the configured marker it is, if any.
    ///
    /// `dictionary` is the detection's index into [`families`](Self::families).
    pub fn resolve(&self, dictionary: usize, id: usize) -> Option<&Hit> {
        let family = self.families.get(dictionary)?;
        let code = *family.codes().get(id)?;
        self.wanted.get(&(family.nbits_pattern_side(), code))
    }

    /// How many distinct codewords count as a sighting.
    pub fn n_markers(&self) -> usize {
        self.wanted.len()
    }

    /// Configured markers that collapsed onto an earlier one's codeword.
    pub fn aliases(&self) -> &[Alias] {
        &self.aliases
    }
}

/// Names of every predefined family, for error messages.
pub fn known_family_names() -> Vec<&'static str> {
    predefined::ALL.iter().map(|(name, _)| *name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(family: &str, ids: &[usize]) -> MarkerSelector {
        MarkerSelector { family: family.into(), ids: ids.to_vec() }
    }

    /// Where a family sits in the set, for building detections by hand.
    fn dict_index(set: &MarkerSet, name: &str) -> usize {
        let family = predefined::get_by_name(name).unwrap();
        set.families().iter().position(|f| std::ptr::eq(*f, family)).unwrap()
    }

    #[test]
    fn each_family_is_passed_once_and_whole() {
        let set = MarkerSet::new(&[
            sel("DICT_5X5_250", &[1, 2]),
            sel("DICT_6X6_50", &[7]),
            sel("DICT_5X5_250", &[3]),
        ])
        .unwrap();
        assert_eq!(set.families().len(), 2);
        assert_eq!(set.families()[0].n_codes(), 250, "the family is not shrunk");
        assert_eq!(set.n_markers(), 4);
    }

    #[test]
    fn configured_ids_resolve_to_the_configured_identity() {
        let set = MarkerSet::new(&[sel("DICT_5X5_250", &[3, 30, 22])]).unwrap();
        assert_eq!(set.resolve(0, 30).unwrap(), &Hit { family: "DICT_5X5_250", id: 30 });
        assert_eq!(set.resolve(0, 22).unwrap(), &Hit { family: "DICT_5X5_250", id: 22 });
    }

    #[test]
    fn unconfigured_ids_of_a_configured_family_are_dropped() {
        let set = MarkerSet::new(&[sel("DICT_5X5_250", &[3])]).unwrap();
        assert!(set.resolve(0, 4).is_none());
        assert!(set.resolve(0, 250).is_none(), "out of range");
        assert!(set.resolve(1, 3).is_none(), "no such dictionary");
    }

    #[test]
    fn duplicate_selections_are_collapsed() {
        let set = MarkerSet::new(&[sel("DICT_4X4_50", &[5, 5]), sel("DICT_4X4_50", &[5])]).unwrap();
        assert_eq!(set.n_markers(), 1);
        assert!(set.aliases().is_empty());
    }

    /// The nested-prefix case: `DICT_4X4_50` is literally the first 50 codes of
    /// `DICT_4X4_250`, so these two names denote one card.
    #[test]
    fn overlapping_dictionaries_collapse_to_one_codeword() {
        let set = MarkerSet::new(&[sel("DICT_4X4_50", &[4]), sel("DICT_4X4_250", &[4])]).unwrap();
        assert_eq!(set.n_markers(), 1, "the same pattern must be counted once");
        assert_eq!(
            set.aliases(),
            &[Alias {
                reported: Hit { family: "DICT_4X4_50", id: 4 },
                also: Hit { family: "DICT_4X4_250", id: 4 },
            }]
        );
        // Whichever dictionary the detector reports it under, it maps to the first name.
        let expected = Hit { family: "DICT_4X4_50", id: 4 };
        assert_eq!(set.resolve(dict_index(&set, "DICT_4X4_50"), 4).unwrap(), &expected);
        assert_eq!(set.resolve(dict_index(&set, "DICT_4X4_250"), 4).unwrap(), &expected);
    }

    /// The case matching by name would miss: the detector reports id 4 under
    /// `DICT_4X4_250`, the first dictionary in the list, where only id 100 is configured.
    #[test]
    fn a_marker_reported_under_an_overlapping_dictionary_still_counts() {
        let set =
            MarkerSet::new(&[sel("DICT_4X4_250", &[100]), sel("DICT_4X4_50", &[4])]).unwrap();
        assert_eq!(
            set.resolve(dict_index(&set, "DICT_4X4_250"), 4).unwrap(),
            &Hit { family: "DICT_4X4_50", id: 4 }
        );
    }

    /// The only overlap in the whole predefined set where the *ids* differ too, so it is
    /// the only one where a reported id can surprise the operator.
    #[test]
    fn the_apriltag_overlap_is_reported_under_one_name() {
        let set = MarkerSet::new(&[
            sel("DICT_4X4_1000", &[854]),
            sel("DICT_APRILTAG_16H5", &[18]),
        ])
        .unwrap();
        assert_eq!(set.n_markers(), 1);
        assert_eq!(set.aliases().len(), 1);
        assert_eq!(set.aliases()[0].also, Hit { family: "DICT_APRILTAG_16H5", id: 18 });
        assert_eq!(
            set.resolve(dict_index(&set, "DICT_APRILTAG_16H5"), 18).unwrap(),
            &Hit { family: "DICT_4X4_1000", id: 854 }
        );
    }

    /// Guards the claim in the module docs. If a dictionary update introduces another
    /// cross-family overlap at differing ids, the docs and the alias warning need
    /// revisiting, so fail rather than let it pass unnoticed.
    #[test]
    fn no_unexpected_cross_family_collisions() {
        // code -> the (family, id) that first claimed it, within one grid size.
        let mut seen: HashMap<(usize, u64), (&str, usize)> = HashMap::new();
        let mut differing = Vec::new();
        for (name, family) in predefined::ALL.iter() {
            for (id, &code) in family.codes().iter().enumerate() {
                let key = (family.nbits_pattern_side(), code);
                match seen.get(&key) {
                    Some(&(prev_name, prev_id)) if prev_id != id => {
                        differing.push(format!("{prev_name}:{prev_id} == {name}:{id}"));
                    }
                    Some(_) => {}
                    None => {
                        seen.insert(key, (name, id));
                    }
                }
            }
        }
        assert_eq!(
            differing,
            vec!["DICT_4X4_1000:854 == DICT_APRILTAG_16H5:18".to_string()],
            "the set of cross-family collisions at differing ids has changed"
        );
    }

    #[test]
    fn rejects_unknown_family_and_says_what_is_valid() {
        let err = MarkerSet::new(&[sel("DICT_9X9_1", &[0])]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("DICT_9X9_1"), "{msg}");
        assert!(msg.contains("DICT_4X4_50"), "error should list valid families: {msg}");
    }

    #[test]
    fn rejects_out_of_range_id() {
        let err = MarkerSet::new(&[sel("DICT_4X4_50", &[50])]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("50 markers") || msg.contains("has 50"), "{msg}");
    }
}
