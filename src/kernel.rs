use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use bytes::Bytes;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::runtime::Runtime;
use zeromq::{DealerSocket, Socket, SocketRecv, SocketSend, SubSocket, ZmqMessage};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const READY_PROBE: Duration = Duration::from_millis(50);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const DELIMITER: &[u8] = b"<IDS|MSG>";
const LOG_TAIL: usize = 15;

#[cfg(unix)]
mod platform {
    use std::fs::File;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};

    use anyhow::Context;
    use rustix::process::{Pid, Signal, kill_process_group, test_kill_process};
    use serde_json::Value;

    pub const VENV_PYTHON: [&str; 2] = ["bin", "python"];
    pub const FALLBACK_PYTHON: &str = "python3";

    pub struct Launched {
        child: Child,
    }

    impl Launched {
        pub fn pid(&self) -> u32 {
            self.child.id()
        }

        pub fn exited(&mut self) -> bool {
            self.child
                .try_wait()
                .map_or(true, |status| status.is_some())
        }

        pub fn kill(&mut self) {
            let _ = self.child.kill();
        }
    }

    pub fn launch(
        interpreter: &Path,
        connection: &Path,
        log_path: &Path,
        workdir: &Path,
    ) -> anyhow::Result<Launched> {
        let log = File::create(log_path)?;
        let child = Command::new(interpreter)
            .args(["-m", "ipykernel_launcher", "-f"])
            .arg(connection)
            .current_dir(workdir)
            .env_remove("JPY_PARENT_PID")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0)
            .spawn()
            .with_context(|| format!("cannot launch {}", interpreter.display()))?;
        Ok(Launched { child })
    }

    fn pid_of(pid: u32) -> Option<Pid> {
        i32::try_from(pid).ok().and_then(Pid::from_raw)
    }

    pub fn is_running(pid: u32, _connection: &Value) -> bool {
        pid_of(pid).is_some_and(|pid| test_kill_process(pid).is_ok())
    }

    pub fn terminate_tree(pid: u32) {
        if let Some(group) = pid_of(pid) {
            let _ = kill_process_group(group, Signal::TERM);
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::net::{SocketAddr, TcpStream};
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use anyhow::{Context, bail};
    use serde_json::Value;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

    pub const VENV_PYTHON: [&str; 2] = ["Scripts", "python.exe"];
    pub const FALLBACK_PYTHON: &str = "python";

    const LAUNCHER: &str = "import importlib.util, subprocess, sys
if importlib.util.find_spec('ipykernel') is None:
    sys.exit('ipykernel is not installed for ' + sys.executable)
log = open(sys.argv[2], 'wb')
kernel = subprocess.Popen(
    [sys.executable, '-m', 'ipykernel_launcher', '-f', sys.argv[1]],
    cwd=sys.argv[3], stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
    creationflags=subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.DETACHED_PROCESS,
)
print(kernel.pid)
";

    pub struct Launched {
        pid: u32,
    }

    impl Launched {
        pub fn pid(&self) -> u32 {
            self.pid
        }

        pub fn exited(&mut self) -> bool {
            let pid = self.pid;
            Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .is_ok_and(|output| {
                    !String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
                })
        }

        pub fn kill(&mut self) {
            terminate_tree(self.pid);
        }
    }

    pub fn launch(
        interpreter: &Path,
        connection: &Path,
        log_path: &Path,
        workdir: &Path,
    ) -> anyhow::Result<Launched> {
        let output = Command::new(interpreter)
            .args(["-c", LAUNCHER])
            .arg(connection)
            .arg(log_path)
            .arg(workdir)
            .env_remove("JPY_PARENT_PID")
            .stdin(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .with_context(|| format!("cannot launch {}", interpreter.display()))?;
        if !output.status.success() {
            bail!(
                "kernel exited using {}:\n{}\nis ipykernel installed there? `uv add --dev ipykernel`",
                interpreter.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let pid = String::from_utf8_lossy(&output.stdout).trim().parse()?;
        Ok(Launched { pid })
    }

    pub fn is_running(_pid: u32, connection: &Value) -> bool {
        let ip = connection["ip"].as_str().unwrap_or("127.0.0.1");
        format!("{ip}:{}", connection["hb_port"])
            .parse::<SocketAddr>()
            .is_ok_and(|address| TcpStream::connect_timeout(&address, PROBE_TIMEOUT).is_ok())
    }

    pub fn terminate_tree(pid: u32) {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
}

#[derive(Default)]
pub struct Execution {
    pub outputs: Vec<Value>,
    pub count: Value,
}

impl Execution {
    #[must_use]
    pub fn failed(&self) -> bool {
        self.outputs
            .iter()
            .any(|output| output["output_type"] == "error")
    }

    fn collect(&mut self, msg_type: &str, content: &Value) {
        match msg_type {
            "stream" => {
                if let Some(last) = self.outputs.last_mut()
                    && last["output_type"] == "stream"
                    && last["name"] == content["name"]
                {
                    let merged = format!(
                        "{}{}",
                        last["text"].as_str().unwrap_or_default(),
                        content["text"].as_str().unwrap_or_default()
                    );
                    last["text"] = Value::from(merged);
                } else {
                    self.outputs.push(json!({
                        "output_type": "stream", "name": content["name"], "text": content["text"]
                    }));
                }
            }
            "display_data" => self.outputs.push(json!({
                "output_type": "display_data", "data": content["data"], "metadata": content["metadata"]
            })),
            "execute_result" => self.outputs.push(json!({
                "output_type": "execute_result", "data": content["data"],
                "metadata": content["metadata"], "execution_count": content["execution_count"]
            })),
            "error" => self.outputs.push(json!({
                "output_type": "error", "ename": content["ename"],
                "evalue": content["evalue"], "traceback": content["traceback"]
            })),
            "clear_output" => self.outputs.clear(),
            _ => {}
        }
    }
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
}

#[must_use]
pub fn state_dir(notebook: &Path) -> PathBuf {
    let digest = hex::encode(Sha256::digest(
        absolute(notebook).to_string_lossy().as_bytes(),
    ));
    let stem = notebook.file_stem().unwrap_or_default().to_string_lossy();
    std::env::temp_dir()
        .join("nblean")
        .join(format!("{stem}-{}", &digest[..12]))
}

fn parent_dir(notebook: &Path) -> PathBuf {
    absolute(notebook)
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_owned)
}

fn find_python(start: &Path) -> PathBuf {
    start
        .ancestors()
        .map(|directory| {
            platform::VENV_PYTHON
                .iter()
                .fold(directory.join(".venv"), |path, part| path.join(part))
        })
        .find(|candidate| candidate.exists())
        .unwrap_or_else(|| PathBuf::from(platform::FALLBACK_PYTHON))
}

fn read_connection(state: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(state.join("kernel.json")).ok()?).ok()
}

#[must_use]
pub fn running_pid(notebook: &Path) -> Option<u32> {
    let state = state_dir(notebook);
    let pid: u32 = fs::read_to_string(state.join("kernel.pid"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let connection = read_connection(&state)?;
    platform::is_running(pid, &connection).then_some(pid)
}

pub fn start(notebook: &Path, python: Option<&Path>) -> anyhow::Result<()> {
    let state = state_dir(notebook);
    fs::create_dir_all(&state)?;
    let connection = state.join("kernel.json");
    let _ = fs::remove_file(&connection);
    let workdir = parent_dir(notebook);
    let interpreter = python.map_or_else(|| find_python(&workdir), Path::to_owned);
    let log_path = state.join("kernel.log");
    let mut kernel = platform::launch(&interpreter, &connection, &log_path, &workdir)?;
    fs::write(state.join("kernel.pid"), kernel.pid().to_string())?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        if read_connection(&state).is_some() {
            return Ok(());
        }
        if kernel.exited() {
            let log_text = fs::read_to_string(&log_path).unwrap_or_default();
            let lines: Vec<&str> = log_text.lines().collect();
            let tail = lines[lines.len().saturating_sub(LOG_TAIL)..].join("\n");
            bail!(
                "kernel exited using {}:\n{tail}\nis ipykernel installed there? `uv add --dev ipykernel`",
                interpreter.display()
            );
        }
        sleep(POLL_INTERVAL);
    }
    kernel.kill();
    bail!(
        "kernel did not start within {}s, see {}",
        STARTUP_TIMEOUT.as_secs(),
        log_path.display()
    )
}

pub fn stop(notebook: &Path) -> bool {
    let pid = running_pid(notebook);
    if let Some(pid) = pid {
        platform::terminate_tree(pid);
    }
    let state = state_dir(notebook);
    for name in ["kernel.json", "kernel.pid"] {
        let _ = fs::remove_file(state.join(name));
    }
    pid.is_some()
}

struct Message {
    msg_type: String,
    parent_id: String,
    content: Value,
}

impl Message {
    fn parse(message: &ZmqMessage) -> anyhow::Result<Self> {
        let frames: Vec<&Bytes> = message.iter().collect();
        let start = frames
            .iter()
            .position(|frame| frame.as_ref() == DELIMITER)
            .context("malformed kernel message, no delimiter")?;
        let field = |offset: usize| -> anyhow::Result<Value> {
            let frame = frames
                .get(start + offset)
                .context("truncated kernel message")?;
            Ok(serde_json::from_slice(frame)?)
        };
        let header = field(2)?;
        let parent = field(3)?;
        Ok(Self {
            msg_type: header["msg_type"].as_str().unwrap_or_default().to_owned(),
            parent_id: parent["msg_id"].as_str().unwrap_or_default().to_owned(),
            content: field(5)?,
        })
    }
}

pub struct Client {
    shell: DealerSocket,
    iopub: SubSocket,
    key: Vec<u8>,
    session: String,
    runtime: Runtime,
}

impl Client {
    pub fn connect(notebook: &Path, python: Option<&Path>) -> anyhow::Result<Self> {
        if running_pid(notebook).is_none() {
            start(notebook, python)?;
        }
        let info = read_connection(&state_dir(notebook))
            .context("kernel connection file is unreadable")?;
        let ip = info["ip"].as_str().unwrap_or("127.0.0.1");
        let transport = info["transport"].as_str().unwrap_or("tcp");
        let endpoint = |port: &str| format!("{transport}://{ip}:{}", info[port]);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let sockets = runtime.block_on(async {
            tokio::time::timeout(IO_TIMEOUT, async {
                let mut shell = DealerSocket::new();
                shell.connect(&endpoint("shell_port")).await?;
                let mut iopub = SubSocket::new();
                iopub.connect(&endpoint("iopub_port")).await?;
                iopub.subscribe("").await?;
                anyhow::Ok((shell, iopub))
            })
            .await
        });
        let (shell, iopub) =
            sockets.map_err(|_| anyhow::anyhow!("timed out connecting to the kernel"))??;
        let mut client = Self {
            shell,
            iopub,
            key: info["key"].as_str().unwrap_or_default().as_bytes().to_vec(),
            session: uuid::Uuid::new_v4().to_string(),
            runtime,
        };
        client.wait_until_ready()?;
        Ok(client)
    }

    fn sign(&self, parts: &[&[u8]]) -> anyhow::Result<String> {
        if self.key.is_empty() {
            return Ok(String::new());
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key)?;
        for part in parts {
            mac.update(part);
        }
        Ok(hex::encode(mac.finalize().into_bytes()))
    }

    fn send(&mut self, msg_type: &str, content: &Value) -> anyhow::Result<String> {
        let msg_id = uuid::Uuid::new_v4().to_string();
        let header = serde_json::to_vec(&json!({
            "msg_id": msg_id, "session": self.session, "username": "nblean",
            "date": jiff::Timestamp::now().to_string(), "msg_type": msg_type, "version": "5.3"
        }))?;
        let empty = b"{}".to_vec();
        let body = serde_json::to_vec(content)?;
        let signature = self.sign(&[&header, &empty, &empty, &body])?;
        let frames: Vec<Bytes> = [
            DELIMITER.to_vec(),
            signature.into_bytes(),
            header,
            empty.clone(),
            empty,
            body,
        ]
        .into_iter()
        .map(Bytes::from)
        .collect();
        let message =
            ZmqMessage::try_from(frames).map_err(|_| anyhow::anyhow!("empty kernel message"))?;
        self.runtime
            .block_on(async { tokio::time::timeout(IO_TIMEOUT, self.shell.send(message)).await })
            .map_err(|_| anyhow::anyhow!("timed out sending {msg_type} to the kernel"))??;
        Ok(msg_id)
    }

    fn receive_iopub(&mut self, wait: Option<Duration>) -> anyhow::Result<Option<Message>> {
        let received = match wait {
            Some(limit) => match self
                .runtime
                .block_on(async { tokio::time::timeout(limit, self.iopub.recv()).await })
            {
                Ok(result) => result?,
                Err(_) => return Ok(None),
            },
            None => self.runtime.block_on(self.iopub.recv())?,
        };
        Message::parse(&received).map(Some)
    }

    fn receive_shell(&mut self) -> anyhow::Result<Message> {
        let received = self.runtime.block_on(self.shell.recv())?;
        Message::parse(&received)
    }

    fn wait_until_ready(&mut self) -> anyhow::Result<()> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let mut probes: Vec<String> = Vec::new();
        while Instant::now() < deadline {
            probes.push(self.send("kernel_info_request", &json!({}))?);
            while let Some(message) = self.receive_iopub(Some(READY_PROBE))? {
                if message.msg_type == "iopub_welcome" || probes.contains(&message.parent_id) {
                    return Ok(());
                }
            }
        }
        bail!(
            "kernel did not answer within {}s",
            STARTUP_TIMEOUT.as_secs()
        )
    }

    pub fn execute(&mut self, code: &str, store_history: bool) -> anyhow::Result<Execution> {
        let request = self.send(
            "execute_request",
            &json!({
                "code": code, "silent": false, "store_history": store_history,
                "user_expressions": {}, "allow_stdin": false, "stop_on_error": true
            }),
        )?;
        let mut execution = Execution::default();
        loop {
            let message = self
                .receive_iopub(None)?
                .context("kernel closed the iopub channel")?;
            if message.parent_id != request {
                continue;
            }
            if message.msg_type == "status" && message.content["execution_state"] == "idle" {
                break;
            }
            execution.collect(&message.msg_type, &message.content);
        }
        loop {
            let reply = self.receive_shell()?;
            if reply.parent_id == request {
                execution.count = reply.content["execution_count"].clone();
                return Ok(execution);
            }
        }
    }
}
