//! Task Scheduler policy and adapter. The native port must use the current
//! interactive token (no password/elevation) and conditional ownership checks.
use super::{
    authority::StopToken, safe_text, supervisor::ipc::Scope, xml, Binding, Platform, Registration,
};
use anyhow::{ensure, Result};

pub struct Task {
    pub owner_sid: String,
    pub definition: String,
    pub running: bool,
}
pub trait TaskApi {
    fn current_sid(&self) -> Result<String>;
    fn read(&mut self, name: &str) -> Result<Option<Task>>;
    fn verify_payload(&mut self, binding: &Binding) -> Result<()>;
    /// TASK_CREATE, never TASK_CREATE_OR_UPDATE. A collision is an error.
    fn create(&mut self, name: &str, definition: &str) -> Result<()>;
    /// Native implementations re-read owner and definition before each effect.
    fn start_owned(&mut self, name: &str, definition: &str, owner: &str) -> Result<()>;
    /// Stop the supervisor and confirm its Job Object's children have exited.
    fn stop_owned(&mut self, name: &str, definition: &str, owner: &str) -> Result<()>;
    fn delete_owned(&mut self, name: &str, definition: &str, owner: &str) -> Result<()>;
}
pub struct TaskScheduler<A>(pub A);
pub fn task_name(binding: &Binding) -> String {
    format!("Tessera-Sync-{}", binding.instance)
}
pub(super) fn path(value: &str) -> Result<()> {
    safe_text(value)?;
    let bytes = value.as_bytes();
    ensure!(
        bytes.len() > 3 && bytes[0].is_ascii_alphabetic() && bytes[1..3] == *b":\\",
        "absolute local Windows path required"
    );
    ensure!(
        !value.contains('"')
            && !value.contains('/')
            && !value[2..].contains(':')
            && value.split('\\').all(|p| p != "." && p != ".."),
        "unsafe Windows path"
    );
    Ok(())
}
pub fn definition(binding: &Binding) -> Result<String> {
    path(&binding.supervisor)?;
    path(&binding.state_directory)?;
    safe_text(&binding.owner)?;
    safe_text(&binding.device_identity)?;
    ensure!(
        binding.owner.starts_with("S-1-")
            && binding
                .owner
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b'-' || b == b'S'),
        "invalid owner SID"
    );
    // Only the private state location and public instance ID enter argv.
    // A trailing backslash before a closing quote would escape that quote.
    let state = binding.state_directory.trim_end_matches('\\');
    ensure!(state.len() > 2, "private state cannot be the drive root");
    let args = format!("--instance {} --state \"{}\"", binding.instance, state);
    Ok(format!(
        r#"<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task"><RegistrationInfo><Description>Okilum Sync {} {} {}</Description></RegistrationInfo><Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId></LogonTrigger></Triggers><Principals><Principal id="Owner"><UserId>{}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals><Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure></Settings><Actions Context="Owner"><Exec><Command>{}</Command><Arguments>{}</Arguments><WorkingDirectory>{}</WorkingDirectory></Exec></Actions></Task>"#,
        binding.installation,
        binding.instance,
        xml(&binding.device_identity),
        xml(&binding.owner),
        xml(&binding.owner),
        xml(&binding.supervisor),
        xml(&args),
        xml(state)
    ))
}
fn parse_task(source: &str) -> Result<xmltree::Element> {
    let source = source.trim_start_matches('\u{feff}');
    let source = if source
        .strip_prefix("<?xml")
        .is_some_and(|rest| rest.starts_with([' ', '\t', '\r', '\n']))
    {
        let end = source
            .find("?>")
            .ok_or_else(|| anyhow::anyhow!("unclosed XML declaration"))?;
        &source[end + 2..]
    } else {
        source
    };
    Ok(xmltree::Element::parse(source.as_bytes())?)
}
fn canonical(source: &str) -> Result<String> {
    use xmltree::{Element, XMLNode};
    const NS: &str = "http://schemas.microsoft.com/windows/2004/02/mit/task";
    fn plain(node: &Element, text: &str) -> bool {
        node.attributes.is_empty()
            && node
                .children
                .iter()
                .all(|c| matches!(c, XMLNode::Text(_) | XMLNode::CData(_)))
            && node.get_text().as_deref() == Some(text)
    }
    fn normalize(node: &mut Element, parent: &str) {
        let path = format!("{parent}/{}", node.name);
        for child in &mut node.children {
            if let XMLNode::Element(el) = child {
                normalize(el, &path);
            }
        }
        node.children.retain(|child| {
            let XMLNode::Element(el) = child else {
                return true;
            };
            if el.namespace.as_deref() != Some(NS) {
                return true;
            }
            let ignored_metadata = path == "/Task/RegistrationInfo"
                && matches!(el.name.as_str(), "URI" | "SecurityDescriptor");
            let default = match (path.as_str(), el.name.as_str()) {
                ("/Task/Principals/Principal", "RunLevel") => Some("LeastPrivilege"),
                ("/Task/Triggers/LogonTrigger", "Enabled") | ("/Task/Settings", "Enabled") => {
                    Some("true")
                }
                ("/Task/Settings", "UseUnifiedSchedulingEngine") => Some("true"),
                ("/Task/Settings/IdleSettings", "StopOnIdleEnd") => Some("true"),
                ("/Task/Settings/IdleSettings", "RestartOnIdle") => Some("false"),
                _ => None,
            };
            let empty_idle = path == "/Task/Settings"
                && el.name == "IdleSettings"
                && el.attributes.is_empty()
                && el
                    .children
                    .iter()
                    .all(|c| matches!(c, XMLNode::Text(t) if t.trim().is_empty()));
            !(ignored_metadata || default.is_some_and(|value| plain(el, value)) || empty_idle)
        });
    }
    fn reject_mixed_content(node: &Element) -> Result<()> {
        let has_elements = node
            .children
            .iter()
            .any(|c| matches!(c, XMLNode::Element(_)));
        let has_text = node
            .children
            .iter()
            .any(|c| matches!(c, XMLNode::Text(t) | XMLNode::CData(t) if !t.trim().is_empty()));
        ensure!(
            !(has_elements && has_text),
            "mixed task XML content is unsupported"
        );
        for child in &node.children {
            if let XMLNode::Element(el) = child {
                reject_mixed_content(el)?;
            }
        }
        Ok(())
    }
    fn visit(node: &Element) -> String {
        fn field(out: &mut String, value: &str) {
            out.push_str(&format!("{}:{value}", value.len()));
        }
        let mut out = String::new();
        field(&mut out, &node.name);
        field(&mut out, node.namespace.as_deref().unwrap_or(""));
        let mut attrs: Vec<_> = node.attributes.iter().collect();
        attrs.sort();
        out.push('[');
        for (key, value) in attrs {
            field(&mut out, key);
            field(&mut out, value);
        }
        out.push(']');
        out.push('{');
        let mut children = Vec::new();
        for child in &node.children {
            match child {
                XMLNode::Element(el) => children.push(visit(el)),
                XMLNode::Text(s) | XMLNode::CData(s) if !s.trim().is_empty() => field(&mut out, s),
                _ => (),
            }
        }
        // The owned definition has one principal, trigger and action. Sorting
        // fields preserves duplicates and unknown nodes, which still mismatch.
        children.sort();
        for child in children {
            field(&mut out, &child);
        }
        out.push('}');
        out
    }
    let mut element = parse_task(source)?;
    reject_mixed_content(&element)?;
    normalize(&mut element, "");
    Ok(visit(&element))
}

#[cfg(any(target_os = "windows", test))]
fn resolve_task_accounts(source: &str, resolve: impl Fn(&str) -> Result<String>) -> Result<String> {
    use xmltree::XMLNode;
    let mut root = parse_task(source)?;
    for (section, item) in [("Principals", "Principal"), ("Triggers", "LogonTrigger")] {
        if let Some(section) = root.get_mut_child(section) {
            for child in &mut section.children {
                if let XMLNode::Element(node) = child {
                    if node.name == item {
                        if let Some(user) = node.get_mut_child("UserId") {
                            ensure!(
                                user.children
                                    .iter()
                                    .all(|c| matches!(c, XMLNode::Text(_) | XMLNode::CData(_))),
                                "invalid task user identity"
                            );
                            let sid = resolve(user.get_text().as_deref().unwrap_or(""))?;
                            user.children = vec![XMLNode::Text(sid)];
                        }
                    }
                }
            }
        }
    }
    let mut bytes = Vec::new();
    root.write(&mut bytes)?;
    Ok(String::from_utf8(bytes)?)
}
impl<A: TaskApi> TaskScheduler<A> {
    fn expected(&self, binding: &Binding) -> Result<String> {
        ensure!(
            self.0.current_sid()? == binding.owner,
            "task belongs to another user"
        );
        definition(binding)
    }
}
impl<A: TaskApi> Platform for TaskScheduler<A> {
    fn inspect(&mut self, binding: &Binding) -> Result<Registration> {
        let expected = self.expected(binding)?;
        let Some(task) = self.0.read(&task_name(binding))? else {
            return Ok(Registration::Absent);
        };
        ensure!(
            task.owner_sid == binding.owner
                && canonical(&task.definition)? == canonical(&expected)?,
            "task ownership or definition changed"
        );
        Ok(if task.running {
            Registration::Running
        } else {
            Registration::Stopped
        })
    }
    fn verify_payload(&mut self, binding: &Binding) -> Result<()> {
        self.expected(binding)?;
        self.0.verify_payload(binding)
    }
    fn register(&mut self, binding: &Binding) -> Result<()> {
        ensure!(
            self.inspect(binding)? == Registration::Absent,
            "task already exists"
        );
        let xml = self.expected(binding)?;
        self.0.create(&task_name(binding), &xml)
    }
    fn start(&mut self, binding: &Binding) -> Result<()> {
        ensure!(
            self.inspect(binding)? == Registration::Stopped,
            "task is not stopped"
        );
        let xml = self.expected(binding)?;
        self.0
            .start_owned(&task_name(binding), &xml, &binding.owner)
    }
    fn stop(&mut self, binding: &Binding) -> Result<()> {
        if self.inspect(binding)? == Registration::Absent {
            return Ok(());
        }
        let xml = self.expected(binding)?;
        self.0.stop_owned(&task_name(binding), &xml, &binding.owner)
    }
    /// No authenticated supervisor endpoint is wired on this adapter yet, so the
    /// controller stops natively under its lock instead of sending a token.
    fn supervisor_scope(&mut self, _: &Binding) -> Result<Option<Scope>> {
        Ok(None)
    }
    fn stop_supervisor(&mut self, _: &Binding, _: &StopToken) -> Result<()> {
        anyhow::bail!("supervisor IPC is not wired on this adapter")
    }
    fn unregister(&mut self, binding: &Binding) -> Result<()> {
        let state = self.inspect(binding)?;
        if state == Registration::Absent {
            return Ok(());
        }
        ensure!(
            state == Registration::Stopped,
            "owned process must stop before deleting task"
        );
        let xml = self.expected(binding)?;
        self.0
            .delete_owned(&task_name(binding), &xml, &binding.owner)
    }
}

#[cfg(target_os = "windows")]
pub mod native;

#[cfg(target_os = "windows")]
pub mod security;

#[cfg(target_os = "windows")]
pub mod private;

#[cfg(test)]
mod xml_tests {
    use super::*;

    #[test]
    fn decoded_scheduler_utf16_declaration_does_not_change_identity() {
        let xml =
            "<Task><Actions><Exec><Command>C:\\Олег\\sync.exe</Command></Exec></Actions></Task>";
        let decoded = format!("\u{feff}<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n{xml}");
        assert_eq!(canonical(xml).unwrap(), canonical(&decoded).unwrap());
        assert_ne!(
            canonical(xml).unwrap(),
            canonical(&decoded.replace("sync.exe", "other.exe")).unwrap()
        );
        assert!(canonical("<?xml version=\"1.0\" <Task/>").is_err());
        assert!(canonical("<?xml version=\"1.0\" encoding=\"UTF-16\"?><Task>").is_err());
    }
}

#[cfg(test)]
mod scheduler_roundtrip_tests {
    use super::*;
    const EXPECTED: &str = include_str!("../../tests/fixtures/scheduler-expected.xml");
    const RETURNED: &str = include_str!("../../tests/fixtures/scheduler-returned.xml");
    fn resolve(value: &str) -> Result<String> {
        if value == r"runnervmdlhio\runneradmin" {
            Ok("S-1-5-21-3827877701-3061038111-106515639-500".into())
        } else if value.starts_with("S-1-") {
            Ok(value.into())
        } else {
            anyhow::bail!("unknown fixture account")
        }
    }
    #[test]
    fn mixed_scheduler_content_is_rejected_before_reordering() {
        for changed in [
            RETURNED.replace(
                "<Actions Context=\"Owner\">",
                "<Actions Context=\"Owner\">stray",
            ),
            RETURNED.replace("</Exec>", "stray</Exec>"),
        ] {
            assert!(canonical(&changed).is_err());
        }
    }
    #[test]
    fn recorded_windows_xml_matches_owned_definition_semantically() {
        let actual = resolve_task_accounts(RETURNED, resolve).unwrap();
        assert_eq!(canonical(EXPECTED).unwrap(), canonical(&actual).unwrap());
    }
    #[test]
    fn scheduler_normalization_preserves_identity_and_execution_boundaries() {
        for changed in [
            RETURNED.replace("never-executed.exe", "foreign.exe"),
            RETURNED.replace("--instance", "--foreign"),
            RETURNED.replace("S-1-5-21-3827877701-3061038111-106515639-500", "S-1-5-18"),
            RETURNED.replace(
                "</Principal>",
                "<RunLevel>HighestAvailable</RunLevel></Principal>",
            ),
            RETURNED.replace("InteractiveToken", "Password"),
            RETURNED.replace("<LogonTrigger>", "<LogonTrigger><Enabled>false</Enabled>"),
            RETURNED.replace("<Count>3</Count>", "<Count>4</Count>"),
            RETURNED.replace("Context=\"Owner\"", "Context=\"Foreign\""),
            RETURNED.replace(
                "</Settings>",
                "<UnknownOption>true</UnknownOption></Settings>",
            ),
            RETURNED.replace(
                "</Actions>",
                "<Exec><Command>foreign.exe</Command></Exec></Actions>",
            ),
            RETURNED.replace(
                "<RestartOnIdle>false</RestartOnIdle>",
                "<RestartOnIdle>true</RestartOnIdle>",
            ),
        ] {
            let actual = resolve_task_accounts(&changed, resolve).unwrap();
            assert_ne!(canonical(EXPECTED).unwrap(), canonical(&actual).unwrap());
        }
        assert!(resolve_task_accounts(
            &RETURNED.replace(r"runnervmdlhio\runneradmin", "foreign-user"),
            resolve
        )
        .is_err());
    }
}
