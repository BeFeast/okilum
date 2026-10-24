//! Run the actual Feedback payload/transport tests without linking GPUI.
//! Rendering and diagnostic-file persistence remain shell/native acceptance.
//! Every HTTP request in this target uses an injected loopback mock endpoint.
#[allow(dead_code)]
#[path = "../../okilum-shell/src/reader_feedback/model.rs"]
mod model;

// Only the UI diagnostic sink and disabled Sprint mode are substituted. Payload,
// attachment reopening, endpoint policy and receipt validation are production code.
mod reader_diagnostics {
    pub fn network_failure(_operation: &str, _error: &anyhow::Error) {}
    pub fn feedback_delivery(_endpoint: &str) {}
}
#[allow(dead_code)]
mod sprint {
    pub fn enabled() -> bool {
        false
    }
    pub fn context() -> serde_json::Value {
        serde_json::json!({})
    }
    pub fn title() -> String {
        "isolated mock".into()
    }
}
