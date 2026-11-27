//! Reader-integration tests for `okilum-shell-brain` that exercise `reader_open`
//! alongside `BrainView` (#3236, epic #3241).
use crate::brain::BrainView;
use ::core::prelude::v1::test;
use gpui::TestAppContext;
use uuid::Uuid;

#[gpui::test]
fn reader_file_delivery_preserves_managed_dirty_recovery(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::test_init(cx);
        crate::reader_open::install(cx);
    });
    let root = std::env::temp_dir().join(format!("okilum-managed-open327-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("read.md");
    std::fs::write(&file, "# Read only\n").unwrap();
    cx.add_window_view(|window, cx| {
        let mut view = BrainView::new("127.0.0.1:1".parse().unwrap(), window, cx);
        view.test_assert_dirty_recovery_preserved_across(window, cx, |cx| {
            crate::reader_open::dispatch_urls(
                vec![url::Url::from_file_path(&file).unwrap().into()],
                cx,
            );
        });
        view
    });
    std::fs::remove_dir_all(root).unwrap();
}
