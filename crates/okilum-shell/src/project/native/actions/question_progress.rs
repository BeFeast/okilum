//! Request-local presentation only. Answer validity remains the backend's rule.
use okilum_t3::actions::{valid_answers, Request};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
    time::Duration,
};

pub(super) const AUTO_ADVANCE_DELAY: Duration = Duration::from_millis(200);

#[derive(Clone, Debug)]
pub(super) struct Transition {
    pub request: String,
    pub question: String,
    revision: u64,
    schema_revision: u64,
    instance: Arc<()>,
}

#[derive(Debug)]
pub(super) struct QuestionProgress {
    item: Value,
    mode: Option<String>,
    ids: Vec<String>,
    active: Option<String>,
    collapsed: Option<String>,
    revision: u64,
    schema_revision: u64,
    instance: Arc<()>,
    pub submitting: bool,
}

fn question_ids(item: &Value) -> Option<Vec<String>> {
    let questions = item["questions"]
        .as_array()
        .filter(|questions| !questions.is_empty())?;
    let mut seen = HashSet::new();
    questions
        .iter()
        .map(|question| {
            let id = question["id"].as_str().filter(|id| !id.trim().is_empty())?;
            seen.insert(id).then(|| id.to_owned())
        })
        .collect()
}

impl QuestionProgress {
    pub fn new(request: &Request) -> Self {
        let ids = question_ids(&request.item).unwrap_or_default();
        Self {
            active: ids.first().cloned(),
            item: request.item.clone(),
            mode: request.response_mode.clone(),
            ids,
            collapsed: None,
            revision: 0,
            schema_revision: 0,
            instance: Arc::new(()),
            submitting: false,
        }
    }

    pub fn reconcile(&mut self, request: &Request) -> bool {
        if self.item == request.item && self.mode == request.response_mode {
            return false;
        }
        self.invalidate();
        self.schema_revision = self.schema_revision.wrapping_add(1);
        let replaced = self.item["id"] != request.item["id"];
        self.item = request.item.clone();
        self.mode = request.response_mode.clone();
        self.ids = question_ids(&request.item).unwrap_or_default();
        if replaced {
            self.instance = Arc::new(());
            self.active = self.ids.first().cloned();
            self.collapsed = None;
            self.submitting = false;
        } else {
            if self.active.as_ref().is_none_or(|id| !self.ids.contains(id)) {
                self.active = self.ids.first().cloned();
            }
            if self
                .collapsed
                .as_ref()
                .is_some_and(|id| !self.ids.contains(id))
            {
                self.collapsed = None;
            }
        }
        true
    }

    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }
    pub fn position(&self) -> Option<(usize, usize)> {
        let index = self
            .ids
            .iter()
            .position(|id| Some(id.as_str()) == self.active())?;
        Some((index, self.ids.len()))
    }
    pub fn collapsed(&self) -> bool {
        self.active.is_some() && self.active == self.collapsed
    }
    pub fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn toggle_collapsed(&mut self) {
        self.invalidate();
        self.collapsed = if self.collapsed() {
            None
        } else {
            self.active.clone()
        };
    }
    pub fn previous(&mut self) -> bool {
        let Some((index, _)) = self.position().filter(|(index, _)| *index > 0) else {
            return false;
        };
        self.invalidate();
        self.active = Some(self.ids[index - 1].clone());
        true
    }
    pub fn next(&mut self) -> bool {
        let Some((index, _)) = self.position().filter(|(index, count)| index + 1 < *count) else {
            return false;
        };
        self.invalidate();
        self.active = Some(self.ids[index + 1].clone());
        true
    }
    pub fn begin_submission(&mut self) {
        self.invalidate();
        self.submitting = true;
    }
    pub fn finish_submission(&mut self) {
        if self.submitting {
            self.submitting = false;
            self.invalidate();
        }
    }
    pub fn transition(&self, request: &str) -> Option<Transition> {
        Some(Transition {
            request: request.into(),
            question: self.active()?.into(),
            revision: self.revision,
            schema_revision: self.schema_revision,
            instance: self.instance.clone(),
        })
    }
    pub fn accepts(&self, transition: &Transition) -> bool {
        Arc::ptr_eq(&self.instance, &transition.instance)
            && self.revision == transition.revision
            && self.active() == Some(transition.question.as_str())
            && !self.collapsed()
            && !self.submitting
    }
    pub fn same_submission(&self, transition: &Transition) -> bool {
        Arc::ptr_eq(&self.instance, &transition.instance)
            && self.revision == transition.revision
            && self.submitting
    }
    pub fn same_instance(&self, transition: &Transition) -> bool {
        Arc::ptr_eq(&self.instance, &transition.instance)
    }
    pub fn interaction_current(&self, transition: &Transition) -> bool {
        self.same_instance(transition)
            && self.schema_revision == transition.schema_revision
            && self.active() == Some(transition.question.as_str())
    }
}

#[cfg(test)]
mod state_tests;

/// A one-question projection preserves the authoritative response mode. Missing
/// optional answers can advance; final submission still uses full State::allows.
pub(super) fn question_complete(
    request: &Request,
    question: &str,
    answers: &BTreeMap<String, Value>,
) -> bool {
    if question_ids(&request.item).is_none() {
        return false;
    }
    let Some(value) = request.item["questions"]
        .as_array()
        .and_then(|questions| questions.iter().find(|value| value["id"] == question))
    else {
        return false;
    };
    let mut item = request.item.clone();
    item["questions"] = Value::Array(vec![value.clone()]);
    item["responseMode"] = Value::String(
        if request.response_mode.as_deref() == Some("message") {
            "message"
        } else {
            "live"
        }
        .into(),
    );
    let answer = answers
        .get(question)
        .map(|answer| (question.to_owned(), answer.clone()))
        .into_iter()
        .collect();
    valid_answers(&item, &answer)
}
