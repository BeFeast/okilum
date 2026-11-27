//! Digit choices use the exact controls and option order from the expanded question.
use super::*;
use gpui_component::WindowExt as _;

pub(super) struct QuestionKeys {
    control: QuestionControl,
    options: Vec<Option<String>>,
}

impl QuestionKeys {
    pub(super) fn new(control: QuestionControl, options: &[serde_json::Value]) -> Self {
        Self {
            control,
            options: options
                .iter()
                .map(|option| {
                    option["value"]
                        .as_str()
                        .or(option["label"].as_str())
                        .map(str::to_owned)
                })
                .collect(),
        }
    }
}

impl Actions {
    /// The conversation ancestor routes bubbling keys here, including focus on
    /// history and other noneditable controls outside the pinned request itself.
    pub(in crate::project) fn handle_question_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = event.keystroke.modifiers;
        if modifiers.control
            || modifiers.alt
            || modifiers.platform
            || window.has_focused_input(cx)
            // The rendered focus path is current even before an existing
            // input's next paint refreshes the component-level registry.
            || window.context_stack().iter().any(|context| context.contains("Input"))
            || self
                .inputs
                .values()
                .any(|input| input.read(cx).focus_handle(cx).is_focused(window))
        {
            return false;
        }
        let logical_key = event
            .keystroke
            .key_char
            .as_deref()
            .unwrap_or(event.keystroke.key.as_str());
        let index = match logical_key {
            "1" => 0,
            "2" => 1,
            "3" => 2,
            "4" => 3,
            "5" => 4,
            "6" => 5,
            "7" => 6,
            "8" => 7,
            "9" => 8,
            _ => return false,
        };
        let Some(keys) = &self.question_keys else {
            return false;
        };
        if !self.pinned
            || !self.question_control_current(&keys.control)
            || !self.question_enabled(&keys.control.transition.request)
            || self.question_progress[&keys.control.transition.request].collapsed()
        {
            return false;
        }
        let Some(value) = keys.options.get(index).and_then(Clone::clone) else {
            return false;
        };
        let control = keys.control.clone();
        self.select_question_option(
            &control.transition.request,
            &control.transition.question,
            &value,
            window,
            cx,
        );
        cx.stop_propagation();
        true
    }
}
