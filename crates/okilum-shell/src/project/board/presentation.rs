//! Source publications invalidate presentation, while viewport changes reuse it.
use super::*;

pub(super) struct State {
    at: u64,
    pub binding: Arc<Binding>,
    pub columns: [Vec<usize>; 5],
    pub counts: [usize; 5],
}
impl Board {
    pub(super) fn compare_scopes(
        &self,
        a: &okilum_t3::board::Scope,
        b: &okilum_t3::board::Scope,
    ) -> std::cmp::Ordering {
        match (
            self.snapshot.forgejo.value.get(&a.number),
            self.snapshot.forgejo.value.get(&b.number),
        ) {
            (Some(a), Some(b)) => okilum_t3::board::order::compare(a, b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.number.cmp(&a.number),
        }
    }
    pub(super) fn presentation(&mut self) -> Arc<State> {
        self.presentation_at(now())
    }
    fn presentation_at(&mut self, at: u64) -> Arc<State> {
        // Classification also uses second-resolution freshness and PR recency.
        // A resize crossing that boundary must not retain an obsolete column.
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
                self.projection_time_at(at),
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
        for column in &mut columns {
            column.sort_by(|&a, &b| self.compare_scopes(&binding.issues[a], &binding.issues[b]));
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

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[gpui::test]
    fn viewport_reuses_membership_but_source_publication_and_freshness_rebuild_it(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::test_init);
        let temp = tempfile::tempdir().unwrap();
        let mut board = None;
        let (_, visual) = cx.add_window_view(|window, cx| {
            board = Some(
                cx.new(|cx| Board::performance_fixture(temp.path().join("board"), window, cx)),
            );
            // An unmounted Board isolates source notification delivery from
            // redraws: the publication control must invalidate the same epoch.
            Root::new(cx.new(|_| gpui::EmptyView), window, cx)
        });
        visual.run_until_parked();
        let board = board.unwrap();
        let first = board.update(visual, |board, _| board.presentation_at(100));
        assert_eq!(first.binding.issues.len(), 240);
        let again = board.update(visual, |board, _| board.presentation_at(100));
        assert!(
            Arc::ptr_eq(&first, &again),
            "viewport work must reuse source projection"
        );
        board.update(visual, |board, cx| {
            board.binding.as_mut().unwrap().issues[0].expected =
                "Published synthetic context".into();
            cx.notify();
        });
        visual.run_until_parked();
        let changed = board.update(visual, |board, _| board.presentation_at(100));
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_eq!(
            changed.binding.issues[0].expected,
            "Published synthetic context"
        );
        assert_ne!(
            first.binding.issues[0].expected,
            changed.binding.issues[0].expected
        );
        let next_second = board.update(visual, |board, _| board.presentation_at(101));
        assert!(
            !Arc::ptr_eq(&changed, &next_second),
            "freshness/recency must be reevaluated"
        );
        assert_eq!(next_second.counts.iter().sum::<usize>(), 240);
    }
}
