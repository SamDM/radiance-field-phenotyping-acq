//! Resolve configured `family: ids` selectors into the marker families the detector runs.
//!
//! # Why families get merged
//!
//! `detect_markers` takes one family per call, and almost all of its cost — adaptive
//! threshold, contour finding, polygon approximation, perspective warp per candidate — is
//! spent *before* any family is consulted. Calling it once per configured family would
//! multiply that whole pipeline by the number of families, which the CPU budget cannot
//! absorb.
//!
//! Two families with the same grid size are interchangeable as far as the pipeline is
//! concerned: the same candidate patch is sampled the same way, and only the codeword
//! lookup differs. So selectors are grouped by `nbits_side_pattern` and each group becomes
//! a single [`OwnedMarkerFamily`] carrying just the requested codewords. One pass per grid
//! size, and in the common case where everything is the same size, exactly one pass total.
//!
//! Families of one grid size can still disagree on `nbits_hamming` — DICT_5X5_50 guarantees
//! a distance of 3 while DICT_5X5_250 manages only 2, because it packs five times as many
//! codes into the same grid. A merged group takes the **minimum** across its members, which
//! is the conservative direction: `nbits_hamming` sets how far a candidate may be corrected
//! toward a codeword, so the smallest value yields the strictest matching. Erring strict is
//! right here — a missed detection costs a second of latency, a false one starts a
//! recording nobody asked for.
//!
//! Shrinking the code list is a second, larger win: identification compares each candidate
//! against every codeword in all four rotations, so going from 250 codes to three cuts the
//! inner loop by two orders of magnitude.
//!
//! The catch is that `MarkerCorners::id` is an *index into the code list*, not the marker
//! ID. [`MergedFamily::resolve`] maps it back.
//!
//! # When two configured markers are the same marker
//!
//! Merging can be handed the same codeword twice under different names, because the
//! predefined dictionaries overlap. `DICT_4X4_50` is a prefix of `DICT_4X4_100`, which is
//! a prefix of `DICT_4X4_250`, and so on — so `DICT_4X4_50:4` and `DICT_4X4_250:4` are one
//! physical card with two names. Across every predefined family there is exactly one
//! overlap where the *ids* also differ: `DICT_4X4_1000:854` is the same pattern as
//! `DICT_APRILTAG_16H5:18`. (`no_unexpected_cross_family_collisions` pins that, so a
//! library update which changes it fails loudly rather than quietly.)
//!
//! None of this can trigger the wrong recording — a detection of a shared codeword means
//! a configured marker was seen either way, which is the only thing the trigger asks. It
//! only makes the *reported* name ambiguous. So a repeated codeword is stored once, the
//! first configured name wins, and the collapsed name is kept in [`MergedFamily::aliases`]
//! for the startup log. Storing it twice would be worse than cosmetic: the identification
//! loop would compare every candidate against the same pattern twice, and only ever
//! resolve to the first.

use std::collections::BTreeMap;

use arucotools::dictionaries::{predefined, HasMarkerFamily, OwnedMarkerFamily};

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

/// One detector pass: a synthetic family plus the mapping back to real marker identities.
#[derive(Debug)]
pub struct MergedFamily {
    family: OwnedMarkerFamily,
    /// Parallel to `family.codes`, so a detection's index maps straight through.
    origins: Vec<Hit>,
    aliases: Vec<Alias>,
}

impl MergedFamily {
    pub fn family(&self) -> &OwnedMarkerFamily {
        &self.family
    }

    /// Map a detection's code index back to the family and ID the operator configured.
    pub fn resolve(&self, index: usize) -> Option<&Hit> {
        self.origins.get(index)
    }

    /// How many distinct codewords this pass looks for.
    pub fn n_markers(&self) -> usize {
        self.origins.len()
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

/// One grid geometry's worth of accumulated selectors.
#[derive(Default)]
struct Group {
    /// Running minimum over the members; `None` until the first member joins.
    nbits_hamming: Option<usize>,
    codes: Vec<u64>,
    /// Parallel to `codes`.
    origins: Vec<Hit>,
    aliases: Vec<Alias>,
}

/// Validate selectors and group them into one merged family per grid geometry.
pub fn merge(selectors: &[MarkerSelector]) -> Result<Vec<MergedFamily>, MarkerCfgError> {
    // BTreeMap keeps the output order stable across runs, which keeps startup logs
    // and test expectations reproducible. Keyed on grid size; the group's hamming
    // distance is the running minimum over its members.
    let mut groups: BTreeMap<usize, Group> = BTreeMap::new();

    for selector in selectors {
        let family = predefined::get_by_name(&selector.family).ok_or_else(|| {
            MarkerCfgError::UnknownFamily {
                name: selector.family.clone(),
                known: known_family_names(),
            }
        })?;
        let codes = family.codes();

        for &id in &selector.ids {
            if id >= codes.len() {
                return Err(MarkerCfgError::IdOutOfRange {
                    family: family.name,
                    id,
                    n_codes: codes.len(),
                });
            }
            let group = groups.entry(family.nbits_side_pattern).or_default();
            group.nbits_hamming = Some(match group.nbits_hamming {
                Some(h) => h.min(family.nbits_hamming),
                None => family.nbits_hamming,
            });

            let hit = Hit { family: family.name, id };
            // Dedup on the codeword, not on the name: overlapping dictionaries mean two
            // different names can denote the same physical marker, and a pattern listed
            // twice would be compared against twice and resolve only to the first.
            match group.codes.iter().position(|&c| c == codes[id]) {
                Some(i) => {
                    let reported = group.origins[i].clone();
                    // `reported == hit` is the plain duplicate case, which needs no note.
                    if reported != hit && !group.aliases.iter().any(|a| a.also == hit) {
                        group.aliases.push(Alias { reported, also: hit });
                    }
                }
                None => {
                    group.codes.push(codes[id]);
                    group.origins.push(hit);
                }
            }
        }
    }

    Ok(groups
        .into_iter()
        .map(|(nbits_side_pattern, group)| MergedFamily {
            family: OwnedMarkerFamily {
                name: format!("auto-record-{nbits_side_pattern}x{nbits_side_pattern}").into(),
                nbits_side_pattern,
                // Only reachable for a group with at least one member, which always sets it.
                nbits_hamming: group.nbits_hamming.unwrap_or(0),
                codes: group.codes.into(),
            },
            origins: group.origins,
            aliases: group.aliases,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(family: &str, ids: &[usize]) -> MarkerSelector {
        MarkerSelector { family: family.into(), ids: ids.to_vec() }
    }

    #[test]
    fn same_geometry_families_share_one_pass() {
        // DICT_5X5_50 and DICT_5X5_250 differ only in how many codes they define, so the
        // detector cannot tell them apart at the pipeline level — one pass must cover both.
        let merged = merge(&[sel("DICT_5X5_50", &[1, 2]), sel("DICT_5X5_250", &[7])]).unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].n_markers(), 3);
    }

    #[test]
    fn different_geometries_get_their_own_pass() {
        let merged = merge(&[sel("DICT_4X4_50", &[1]), sel("DICT_6X6_250", &[2])]).unwrap();
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merged_family_carries_only_the_requested_codes() {
        let merged = merge(&[sel("DICT_5X5_250", &[3, 30, 22])]).unwrap();
        assert_eq!(merged[0].family().codes().len(), 3);
        // ...rather than the 250 the source family defines.
        assert_eq!(predefined::DICT_5X5_250.codes().len(), 250);
    }

    #[test]
    fn detection_index_maps_back_to_the_configured_identity() {
        let merged = merge(&[sel("DICT_5X5_250", &[3, 30, 22])]).unwrap();
        let f = &merged[0];
        assert_eq!(f.resolve(0).unwrap(), &Hit { family: "DICT_5X5_250", id: 3 });
        assert_eq!(f.resolve(1).unwrap(), &Hit { family: "DICT_5X5_250", id: 30 });
        assert_eq!(f.resolve(2).unwrap(), &Hit { family: "DICT_5X5_250", id: 22 });
        assert!(f.resolve(3).is_none());
    }

    #[test]
    fn merged_codes_match_the_source_family() {
        let merged = merge(&[sel("DICT_5X5_250", &[30])]).unwrap();
        assert_eq!(merged[0].family().codes()[0], predefined::DICT_5X5_250.codes()[30]);
    }

    #[test]
    fn duplicate_selections_are_collapsed() {
        let merged = merge(&[sel("DICT_4X4_50", &[5, 5]), sel("DICT_4X4_50", &[5])]).unwrap();
        assert_eq!(merged[0].n_markers(), 1);
    }

    /// The nested-prefix case: `DICT_4X4_50` is literally the first 50 codes of
    /// `DICT_4X4_250`, so these two names denote one card.
    #[test]
    fn overlapping_dictionaries_collapse_to_one_codeword() {
        let merged = merge(&[sel("DICT_4X4_50", &[4]), sel("DICT_4X4_250", &[4])]).unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].n_markers(), 1, "the same pattern must be stored once");
        assert_eq!(
            merged[0].aliases(),
            &[Alias {
                reported: Hit { family: "DICT_4X4_50", id: 4 },
                also: Hit { family: "DICT_4X4_250", id: 4 },
            }]
        );
        // Whichever name a detection is reported under, it is a configured marker, so the
        // trigger behaves identically. Only the label differs.
        assert_eq!(merged[0].resolve(0).unwrap().family, "DICT_4X4_50");
    }

    /// The only overlap in the whole predefined set where the *ids* differ too, so it is
    /// the only one where a reported id can surprise the operator.
    #[test]
    fn the_apriltag_overlap_is_reported_under_one_name() {
        let merged = merge(&[
            sel("DICT_4X4_1000", &[854]),
            sel("DICT_APRILTAG_16H5", &[18]),
        ])
        .unwrap();
        assert_eq!(merged[0].n_markers(), 1);
        assert_eq!(merged[0].aliases().len(), 1);
        assert_eq!(merged[0].aliases()[0].also, Hit { family: "DICT_APRILTAG_16H5", id: 18 });
    }

    /// Guards the claim in the module docs. If a dictionary update introduces another
    /// cross-family overlap at differing ids, the docs and the alias warning need
    /// revisiting, so fail rather than let it pass unnoticed.
    #[test]
    fn no_unexpected_cross_family_collisions() {
        use std::collections::HashMap;

        // code -> the (family, id) that first claimed it, within one grid size.
        let mut seen: HashMap<(usize, u64), (&str, usize)> = HashMap::new();
        let mut differing = Vec::new();
        for (name, _) in predefined::ALL.iter() {
            let family = predefined::get_by_name(name).expect("ALL names resolve");
            for (id, &code) in family.codes().iter().enumerate() {
                let key = (family.nbits_side_pattern, code);
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
        let err = merge(&[sel("DICT_9X9_1", &[0])]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("DICT_9X9_1"), "{msg}");
        assert!(msg.contains("DICT_4X4_50"), "error should list valid families: {msg}");
    }

    #[test]
    fn rejects_out_of_range_id() {
        let err = merge(&[sel("DICT_4X4_50", &[50])]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("50 markers") || msg.contains("has 50"), "{msg}");
    }
}
