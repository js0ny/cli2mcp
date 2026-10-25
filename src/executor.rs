use std::{
    future::{Future, poll_fn},
    io,
    path::Path,
    process::{ExitStatus, Stdio},
    task::Poll,
    time::Duration,
};

use rmcp::model::{CallToolResult, ContentBlock};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
};

use crate::{config::ResolvedToolConfig, template::ArgumentTemplate};

#[cfg(not(unix))]
compile_error!("cli2mcp currently requires Unix process-group support");

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

const SUCCESS_CODE: i32 = 0;
const STREAM_BUFFER_SIZE: usize = 8 * 1024;
const NO_STDOUT: &str = "Exit 0, no stdout";

/// Runs one configured tool and encodes the outcome as an MCP tool result.
///
/// `None` means the request was cancelled: the process group has already been killed and
/// reaped, and no response may be written for that JSON-RPC id.
pub(crate) async fn execute(
    tool: &ResolvedToolConfig,
    templates: &[ArgumentTemplate],
    arguments: &serde_json::Map<String, serde_json::Value>,
    cancelled: impl Future<Output = ()> + Send,
) -> Option<CallToolResult> {
    let mut argv = Vec::with_capacity(templates.len());
    for template in templates {
        match template.render(arguments) {
            Ok(value) => argv.push(value.into_owned()),
            Err(error) => {
                return Some(Report::error(format!("Invalid arguments: {error}")).into_result());
            }
        }
    }

    let mut cancelled = std::pin::pin!(cancelled);
    if matches!(
        poll_fn(|context| Poll::Ready(cancelled.as_mut().poll(context))).await,
        Poll::Ready(())
    ) {
        return None;
    }

    let mut owner = match spawn(tool, &argv) {
        Ok(owner) => owner,
        Err(error) => {
            return Some(Report::error(format!("Failed to start process: {error}")).into_result());
        }
    };
    let (Some(stdout_reader), Some(stderr_reader)) =
        (owner.child.stdout.take(), owner.child.stderr.take())
    else {
        return Some(
            Report::error("Failed to start process: output pipes are not captured".to_owned())
                .into_result(),
        );
    };

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let selected = tokio::select! {
        biased;
        () = cancelled.as_mut() => Err(Interrupted::Cancelled),
        () = tokio::time::sleep(Duration::from_millis(tool.timeout_ms)) => Err(Interrupted::TimedOut),
        attempt = collect(&mut owner.child, stdout_reader, stderr_reader, &mut stdout, &mut stderr) => Ok(attempt),
    };

    match selected {
        Ok(Attempt::Exited(status)) => {
            owner.disarm();
            Some(report_for_status(status, stdout, stderr).into_result())
        }
        Ok(Attempt::ReadFailed(error)) => {
            let mut report = Report::error(format!("Failed to read process output: {error}"));
            report.extra.extend(terminate_and_reap(&mut owner).await);
            report.stdout = stdout;
            report.stderr = stderr;
            Some(report.into_result())
        }
        Ok(Attempt::WaitFailed(error)) => {
            let mut report = Report::error(format!("Failed to wait for process: {error}"));
            report.extra.extend(owner.terminate_group());
            report.stdout = stdout;
            report.stderr = stderr;
            Some(report.into_result())
        }
        Err(Interrupted::TimedOut) => {
            let mut report = Report::error(format!("Timed out after {} ms", tool.timeout_ms));
            report.extra.extend(terminate_and_reap(&mut owner).await);
            report.stdout = stdout;
            report.stderr = stderr;
            Some(report.into_result())
        }
        Err(Interrupted::Cancelled) => {
            terminate_and_reap(&mut owner).await;
            None
        }
    }
}

/// Keeps the direct child unreaped until both output streams reach EOF, so its pgid cannot be
/// recycled by an unrelated process group while the kill guard is still armed.
async fn collect(
    child: &mut Child,
    mut stdout_reader: tokio::process::ChildStdout,
    mut stderr_reader: tokio::process::ChildStderr,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
) -> Attempt {
    let drained = tokio::try_join!(
        drain(&mut stdout_reader, stdout),
        drain(&mut stderr_reader, stderr),
    );
    if let Err(error) = drained {
        return Attempt::ReadFailed(error);
    }
    match child.wait().await {
        Ok(status) => Attempt::Exited(status),
        Err(error) => Attempt::WaitFailed(error),
    }
}

async fn drain<R: AsyncRead + Unpin>(reader: &mut R, buffer: &mut Vec<u8>) -> io::Result<()> {
    let mut chunk = [0_u8; STREAM_BUFFER_SIZE];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

async fn terminate_and_reap(owner: &mut ProcessOwner) -> Option<String> {
    let note = owner.terminate_group();
    match owner.child.wait().await {
        Ok(_) => note,
        Err(error) => Some(match note {
            Some(note) => format!("{note}; Failed to wait for process: {error}"),
            None => format!("Failed to wait for process: {error}"),
        }),
    }
}

struct ProcessOwner {
    child: Child,
    group: Option<nix::unistd::Pid>,
}

impl ProcessOwner {
    fn disarm(&mut self) {
        self.group = None;
    }

    fn terminate_group(&mut self) -> Option<String> {
        let group = self.group.take()?;
        match nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => None,
            Err(error) => Some(format!("Failed to terminate process group: {error}")),
        }
    }
}

impl Drop for ProcessOwner {
    fn drop(&mut self) {
        self.terminate_group();
    }
}

fn spawn(tool: &ResolvedToolConfig, argv: &[String]) -> io::Result<ProcessOwner> {
    let mut command = Command::new(&tool.exec);
    command
        .args(argv)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    for name in &tool.env_inherit {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.envs(&tool.env);

    if let Some(cwd) = tool.cwd.as_deref().or(tool.cwd_fallback.as_deref()) {
        let cwd = Path::new(cwd);
        let cwd = if cwd.is_absolute() {
            cwd.to_path_buf()
        } else {
            std::env::current_dir()?.join(cwd)
        };
        command.current_dir(cwd);
    }

    #[cfg(unix)]
    command.process_group(0);

    let child = command.spawn()?;
    let group = child
        .id()
        .filter(|pid| *pid > 1)
        .map(|pid| nix::unistd::Pid::from_raw(pid as i32));
    Ok(ProcessOwner { child, group })
}

enum Attempt {
    Exited(ExitStatus),
    ReadFailed(io::Error),
    WaitFailed(io::Error),
}

enum Interrupted {
    Cancelled,
    TimedOut,
}

struct Report {
    is_error: bool,
    headline: String,
    extra: Vec<String>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Report {
    fn error(headline: String) -> Self {
        Self {
            is_error: true,
            headline,
            extra: Vec::new(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    fn into_result(self) -> CallToolResult {
        if !self.is_error {
            let text = if self.stdout.is_empty() {
                NO_STDOUT.to_owned()
            } else {
                decode(self.stdout)
            };
            return CallToolResult::success(vec![ContentBlock::text(text)]);
        }

        let mut content = vec![ContentBlock::text(self.headline)];
        content.extend(self.extra.into_iter().map(ContentBlock::text));
        if !self.stdout.is_empty() {
            content.push(ContentBlock::text(format!(
                "[stdout]\n{}",
                decode(self.stdout)
            )));
        }
        if !self.stderr.is_empty() {
            content.push(ContentBlock::text(format!(
                "[stderr]\n{}",
                decode(self.stderr)
            )));
        }
        CallToolResult::error(content)
    }
}

fn report_for_status(status: ExitStatus, stdout: Vec<u8>, stderr: Vec<u8>) -> Report {
    let headline = match status.code() {
        Some(SUCCESS_CODE) => {
            return Report {
                is_error: false,
                headline: String::new(),
                extra: Vec::new(),
                stdout,
                stderr,
            };
        }
        Some(code) => format!("Exit code: {code}"),
        None => match status.signal() {
            Some(signal) => format!("Terminated by signal: {signal}"),
            None => "Process terminated without an exit code".to_owned(),
        },
    };
    Report {
        is_error: true,
        headline,
        extra: Vec::new(),
        stdout,
        stderr,
    }
}

fn decode(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}
