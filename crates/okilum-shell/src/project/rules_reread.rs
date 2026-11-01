//! Adapter for the explicit Project rules offer. Parent integration owns the
//! save hook and button: no gateway calls occur while observing a source save.
use okilum_t3::{
    actions::{DispatchMode, Intent},
    rules_refresh::{RulesOffer, RulesRefresh, RulesTarget},
};

/// On the offer button click, pass the captured offer and the currently verified
/// Manager identity. Feed this intent through the existing durable action path,
/// targeting `clicked.target` (including origin/project), never a later selection.
/// Do not replace or clear the user's independent composer draft.
///
/// After journal preparation call `RulesRefresh::prepared` with its request ID
/// before transport dispatch. Keep the returned receipt with the worker; pass
/// it to `acknowledge` only on matching queue acceptance. Retry uses that same
/// retained journal record. Neither queue acceptance nor a send failure asserts
/// that the Manager actually reloaded rules.
pub fn rules_reread_intent(
    refresh: &RulesRefresh,
    clicked: &RulesOffer,
    selected: &RulesTarget,
) -> Result<Intent, &'static str> {
    let text = refresh.message(clicked, selected)?;
    Ok(Intent::message(text, DispatchMode::QueueAfterActive))
}
