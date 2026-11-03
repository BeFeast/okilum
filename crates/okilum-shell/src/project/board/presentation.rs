//! Source publications invalidate presentation, while viewport changes reuse it.
use super::*;

pub(super) struct State {
    at: u64,
    pub binding: Arc<Binding>,
    pub columns: [Vec<usize>; 5],
    pub counts: [usize; 5],
}
impl Board {
    pub(super) fn presentation(&mut self) -> Arc<State> {
        // Classification also uses second-resolution freshness and PR recency.
        // A resize crossing that boundary must not retain an obsolete column.
        let at = now();
        if let Some(state) = &self.presentation {
            if state.at == at {
                return state.clone();
            }
        }
        let source = self.binding.as_ref().expect("bound Board presentation");
        let binding = Arc::new(if self.snapshot.forgejo.last_success.is_some() {
            okilum_t3::board::live::project(
                source,
                &self.snapshot.forgejo.value,
                self.projection_time(),
            )
        } else {
            source.clone()
        });
        let mut columns: [Vec<usize>; 5] = Default::default();
        for (index, scope) in binding.issues.iter().enumerate() {
            if self
                .filter
                .as_ref()
                .is_none_or(|filter| filter == &scope.executor)
            {
                columns[self.delivery_column(scope) as usize].push(index);
            }
        }
        let counts = self.column_counts(&binding);
        let state = Arc::new(State {
            at,
            binding,
            columns,
            counts,
        });
        self.presentation = Some(state.clone());
        state
    }
}
