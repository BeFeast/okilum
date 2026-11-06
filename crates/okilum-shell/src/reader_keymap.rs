//! User key bindings over the complete installed defaults (#2626).
//! Parsing is transactional: an invalid file retains the last working keymap.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

mod panel;
pub(crate) use panel::KeyboardSettings;
#[cfg(test)]
mod tests;

const MAX_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Override {
    /// Global, ReadMode, Editor, or an explicit GPUI context predicate.
    #[serde(default = "global_context")]
    pub context: String,
    pub key: String,
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

fn global_context() -> String {
    "Global".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Document {
    pub version: u32,
    pub bindings: Vec<Override>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: 1,
            bindings: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub action: String,
    pub args: Option<serde_json::Value>,
    pub label: String,
    pub context: String,
    pub keys: Vec<String>,
    pub defaults: Vec<String>,
    pub changed: bool,
}

pub(crate) struct Keymap {
    defaults: Vec<KeyBinding>,
    pub path: Option<PathBuf>,
    pub document: Document,
    pub error: Option<String>,
    pub rows: Vec<Row>,
    seen: Option<Vec<u8>>,
    read_error: bool,
    pub generation: u64,
    pub recording: Option<WindowId>,
}
impl Global for Keymap {}

fn context(value: &str) -> anyhow::Result<Option<String>> {
    let value = match value {
        "Global" | "" => return Ok(None),
        "ReadMode" => "ReaderRead && !Input && !ReaderTree",
        "Editor" => "ReaderSource > Input",
        other => other,
    };
    Ok(Some(
        gpui::KeyBindingContextPredicate::parse(value)?.to_string(),
    ))
}

fn context_of(binding: &KeyBinding) -> String {
    binding
        .predicate()
        .map(|p| p.to_string())
        .unwrap_or_else(global_context)
}

fn keys(binding: &KeyBinding) -> String {
    binding
        .keystrokes()
        .iter()
        .map(|k| k.unparse())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn normalize_key(value: &str) -> anyhow::Result<String> {
    let parts = value
        .split_whitespace()
        .map(gpui::Keystroke::parse)
        .collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(!parts.is_empty(), "A shortcut cannot be empty");
    Ok(parts
        .iter()
        .map(gpui::Keystroke::unparse)
        .collect::<Vec<_>>()
        .join(" "))
}

fn action_args(action: &dyn Action) -> Option<serde_json::Value> {
    action
        .as_any()
        .downcast_ref::<gpui_component::input::Enter>()
        .map(|enter| serde_json::json!({ "secondary": enter.secondary, "shift": enter.shift }))
}

fn target_args(entry: &Override) -> Option<serde_json::Value> {
    entry.args.clone().or_else(|| {
        (entry.action.as_deref() == Some(gpui_component::input::Enter::name_for_type()))
            .then(|| serde_json::json!({ "secondary": false, "shift": false }))
    })
}

fn binding(entry: &Override, cx: &App) -> anyhow::Result<KeyBinding> {
    let predicate = context(&entry.context)?
        .map(|s| gpui::KeyBindingContextPredicate::parse(&s))
        .transpose()?
        .map(Rc::new);
    let action = match &entry.action {
        Some(action) if action == gpui_component::input::Enter::name_for_type() => Box::new(
            serde_json::from_value::<gpui_component::input::Enter>(target_args(entry).unwrap())?,
        )
            as Box<dyn Action>,
        Some(action) => cx.build_action(action, entry.args.clone())?,
        None => {
            anyhow::ensure!(
                entry.args.is_none(),
                "Disabled shortcuts cannot have action arguments"
            );
            Box::new(gpui::NoAction) as Box<dyn Action>
        }
    };
    let key = normalize_key(&entry.key)?;
    Ok(KeyBinding::load(
        &key,
        action,
        predicate,
        false,
        None,
        &gpui::DummyKeyboardMapper,
    )?)
}

/// Remove only exact context/key defaults. More specific overrides and nulls
/// retain GPUI's native context precedence over broader defaults.
fn compile(
    document: &Document,
    defaults: &[KeyBinding],
    cx: &App,
) -> anyhow::Result<Vec<KeyBinding>> {
    anyhow::ensure!(
        document.version == 1,
        "Unsupported keymap version {}",
        document.version
    );
    anyhow::ensure!(
        document.bindings.len() <= 4096,
        "The keymap has more than 4096 overrides"
    );
    let mut occupied = HashSet::new();
    let mut custom = Vec::new();
    for (index, entry) in document.bindings.iter().enumerate() {
        let parsed = binding(entry, cx)
            .map_err(|error| anyhow::anyhow!("Binding {}: {error}", index + 1))?;
        let identity = (context_of(&parsed), keys(&parsed));
        anyhow::ensure!(
            occupied.insert(identity),
            "Binding {} duplicates a shortcut in the same context",
            index + 1
        );
        custom.push(parsed);
    }
    let mut result: Vec<_> = defaults
        .iter()
        .filter(|b| !occupied.contains(&(context_of(b), keys(b))))
        .cloned()
        .collect();
    result.extend(custom);
    Ok(result)
}

fn rows(
    defaults: &[KeyBinding],
    effective: &[KeyBinding],
    document: &Document,
    cx: &App,
) -> Vec<Row> {
    let mut rows = BTreeMap::<(String, String, String), Row>::new();
    let make = |action: &str, scope: String| Row {
        action: action.into(),
        args: None,
        label: reader_shortcuts::action_label(action),
        context: scope,
        keys: Vec::new(),
        defaults: Vec::new(),
        changed: false,
    };
    for name in cx.all_action_names() {
        if cx.build_action(name, None).is_ok()
            && *name != gpui::NoAction::name_for_type()
            && *name != gpui::Unbind::name_for_type()
        {
            rows.insert(
                (name.to_string(), global_context(), String::new()),
                make(name, global_context()),
            );
        }
    }
    for (is_default, bindings) in [(true, defaults), (false, effective)] {
        for binding in bindings {
            if gpui::is_no_action(binding.action()) || gpui::is_unbind(binding.action()) {
                continue;
            }
            let action = binding.action().name();
            let scope = context_of(binding);
            let args = action_args(binding.action());
            let row = rows
                .entry((
                    action.into(),
                    scope.clone(),
                    args.as_ref().map(ToString::to_string).unwrap_or_default(),
                ))
                .or_insert_with(|| {
                    let mut row = make(action, scope);
                    if let Some(enter) = binding
                        .action()
                        .as_any()
                        .downcast_ref::<gpui_component::input::Enter>()
                    {
                        row.label = match (enter.secondary, enter.shift) {
                            (false, false) => "Confirm input",
                            (false, true) => "Confirm input with Shift",
                            (true, _) => "Confirm input with Ctrl/Cmd",
                        }
                        .into();
                    }
                    row.args = args;
                    row
                });
            let list = if is_default {
                &mut row.defaults
            } else {
                &mut row.keys
            };
            let key = keys(binding);
            if !list.contains(&key) {
                list.push(key);
            }
        }
    }
    let mut result: Vec<_> = rows.into_values().collect();
    for row in &mut result {
        row.changed = document.bindings.iter().any(|entry| {
            context(&entry.context)
                .ok()
                .flatten()
                .unwrap_or_else(global_context)
                == row.context
                && ((entry.action.as_deref() == Some(row.action.as_str())
                    && target_args(entry) == row.args)
                    || normalize_key(&entry.key)
                        .ok()
                        .is_some_and(|key| row.defaults.contains(&key)))
        });
    }
    result.sort_by(|a, b| a.label.cmp(&b.label).then(a.context.cmp(&b.context)));
    result
}

fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("keymap.json exceeds 256 KiB".into());
    }
    Ok(Some(bytes))
}

fn adopt(bytes: Result<Option<Vec<u8>>, String>, cx: &mut App) {
    let state = cx.global::<Keymap>();
    if bytes.as_ref().is_ok_and(|bytes| *bytes == state.seen) && !state.read_error {
        return;
    }
    let parsed = bytes
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))
        .and_then(|bytes| match bytes {
            Some(bytes) => Ok(serde_json::from_slice::<Document>(bytes)?),
            None => Ok(Document::default()),
        })
        .and_then(|document| Ok((compile(&document, &state.defaults, cx)?, document)));
    match parsed {
        Ok((effective, document)) => {
            let rows = rows(&state.defaults, &effective, &document, cx);
            cx.clear_key_bindings();
            cx.bind_keys(effective);
            #[cfg(all(unix, feature = "brain"))]
            brain::app_quit::set_menus(cx);
            #[cfg(not(all(unix, feature = "brain")))]
            reader_app_menu::set_menus(cx);
            cx.update_global::<Keymap, _>(|state, _| {
                state.document = document;
                state.rows = rows;
                state.seen = bytes.unwrap();
                state.error = None;
                state.read_error = false;
                state.generation += 1;
            });
        }
        Err(error) => {
            let message = format!(
                "Could not load keymap.json: {error}. The last working shortcuts are still active."
            );
            if state.error.as_deref() == Some(&message) {
                return;
            }
            cx.update_global::<Keymap, _>(|state, _| {
                state.read_error = bytes.is_err();
                if let Ok(bytes) = bytes {
                    state.seen = bytes;
                }
                state.error = Some(message);
                state.generation += 1;
            });
        }
    }
    cx.refresh_windows();
}

pub(crate) fn install(cx: &mut App) {
    let path = reader_layout::config_base().map(|base| base.join("okilum/keymap.json"));
    install_at(path.clone(), cx);
    if let Some(path) = path {
        poll_file(path, cx);
    }
}

fn poll_file(path: PathBuf, cx: &mut App) {
    // Never poll the filesystem on the input/render executor. A malformed
    // edit is retried on the next change and does not reset working bindings.
    cx.spawn(async move |cx| loop {
        cx.background_executor().timer(Duration::from_secs(1)).await;
        let Ok(generation) = cx.update(|cx| cx.global::<Keymap>().generation) else {
            break;
        };
        let path = path.clone();
        let bytes = cx
            .background_executor()
            .spawn(async move { read(&path) })
            .await;
        if cx
            .update(|cx| {
                if cx.global::<Keymap>().generation == generation {
                    adopt(bytes, cx);
                }
            })
            .is_err()
        {
            break;
        }
    })
    .detach();
}

fn install_at(path: Option<PathBuf>, cx: &mut App) {
    let defaults: Vec<_> = cx.key_bindings().borrow().bindings().cloned().collect();
    let rows = rows(&defaults, &defaults, &Document::default(), cx);
    cx.set_global(Keymap {
        defaults,
        path: path.clone(),
        document: Document::default(),
        error: None,
        rows,
        seen: None,
        read_error: false,
        generation: 0,
        recording: None,
    });
    if let Some(path) = path {
        adopt(read(&path), cx);
    }
}

pub(crate) fn is_recording(window: &Window, cx: &App) -> bool {
    cx.try_global::<Keymap>()
        .is_some_and(|map| map.recording == Some(window.window_id()))
}

fn set_recording(value: Option<WindowId>, cx: &mut App) {
    if cx.has_global::<Keymap>() {
        cx.update_global::<Keymap, _>(|map, _| map.recording = value);
    }
}

pub(crate) fn key_label(key: &str) -> String {
    key.split_whitespace()
        .map(|key| platform::labels::Os::CURRENT.shortcut(key))
        .collect::<Vec<_>>()
        .join(" ")
}

fn save(path: &Path, expected: &Option<Vec<u8>>, document: &Document) -> anyhow::Result<Vec<u8>> {
    use std::io::Write;
    anyhow::ensure!(
        &read(path).map_err(|e| anyhow::anyhow!("{e}"))? == expected,
        "keymap.json changed on disk. Reload the list before saving again"
    );
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("No keymap directory"))?;
    std::fs::create_dir_all(parent)?;
    let mut bytes = serde_json::to_vec_pretty(document)?;
    bytes.push(b'\n');
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    anyhow::ensure!(
        &read(path).map_err(|e| anyhow::anyhow!("{e}"))? == expected,
        "keymap.json changed during save. Its newer contents were kept"
    );
    temporary.persist(path)?;
    Ok(bytes)
}

fn put(document: &mut Document, entry: Override) {
    let scope = context(&entry.context).ok();
    let key = normalize_key(&entry.key).ok();
    document
        .bindings
        .retain(|old| context(&old.context).ok() != scope || normalize_key(&old.key).ok() != key);
    document.bindings.push(entry);
}

/// Warnings describe real overlaps, including chord prefixes. Different Read
/// and Editor contexts are independent; broader/global bindings may be shadowed.
fn conflicts(row: &Row, key: &str, cx: &App) -> (Vec<String>, Vec<String>) {
    let mut same = Vec::new();
    let mut warnings = Vec::new();
    let scope = context(&row.context).ok().flatten();
    let candidate = scope
        .as_deref()
        .and_then(|s| gpui::KeyBindingContextPredicate::parse(s).ok());
    for other in &cx.global::<Keymap>().rows {
        if other.action == row.action && other.args == row.args && other.context == row.context {
            continue;
        }
        if !other.keys.iter().any(|other| {
            other == key
                || other.starts_with(&format!("{key} "))
                || key.starts_with(&format!("{other} "))
        }) {
            continue;
        }
        if other.context == row.context && other.keys.iter().any(|other| other == key) {
            same.push(other.label.clone());
            continue;
        }
        let other_predicate = context(&other.context)
            .ok()
            .flatten()
            .and_then(|s| gpui::KeyBindingContextPredicate::parse(&s).ok());
        let overlap = match (&candidate, other_predicate) {
            (Some(a), Some(b)) => a.is_superset(&b) || b.is_superset(a),
            _ => true,
        };
        if overlap {
            warnings.push(format!(
                "{} ({}) may be shadowed",
                other.label, other.context
            ));
        }
    }
    if cfg!(target_os = "windows") && key.contains("ctrl-") && key.contains("alt-") {
        warnings.push("Ctrl+Alt can be AltGr on Windows and may interfere with typing.".into());
    }
    if key.contains("super-")
        || [
            "alt-tab",
            "alt-f4",
            "ctrl-alt-delete",
            "ctrl-space",
            "cmd-space",
            "cmd-tab",
            "cmd-alt-escape",
        ]
        .contains(&key)
    {
        warnings.push("Your operating system or window manager may reserve this shortcut.".into());
    }
    same.sort();
    same.dedup();
    warnings.sort();
    warnings.dedup();
    (same, warnings)
}

pub(crate) fn stop_recording(window: &Window, cx: &mut App) {
    if is_recording(window, cx) {
        set_recording(None, cx);
    }
}
