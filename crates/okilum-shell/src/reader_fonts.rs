//! Document typography. The application chrome keeps the branded UI font.
use super::*;
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};

#[derive(Clone, Copy)]
pub(crate) enum Role {
    Note,
    Code,
}
impl Role {
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Note => 0,
            Self::Code => 1,
        }
    }
    pub(crate) fn default_family(self) -> &'static str {
        match self {
            Self::Note => brand::SANS_FONT,
            Self::Code => brand::MONO_FONT,
        }
    }
}

struct Catalog(Vec<SharedString>);
impl Global for Catalog {}
impl Catalog {
    fn new(names: Vec<String>) -> Self {
        let mut names = names;
        names.extend([brand::SANS_FONT.into(), brand::MONO_FONT.into()]);
        names.retain(|name| !name.trim().is_empty());
        names.sort_by_cached_key(|name| name.to_lowercase());
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        Self(names.into_iter().map(SharedString::from).collect())
    }
    fn resolve(&self, selected: &str, role: Role) -> SharedString {
        self.0
            .iter()
            .find(|name| name.eq_ignore_ascii_case(selected))
            .cloned()
            .unwrap_or_else(|| role.default_family().into())
    }
}

/// Enumerate once after registering embedded fonts, never per frame or note.
pub(crate) fn install(cx: &mut App) {
    if cx.try_global::<Catalog>().is_none() {
        cx.set_global(Catalog::new(cx.text_system().all_font_names()));
    }
}
pub(crate) fn family(role: Role, cx: &App) -> SharedString {
    cx.try_global::<Catalog>()
        .map(|catalog| catalog.resolve(reader_ui_state::selected_font(role, cx), role))
        .unwrap_or_else(|| role.default_family().into())
}
pub(crate) fn note(cx: &App) -> SharedString {
    family(Role::Note, cx)
}
/// Document styles shared by notes, rendered Live blocks and hover previews.
pub(crate) fn text_style(cx: &App) -> TextViewStyle {
    let mut style = reader_text_style(cx.theme());
    style.inline_code_font_family = Some(code(cx));
    style.code_block.text.font_family = Some(code(cx));
    style
}
pub(crate) fn code(cx: &App) -> SharedString {
    family(Role::Code, cx)
}

#[derive(Clone)]
struct FontItem(SharedString);
impl SelectItem for FontItem {
    type Value = SharedString;
    fn title(&self) -> SharedString {
        self.0.clone()
    }
    fn value(&self) -> &SharedString {
        &self.0
    }
    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .font_family(self.0.clone())
            .truncate()
            .child(format!("{} · Aa 0123", self.0))
    }
}
type Choice = SelectState<SearchableVec<FontItem>>;

#[derive(Default)]
pub(crate) struct Picker {
    choices: Option<[Entity<Choice>; 2]>,
    subscriptions: Vec<Subscription>,
}
impl Picker {
    fn init(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.choices.is_some() {
            return;
        }
        install(cx);
        self.choices = Some([Role::Note, Role::Code].map(|role| {
            let selected = family(role, cx);
            let items = cx
                .global::<Catalog>()
                .0
                .iter()
                .cloned()
                .map(FontItem)
                .collect::<Vec<_>>();
            let index = items
                .iter()
                .position(|item| item.0 == selected)
                .map(|row| gpui_component::IndexPath::default().row(row));
            let choice = cx.new(|cx| {
                SelectState::new(SearchableVec::new(items), index, window, cx).searchable(true)
            });
            self.subscriptions
                .push(cx.subscribe(&choice, move |_, _, event, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        reader_ui_state::set_selected_font(role, value, cx);
                    }
                }));
            choice
        }));
    }
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for role in [Role::Note, Role::Code] {
            reader_ui_state::set_selected_font(role, role.default_family(), cx);
            if let Some(choices) = &self.choices {
                choices[role.index()].update(cx, |choice, cx| {
                    choice.set_selected_value(&role.default_family().into(), window, cx);
                });
            }
        }
        cx.notify();
    }
}
impl Render for Picker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.init(window, cx);
        let choices = self.choices.as_ref().unwrap();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child("Document fonts")
                    .child(
                        Button::new("document-fonts-default")
                            .debug_selector(|| "document-fonts-default".into())
                            .ghost()
                            .icon(IconName::Undo2)
                            .accessibility_label("Restore default fonts")
                            .tooltip("Default fonts: Noto Sans and Cascadia Code")
                            .on_click(cx.listener(|this, _, window, cx| this.reset(window, cx))),
                    ),
            )
            .children(
                [
                    (
                        Role::Note,
                        "Note text",
                        "Reader, Live Preview and tables.",
                        "note-font",
                    ),
                    (
                        Role::Code,
                        "Code",
                        "Source, code files and code spans.",
                        "code-font",
                    ),
                ]
                .map(|(role, label, help, id)| {
                    reader_settings::setting_row(
                        label,
                        help,
                        div().debug_selector(move || id.into()).child(
                            Select::new(&choices[role.index()])
                                .w(px(240.))
                                .menu_width(px(360.))
                                .menu_max_h(px(280.))
                                .accessibility_label(label),
                        ),
                        cx,
                    )
                }),
            )
            .child(
                div()
                    .font_family(note(cx))
                    .child("The quick brown fox · אבג · 😀"),
            )
            .child(div().font_family(code(cx)).child("let answer = 42; // 😀"))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod test_text_system;
