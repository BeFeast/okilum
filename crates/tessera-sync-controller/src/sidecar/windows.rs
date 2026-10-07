//! Task Scheduler policy and adapter. The native port must use the current
//! interactive token (no password/elevation) and conditional ownership checks.
use super::{safe_text, xml, Binding, Platform, Registration};
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
fn path(value: &str) -> Result<()> {
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
        r#"<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task"><RegistrationInfo><Description>Tessera Sync {} {} {}</Description></RegistrationInfo><Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId></LogonTrigger></Triggers><Principals><Principal id="Owner"><UserId>{}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals><Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure></Settings><Actions Context="Owner"><Exec><Command>{}</Command><Arguments>{}</Arguments><WorkingDirectory>{}</WorkingDirectory></Exec></Actions></Task>"#,
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
fn canonical(source: &str) -> Result<String> {
    use xmltree::{Element, XMLNode};
    fn visit(node: &Element, out: &mut String) {
        // Length-delimited fields avoid ambiguous concatenations; unknown nodes
        // and attributes remain significant, so extra privileged actions fail.
        fn field(out: &mut String, value: &str) {
            out.push_str(&format!("{}:{value}", value.len()));
        }
        field(out, &node.name);
        field(out, node.namespace.as_deref().unwrap_or(""));
        let mut attrs: Vec<_> = node.attributes.iter().collect();
        attrs.sort();
        out.push('[');
        for (key, value) in attrs {
            field(out, key);
            field(out, value);
        }
        out.push(']');
        out.push('{');
        for child in &node.children {
            match child {
                XMLNode::Element(el) => visit(el, out),
                XMLNode::Text(s) | XMLNode::CData(s) if !s.trim().is_empty() => field(out, s),
                _ => {}
            }
        }
        out.push('}');
    }
    let element = Element::parse(source.as_bytes())?;
    let mut out = String::new();
    visit(&element, &mut out);
    Ok(out)
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
