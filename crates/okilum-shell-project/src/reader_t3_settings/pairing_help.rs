//! Shared, explicit CLI fallback for the existing browser-first onboarding.
use super::*;
use okilum_t3::Environment;

fn shell_argument(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn command(env: &Environment) -> String {
    format!(
        "t3 auth pairing create --label Okilum --ttl 5m --scope orchestration:read --scope orchestration:operate --base-url {}",
        shell_argument(&env.origin())
    )
}

pub(crate) fn view(env: &Environment) -> AnyElement {
    let command = command(env);
    let copy = command.clone();
    v_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .debug_selector(|| "manager-cli-help".into())
        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("CLI fallback"))
        .child(div().text_sm().whitespace_normal().child(
            "On the T3 server, run this in bash/zsh as the account running T3. Use the same T3 data directory; for a custom installation, add --base-dir followed by that directory.",
        ))
        .child(div().text_xs().font_family(source_presentation::CODE_FONT).whitespace_normal().child(command))
        .child(
            Button::new("manager-copy-pairing-command")
                .debug_selector(|| "manager-copy-pairing-command".into())
                .ghost()
                .small()
                .label("Copy command")
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                }),
        )
        .child(div().text_sm().whitespace_normal().child(
            "The command prints a one-time pair link/code valid for five minutes. Paste that output into the field above and choose Use pasted link. It connects this device; it is separate from your T3 sign-in.",
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn manager_2523_command_uses_selected_origin_scoped_grant_and_shell_quoting() {
        for origin in ["https://t3.example:8443", "http://[::1]:23110"] {
            let env = Environment::new("app", origin).unwrap();
            assert_eq!(command(&env), format!(
                "t3 auth pairing create --label Okilum --ttl 5m --scope orchestration:read --scope orchestration:operate --base-url '{origin}'"
            ));
        }
        assert_eq!(shell_argument("a'b;$HOME`id`"), "'a'\\''b;$HOME`id`'");
    }

    struct Help(Environment);
    impl Render for Help {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            view(&self.0)
        }
    }

    #[gpui::test]
    fn manager_2523_copy_button_copies_the_displayed_servers_command(cx: &mut TestAppContext) {
        cx.update(crate::test_init);
        let env = Environment::new("app", "https://configured.example:8443").unwrap();
        let expected = command(&env);
        let (_, visual) = cx.add_window_view(|_, _| Help(env));
        visual.simulate_resize(size(px(560.), px(500.)));
        visual.run_until_parked();
        visual.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("sentinel".into())));
        let button = visual
            .debug_bounds("manager-copy-pairing-command")
            .expect("copy button is visible");
        visual.simulate_mouse_down(button.center(), MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(button.center(), MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        visual.update(|_, cx| assert_eq!(cx.read_from_clipboard().unwrap().text(), Some(expected)));
    }
}
