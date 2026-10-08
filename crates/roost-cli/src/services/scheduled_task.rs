//! The Windows service: a `.cmd` launcher script that sets the environment and
//! runs `roost <role>` with its output appended to the role's logs, and a
//! per-user logon Scheduled Task under `\Roost\` that runs it through a
//! headless console. Rendered by `definition_text`, driven by `service_argv`,
//! probed by `status::service_probe`, read back by `status::service_definition`
//! and `deploy::installed`. Every PowerShell value is a single-quoted literal.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use roost_host::{ProtocolError, ProtocolResult};
use roost_platform::powershell_single_quote;

use crate::services::service_spec::ServiceSpec;

/// The Task Scheduler folder every Roost task lives in.
pub const TASK_PATH: &str = r"\Roost\";

/// The launcher script for `spec`, CRLF-terminated.
///
/// `%` is doubled because cmd expands it inside `set`; a `"`, a line break, or
/// an `=` in a name cannot be carried by `set "K=V"` and is refused.
pub fn render_launcher(spec: &ServiceSpec) -> ProtocolResult<String> {
    let mut text = String::new();
    text.push_str("@echo off\r\n");
    let _ = write!(
        text,
        "rem Roost {} service launcher. Written by roost; local edits are overwritten.\r\n",
        spec.role.display_name()
    );
    text.push_str("chcp 65001 >nul\r\n");
    for (key, value) in &spec.environment {
        refuse_unquotable(key, value)?;
        let _ = write!(text, "set \"{key}={}\"\r\n", value.replace('%', "%%"));
    }
    let log_dir = spec.log_dir.display();
    let _ = write!(
        text,
        "\"{}\" {} 1>> \"{log_dir}\\main.out.log\" 2>> \"{log_dir}\\main.err.log\"\r\n",
        spec.program.display(),
        spec.role.subcommand()
    );
    Ok(text)
}

/// Whether `text` is a whole launcher: it opens with `@echo off` and its last
/// line runs a role with its output redirected.
pub fn launcher_is_complete(text: &str) -> bool {
    let first = text.lines().next().map(str::trim_end);
    let last = last_line(text);
    first == Some("@echo off")
        && last.is_some_and(|line| {
            line.starts_with('"')
                && (line.contains("\" worker 1>> \"") || line.contains("\" coord 1>> \""))
        })
}

/// The environment a launcher sets, with cmd's `%%` read back as `%`.
pub fn parse_launcher_environment(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let entry = line.trim().strip_prefix("set \"")?.strip_suffix('"')?;
            let (key, value) = entry.split_once('=')?;
            Some((key.to_string(), value.replace("%%", "%")))
        })
        .collect()
}

/// The program a launcher runs: the text between the first two quotes of its
/// last line.
pub fn launcher_program(text: &str) -> Option<PathBuf> {
    let line = last_line(text)?;
    let rest = line.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(PathBuf::from(&rest[..end]))
}

/// Register (or replace) the logon task for `label` running `launcher`, then
/// start it now.
///
/// The user is read from the process token, not `USERDOMAIN`: over an ssh
/// logon `USERDOMAIN` is `WORKGROUP`, which Task Scheduler cannot map to a SID.
pub fn register_and_start_script(label: &str, launcher: &Path) -> String {
    let argument = powershell_single_quote(&format!(
        "--headless cmd.exe /d /c \"{}\"",
        launcher.display()
    ));
    let task = task_selector(label);
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         $user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name\n\
         $action = New-ScheduledTaskAction -Execute \"$env:SystemRoot\\System32\\conhost.exe\" -Argument {argument}\n\
         $trigger = New-ScheduledTaskTrigger -AtLogOn -User $user\n\
         $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited\n\
         $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -MultipleInstances IgnoreNew\n\
         Register-ScheduledTask {task} -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null\n\
         Start-ScheduledTask {task}\n"
    )
}

/// Stop the task's running instance, if there is one.
pub fn stop_script(label: &str) -> String {
    format!(
        "Stop-ScheduledTask {} -ErrorAction SilentlyContinue",
        task_selector(label)
    )
}

/// Print `active` and exit 0 when the task is running, else print `inactive`
/// and exit 1: the same two answers `systemctl is-active` gives.
pub fn running_probe_script(label: &str) -> String {
    format!(
        "$t = Get-ScheduledTask {} -ErrorAction SilentlyContinue; \
         if ($t -and $t.State -eq 'Running') {{ 'active'; exit 0 }} else {{ 'inactive'; exit 1 }}",
        task_selector(label)
    )
}

/// Print `State=<task state or Missing>` and `MainPID=<pid or 0>`, the pid
/// being the first `roost.exe` whose command line runs `subcommand`. The
/// subcommand is the line's last word once cmd has taken the redirections off.
pub fn state_report_script(label: &str, subcommand: &str) -> String {
    let pattern = powershell_single_quote(&format!("\\s{subcommand}(\\s|$)"));
    format!(
        "$t = Get-ScheduledTask {} -ErrorAction SilentlyContinue\n\
         $state = if ($t) {{ $t.State }} else {{ 'Missing' }}\n\
         $p = Get-CimInstance Win32_Process -Filter \"Name='roost.exe'\" | Where-Object {{ $_.CommandLine -match {pattern} }} | Select-Object -First 1\n\
         $procId = if ($p) {{ $p.ProcessId }} else {{ 0 }}\n\
         \"State=$state\"\n\
         \"MainPID=$procId\"\n",
        task_selector(label)
    )
}

/// The argv that runs `script` in Windows PowerShell, without a profile or a
/// prompt.
pub fn powershell_argv(script: &str) -> Vec<String> {
    [
        "powershell.exe",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ]
    .map(str::to_string)
    .to_vec()
}

fn task_selector(label: &str) -> String {
    format!(
        "-TaskPath {} -TaskName {}",
        powershell_single_quote(TASK_PATH),
        powershell_single_quote(label)
    )
}

fn last_line(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim_end)
        .rfind(|line| !line.is_empty())
}

fn refuse_unquotable(key: &str, value: &str) -> ProtocolResult<()> {
    let unquotable = |text: &str| text.contains(['"', '\r', '\n']);
    if unquotable(key) || key.contains('=') || unquotable(value) {
        return Err(ProtocolError::new(
            "service.environment",
            format!(
                "{key} cannot be written into a Windows launcher: it holds a quote, a line break or, in its name, an `=`"
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::{
        launcher_is_complete, launcher_program, parse_launcher_environment,
        register_and_start_script, render_launcher,
    };
    use crate::services::memory_limits::ResourceLimits;
    use crate::services::service_spec::{ServiceRole, ServiceSpec};

    fn spec(environment: &[(&str, &str)]) -> ServiceSpec {
        ServiceSpec {
            role: ServiceRole::Worker,
            label: "roost3-worker".to_string(),
            definition_path: PathBuf::from(
                r"C:\Users\op\AppData\Local\RoostWorkerV3\service\roost3-worker.cmd",
            ),
            program: PathBuf::from(
                r"C:\Users\op\AppData\Local\RoostWorkerV3\versions\v3.1\bin\roost.exe",
            ),
            working_directory: PathBuf::from(
                r"C:\Users\op\AppData\Local\RoostWorkerV3\versions\v3.1\bin",
            ),
            log_dir: PathBuf::from(r"C:\Users\op\AppData\Local\RoostWorkerV3\logs"),
            data_dir: PathBuf::from(r"C:\Users\op\AppData\Local\RoostWorkerV3"),
            limits: ResourceLimits::worker(0),
            environment: environment
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn a_percent_is_doubled_and_read_back_single() {
        let text = render_launcher(&spec(&[("ROOST_WORKER_LABEL", "50% box")])).unwrap();
        assert!(
            text.contains("set \"ROOST_WORKER_LABEL=50%% box\"\r\n"),
            "{text}"
        );
        assert_eq!(
            parse_launcher_environment(&text)
                .get("ROOST_WORKER_LABEL")
                .map(String::as_str),
            Some("50% box")
        );
    }

    #[test]
    fn a_value_with_a_quote_is_refused() {
        let refused = render_launcher(&spec(&[("ROOST_WORKER_LABEL", "a\"b")])).unwrap_err();
        assert_eq!(refused.field, "service.environment");
    }

    #[test]
    fn the_environment_and_the_program_round_trip() {
        let environment = [
            ("ROOST_COORDINATOR_URL", "https://mike.roosttt.com"),
            ("ROOST_WORKER_LABEL", "Build PC"),
        ];
        let text = render_launcher(&spec(&environment)).unwrap();
        assert!(launcher_is_complete(&text), "{text}");
        assert_eq!(
            parse_launcher_environment(&text),
            spec(&environment).environment
        );
        assert_eq!(
            launcher_program(&text),
            Some(PathBuf::from(
                r"C:\Users\op\AppData\Local\RoostWorkerV3\versions\v3.1\bin\roost.exe"
            ))
        );
        assert!(!launcher_is_complete(
            &text.replace("\" worker 1>> \"", "\" worker \"")
        ));
    }

    #[test]
    fn a_quote_in_the_label_stays_inside_its_literal() {
        let script = register_and_start_script("it's", &PathBuf::from(r"C:\l.cmd"));
        assert!(script.contains("-TaskName 'it''s'"), "{script}");
        assert!(!script.contains("-TaskName 'it's'"), "{script}");
    }
}
