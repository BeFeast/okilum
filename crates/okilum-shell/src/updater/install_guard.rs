//! Shared final installation boundary, after asynchronous freshness verification.
//! Protection reads the current editor state here; an earlier save is insufficient.

pub(crate) fn finish<C, E>(
    context: &mut C,
    protect: impl FnOnce(&mut C) -> Result<(), E>,
    persist: impl FnOnce(&mut C) -> Result<(), E>,
    commit: impl FnOnce(&mut C) -> Result<(), E>,
    refuse: impl FnOnce(&mut C),
) -> Result<(), E> {
    let result = protect(context)
        .and_then(|()| persist(context))
        .and_then(|()| commit(context));
    if result.is_err() {
        refuse(context);
    }
    result
}
