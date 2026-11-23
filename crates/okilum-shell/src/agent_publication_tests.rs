use super::agent_publication_is_current;
use std::sync::Arc;

#[test]
fn shared_publication_replacement_retires_same_generation_results() {
    let captured = Arc::new("same inventory contents");
    let retained = captured.clone();
    assert!(agent_publication_is_current(&captured, &retained, 7, 7));
    let shared_refresh = Arc::new("same inventory contents");
    assert!(!agent_publication_is_current(
        &captured,
        &shared_refresh,
        7,
        7
    ));
    assert!(!agent_publication_is_current(&captured, &retained, 7, 8));
    assert!(agent_publication_is_current(
        &shared_refresh,
        &shared_refresh,
        8,
        8
    ));
}
