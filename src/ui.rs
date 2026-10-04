use super::*;
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path as RoutePath, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Serialize, Deserialize)]
struct AgentInfo {
    role: String,
    adapter: String,
    pane: String,
    session: String,
    worktree: PathBuf,
}

pub(super) fn write_roster(runtime: &Runtime, agents: &[Agent]) -> Result<()> {
    let panes = read_panes(&runtime.home)?;
    let roster: Vec<_> = agents
        .iter()
        .map(|agent| AgentInfo {
            role: agent.role.clone(),
            adapter: agent.adapter.name().to_owned(),
            pane: panes[&agent.role].clone(),
            session: format!("tmuxor-{}", agent.role),
            worktree: runtime.worktrees.join(format!("tmuxor-{}", agent.role)),
        })
        .collect();
    atomic_write(
        &runtime.home.join("agents.json"),
        &serde_json::to_string(&roster)?,
    )
}

// The OS releases this lock even if a broker is killed. Both frontends must
// hold it for their lifetime, including while waiting for human input.
pub(super) fn broker_lock(home: &Path) -> Result<fs::File> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("broker.lock"))?;
    file.try_lock().map_err(
        |_| "another broker is already running for this team; quit it before starting a new broker",
    )?;
    Ok(file)
}

pub(super) fn open_browser(url: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(error) = Command::new(program)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        eprintln!("tmuxor: could not open a browser: {error}; open {url} manually");
    }
}

pub(super) fn wait_for_url(home: &Path) -> Result<String> {
    for _ in 0..100 {
        if let Ok(contents) = fs::read_to_string(home.join("ui.json"))
            && let Ok(info) = serde_json::from_str::<serde_json::Value>(&contents)
            && let Some(url) = info["url"].as_str()
        {
            return Ok(url.to_owned());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err("dashboard did not start; inspect tmuxor-broker with: tmux attach -t tmuxor-broker".into())
}

pub(super) fn run(arguments: &[String]) -> Result<()> {
    let mut repo = None;
    let mut port = 0u16;
    let mut open = true;
    let mut args = arguments.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-view" => open = false,
            "--port" => {
                port = args
                    .next()
                    .ok_or("--port requires a value")?
                    .parse()
                    .map_err(|_| "--port must be between 0 and 65535")?
            }
            value if !value.starts_with('-') && repo.is_none() => repo = Some(PathBuf::from(value)),
            _ => return Err("usage: tmuxor ui <repo> [--port PORT] [--no-view]".into()),
        }
    }
    let repo = repo.ok_or("usage: tmuxor ui <repo> [--port PORT] [--no-view]")?;
    let repo = PathBuf::from(git_output(&repo, ["rev-parse", "--show-toplevel"])?.trim());
    let home = repo.join(".tmuxor");
    if !home.join("panes.tsv").is_file() {
        return Err("no team found; start one with: tmuxor <repo> --ui".into());
    }
    let _lock = broker_lock(&home)?;
    let agents = if home.join("agents.json").is_file() {
        serde_json::from_str(&fs::read_to_string(home.join("agents.json"))?)?
    } else {
        let mut agents: Vec<_> = read_panes(&home)?
            .into_iter()
            .map(|(role, pane)| AgentInfo {
                session: format!("tmuxor-{role}"),
                worktree: repo.join(format!(".claude-worktrees/tmuxor-{role}")),
                role,
                pane,
                adapter: "agent".to_owned(),
            })
            .collect();
        agents.sort_by(|a, b| a.role.cmp(&b.role));
        agents
    };
    let mut random = [0u8; 32];
    fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
    let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(async {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
        let address = listener.local_addr()?.to_string();
        let url = format!("http://{address}/#token={token}");
        let app = Arc::new(App {
            home: home.clone(), repo, agents, token, address,
            broker: Mutex::new(Broker::default()),
        });
        fs::create_dir_all(home.join("queue"))?;
        fs::create_dir_all(home.join("reviewed"))?;
        fs::create_dir_all(home.join("inflight"))?;
        // A crashed delivery remains visible for manual review; it must never
        // be retried automatically because Enter may already have been sent.
        write_private(&home.join("ui.json"), &serde_json::json!({"url": url, "pid": std::process::id()}).to_string())?;
        println!("dashboard: {url}");
        println!("Agent sessions keep running when you close the browser. Ctrl-C stops this dashboard.");
        if open { open_browser(&url); }
        let result = axum::serve(listener, router(app)).with_graceful_shutdown(shutdown()).await;
        let _ = fs::remove_file(home.join("ui.json"));
        result?;
        Ok(())
    })
}

async fn shutdown() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("signal handler");
    tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
}

fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = path.with_extension("part");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(contents.as_bytes())?;
    fs::rename(temporary, path)?;
    Ok(())
}

struct App {
    home: PathBuf,
    repo: PathBuf,
    agents: Vec<AgentInfo>,
    token: String,
    address: String,
    broker: Mutex<Broker>,
}

#[derive(Default)]
struct Broker {
    busy: HashMap<String, u128>,
}

#[derive(Serialize)]
struct Pending {
    id: String,
    #[serde(flatten)]
    message: QueueMessage,
    uncertain: bool,
}

#[derive(Serialize)]
struct AgentView {
    #[serde(flatten)]
    agent: AgentInfo,
    status: &'static str,
    busy: bool,
}

#[derive(Serialize)]
struct Snapshot {
    repo: PathBuf,
    agents: Vec<AgentView>,
    messages: Vec<Pending>,
    history: Vec<serde_json::Value>,
}

type ApiResult<T> = std::result::Result<T, ApiError>;
#[derive(Debug)]
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error": self.1}))).into_response()
    }
}
fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}
fn bad_request(message: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.to_owned())
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    tokio::task::spawn_blocking(job).await.map_err(internal)?
}

fn router(app: Arc<App>) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async {
                asset(
                    "text/html; charset=utf-8",
                    include_str!("../web/index.html"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async { asset("text/css; charset=utf-8", include_str!("../web/style.css")) }),
        )
        .route(
            "/vendor/xterm.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../web/vendor/xterm.js"),
                )
            }),
        )
        .route(
            "/vendor/xterm.css",
            get(|| async {
                asset(
                    "text/css; charset=utf-8",
                    include_str!("../web/vendor/xterm.css"),
                )
            }),
        )
        .route(
            "/vendor/addon-fit.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../web/vendor/addon-fit.js"),
                )
            }),
        )
        .route("/api/state", get(snapshot))
        .route("/api/messages/{id}", post(review))
        .route("/api/agents/{role}/message", post(operator_message))
        .route("/terminal/{role}", get(terminal))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), authorize))
        .with_state(app)
}

fn asset(kind: &'static str, text: &'static str) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, kind)], text)
}

async fn authorize(
    State(app): State<Arc<App>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let origin = format!("http://{}", app.address);
    if headers.get(header::HOST).and_then(|h| h.to_str().ok()) != Some(app.address.as_str())
        || headers
            .get(header::ORIGIN)
            .is_some_and(|h| h.to_str().ok() != Some(origin.as_str()))
    {
        return (
            StatusCode::FORBIDDEN,
            "Use the dashboard URL printed by tmuxor.",
        )
            .into_response();
    }
    let path = request.uri().path();
    if path.starts_with("/api/") || path.starts_with("/terminal/") {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .or_else(|| {
                request
                    .uri()
                    .query()?
                    .split('&')
                    .find_map(|part| part.strip_prefix("token="))
            });
        if token != Some(app.token.as_str()) {
            return ApiError(
                StatusCode::UNAUTHORIZED,
                "Open the dashboard using the full URL printed by tmuxor.".to_owned(),
            )
            .into_response();
        }
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"));
    response
}

fn queue_id(id: &str) -> bool {
    id.strip_suffix(".json")
        .is_some_and(|stem| !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_digit()))
}

fn pending(home: &Path) -> ApiResult<Vec<Pending>> {
    let mut messages = Vec::new();
    for (directory, uncertain) in [("queue", false), ("inflight", true)] {
        for entry in fs::read_dir(home.join(directory)).map_err(internal)? {
            let entry = entry.map_err(internal)?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if !queue_id(&id) || !entry.file_type().map_err(internal)?.is_file() {
                continue;
            }
            messages.push(Pending {
                id,
                message: read_queue_message(&entry.path()).map_err(internal)?,
                uncertain,
            });
        }
    }
    messages.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(messages)
}

fn update_busy(broker: &mut Broker, messages: &[Pending]) {
    for pending in messages {
        let completed = pending
            .id
            .trim_end_matches(".json")
            .parse::<u128>()
            .unwrap_or(0);
        if broker
            .busy
            .get(&pending.message.from)
            .is_some_and(|sent| completed > *sent)
        {
            broker.busy.remove(&pending.message.from);
        }
    }
}

async fn snapshot(State(app): State<Arc<App>>) -> ApiResult<Json<Snapshot>> {
    blocking(move || {
        let mut broker = app.broker.lock().map_err(internal)?;
        let messages = pending(&app.home)?;
        update_busy(&mut broker, &messages);
        let panes =
            tmux_output(["list-panes", "-a", "-F", "#{pane_id}\t#{pane_dead}"]).unwrap_or_default();
        let panes: HashMap<_, _> = panes
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .collect();
        let agents = app
            .agents
            .iter()
            .map(|agent| {
                let status = match panes.get(agent.pane.as_str()) {
                    None => "offline",
                    Some(&"1") => "exited",
                    _ if messages.iter().any(|m| m.message.from == agent.role) => "review",
                    _ if broker.busy.contains_key(&agent.role) => "active",
                    _ => "running",
                };
                AgentView {
                    agent: agent.clone(),
                    status,
                    busy: broker.busy.contains_key(&agent.role),
                }
            })
            .collect();
        let mut files: Vec<_> = fs::read_dir(app.home.join("reviewed"))
            .map_err(internal)?
            .filter_map(|e| e.ok())
            .filter(|e| queue_id(&e.file_name().to_string_lossy()))
            .map(|e| e.path())
            .collect();
        files.sort();
        let history = files
            .iter()
            .rev()
            .take(30)
            .filter_map(|path| serde_json::from_str(&fs::read_to_string(path).ok()?).ok())
            .collect();
        Ok(Json(Snapshot {
            repo: app.repo.clone(),
            agents,
            messages,
            history,
        }))
    })
    .await
}

#[derive(Deserialize)]
struct Decision {
    action: String,
    to: Option<String>,
    text: Option<String>,
    #[serde(default)]
    confirm_busy: bool,
}

fn ensure_live(agent: &AgentInfo) -> ApiResult<()> {
    let dead = tmux_output(["display-message", "-p", "-t", &agent.pane, "#{pane_dead}"])
        .map_err(|_| bad_request("This agent's session is no longer available."))?;
    if dead.trim() != "0" {
        return Err(bad_request(
            "This agent has exited. Its terminal is available for inspection.",
        ));
    }
    Ok(())
}

fn send_text(app: &App, agent: &AgentInfo, text: &str, from: &str) -> ApiResult<()> {
    ensure_live(agent)?;
    let path = app.home.join(format!("send-{}.txt", timestamp_nanos()));
    fs::write(&path, text).map_err(internal)?;
    let result = deliver(&agent.pane, &path, from).map_err(internal);
    let _ = fs::remove_file(path);
    result
}

async fn review(
    State(app): State<Arc<App>>,
    RoutePath(id): RoutePath<String>,
    Json(decision): Json<Decision>,
) -> ApiResult<Json<serde_json::Value>> {
    blocking(move || {
        if !queue_id(&id) { return Err(bad_request("Invalid message ID.")); }
        if !matches!(decision.action.as_str(), "send" | "drop") { return Err(bad_request("Choose send or drop.")); }
        let mut broker = app.broker.lock().map_err(internal)?;
        let messages = pending(&app.home)?;
        update_busy(&mut broker, &messages);
        let message = messages.iter().find(|m| m.id == id).ok_or_else(|| ApiError(StatusCode::CONFLICT, "This message has already been reviewed. Refresh the inbox.".to_owned()))?;
        if message.uncertain && decision.action == "send" {
            return Err(bad_request("Delivery was interrupted. Inspect the recipient's terminal, then dismiss this item and send any needed follow-up manually."));
        }
        let to = decision.to.unwrap_or_else(|| message.message.to.clone());
        let text = decision.text.unwrap_or_else(|| message.message.text.clone());
        let target = if decision.action == "send" && !is_terminal_target(&to) {
            let agent = app.agents.iter().find(|agent| agent.role == to).ok_or_else(|| bad_request("Pick a recipient before sending."))?;
            if text.trim().is_empty() { return Err(bad_request("The message cannot be empty.")); }
            ensure_live(agent)?;
            if broker.busy.contains_key(&to) && !decision.confirm_busy {
                return Err(ApiError(StatusCode::CONFLICT, "The recipient may still be working. Confirm delivery to continue.".to_owned()));
            }
            Some(agent)
        } else { None };
        let source = app.home.join(if message.uncertain { "inflight" } else { "queue" }).join(&id);
        let claimed = app.home.join("inflight").join(&id);
        if !message.uncertain { fs::rename(&source, &claimed).map_err(internal)?; }
        if let Some(agent) = target {
            let attempted = QueueMessage { from: message.message.from.clone(), to: to.clone(), text: text.clone() };
            atomic_write(&claimed, &serde_json::to_string(&attempted).map_err(internal)?).map_err(internal)?;
            // An error can occur after pasting. Keep the exact attempted
            // message visible instead of offering a potentially duplicate retry.
            send_text(&app, agent, &text, &message.message.from)?;
            broker.busy.insert(to.clone(), timestamp_nanos());
        }
        let action = if decision.action == "drop" { "dropped" } else if target.is_none() { "acknowledged" } else { "sent" };
        let record = serde_json::json!({"id": id, "from": message.message.from, "to": to, "text": text, "action": action, "time": timestamp()});
        atomic_write(&app.home.join("reviewed").join(&id), &record.to_string()).map_err(internal)?;
        fs::remove_file(&claimed).map_err(internal)?;
        let _ = log(&app.home, &format!("{} {} -> {} {action} (dashboard)", timestamp(), message.message.from, to));
        Ok(Json(record))
    }).await
}

#[derive(Deserialize)]
struct OperatorMessage {
    text: String,
    #[serde(default)]
    confirm_busy: bool,
}

async fn operator_message(
    State(app): State<Arc<App>>,
    RoutePath(role): RoutePath<String>,
    Json(message): Json<OperatorMessage>,
) -> ApiResult<StatusCode> {
    blocking(move || {
        if message.text.trim().is_empty() {
            return Err(bad_request("The message cannot be empty."));
        }
        let mut broker = app.broker.lock().map_err(internal)?;
        update_busy(&mut broker, &pending(&app.home)?);
        let agent = app
            .agents
            .iter()
            .find(|a| a.role == role)
            .ok_or_else(|| bad_request("Unknown agent."))?;
        if broker.busy.contains_key(&role) && !message.confirm_busy {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "The agent may still be working. Confirm delivery to continue.".to_owned(),
            ));
        }
        send_text(&app, agent, &message.text, "OPERATOR")?;
        broker.busy.insert(role, timestamp_nanos());
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

async fn terminal(
    State(app): State<Arc<App>>,
    RoutePath(role): RoutePath<String>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let agent = app
        .agents
        .iter()
        .find(|a| a.role == role)
        .cloned()
        .ok_or_else(|| bad_request("Unknown agent."))?;
    Ok(ws
        .max_message_size(1024 * 1024)
        .on_upgrade(move |socket| terminal_socket(socket, app, agent)))
}

struct TerminalConnection {
    master: Box<dyn MasterPty + Send>,
    input: tokio::sync::mpsc::Sender<String>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}
impl Drop for TerminalConnection {
    fn drop(&mut self) {
        // Only this browser's tmux client exits; the agent session stays alive.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum TerminalInput {
    Input { data: String },
    Resize { cols: u16, rows: u16 },
}

async fn terminal_socket(mut socket: WebSocket, app: Arc<App>, agent: AgentInfo) {
    let connection = (|| -> Result<_> {
        let pair = native_pty_system().openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let mut command = CommandBuilder::new("tmux");
        command.args(["attach-session", "-t", &agent.session]);
        command.env_remove("TMUX");
        command.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        // Keep the sender inside TerminalConnection: its Drop kills the tmux
        // client BEFORE the writer is dropped. portable-pty sends an EOF byte
        // when its writer is dropped, which a live tmux client would otherwise
        // forward to the agent, even when a browser merely refreshes.
        let (input, mut input_rx) = tokio::sync::mpsc::channel::<String>(16);
        thread::spawn(move || {
            let mut writer = writer;
            while let Some(data) = input_rx.blocking_recv() {
                if writer.write_all(data.as_bytes()).is_err() {
                    break;
                }
            }
        });
        Ok((
            TerminalConnection {
                master: pair.master,
                input,
                child,
            },
            reader,
        ))
    })()
    .map_err(|error| error.to_string());
    let (connection, mut reader) = match connection {
        Ok(value) => value,
        Err(error) => {
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({"error": error.to_string()})
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(size) => {
                    if tx.blocking_send(buffer[..size].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    let mut last_seen = std::time::Instant::now();
    loop {
        tokio::select! {
            output = rx.recv() => match output {
                Some(bytes) => if socket.send(Message::Binary(bytes.into())).await.is_err() { break; },
                None => break,
            },
            input = socket.recv() => {
                let Some(Ok(input)) = input else { break; };
                last_seen = std::time::Instant::now();
                match input {
                    Message::Text(json) => match serde_json::from_str::<TerminalInput>(&json) {
                        Ok(TerminalInput::Input { data }) => {
                            if data.contains('\r') && let Ok(mut broker) = app.broker.lock() {
                                broker.busy.insert(agent.role.clone(), timestamp_nanos());
                            }
                            if connection.input.send(data).await.is_err() { break; }
                        },
                        Ok(TerminalInput::Resize { cols, rows }) if (2..=500).contains(&cols) && (2..=200).contains(&rows) => {
                            let _ = connection.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                        },
                        _ => {},
                    },
                    Message::Close(_) => break,
                    _ => {},
                }
            },
            _ = heartbeat.tick() => {
                if last_seen.elapsed() > Duration::from_secs(60) || socket.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            },
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn fixture() -> Arc<App> {
        let home = temporary_path("tmuxor-ui-test");
        for directory in ["queue", "inflight", "reviewed"] {
            fs::create_dir_all(home.join(directory)).unwrap();
        }
        Arc::new(App {
            repo: home.join("repo"),
            home,
            agents: Vec::new(),
            token: "test-token".to_owned(),
            address: "127.0.0.1:1234".to_owned(),
            broker: Mutex::new(Broker::default()),
        })
    }

    #[tokio::test]
    async fn protects_api_from_missing_credentials_and_foreign_origins() {
        let app = fixture();
        for (token, origin, host, expected) in [
            (None, None, "127.0.0.1:1234", StatusCode::UNAUTHORIZED),
            (
                Some("wrong"),
                None,
                "127.0.0.1:1234",
                StatusCode::UNAUTHORIZED,
            ),
            (
                Some("test-token"),
                Some("https://other.example"),
                "127.0.0.1:1234",
                StatusCode::FORBIDDEN,
            ),
            (
                Some("test-token"),
                None,
                "other.example:1234",
                StatusCode::FORBIDDEN,
            ),
            (
                Some("test-token"),
                Some("http://127.0.0.1:1234"),
                "127.0.0.1:1234",
                StatusCode::OK,
            ),
        ] {
            let mut request = Request::builder().uri("/api/state").header("host", host);
            if let Some(token) = token {
                request = request.header("authorization", format!("Bearer {token}"));
            }
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            let response = router(app.clone())
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
        let response = router(app.clone())
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header("host", &app.address)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response.headers()[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        fs::remove_dir_all(&app.home).unwrap();
    }

    #[tokio::test]
    async fn review_preserves_edits_and_consumes_a_message_once() {
        let app = fixture();
        let id = "123.json";
        fs::write(
            app.home.join("queue").join(id),
            r#"{"from":"builder","to":"human","text":"Original"}"#,
        )
        .unwrap();
        let request = || {
            Request::builder()
                .method("POST")
                .uri("/api/messages/123.json")
                .header("host", &app.address)
                .header("authorization", "Bearer test-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"action":"send","to":"done","text":"Edited conclusion"}"#,
                ))
                .unwrap()
        };
        let response = router(app.clone()).oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["action"], "acknowledged");
        assert_eq!(body["text"], "Edited conclusion");
        assert_eq!(body["to"], "done");
        assert!(!app.home.join("queue").join(id).exists());
        assert!(app.home.join("reviewed").join(id).is_file());
        assert_eq!(
            router(app.clone())
                .oneshot(request())
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        fs::remove_dir_all(&app.home).unwrap();
    }

    #[tokio::test]
    async fn invalid_recipients_do_not_consume_messages_and_uncertain_deliveries_cannot_replay() {
        let app = fixture();
        let path = app.home.join("queue/123.json");
        fs::write(&path, r#"{"from":"builder","to":"missing","text":"Ready"}"#).unwrap();
        let decision = || Decision {
            action: "send".to_owned(),
            to: None,
            text: None,
            confirm_busy: false,
        };
        assert!(
            review(
                State(app.clone()),
                RoutePath("123.json".to_owned()),
                Json(decision())
            )
            .await
            .is_err()
        );
        assert!(path.exists());
        fs::rename(&path, app.home.join("inflight/123.json")).unwrap();
        let mut retry = decision();
        retry.to = Some("human".to_owned());
        assert!(
            review(
                State(app.clone()),
                RoutePath("123.json".to_owned()),
                Json(retry)
            )
            .await
            .is_err()
        );
        let mut dismiss = decision();
        dismiss.action = "drop".to_owned();
        assert!(
            review(
                State(app.clone()),
                RoutePath("123.json".to_owned()),
                Json(dismiss)
            )
            .await
            .is_ok()
        );
        assert!(!app.home.join("inflight/123.json").exists());
        fs::remove_dir_all(&app.home).unwrap();
    }

    #[test]
    fn a_team_has_only_one_broker_owner() {
        let app = fixture();
        let first = broker_lock(&app.home).unwrap();
        assert!(broker_lock(&app.home).is_err());
        drop(first);
        assert!(broker_lock(&app.home).is_ok());
        fs::remove_dir_all(&app.home).unwrap();
    }

    #[test]
    fn only_completion_after_the_latest_input_clears_activity() {
        let mut broker = Broker::default();
        broker.busy.insert("builder".to_owned(), 200);
        let mut messages = vec![Pending {
            id: "100.json".to_owned(),
            message: QueueMessage {
                from: "builder".to_owned(),
                to: "human".to_owned(),
                text: "Done".to_owned(),
            },
            uncertain: false,
        }];
        update_busy(&mut broker, &messages);
        assert!(broker.busy.contains_key("builder"));
        messages[0].id = "300.json".to_owned();
        update_busy(&mut broker, &messages);
        assert!(!broker.busy.contains_key("builder"));
    }

    #[test]
    fn rejects_queue_path_traversal() {
        for id in ["../123.json", "/123.json", "123.json/..", "a.json", ".json"] {
            assert!(!queue_id(id));
        }
        assert!(queue_id("123456789.json"));
    }
}
