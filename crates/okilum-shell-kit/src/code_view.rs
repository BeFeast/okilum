//! File view choices (Table, Tree, Formatted, Raw) remembered per file type.
use crate::brand;
use gpui_component::Icon;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum View {
    Table,
    Tree,
    Formatted,
    #[default]
    Raw,
}
impl View {
    pub const ALL: [Self; 4] = [Self::Table, Self::Tree, Self::Formatted, Self::Raw];
    pub fn label(self) -> &'static str {
        match self {
            Self::Table => "Table",
            Self::Tree => "Tree",
            Self::Formatted => "Formatted",
            Self::Raw => "Raw",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::Table => "code-view-table",
            Self::Tree => "code-view-tree",
            Self::Formatted => "code-view-formatted",
            Self::Raw => "code-view-raw",
        }
    }
    pub fn icon(self) -> Icon {
        Icon::default().path(match self {
            Self::Table => brand::CODE_VIEW_TABLE_ICON,
            Self::Tree => brand::CODE_VIEW_TREE_ICON,
            Self::Formatted => "icons/list.svg",
            Self::Raw => "icons/code-xml.svg",
        })
    }
}
