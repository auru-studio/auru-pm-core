//! Integrity checks for a merged Live Set.
//!
//! A three-way merge that reports no conflicts has proven something narrow:
//! no single field was edited differently on both sides. That is not the same
//! as the result being a valid Live Set, and for Ableton the gap is concrete.
//!
//! Live allocates modulation and automation identities from one monotonic
//! counter at `LiveSet/NextPointeeId`. Two people working from the same
//! ancestor each add a modulation; both allocate the *same* next id and both
//! advance the counter to the *same* value. To a structural merge these are
//! identical edits to one scalar — no conflict, clean result. The merged set
//! now contains two different elements claiming one identity, and the counter
//! will hand that identity out again.
//!
//! Nothing in a format-agnostic merge can see that. So a clean merge is
//! checked here before it is called clean, and a failure is surfaced for a
//! person to decide on rather than written out as a working project.
//!
//! Measured against a real Live 12 set, a healthy file satisfies both
//! invariants exactly: 10,272 modulation identities, all distinct, the highest
//! being 37903 against a `NextPointeeId` of 37904.

use std::collections::BTreeMap;
use std::fmt;

use crate::project_format::XmlElement;

/// Something wrong with a Live Set's internal consistency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IntegrityProblem {
    /// Two elements claim the same modulation identity. Live routes
    /// modulation by this id, so a duplicate makes the routing ambiguous.
    DuplicateModulationId {
        id: u64,
        /// Tags that claimed it, in document order.
        tags: Vec<String>,
    },
    /// The allocator would hand out an identity already in use.
    ModulationCounterTooLow {
        next_pointee_id: u64,
        highest_allocated: u64,
    },
    /// Two sibling elements share an `Id`. This is the identity the
    /// three-way merge matches array members on, so a collision means
    /// subsequent merges of this set would pair up the wrong elements.
    DuplicateSiblingId { parent: String, id: String },
}

impl fmt::Display for IntegrityProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateModulationId { id, tags } => write!(
                formatter,
                "modulation identity {id} is claimed by {} elements ({})",
                tags.len(),
                tags.join(", ")
            ),
            Self::ModulationCounterTooLow {
                next_pointee_id,
                highest_allocated,
            } => write!(
                formatter,
                "the modulation counter is {next_pointee_id} but {highest_allocated} is already \
                 in use; the next modulation added would collide"
            ),
            Self::DuplicateSiblingId { parent, id } => write!(
                formatter,
                "two children of <{parent}> share Id \"{id}\", which version matching relies on"
            ),
        }
    }
}

/// Check a Live Set for problems a structural merge cannot detect.
///
/// An empty result means the set is internally consistent as far as we can
/// tell. This is a safety net over merge output, not a full schema validation:
/// it reports only things we can state with confidence, so that a non-empty
/// result is always worth a person's attention.
pub(crate) fn validate(root: &XmlElement) -> Vec<IntegrityProblem> {
    let mut problems = Vec::new();
    check_modulation_identities(root, &mut problems);
    check_sibling_ids(root, &mut problems);
    problems
}

/// Whether an element draws its `Id` from the `NextPointeeId` counter.
///
/// Determined empirically against a real set: `Pointee`, every `*Target` tag
/// (`AutomationTarget`, `ModulationTarget`, `VolumeModulationTarget`, …) and
/// the indexed `ControllerTargets.N` all draw from one global space and are
/// unique across the document. Other `Id`-bearing tags — `FloatEvent`,
/// `ClipSlot`, `PluginFloatParameter` — are per-parent counters and repeat
/// legitimately, so they must not be checked here.
fn draws_from_pointee_counter(tag: &str) -> bool {
    tag == "Pointee" || tag.ends_with("Target") || tag.starts_with("ControllerTargets.")
}

fn check_modulation_identities(root: &XmlElement, problems: &mut Vec<IntegrityProblem>) {
    let mut claimed: BTreeMap<u64, Vec<String>> = BTreeMap::new();
    for element in root.descendants() {
        if !draws_from_pointee_counter(&element.tag) {
            continue;
        }
        let Some(id) = element
            .attribute("Id")
            .and_then(|id| id.parse::<u64>().ok())
        else {
            continue;
        };
        claimed.entry(id).or_default().push(element.tag.clone());
    }

    for (id, tags) in &claimed {
        if tags.len() > 1 {
            problems.push(IntegrityProblem::DuplicateModulationId {
                id: *id,
                tags: tags.clone(),
            });
        }
    }

    let Some(highest_allocated) = claimed.keys().next_back().copied() else {
        return;
    };
    let next = root
        .child("LiveSet")
        .and_then(|live_set| live_set.child_value("NextPointeeId"))
        .and_then(|value| value.parse::<u64>().ok());
    if let Some(next_pointee_id) = next {
        if next_pointee_id <= highest_allocated {
            problems.push(IntegrityProblem::ModulationCounterTooLow {
                next_pointee_id,
                highest_allocated,
            });
        }
    }
}

/// Verify the invariant version matching depends on.
///
/// Ableton `Id` values are unique among siblings but repeat across different
/// parents — `AutomationLane Id="0"` legitimately appears 18 times in one real
/// set. Only the sibling scope matters, because that is the scope the merge
/// matches array members in.
fn check_sibling_ids(root: &XmlElement, problems: &mut Vec<IntegrityProblem>) {
    for element in root.descendants() {
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for child in element.child_elements() {
            if let Some(id) = child.attribute("Id") {
                *seen.entry(id).or_insert(0) += 1;
            }
        }
        for (id, count) in seen {
            if count > 1 {
                problems.push(IntegrityProblem::DuplicateSiblingId {
                    parent: element.tag.clone(),
                    id: id.to_owned(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ableton::test_support::parse_xml;

    fn live_set(body: &str, next_pointee_id: u64) -> XmlElement {
        parse_xml(&format!(
            r#"<Ableton><LiveSet>
                <NextPointeeId Value="{next_pointee_id}" />
                {body}
            </LiveSet></Ableton>"#
        ))
    }

    #[test]
    fn a_consistent_set_should_report_no_problems() {
        let root = live_set(
            r#"<Tracks>
                <MidiTrack Id="1"><AutomationTarget Id="100" /></MidiTrack>
                <MidiTrack Id="2"><ModulationTarget Id="101" /><Pointee Id="102" /></MidiTrack>
            </Tracks>"#,
            103,
        );
        assert_eq!(validate(&root), vec![]);
    }

    #[test]
    fn two_branches_allocating_the_same_modulation_id_should_be_caught() {
        // Exactly the merge outcome a structural three-way merge calls clean:
        // both sides added a modulation, both took id 500, both advanced the
        // counter to 501. No field conflicts — but the set is broken.
        let root = live_set(
            r#"<Tracks>
                <MidiTrack Id="1"><ModulationTarget Id="500" /></MidiTrack>
                <MidiTrack Id="2"><AutomationTarget Id="500" /></MidiTrack>
            </Tracks>"#,
            501,
        );
        let problems = validate(&root);
        assert_eq!(
            problems,
            vec![IntegrityProblem::DuplicateModulationId {
                id: 500,
                tags: vec!["ModulationTarget".to_owned(), "AutomationTarget".to_owned()],
            }]
        );
    }

    #[test]
    fn a_counter_that_would_reissue_a_live_identity_should_be_caught() {
        let root = live_set(r#"<Tracks><Pointee Id="900" /></Tracks>"#, 900);
        assert_eq!(
            validate(&root),
            vec![IntegrityProblem::ModulationCounterTooLow {
                next_pointee_id: 900,
                highest_allocated: 900,
            }]
        );
    }

    #[test]
    fn a_counter_one_past_the_highest_identity_should_be_accepted() {
        // The exact relationship a real set holds: max 37903, counter 37904.
        let root = live_set(r#"<Tracks><Pointee Id="37903" /></Tracks>"#, 37904);
        assert_eq!(validate(&root), vec![]);
    }

    #[test]
    fn per_parent_counters_should_not_be_treated_as_modulation_identities() {
        // FloatEvent, ClipSlot and PluginFloatParameter repeat legitimately
        // across parents. Checking them would fail every healthy project.
        let root = live_set(
            r#"<Tracks>
                <MidiTrack Id="1"><Envelopes><FloatEvent Id="0" /></Envelopes></MidiTrack>
                <MidiTrack Id="2"><Envelopes><FloatEvent Id="0" /></Envelopes></MidiTrack>
            </Tracks>"#,
            10,
        );
        assert_eq!(validate(&root), vec![]);
    }

    #[test]
    fn ids_repeating_across_different_parents_should_be_accepted() {
        // `AutomationLane Id="0"` appears 18 times in one real project.
        let root = live_set(
            r#"<Tracks>
                <MidiTrack Id="1"><AutomationLanes><AutomationLane Id="0" /></AutomationLanes></MidiTrack>
                <MidiTrack Id="2"><AutomationLanes><AutomationLane Id="0" /></AutomationLanes></MidiTrack>
            </Tracks>"#,
            10,
        );
        assert_eq!(validate(&root), vec![]);
    }

    #[test]
    fn siblings_sharing_an_id_should_be_caught() {
        // Version matching pairs array members by Id within one parent, so a
        // sibling collision would make later merges pair the wrong elements.
        let root = live_set(
            r#"<Tracks><MidiTrack Id="7" /><MidiTrack Id="7" /></Tracks>"#,
            10,
        );
        assert_eq!(
            validate(&root),
            vec![IntegrityProblem::DuplicateSiblingId {
                parent: "Tracks".to_owned(),
                id: "7".to_owned(),
            }]
        );
    }

    #[test]
    fn indexed_controller_targets_should_be_checked() {
        let root = live_set(
            r#"<Tracks>
                <MidiTrack Id="1"><ControllerTargets.0 Id="700" /></MidiTrack>
                <MidiTrack Id="2"><ControllerTargets.7 Id="700" /></MidiTrack>
            </Tracks>"#,
            701,
        );
        assert!(matches!(
            validate(&root).as_slice(),
            [IntegrityProblem::DuplicateModulationId { id: 700, .. }]
        ));
    }

    #[test]
    fn a_set_with_no_modulation_should_validate() {
        let root = live_set("<Tracks />", 1);
        assert_eq!(validate(&root), vec![]);
    }

    #[test]
    fn problems_should_read_as_plain_sentences() {
        // These reach a person deciding whether to keep a merge.
        let problem = IntegrityProblem::ModulationCounterTooLow {
            next_pointee_id: 500,
            highest_allocated: 500,
        };
        assert_eq!(
            problem.to_string(),
            "the modulation counter is 500 but 500 is already in use; \
             the next modulation added would collide"
        );
    }
}
