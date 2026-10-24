use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    sync::{
        LazyLock,
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde_json::{Value, json};
use tempfile::TempDir;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(8);

static FIXTURE_DIR: LazyLock<TempDir> =
    LazyLock::new(|| tempfile::tempdir().expect("fixture temp dir"));

static FIXTURE: LazyLock<PathBuf> = LazyLock::new(|| {
    let path = FIXTURE_DIR.path().join("process-fixture");
    let output = Command::new("rustc")
        .args(["--edition=2024", "tests/support/process_fixture.rs", "-o"])
        .arg(&path)
        .output()
        .expect("run rustc for the fixture");
    assert!(
        output.status.success(),
        "fixture compilation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    path
});

fn fixture() -> &'static Path {
    FIXTURE.as_path()
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "cli2mcp-tests", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

fn fixture_tool(name: &str, args: &[&str], timeout_ms: Option<u64>, extra: &str) -> String {
    let exec = serde_json::to_string(fixture().to_str().unwrap()).unwrap();
    let args = args
        .iter()
        .map(|arg| serde_json::to_string(arg).unwrap())
        .collect::<Vec<_>>()
        .join(", ");
    let timeout = timeout_ms
        .map(|timeout| format!("timeout_ms = {timeout}\n"))
        .unwrap_or_default();
    format!(
        "[[tools]]\nexpose_name = {name:?}\ndescription = \"fixture tool\"\nexec = {exec}\nargs = [{args}]\n{timeout}{extra}\n"
    )
}

fn write_config(dir: &TempDir, tools: &str) -> PathBuf {
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        format!("[default]\ntimeout_ms = 2000\nenv_inherit = []\n{tools}"),
    )
    .unwrap();
    path
}

struct Service {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
}

impl Service {
    fn start(config: &Path, dir: &Path) -> Self {
        Self::start_with(config, dir, |_| {})
    }

    fn start_with(
        config: &Path,
        dir: &Path,
        configure: impl FnOnce(&mut Command),
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cli2mcp"));
        command
            .arg(config)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        configure(&mut command);
        let mut child = command.spawn().expect("spawn cli2mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            lines,
        }
    }

    fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        let mut params = params;
        params
            .as_object_mut()
            .expect("params are an object")
            .insert("_meta".into(), meta());
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.recv_until(id)
    }

    fn call_request(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        self.request(id, "tools/call", json!({"name": name, "arguments": arguments}))
    }

    fn call(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        let message = self.call_request(id, name, arguments);
        assert!(message.get("error").is_none(), "unexpected error: {message}");
        message["result"].clone()
    }

    fn recv_until(&mut self, id: u64) -> Value {
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(remaining)
                .unwrap_or_else(|_| panic!("no response for id {id}"));
            let message: Value = serde_json::from_str(&line)
                .unwrap_or_else(|error| panic!("invalid response {line:?}: {error}"));
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                return message;
            }
        }
    }

    fn drain_for(&mut self, duration: Duration) -> Vec<Value> {
        let deadline = Instant::now() + duration;
        let mut drained = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return drained;
            }
            match self.lines.recv_timeout(remaining) {
                Ok(line) => drained.push(serde_json::from_str(&line).expect("valid response")),
                Err(_) => return drained,
            }
        }
    }

    fn shutdown(&mut self) -> ExitStatus {
        drop(self.stdin.take());
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "service did not exit after stdin EOF"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_for_pid(path: &Path) -> u32 {
    for _ in 0..400 {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("pid file {} was not written", path.display());
}

fn process_running(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let state = stat
        .rsplit_once(") ")
        .map(|(_, rest)| rest.split_whitespace().next().unwrap_or_default())
        .unwrap_or_default();
    !matches!(state, "Z" | "X" | "x")
}

fn wait_until_dead(pid: u32) {
    for _ in 0..200 {
        if !process_running(pid) {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("process {pid} is still running");
}

fn text_of(result: &Value) -> Vec<String> {
    result["content"]
        .as_array()
        .expect("content array")
        .iter()
        .map(|block| {
            assert_eq!(block["type"], "text");
            block["text"].as_str().expect("text block").to_owned()
        })
        .collect()
}

#[test]
fn discovers_tools_and_lists_them_in_configuration_order() {
    let dir = tempfile::tempdir().unwrap();
    let tools = format!(
        "{}{}{}",
        fixture_tool("echo", &["argv", "{1}"], None, ""),
        fixture_tool("plain", &["cwd"], None, "effect = \"ro\"\n"),
        fixture_tool("writer", &["cwd"], None, "effect = \"write\"\n"),
    );
    let mut service = Service::start(&write_config(&dir, &tools), dir.path());

    let discover = service.request(1, "server/discover", json!({}));
    let discover = &discover["result"];
    assert_eq!(discover["resultType"], "complete");
    assert!(
        discover["supportedVersions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|version| version == "2026-07-28")
    );
    assert_eq!(discover["capabilities"], json!({"tools": {}}));
    assert_eq!(discover["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "cli2mcp");

    let listed = service.request(2, "tools/list", json!({}));
    let listed = &listed["result"];
    assert_eq!(listed["resultType"], "complete");
    let tools = listed["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|tool| tool["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["echo", "plain", "writer"]);

    assert_eq!(
        tools[0]["inputSchema"],
        json!({
            "type": "object",
            "properties": {"1": {"type": "string"}},
            "required": ["1"],
            "additionalProperties": false,
        })
    );
    assert_eq!(tools[0]["annotations"], json!({"readOnlyHint": false, "destructiveHint": true}));
    assert_eq!(tools[1]["annotations"], json!({"readOnlyHint": true, "destructiveHint": false}));
    assert_eq!(tools[2]["annotations"], json!({"readOnlyHint": false, "destructiveHint": false}));
    assert_eq!(
        tools[1]["inputSchema"],
        json!({"type": "object", "properties": {}, "additionalProperties": false})
    );

    assert!(service.shutdown().success());
}

#[test]
fn substituted_arguments_are_single_literal_argv_entries() {
    let dir = tempfile::tempdir().unwrap();
    let sentinel = dir.path().join("must-not-exist");
    let tools = format!(
        "{}{}",
        fixture_tool("echo", &["argv", "{1}", "{2}"], None, ""),
        fixture_tool("stdin", &["stdin"], None, ""),
    );
    let mut service = Service::start(&write_config(&dir, &tools), dir.path());

    let literal = format!(
        "a b; $(touch {}) * {{2}}\n\"quoted\"",
        sentinel.display()
    );
    let result = service.call(1, "echo", json!({"1": literal, "2": "{1}"}));
    assert_eq!(result["isError"], false);
    assert_eq!(
        text_of(&result),
        [format!("{:?}\n", vec![literal, "{1}".to_owned()])]
    );
    assert!(!sentinel.exists(), "argument was interpreted by a shell");

    let stdin = service.call(2, "stdin", json!({}));
    assert_eq!(text_of(&stdin), ["eof:0\n"]);
}

#[test]
fn exit_status_and_streams_map_to_tool_results() {
    let dir = tempfile::tempdir().unwrap();
    let tools = [
        fixture_tool("empty", &["exit", "0", "", ""], None, ""),
        fixture_tool("text", &["exit", "0", "hello", "ignored"], None, ""),
        fixture_tool("blank", &["exit", "0", " \n", ""], None, ""),
        fixture_tool("partial", &["exit", "3", "partial", "boom"], None, ""),
        fixture_tool("silent", &["exit", "3", "", ""], None, ""),
        fixture_tool("invalid", &["invalid-utf8"], None, ""),
    ]
    .concat();
    let mut service = Service::start(&write_config(&dir, &tools), dir.path());

    assert_eq!(text_of(&service.call(1, "empty", json!({}))), ["Exit 0, no stdout"]);
    assert_eq!(text_of(&service.call(2, "text", json!({}))), ["hello"]);
    assert_eq!(text_of(&service.call(3, "blank", json!({}))), [" \n"]);
    assert_eq!(
        text_of(&service.call(4, "partial", json!({}))),
        ["Exit code: 3", "[stdout]\npartial", "[stderr]\nboom"]
    );
    assert_eq!(text_of(&service.call(5, "silent", json!({}))), ["Exit code: 3"]);
    assert_eq!(text_of(&service.call(6, "invalid", json!({}))), ["\u{fffd}\u{fffd}a"]);

    assert_eq!(service.call(7, "partial", json!({}))["isError"], true);
    assert_eq!(service.call(8, "text", json!({}))["isError"], false);
    assert_eq!(service.request(9, "tools/list", json!({}))["result"]["resultType"], "complete");
}

#[test]
fn large_output_is_collected_from_both_streams() {
    let dir = tempfile::tempdir().unwrap();
    let tools = [
        fixture_tool("large", &["large", "262144"], None, ""),
        fixture_tool("large-fail", &["large-fail", "262144"], None, ""),
    ]
    .concat();
    let mut service = Service::start(&write_config(&dir, &tools), dir.path());

    let result = service.call(1, "large", json!({}));
    assert_eq!(result["isError"], false);
    assert_eq!(text_of(&result), ["a".repeat(262_144)]);

    let failed = service.call(2, "large-fail", json!({}));
    assert_eq!(failed["isError"], true);
    assert_eq!(
        text_of(&failed),
        [
            "Exit code: 5".to_owned(),
            format!("[stdout]\n{}", "a".repeat(262_144)),
            format!("[stderr]\n{}", "b".repeat(262_144)),
        ]
    );
}

#[test]
fn invalid_arguments_fail_without_spawning() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("guard.pid");
    let tools = fixture_tool("guard", &["hold", "{1}", "60000"], None, "");
    let mut service = Service::start(&write_config(&dir, &tools), dir.path());

    for (id, arguments) in [
        (1, json!({})),
        (2, json!({"1": "x", "2": "extra"})),
        (3, json!({"1": 7})),
        (4, json!({"1": null})),
    ] {
        let result = service.call(id, "guard", arguments);
        assert_eq!(result["isError"], true);
        let text = text_of(&result);
        assert!(
            text[0].starts_with("Invalid arguments: "),
            "unexpected text: {text:?}"
        );
    }
    assert!(!pid_file.exists());
}

#[test]
fn unknown_tool_is_a_protocol_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut service = Service::start(&write_config(&dir, ""), dir.path());

    let response = service.call_request(1, "missing", json!({}));
    assert_eq!(response["error"]["code"], -32602);
    assert_eq!(response["error"]["message"], "Unknown tool: missing");

    let listed = service.request(2, "tools/list", json!({}));
    assert_eq!(listed["result"]["tools"], json!([]));
}

#[test]
fn environment_and_working_directory_follow_the_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let startup = dir.path().join("startup");
    std::fs::create_dir_all(startup.join("sub")).unwrap();
    std::fs::create_dir_all(startup.join("fallback")).unwrap();

    let bare = serde_json::to_string(fixture().file_name().unwrap().to_str().unwrap()).unwrap();
    let tools = format!(
        "{}{}{}{}",
        fixture_tool("env", &["env", "CLI2MCP_SENTINEL"], None, ""),
        fixture_tool(
            "env-explicit",
            &["env", "CLI2MCP_SENTINEL"],
            None,
            "env = { CLI2MCP_SENTINEL = \"explicit\" }\n"
        ),
        format!(
            "[[tools]]\nexpose_name = \"bare\"\ndescription = \"bare name\"\nexec = {bare}\nargs = [\"cwd\"]\nenv = {{ PATH = {:?} }}\n",
            fixture().parent().unwrap().to_str().unwrap()
        ),
        fixture_tool("cwd", &["cwd"], None, "cwd = \"sub\"\n"),
    );
    let config = write_config(&dir, &tools);
    let mut service = Service::start_with(&config, &startup, |command| {
        command.env("CLI2MCP_SENTINEL", "from-service");
    });

    assert_eq!(text_of(&service.call(1, "env", json!({}))), ["<unset>\n"]);
    assert_eq!(
        text_of(&service.call(2, "env-explicit", json!({}))),
        ["explicit\n"]
    );
    assert_eq!(
        text_of(&service.call(3, "bare", json!({}))),
        [format!("{}\n", startup.display())]
    );
    assert_eq!(
        text_of(&service.call(4, "cwd", json!({}))),
        [format!("{}\n", startup.join("sub").display())]
    );

    let bad = format!(
        "{}{}",
        fixture_tool("bad-cwd", &["cwd"], None, "cwd = \"missing\"\n"),
        fixture_tool("fallback", &["cwd"], None, ""),
    );
    let fallback_config = dir.path().join("fallback.toml");
    std::fs::write(
        &fallback_config,
        format!(
            "[default]\ntimeout_ms = 2000\nenv_inherit = []\ncwd_fallback = \"fallback\"\n{bad}"
        ),
    )
    .unwrap();
    let mut fallback = Service::start(&fallback_config, &startup);

    let failed = fallback.call(5, "bad-cwd", json!({}));
    assert_eq!(failed["isError"], true);
    assert!(
        text_of(&failed)[0].starts_with("Failed to start process: "),
        "unexpected text: {:?}",
        text_of(&failed)
    );
    assert_eq!(
        text_of(&fallback.call(6, "fallback", json!({}))),
        [format!("{}\n", startup.join("fallback").display())],
        "cwd_fallback from [default] must apply when cwd is absent"
    );
}

#[test]
fn timeout_terminates_the_whole_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("ready.pid");
    let child = dir.path().join("child.pid");
    let tools = fixture_tool("timeout", &["timeout", "{1}", "{2}"], Some(300), "");
    let mut service = Service::start(&write_config(&dir, &tools), dir.path());

    service.send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "_meta": meta(),
            "name": "timeout",
            "arguments": {"1": ready.to_str().unwrap(), "2": child.to_str().unwrap()},
        },
    }));
    let leader = wait_for_pid(&ready);
    let descendant = wait_for_pid(&child);
    assert!(process_running(leader) && process_running(descendant));

    let response = service.recv_until(1);
    let result = &response["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(text_of(result)[0], "Timed out after 300 ms");
    wait_until_dead(leader);
    wait_until_dead(descendant);
}

#[test]
fn leader_exit_does_not_end_the_call_while_a_descendant_holds_the_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("ready.pid");
    let child = dir.path().join("child.pid");
    let tools = fixture_tool("leader", &["leader-exits", "{1}", "{2}"], Some(300), "");
    let mut service = Service::start(&write_config(&dir, &tools), dir.path());

    let started = Instant::now();
    service.send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "_meta": meta(),
            "name": "leader",
            "arguments": {"1": ready.to_str().unwrap(), "2": child.to_str().unwrap()},
        },
    }));
    let leader = wait_for_pid(&ready);
    let descendant = wait_for_pid(&child);

    let response = service.recv_until(1);
    assert_eq!(
        text_of(&response["result"])[0],
        "Timed out after 300 ms",
        "the leader exited, but the call must still wait for pipe EOF"
    );
    assert!(started.elapsed() >= Duration::from_millis(300));
    wait_until_dead(leader);
    wait_until_dead(descendant);
}

#[test]
fn timeout_still_applies_after_the_output_streams_close() {
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("ready.pid");
    let tools = fixture_tool("sleepy", &["close-then-sleep", "{1}", "60000"], Some(300), "");
    let mut service = Service::start(&write_config(&dir, &tools), dir.path());

    service.send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "_meta": meta(),
            "name": "sleepy",
            "arguments": {"1": ready.to_str().unwrap()},
        },
    }));
    let pid = wait_for_pid(&ready);

    let response = service.recv_until(1);
    assert_eq!(text_of(&response["result"])[0], "Timed out after 300 ms");
    wait_until_dead(pid);
}

#[test]
fn signal_termination_is_reported_without_a_fake_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("ready.pid");
    let tools = fixture_tool("signal", &["signal-ready", "{1}"], None, "");
    let mut service = Service::start(&write_config(&dir, &tools), dir.path());

    service.send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "_meta": meta(),
            "name": "signal",
            "arguments": {"1": ready.to_str().unwrap()},
        },
    }));
    let pid = wait_for_pid(&ready);
    assert!(process_running(pid));
    kill(Pid::from_raw(pid as i32), Signal::SIGTERM).unwrap();

    let response = service.recv_until(1);
    let result = &response["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(text_of(result), ["Terminated by signal: 15"]);
}

#[test]
fn cancellation_kills_the_group_and_suppresses_the_response() {
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("ready.pid");
    let tools = fixture_tool("cancel", &["hold", "{1}", "60000"], None, "");
    let mut service = Service::start(&write_config(&dir, &tools), dir.path());

    service.send(json!({
        "jsonrpc": "2.0",
        "id": 20,
        "method": "tools/call",
        "params": {
            "_meta": meta(),
            "name": "cancel",
            "arguments": {"1": ready.to_str().unwrap()},
        },
    }));
    let pid = wait_for_pid(&ready);
    assert!(process_running(pid));

    service.send(json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 20, "reason": "test"},
    }));
    let listed = service.request(21, "tools/list", json!({}));
    assert_eq!(listed["result"]["resultType"], "complete");

    let late = service.drain_for(Duration::from_millis(300));
    assert!(
        late.iter().all(|message| message.get("id").and_then(Value::as_u64) != Some(20)),
        "cancelled request produced a response: {late:?}"
    );
    wait_until_dead(pid);
}

#[test]
fn service_exits_after_stdin_eof() {
    let dir = tempfile::tempdir().unwrap();
    let mut service = Service::start(&write_config(&dir, ""), dir.path());
    service.request(1, "server/discover", json!({}));
    assert!(service.shutdown().success());
}

#[test]
fn configuration_and_template_errors_are_reported_before_startup() {
    let dir = tempfile::tempdir().unwrap();
    for (contents, expected) in [
        (
            "[default]\ntimeout_ms = 0\n".to_owned(),
            "cli2mcp: default.timeout_ms must be greater than 0",
        ),
        (
            format!(
                "[[tools]]\nexpose_name = \"x\"\ndescription = \"x\"\nexec = \"printf\"\nargs = [\"{{0}}\"]\n"
            ),
            "cli2mcp: tool 'x': args[0]: placeholder numbers start at 1",
        ),
    ] {
        let config = dir.path().join("bad.toml");
        std::fs::write(&config, contents).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_cli2mcp"))
            .arg(&config)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(expected),
            "expected {expected:?} in {stderr:?}"
        );
        assert!(output.stdout.is_empty());
    }
}
