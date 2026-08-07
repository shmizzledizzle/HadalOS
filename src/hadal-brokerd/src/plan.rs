//! Plans: a sequence of typed actions, and the comprehension check over them.
//!
//! Lives beside the `Action` enum it counts. This is the Android capability
//! set; the desktop broker's enum is Portage/systemd-shaped, and the logic
//! ports unchanged because it depends only on `Action::capability()` and
//! `Capability::tier()`, which both enums have.
//!
//! See `docs/ifixit.md` for why a plan is a `Vec<Action>` rather than a script.
//! The short version: a comprehension question is only a safeguard if it is
//! true of what will run, deriving that from arbitrary shell is undecidable,
//! and the enum is already the analysable form.

use std::collections::BTreeMap;

use crate::action::Action;
use crate::capability::{Capability, Tier};

#[derive(Debug, Clone)]
pub struct Plan {
    pub steps: Vec<Action>,
    /// The model's own words. Displayed, never trusted, and never the basis of
    /// the question — that is the whole point.
    pub rationale: String,
}

/// What a plan does, counted from parsed variants.
///
/// Counting enum variants rather than reading text is what makes the question
/// provably true: this is the same structure the executor walks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effects {
    counts: BTreeMap<&'static str, usize>,
}

impl Effects {
    pub fn of(plan: &Plan) -> Self {
        let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        for step in &plan.steps {
            // Every category present is listed, including at zero, so a plan
            // cannot hide a category by omitting it.
            *counts.entry(category(step)).or_insert(0) += 1;
        }
        for c in ALL_CATEGORIES {
            counts.entry(c).or_insert(0);
        }
        Effects { counts }
    }

    pub fn get(&self, category: &str) -> usize {
        self.counts.get(category).copied().unwrap_or(0)
    }

    /// Categories that actually occur, for display.
    pub fn nonzero(&self) -> Vec<(&'static str, usize)> {
        self.counts.iter().filter(|(_, n)| **n > 0).map(|(k, n)| (*k, *n)).collect()
    }
}

const ALL_CATEGORIES: &[&str] = &[
    "restart a service",
    "revoke a permission",
    "change an app's network access",
    "change a device setting",
    "read system state",
    "search online",
];

fn category(action: &Action) -> &'static str {
    match action.capability() {
        Capability::RestartService => "restart a service",
        Capability::RevokePermission => "revoke a permission",
        Capability::SetAppNetworkPolicy => "change an app's network access",
        Capability::WriteSetting => "change a device setting",
        Capability::NetworkLookup => "search online",
        _ => "read system state",
    }
}

/// The question, and the number that answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub summary: Vec<(&'static str, usize)>,
    pub asked_about: &'static str,
    pub answer: usize,
}

impl Question {
    /// Ask about a *mutating* category when there is one, and not always the
    /// largest. Always asking about the biggest number makes the answer
    /// guessable from the summary without reading which category was named.
    ///
    /// `seed` picks among the eligible categories. The caller supplies
    /// something unpredictable per proposal; a fixed choice would be learnable.
    pub fn derive(plan: &Plan, seed: u64) -> Option<Question> {
        let effects = Effects::of(plan);
        let summary = effects.nonzero();
        if summary.is_empty() {
            return None;
        }

        // Prefer categories that change something. A question about how many
        // things were *read* does not establish that the user understood what
        // the plan will alter.
        let mutating: Vec<&str> = plan
            .steps
            .iter()
            .filter(|s| s.capability().tier() == Tier::Mutate)
            .map(|s| category(s))
            .collect();

        let pool: Vec<&'static str> = if mutating.is_empty() {
            ALL_CATEGORIES.iter().filter(|c| effects.get(c) > 0).copied().collect()
        } else {
            let mut seen: Vec<&'static str> = Vec::new();
            for s in &plan.steps {
                let c = category(s);
                if s.capability().tier() == Tier::Mutate && !seen.contains(&c) {
                    seen.push(c);
                }
            }
            seen
        };
        if pool.is_empty() {
            return None;
        }

        let asked_about = pool[(seed as usize) % pool.len()];
        Some(Question { summary, asked_about, answer: effects.get(asked_about) })
    }

    pub fn render(&self) -> String {
        let mut out = String::from("This plan will:\n");
        for (cat, n) in &self.summary {
            out.push_str(&format!("  {n:>3}  {cat}\n"));
        }
        out.push_str(&format!("\nHow many will it {}?", self.asked_about));
        out
    }

    /// Exact match on a parsed integer.
    ///
    /// Deliberately not lenient. "one" is rejected, whitespace is not, and a
    /// wrong answer is final — the caller must discard the plan rather than
    /// re-prompt. Retrying until the number is right is the same as no check.
    pub fn accepts(&self, input: &str) -> bool {
        input.trim().parse::<usize>() == Ok(self.answer)
    }
}

/// The highest tier any step needs — the tier the whole plan is gated at.
///
/// A plan is authorised as one unit, so it must be authorised at its most
/// privileged step. Gating on the first or the average would let a mutation
/// ride along behind a read.
pub fn plan_tier(plan: &Plan) -> Option<Tier> {
    plan.steps
        .iter()
        .map(|s| s.capability().tier())
        .max_by_key(|t| match t {
            Tier::Read => 0,
            Tier::Inspect => 1,
            Tier::Egress => 2,
            Tier::Mutate => 3,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Action {
        crate::parse_proposal(json).expect("fixture should parse")
    }

    fn plan(jsons: &[&str]) -> Plan {
        Plan {
            steps: jsons.iter().map(|j| parse(j)).collect(),
            rationale: "because".into(),
        }
    }

    const RESTART: &str = r#"{"action":"restart-service","service":"hadald"}"#;
    const REVOKE: &str =
        r#"{"action":"revoke-permission","package":"com.foo.bar","permission":"android.permission.CAMERA"}"#;
    const READ: &str = r#"{"action":"read-logcat"}"#;
    const SETTING: &str =
        r#"{"action":"write-setting","change":{"kind":"location-services","enabled":false}}"#;

    #[test]
    fn counts_come_from_parsed_variants() {
        let p = plan(&[RESTART, RESTART, REVOKE, READ]);
        let e = Effects::of(&p);
        assert_eq!(e.get("restart a service"), 2);
        assert_eq!(e.get("revoke a permission"), 1);
        assert_eq!(e.get("read system state"), 1);
        assert_eq!(e.get("change a device setting"), 0);
    }

    /// The answer must match the count, whichever category is picked.
    #[test]
    fn the_answer_is_always_true_of_the_plan() {
        let p = plan(&[RESTART, RESTART, RESTART, REVOKE, SETTING, READ]);
        let e = Effects::of(&p);
        for seed in 0..40u64 {
            let q = Question::derive(&p, seed).expect("has steps");
            assert_eq!(q.answer, e.get(q.asked_about), "seed {seed} asked {}", q.asked_about);
            assert!(q.accepts(&q.answer.to_string()));
        }
    }

    /// A question about reads does not establish that the user understood what
    /// the plan changes.
    #[test]
    fn asks_about_something_that_mutates_when_anything_does() {
        let p = plan(&[READ, READ, READ, REVOKE]);
        for seed in 0..20u64 {
            let q = Question::derive(&p, seed).unwrap();
            assert_eq!(q.asked_about, "revoke a permission", "seed {seed}");
        }
    }

    /// Always asking about the largest count makes the answer guessable from
    /// the summary without reading which category was named.
    #[test]
    fn does_not_always_ask_about_the_same_category() {
        let p = plan(&[RESTART, RESTART, RESTART, REVOKE, SETTING]);
        let asked: std::collections::BTreeSet<_> =
            (0..30u64).map(|s| Question::derive(&p, s).unwrap().asked_about).collect();
        assert!(asked.len() > 1, "only ever asked about {asked:?}");
    }

    #[test]
    fn wrong_answers_are_rejected_and_near_misses_are_not_lenient() {
        let p = plan(&[RESTART, RESTART]);
        let q = Question::derive(&p, 0).unwrap();
        assert!(q.accepts("2"));
        assert!(q.accepts("  2 \n"));
        assert!(!q.accepts("3"));
        assert!(!q.accepts("two"));
        assert!(!q.accepts(""));
        assert!(!q.accepts("2 services"));
    }

    /// A plan is authorised as one unit, so it is gated at its most privileged
    /// step — otherwise a mutation rides along behind a read.
    #[test]
    fn a_plan_is_gated_at_its_highest_tier() {
        assert_eq!(plan_tier(&plan(&[READ, READ])), Some(Tier::Read));
        assert_eq!(plan_tier(&plan(&[READ, REVOKE])), Some(Tier::Mutate));
        assert_eq!(plan_tier(&plan(&[REVOKE, READ])), Some(Tier::Mutate));
    }

    /// A gate that skips itself on small changes is a gate a model can talk
    /// into judging the change small.
    #[test]
    fn even_a_single_step_plan_asks() {
        let q = Question::derive(&plan(&[RESTART]), 0);
        assert!(q.is_some());
        assert_eq!(q.unwrap().answer, 1);
    }

    #[test]
    fn an_empty_plan_has_no_question() {
        assert!(Question::derive(&Plan { steps: vec![], rationale: String::new() }, 0).is_none());
    }

    #[test]
    fn the_rendered_question_states_every_nonzero_category() {
        let p = plan(&[RESTART, REVOKE, READ]);
        let text = Question::derive(&p, 0).unwrap().render();
        for expected in ["restart a service", "revoke a permission", "read system state"] {
            assert!(text.contains(expected), "missing {expected} in:\n{text}");
        }
    }
}
