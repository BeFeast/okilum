//! Read-mode text actions; separate from editing and object-specific menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Command {
    Plain,
    Markdown,
    Rich,
    SelectAll,
    Document,
}

impl Command {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Plain => "Copy",
            Self::Markdown => "Copy as Markdown",
            Self::Rich => "Copy as Rich Text",
            Self::SelectAll => "Select all",
            Self::Document => "Copy entire document as Markdown",
        }
    }
}

pub(super) fn commands(selected: bool) -> &'static [Command] {
    if selected {
        &[Command::Plain, Command::Markdown, Command::Rich]
    } else {
        &[Command::SelectAll, Command::Document]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_and_background_offer_their_own_actions() {
        assert_eq!(
            commands(true),
            &[Command::Plain, Command::Markdown, Command::Rich]
        );
        assert_eq!(commands(false), &[Command::SelectAll, Command::Document]);
        assert_eq!(Command::Markdown.label(), "Copy as Markdown");
        assert_eq!(
            Command::Document.label(),
            "Copy entire document as Markdown"
        );
    }
}
