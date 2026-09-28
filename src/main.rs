use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const DEFAULT_TEAM: &str = include_str!("../examples/team.txt");
const RELAY_MARKER: &str = "[FROM ";

#[derive(Debug, Clone)]
struct Agent {
    adapter: Adapter,
    role: String,
    prompt: String,
}

#[derive(Debug, Clone)]
struct Rule {
    name: String,
    contents: String,
}

#[derive(Debug, Clone, Copy)]
enum Adapter {
    Claude,
    Codex,
}

impl Adapter {
    fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => Err(format!(
                "agent adapter '{value}' is not implemented yet; available adapters: claude, codex"
            )
            .into()),
        }
    }

    fn generate_files(
        self,
        runtime: &Runtime,
        agent: &Agent,
        agents: &[Agent],
        rules: &[Rule],
        permission_mode: &str,
    ) -> Result<()> {
        match self {
            Self::Claude => generate_claude_files(runtime, agent, agents, rules, permission_mode),
            Self::Codex => generate_codex_files(runtime, agent, agents, rules, permission_mode),
        }
    }
}

#[derive(Debug)]
struct LaunchOptions {
    repo: PathBuf,
    team: Option<PathBuf>,
    roles: Option<PathBuf>,
    rules: Option<PathBuf>,
    task: Option<String>,
    permission_mode: String,
    clean: bool,
    open_views: bool,
}

#[derive(Debug)]
struct Runtime {
    repo: PathBuf,
    home: PathBuf,
    worktrees: PathBuf,
    executable: PathBuf,
}

#[derive(Debug)]
struct QueueMessage {
    from: String,
    to: String,
    text: String,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("tmuxor: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut arguments = env::args().skip(1).collect::<Vec<_>>();
    match arguments.first().map(String::as_str) {
        Some("broker") => {
            arguments.remove(0);
            if !arguments.is_empty() {
                return Err("usage: tmuxor broker".into());
            }
            run_broker()
        }
        Some("relay-stop") => {
            arguments.remove(0);
            let require_relayed_input = match arguments.first().map(String::as_str) {
                Some("--require-relayed-input") => {
                    arguments.remove(0);
                    true
                }
                _ => false,
            };
            if !arguments.is_empty() {
                return Err("usage: tmuxor relay-stop [--require-relayed-input]".into());
            }
            relay_stop(require_relayed_input)
        }
        Some("stop") => {
            arguments.remove(0);
            stop(&arguments)
        }
        Some("init") => {
            arguments.remove(0);
            init(&arguments)
        }
        Some("-h") | Some("--help") | None => {
            print_usage();
            Ok(())
        }
        _ => launch(parse_launch_options(arguments)?),
    }
}

fn print_usage() {
    println!(
        "Usage: tmuxor <repo> [--team FILE] [--roles DIR] [--rules DIR] [--task TEXT] [--perm MODE] [--yolo] [--clean] [--no-view]"
    );
    println!("       tmuxor init [repo] [--force]");
    println!("       tmuxor broker");
    println!("       tmuxor relay-stop");
    println!("       tmuxor stop <repo>");
}

fn parse_launch_options(arguments: Vec<String>) -> Result<LaunchOptions> {
    let mut repo = None;
    let mut team = None;
    let mut roles = None;
    let mut rules = None;
    let mut task = None;
    let mut permission_mode = "auto".to_owned();
    let mut clean = false;
    let mut open_views = true;
    let mut index = 0;

    while index < arguments.len() {
        match arguments[index].as_str() {
            "--team" => {
                index += 1;
                team = Some(PathBuf::from(required_argument(
                    &arguments, index, "--team",
                )?));
            }
            "--roles" => {
                index += 1;
                roles = Some(PathBuf::from(required_argument(
                    &arguments, index, "--roles",
                )?));
            }
            "--rules" => {
                index += 1;
                rules = Some(PathBuf::from(required_argument(
                    &arguments, index, "--rules",
                )?));
            }
            "--task" => {
                index += 1;
                task = Some(required_argument(&arguments, index, "--task")?.to_owned());
            }
            "--perm" => {
                index += 1;
                permission_mode = required_argument(&arguments, index, "--perm")?.to_owned();
            }
            "--yolo" => permission_mode = "bypassPermissions".to_owned(),
            "--clean" => clean = true,
            "--no-view" => open_views = false,
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown option: {value}").into());
            }
            value => {
                if repo.replace(PathBuf::from(value)).is_some() {
                    return Err("only one repository may be supplied".into());
                }
            }
        }
        index += 1;
    }

    Ok(LaunchOptions {
        repo: repo.ok_or("a repository is required")?,
        team,
        roles,
        rules,
        task,
        permission_mode,
        clean,
        open_views,
    })
}

fn required_argument<'a>(arguments: &'a [String], index: usize, option: &str) -> Result<&'a str> {
    arguments
        .get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("{option} requires a value").into())
}

fn launch(options: LaunchOptions) -> Result<()> {
    let repo = git_output(&options.repo, ["rev-parse", "--show-toplevel"])?;
    let repo = PathBuf::from(repo.trim());
    let runtime = Runtime {
        home: repo.join(".tmuxor"),
        worktrees: repo.join(".claude-worktrees"),
        executable: env::current_exe()?,
        repo,
    };

    let (agents, entry) = load_agents(
        &runtime.repo,
        options.team.as_deref(),
        options.roles.as_deref(),
    )?;
    let rules = load_rules(&runtime.repo, options.rules.as_deref())?;
    if agents.len() < 2 {
        return Err("need at least two agents in the team file".into());
    }
    if options.clean {
        clean(&runtime, &agents)?;
        println!("cleaned.");
    }
    ensure_sessions_absent(&agents)?;
    fs::create_dir_all(runtime.home.join("queue"))?;

    create_worktrees(&runtime, &agents)?;
    generate_agent_files(&runtime, &agents, &rules, &options.permission_mode)?;
    create_sessions(&runtime, &agents)?;
    start_broker(&runtime)?;
    if options.open_views {
        open_terminal_views(&agents);
    }

    if let Some(task) = options.task {
        deliver_task(&runtime, &entry, &task)?;
    }

    for agent in &agents {
        let pane = read_panes(&runtime.home)?
            .get(&agent.role)
            .cloned()
            .unwrap_or_else(|| "?".to_owned());
        println!("{}\t{}\ttmuxor-{}", agent.role, pane, agent.role);
    }
    println!("entry:  {entry}");
    println!("attach: tmux attach -t tmuxor-broker");
    Ok(())
}

fn load_agents(
    repo: &Path,
    team_override: Option<&Path>,
    roles_override: Option<&Path>,
) -> Result<(Vec<Agent>, String)> {
    let team_path = team_override.map(Path::to_path_buf).or_else(|| {
        ["agora/team.txt", "agora/manifest", "manifest", "team.txt"]
            .into_iter()
            .map(|name| repo.join(name))
            .find(|path| path.is_file())
    });
    let team_contents = match &team_path {
        Some(path) => fs::read_to_string(path)?,
        None => DEFAULT_TEAM.to_owned(),
    };
    let role_directory = roles_override.map(Path::to_path_buf).or_else(|| {
        ["agora/roles", "roles"]
            .into_iter()
            .map(|name| repo.join(name))
            .find(|path| path.is_dir())
    });

    let mut agents = Vec::new();
    let mut entry = None;
    for line in team_contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 2 {
            return Err(format!("malformed team line: {line}").into());
        }
        let prompt = match &role_directory {
            Some(directory) => {
                let path = directory.join(format!("{}.md", fields[1]));
                fs::read_to_string(&path)
                    .map_err(|_| format!("missing role prompt: {}", path.display()))?
            }
            None => builtin_prompt(fields[1])
                .ok_or_else(|| {
                    format!(
                        "no role directory supplied and no built-in prompt for {}",
                        fields[1]
                    )
                })?
                .to_owned(),
        };
        let agent = Agent {
            adapter: Adapter::parse(fields[0])?,
            role: fields[1].to_ascii_lowercase(),
            prompt,
        };
        if fields[2..].contains(&"--entry") {
            entry = Some(agent.role.clone());
        }
        agents.push(agent);
    }
    let entry = entry
        .or_else(|| agents.first().map(|agent| agent.role.clone()))
        .ok_or("team file contains no agents")?;
    Ok((agents, entry))
}

fn load_rules(repo: &Path, rules_override: Option<&Path>) -> Result<Vec<Rule>> {
    let directory = rules_override.map(Path::to_path_buf).or_else(|| {
        ["agora/rules", "rules"]
            .into_iter()
            .map(|name| repo.join(name))
            .find(|path| path.is_dir())
    });
    let Some(directory) = directory else {
        return Ok(Vec::new());
    };
    if !directory.is_dir() {
        return Err(format!("rules directory does not exist: {}", directory.display()).into());
    }

    let mut paths = fs::read_dir(&directory)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|file_type| file_type.is_file())
                .map(|_| entry.path())
        })
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            Ok(Rule {
                name: path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("rule filename is not valid UTF-8")?
                    .to_owned(),
                contents: fs::read_to_string(path)?,
            })
        })
        .collect()
}

fn builtin_prompt(role: &str) -> Option<&'static str> {
    match role {
        "specifier" => Some(include_str!("../examples/roles/specifier.md")),
        "builder" => Some(include_str!("../examples/roles/builder.md")),
        "tester" => Some(include_str!("../examples/roles/tester.md")),
        _ => None,
    }
}

fn clean(runtime: &Runtime, agents: &[Agent]) -> Result<()> {
    for agent in agents {
        let _ = tmux(["kill-session", "-t", &format!("tmuxor-{}", agent.role)]);
        let path = runtime.worktrees.join(format!("tmuxor-{}", agent.role));
        let _ = git(
            &runtime.repo,
            ["worktree", "remove", "--force", &path.display().to_string()],
        );
        let _ = git(
            &runtime.repo,
            ["branch", "-D", &format!("tmuxor/{}", agent.role)],
        );
        if matches!(agent.adapter, Adapter::Codex)
            && let Ok(profile) = codex_profile_path(&agent.role)
        {
            let _ = fs::remove_file(profile);
        }
    }
    let _ = tmux(["kill-session", "-t", "tmuxor-broker"]);
    if runtime.home.exists() {
        fs::remove_dir_all(&runtime.home)?;
    }
    Ok(())
}

fn ensure_sessions_absent(agents: &[Agent]) -> Result<()> {
    for agent in agents {
        let session = format!("tmuxor-{}", agent.role);
        if tmux(["has-session", "-t", &session]).is_ok() {
            return Err(format!("session {session} already exists; re-run with --clean").into());
        }
    }
    Ok(())
}

fn create_worktrees(runtime: &Runtime, agents: &[Agent]) -> Result<()> {
    for agent in agents {
        let path = runtime.worktrees.join(format!("tmuxor-{}", agent.role));
        if path.is_dir() {
            continue;
        }
        let branch = format!("tmuxor/{}", agent.role);
        git(
            &runtime.repo,
            [
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                &path.display().to_string(),
                "HEAD",
            ],
        )
        .or_else(|_| {
            git(
                &runtime.repo,
                [
                    "worktree",
                    "add",
                    "-q",
                    &path.display().to_string(),
                    &branch,
                ],
            )
        })?;
    }
    Ok(())
}

fn generate_agent_files(
    runtime: &Runtime,
    agents: &[Agent],
    rules: &[Rule],
    permission_mode: &str,
) -> Result<()> {
    for agent in agents {
        agent
            .adapter
            .generate_files(runtime, agent, agents, rules, permission_mode)?;
    }
    Ok(())
}

fn generate_claude_files(
    runtime: &Runtime,
    agent: &Agent,
    agents: &[Agent],
    rules: &[Rule],
    permission_mode: &str,
) -> Result<()> {
    let settings_path = runtime.home.join(format!("settings-{}.json", agent.role));
    let hook_command = format!(
        "{} relay-stop",
        shell_quote(&runtime.executable.display().to_string())
    );
    fs::write(
        &settings_path,
        format!(
            "{{\"hooks\":{{\"Stop\":[{{\"hooks\":[{{\"type\":\"command\",\"command\":{}}}]}}]}}}}",
            json_string(&hook_command)
        ),
    )?;

    let prompt_path = runtime.home.join(format!("prompt-{}.txt", agent.role));
    fs::write(
        &prompt_path,
        generated_prompt(agent, agents, rules, runtime),
    )?;

    let launcher_path = runtime.home.join(format!("launch-{}.sh", agent.role));
    let mut launcher = String::from("#!/usr/bin/env sh\nexec claude \\\n");
    launcher.push_str(&format!(
        "  --settings {} \\\n",
        shell_quote(&settings_path.display().to_string())
    ));
    launcher.push_str(&format!(
        "  --permission-mode {} \\\n",
        shell_quote(permission_mode)
    ));
    launcher.push_str(&format!(
        "  --append-system-prompt-file {}",
        shell_quote(&prompt_path.display().to_string())
    ));
    for teammate in agents.iter().filter(|teammate| teammate.role != agent.role) {
        launcher.push_str(&format!(
            " \\\n  --add-dir {}",
            shell_quote(
                &runtime
                    .worktrees
                    .join(format!("tmuxor-{}", teammate.role))
                    .display()
                    .to_string()
            )
        ));
    }
    launcher.push('\n');
    fs::write(&launcher_path, launcher)?;
    make_executable(&launcher_path)
}

fn generate_codex_files(
    runtime: &Runtime,
    agent: &Agent,
    agents: &[Agent],
    rules: &[Rule],
    permission_mode: &str,
) -> Result<()> {
    let prompt = generated_prompt(agent, agents, rules, runtime);
    let prompt_path = runtime.home.join(format!("prompt-{}.txt", agent.role));
    fs::write(&prompt_path, &prompt)?;

    let notify_path = runtime.home.join(format!("notify-{}.sh", agent.role));
    let mut notify = String::from("#!/usr/bin/env sh\n");
    notify.push_str(&format!(
        "TMUXOR_HOME={}\n",
        shell_quote(&runtime.home.display().to_string())
    ));
    notify.push_str(&format!("TMUXOR_ROLE={}\n", shell_quote(&agent.role)));
    notify.push_str("export TMUXOR_HOME TMUXOR_ROLE\n");
    notify.push_str(&format!(
        "printf '%s' \"$1\" | {} relay-stop --require-relayed-input\n",
        shell_quote(&runtime.executable.display().to_string())
    ));
    fs::write(&notify_path, notify)?;
    make_executable(&notify_path)?;

    let profile_path = codex_profile_path(&agent.role)?;
    if let Some(parent) = profile_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let worktree = runtime.worktrees.join(format!("tmuxor-{}", agent.role));
    let mut profile = format!(
        "notify = [{}]\ndeveloper_instructions = {}\n",
        json_string(&notify_path.display().to_string()),
        json_string(&prompt)
    );
    for trusted in [&runtime.repo, &worktree] {
        profile.push_str(&format!(
            "\n[projects.{}]\ntrust_level = \"trusted\"\n",
            json_string(&trusted.display().to_string())
        ));
    }
    fs::write(&profile_path, profile)?;

    let mut arguments = vec!["--profile".to_owned(), codex_profile_name(&agent.role)];
    arguments.extend(codex_permission_arguments(permission_mode)?);

    let launcher_path = runtime.home.join(format!("launch-{}.sh", agent.role));
    let mut launcher = String::from("#!/usr/bin/env sh\nexec codex");
    for argument in &arguments {
        launcher.push_str(&format!(" \\\n  {}", shell_quote(argument)));
    }
    launcher.push('\n');
    fs::write(&launcher_path, launcher)?;
    make_executable(&launcher_path)
}

fn codex_permission_arguments(permission_mode: &str) -> Result<Vec<String>> {
    let arguments: &[&str] = match permission_mode {
        "auto" | "default" => &[
            "--ask-for-approval",
            "on-request",
            "--sandbox",
            "workspace-write",
        ],
        "acceptEdits" => &[
            "--ask-for-approval",
            "never",
            "--sandbox",
            "workspace-write",
        ],
        "plan" => &["--ask-for-approval", "untrusted", "--sandbox", "read-only"],
        "bypassPermissions" => &["--dangerously-bypass-approvals-and-sandbox"],
        _ => {
            return Err(format!(
                "codex has no mapping for permission mode '{permission_mode}'; use auto, default, acceptEdits, plan or bypassPermissions"
            )
            .into());
        }
    };
    Ok(arguments
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect())
}

fn codex_profile_name(role: &str) -> String {
    format!("tmuxor-{role}")
}

fn codex_profile_path(role: &str) -> Result<PathBuf> {
    let home = match env::var("CODEX_HOME") {
        Ok(value) => PathBuf::from(value),
        Err(_) => PathBuf::from(
            env::var("HOME").map_err(|_| "cannot locate the codex home: HOME is not set")?,
        )
        .join(".codex"),
    };
    Ok(home.join(format!("{}.config.toml", codex_profile_name(role))))
}

fn generated_prompt(agent: &Agent, agents: &[Agent], rules: &[Rule], runtime: &Runtime) -> String {
    let mut prompt = String::new();
    if !rules.is_empty() {
        prompt.push_str("# Project rules\n\n");
        prompt.push_str("These shared rules apply to every agent and take precedence over role-specific instructions.\n");
        for rule in rules {
            prompt.push_str(&format!("\n## {}\n\n{}\n", rule.name, rule.contents.trim()));
        }
        prompt.push_str("\n---\n\n");
    }
    prompt.push_str(agent.prompt.trim_end());
    prompt.push_str("\n\n---\n\n");
    prompt.push_str(&format!(
        "You are {}, one agent in a team working on the same repository.\n",
        agent.role.to_ascii_uppercase()
    ));
    prompt.push_str("You each have your own git worktree. Your teammates:\n\n");
    for teammate in agents.iter().filter(|teammate| teammate.role != agent.role) {
        prompt.push_str(&format!(
            "  {:<12} {}\n",
            teammate.role,
            role_summary(&teammate.prompt)
        ));
        prompt.push_str(&format!(
            "  {:<12}   worktree: {}\n",
            "",
            runtime
                .worktrees
                .join(format!("tmuxor-{}", teammate.role))
                .display()
        ));
    }
    prompt.push_str(
        "\n## Addressing a teammate\n\nPut a routing line on a line of its own, ideally the first line:\n\n    [TO: <teammate>]\n\nInclude exactly one. Only your final message of a turn is relayed; if it carries\nno routing line, or more than one, it stops at the operator who must pick a\nrecipient by hand.\n\nUse [TO: human] when you need the operator rather than a teammate, and\n[TO: done] when the work is finished and the chain should stop.\n\n## What the others can see\n\nNothing except the message you send. Your tool calls, your thinking and your\nterminal output are invisible to them. Make each message self-contained: what\nyou did, what you want from the recipient, and any specific question. Reference\nconcrete file paths. Keep it short — this is a conversation, not a report.\n\nA human reviews every message before it is relayed, and may edit, retarget or\ndrop it. You may only write inside your own worktree, but you can read the\nothers' to see what they did.\n",
    );
    prompt
}

fn role_summary(prompt: &str) -> &str {
    prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("No role summary.")
}

fn create_sessions(runtime: &Runtime, agents: &[Agent]) -> Result<()> {
    fs::write(runtime.home.join("panes.tsv"), "")?;
    for agent in agents {
        let role_path = runtime.home.join(format!("launch-{}.sh", agent.role));
        let worktree = runtime.worktrees.join(format!("tmuxor-{}", agent.role));
        let session = format!("tmuxor-{}", agent.role);
        let pane = tmux_output([
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-s",
            &session,
            "-x",
            "200",
            "-y",
            "50",
            "-c",
            &worktree.display().to_string(),
            "-e",
            &format!("TMUXOR_HOME={}", runtime.home.display()),
            "-e",
            &format!("TMUXOR_ROLE={}", agent.role),
            &role_path.display().to_string(),
        ])?;
        tmux([
            "set-window-option",
            "-t",
            &format!("{session}:0"),
            "remain-on-exit",
            "on",
        ])?;
        append_line(
            &runtime.home.join("panes.tsv"),
            &format!("{}\t{}\t{}\n", agent.role, pane.trim(), session),
        )?;
    }
    Ok(())
}

fn start_broker(runtime: &Runtime) -> Result<()> {
    tmux([
        "new-session",
        "-d",
        "-s",
        "tmuxor-broker",
        "-x",
        "200",
        "-y",
        "50",
        "-c",
        &runtime.repo.display().to_string(),
        "-e",
        &format!("TMUXOR_HOME={}", runtime.home.display()),
        &runtime.executable.display().to_string(),
        "broker",
    ])?;
    tmux([
        "set-window-option",
        "-t",
        "tmuxor-broker:0",
        "remain-on-exit",
        "on",
    ])
}

fn stop(arguments: &[String]) -> Result<()> {
    if arguments.len() != 1 {
        return Err("usage: tmuxor stop <repo>".into());
    }

    let repo = git_output(Path::new(&arguments[0]), ["rev-parse", "--show-toplevel"])?;
    let home = PathBuf::from(repo.trim()).join(".tmuxor");
    let mut sessions = read_panes(&home)
        .map(|panes| panes.into_values().collect::<Vec<_>>())
        .unwrap_or_default();
    sessions.push("tmuxor-broker".to_owned());
    sessions.sort();
    sessions.dedup();

    for session in sessions {
        let _ = tmux(["kill-session", "-t", &session]);
    }
    println!("stopped tmuxor sessions; worktrees and runtime files were kept.");
    Ok(())
}

fn init(arguments: &[String]) -> Result<()> {
    let mut target = None;
    let mut force = false;
    for argument in arguments {
        match argument.as_str() {
            "--force" => force = true,
            value if value.starts_with('-') => {
                return Err(format!("unknown option for init: {value}").into());
            }
            value if target.is_none() => target = Some(PathBuf::from(value)),
            value => return Err(format!("unexpected argument for init: {value}").into()),
        }
    }

    let target = target.unwrap_or_else(|| PathBuf::from("."));
    let repo = git_output(&target, ["rev-parse", "--show-toplevel"])?;
    let repo = PathBuf::from(repo.trim());

    let mut files = vec![("agora/team.txt".to_owned(), DEFAULT_TEAM.to_owned())];
    for role in default_roles() {
        let prompt = builtin_prompt(&role)
            .ok_or_else(|| format!("no built-in prompt for {role}"))?
            .to_owned();
        files.push((format!("agora/roles/{role}.md"), prompt));
    }
    files.push(("agora/rules/.gitkeep".to_owned(), String::new()));

    for (relative, contents) in &files {
        if write_scaffold(&repo, relative, contents, force)? {
            println!("created   {relative}");
        } else {
            println!("exists    {relative}");
        }
    }
    for entry in add_gitignore_entries(&repo, &[".tmuxor/", ".claude-worktrees/"])? {
        println!("gitignore {entry}");
    }

    println!("run: tmuxor {}", repo.display());
    Ok(())
}

fn default_roles() -> Vec<String> {
    DEFAULT_TEAM
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_whitespace().nth(1).map(str::to_owned))
        .collect()
}

fn write_scaffold(repo: &Path, relative: &str, contents: &str, force: bool) -> Result<bool> {
    let path = repo.join(relative);
    if path.exists() && !force {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, contents)?;
    Ok(true)
}

fn add_gitignore_entries(repo: &Path, entries: &[&str]) -> Result<Vec<String>> {
    let path = repo.join(".gitignore");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let present = existing
        .lines()
        .map(|line| line.trim().trim_end_matches('/'))
        .collect::<HashSet<_>>();
    let missing = entries
        .iter()
        .filter(|entry| !present.contains(entry.trim_end_matches('/')))
        .map(|entry| (*entry).to_owned())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(missing);
    }

    let mut contents = existing;
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    for entry in &missing {
        contents.push_str(entry);
        contents.push('\n');
    }
    fs::write(&path, contents)?;
    Ok(missing)
}

fn open_terminal_views(agents: &[Agent]) {
    let mut sessions = vec!["tmuxor-broker".to_owned()];
    sessions.extend(agents.iter().map(|agent| format!("tmuxor-{}", agent.role)));

    for session in sessions {
        if let Err(error) = Command::new("x-terminal-emulator")
            .args(terminal_view_arguments(&session))
            .spawn()
        {
            eprintln!(
                "tmuxor: could not open a terminal for {session}: {error}; attach manually with: tmux attach -t {session}"
            );
        }
    }
}

fn terminal_view_arguments(session: &str) -> Vec<String> {
    vec![
        "-e".to_owned(),
        "env".to_owned(),
        "-u".to_owned(),
        "TMUX".to_owned(),
        "tmux".to_owned(),
        "attach-session".to_owned(),
        "-t".to_owned(),
        session.to_owned(),
    ]
}

fn deliver_task(runtime: &Runtime, entry: &str, task: &str) -> Result<()> {
    thread::sleep(Duration::from_secs(4));
    let task_path = runtime.home.join("task.txt");
    fs::write(&task_path, format!("{RELAY_MARKER}OPERATOR]\n\n{task}"))?;
    let pane = read_panes(&runtime.home)?
        .remove(entry)
        .ok_or_else(|| format!("entry role '{entry}' has no pane"))?;
    tmux([
        "load-buffer",
        "-b",
        "tmuxor-task",
        &task_path.display().to_string(),
    ])?;
    tmux(["paste-buffer", "-p", "-b", "tmuxor-task", "-t", &pane])?;
    thread::sleep(Duration::from_millis(400));
    tmux(["send-keys", "-t", &pane, "Enter"])
}

fn run_broker() -> Result<()> {
    let home = PathBuf::from(
        env::var("TMUXOR_HOME").map_err(|_| "tmuxor broker must be launched by tmuxor")?,
    );
    let panes = read_panes(&home)?;
    let roles = panes.keys().cloned().collect::<Vec<_>>();
    let mut busy = HashSet::new();

    println!(
        "\x1b[1mtmuxor broker\x1b[0m \x1b[2m— nothing is relayed without your approval\x1b[0m"
    );
    println!("\x1b[2mteam: {}\x1b[0m", roles.join(" "));
    println!("\x1b[2mqueue: {}\x1b[0m\n", home.join("queue").display());

    loop {
        let Some(path) = next_queue_file(&home.join("queue"))? else {
            thread::sleep(Duration::from_millis(500));
            continue;
        };
        let message = read_queue_message(&path)?;
        busy.remove(&message.from);
        let body_path = temporary_path("tmuxor-msg");
        fs::write(&body_path, &message.text)?;
        let mut to = message.to;

        loop {
            display_message(&message.from, &to, &message.text, &panes, &busy);
            if panes.contains_key(&to) && busy.contains(&to) {
                println!(
                    "\x1b[33mwarning: {to} is mid-turn. Delivering now interleaves this with what it is already doing.\x1b[0m"
                );
            }
            print!(
                "\x1b[32m[s]\x1b[0mend  \x1b[33m[r]\x1b[0metarget  \x1b[33m[e]\x1b[0mdit  \x1b[33m[d]\x1b[0mrop  \x1b[2m[q]uit\x1b[0m > "
            );
            io::stdout().flush()?;
            wait_for_broker_client()?;
            match read_key()? {
                's' | 'S' => {
                    if is_terminal_target(&to) {
                        log(
                            &home,
                            &format!("{} {} -> {} (parked)", timestamp(), message.from, to),
                        )?;
                        println!("\n\x1b[33mchain ends here — nothing relayed\x1b[0m\n");
                        break;
                    }
                    if !panes.contains_key(&to) {
                        println!("\n\x1b[31mno valid recipient — pick one\x1b[0m");
                        to = pick_target(&roles, &busy)?;
                        continue;
                    }
                    deliver(&panes[&to], &body_path, &message.from)?;
                    busy.insert(to.clone());
                    log(
                        &home,
                        &format!(
                            "{} {} -> {} {} bytes",
                            timestamp(),
                            message.from,
                            to,
                            message.text.len()
                        ),
                    )?;
                    println!("\n\x1b[32msent to {to}\x1b[0m\n");
                    break;
                }
                'r' | 'R' => to = pick_target(&roles, &busy)?,
                'e' | 'E' => edit(&body_path)?,
                'd' | 'D' => {
                    log(
                        &home,
                        &format!(
                            "{} {} -> {} DROPPED",
                            timestamp(),
                            message.from,
                            if to.is_empty() { "?" } else { &to }
                        ),
                    )?;
                    println!("\n\x1b[33mdropped\x1b[0m\n");
                    break;
                }
                'q' | 'Q' => {
                    let _ = fs::remove_file(&body_path);
                    println!("\nbye");
                    return Ok(());
                }
                _ => println!(),
            }
        }
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(body_path);
    }
}

fn wait_for_broker_client() -> Result<()> {
    loop {
        let clients = tmux_output(["list-clients", "-t", "tmuxor-broker", "-F", "#{client_tty}"])?;
        if !clients.trim().is_empty() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn display_message(
    from: &str,
    to: &str,
    text: &str,
    panes: &HashMap<String, String>,
    busy: &HashSet<String>,
) {
    let label = if to.is_empty() {
        "\x1b[31mUNROUTED\x1b[0m".to_owned()
    } else if is_terminal_target(to) {
        format!("\x1b[33m{to}\x1b[0m \x1b[2m(ends here)\x1b[0m")
    } else if !panes.contains_key(to) {
        format!("\x1b[31m{to}? unknown\x1b[0m")
    } else if busy.contains(to) {
        format!("\x1b[1m{to}\x1b[0m \x1b[33m(busy)\x1b[0m")
    } else {
        format!("\x1b[1m{to}\x1b[0m")
    };
    println!("\x1b[36m────────────────────────────────────────────────────────\x1b[0m");
    println!(
        "\x1b[1m{from}\x1b[0m \x1b[2m→\x1b[0m {label}  \x1b[2m({} lines)\x1b[0m",
        text.lines().count()
    );
    println!("\x1b[36m────────────────────────────────────────────────────────\x1b[0m");
    print!("{text}");
    if !text.ends_with('\n') {
        println!();
    }
    println!("\x1b[36m────────────────────────────────────────────────────────\x1b[0m");
}

fn pick_target(roles: &[String], busy: &HashSet<String>) -> Result<String> {
    println!();
    for (index, role) in roles.iter().enumerate() {
        println!(
            "  {}) {}{}",
            index + 1,
            role,
            if busy.contains(role) { " (busy)" } else { "" }
        );
    }
    println!("  d) drop instead");
    print!("deliver to > ");
    io::stdout().flush()?;
    let mut choice = String::new();
    io::stdin().read_line(&mut choice)?;
    let choice = choice.trim();
    if choice.is_empty() || matches!(choice, "d" | "D") {
        return Ok(String::new());
    }
    if let Ok(index) = choice.parse::<usize>() {
        return Ok(roles
            .get(index.saturating_sub(1))
            .cloned()
            .unwrap_or_default());
    }
    Ok(roles
        .iter()
        .find(|role| role.as_str() == choice)
        .cloned()
        .unwrap_or_default())
}

fn read_key() -> Result<char> {
    let saved = Command::new("stty")
        .arg("-g")
        .stdin(Stdio::inherit())
        .output()?;
    if !saved.status.success() {
        return Err("could not read terminal settings".into());
    }
    let saved = String::from_utf8(saved.stdout)?.trim().to_owned();
    Command::new("stty")
        .args(["-icanon", "-echo", "min", "1", "time", "0"])
        .status()?;
    let mut byte = [0];
    let result = io::stdin().read_exact(&mut byte);
    let _ = Command::new("stty").arg(&saved).status();
    result?;
    Ok(byte[0] as char)
}

fn edit(path: &Path) -> Result<()> {
    let editor = env::var("EDITOR").unwrap_or_else(|_| "nano".to_owned());
    let status = Command::new(editor).arg(path).status()?;
    if !status.success() {
        return Err("editor exited unsuccessfully".into());
    }
    Ok(())
}

fn deliver(pane: &str, body: &Path, from: &str) -> Result<()> {
    let wrapped = temporary_path("tmuxor-send");
    fs::write(
        &wrapped,
        format!(
            "[FROM {}]\n\n{}",
            from.to_ascii_uppercase(),
            fs::read_to_string(body)?
        ),
    )?;
    let result = (|| {
        tmux([
            "load-buffer",
            "-b",
            "tmuxor",
            &wrapped.display().to_string(),
        ])?;
        tmux(["paste-buffer", "-p", "-b", "tmuxor", "-t", pane])?;
        thread::sleep(Duration::from_millis(400));
        tmux(["send-keys", "-t", pane, "Enter"])
    })();
    let _ = fs::remove_file(wrapped);
    result
}

fn relay_stop(require_relayed_input: bool) -> Result<()> {
    let home = match env::var("TMUXOR_HOME") {
        Ok(value) => PathBuf::from(value),
        Err(_) => return Ok(()),
    };
    let mut payload = String::new();
    io::stdin().read_to_string(&mut payload)?;
    if object_string_field(&payload, "agent_id")?.is_some_and(|agent_id| !agent_id.is_empty()) {
        return Ok(());
    }
    if object_string_field(&payload, "type")?.is_some_and(|kind| kind != "agent-turn-complete") {
        return Ok(());
    }
    if require_relayed_input
        && !object_string_array_field(&payload, "input-messages")?
            .iter()
            .any(|input| input.trim_start().starts_with(RELAY_MARKER))
    {
        return Ok(());
    }
    let message = match object_string_field(&payload, "last_assistant_message")? {
        Some(message) => Some(message),
        None => object_string_field(&payload, "last-assistant-message")?,
    };
    let Some(message) = message else {
        return Ok(());
    };
    if message.is_empty() {
        return Ok(());
    }
    let (to, text) = parse_routing(&message);
    let queue = home.join("queue");
    fs::create_dir_all(&queue)?;
    let record = format!(
        "{{\"from\":{},\"to\":{},\"text\":{}}}",
        json_string(&env::var("TMUXOR_ROLE").unwrap_or_else(|_| "unknown".to_owned())),
        json_string(&to),
        json_string(&text)
    );
    atomic_write(&queue.join(format!("{}.json", timestamp_nanos())), &record)
}

fn parse_routing(message: &str) -> (String, String) {
    let tags = message
        .lines()
        .enumerate()
        .filter_map(|(index, line)| route_target(line).map(|target| (index, target)))
        .collect::<Vec<_>>();
    if tags.len() != 1 {
        return (String::new(), message.to_owned());
    }
    let (tag_line, target) = &tags[0];
    let text = message
        .lines()
        .enumerate()
        .filter_map(|(index, line)| (index != *tag_line).then_some(line))
        .collect::<Vec<_>>()
        .join("\n");
    (
        target.to_ascii_lowercase(),
        text.trim_start_matches('\n').to_owned(),
    )
}

fn route_target(line: &str) -> Option<&str> {
    let line = line.trim();
    let line = line.strip_prefix("**").unwrap_or(line);
    let line = line.strip_suffix("**").unwrap_or(line).trim();
    let middle = line.strip_prefix('[')?.strip_suffix(']')?.trim();
    let target = middle
        .strip_prefix("TO")?
        .strip_prefix(':')
        .or_else(|| middle.strip_prefix("TO "))?
        .trim();
    (!target.is_empty()
        && target
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-')))
    .then_some(target)
}

fn read_queue_message(path: &Path) -> Result<QueueMessage> {
    let record = fs::read_to_string(path)?;
    Ok(QueueMessage {
        from: object_string_field(&record, "from")?.unwrap_or_default(),
        to: object_string_field(&record, "to")?.unwrap_or_default(),
        text: object_string_field(&record, "text")?.unwrap_or_default(),
    })
}

fn next_queue_file(queue: &Path) -> Result<Option<PathBuf>> {
    let mut files = fs::read_dir(queue)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    files.sort();
    Ok(files.into_iter().next())
}

fn read_panes(home: &Path) -> Result<HashMap<String, String>> {
    let mut panes = HashMap::new();
    for line in fs::read_to_string(home.join("panes.tsv"))?.lines() {
        let mut fields = line.split('\t');
        if let (Some(role), Some(pane)) = (fields.next(), fields.next()) {
            panes.insert(role.to_owned(), pane.to_owned());
        }
    }
    Ok(panes)
}

fn is_terminal_target(target: &str) -> bool {
    matches!(target, "done" | "human" | "operator" | "none")
}

fn log(home: &Path, text: &str) -> Result<()> {
    append_line(&home.join("relay.log"), &format!("{text}\n"))
}

fn append_line(path: &Path, text: &str) -> Result<()> {
    use std::fs::OpenOptions;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let temporary = path.with_extension(format!("{}.part", timestamp_nanos()));
    fs::write(&temporary, contents)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn temporary_path(prefix: &str) -> PathBuf {
    env::temp_dir().join(format!("{prefix}-{}", timestamp_nanos()))
}

fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", character as u32))
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

fn object_string_array_field(input: &str, wanted_key: &str) -> Result<Vec<String>> {
    let bytes = input.as_bytes();
    let mut index = skip_whitespace(bytes, 0);
    if bytes.get(index) != Some(&b'{') {
        return Err("expected a JSON object".into());
    }
    index += 1;
    loop {
        index = skip_whitespace(bytes, index);
        if bytes.get(index) == Some(&b'}') {
            return Ok(Vec::new());
        }
        let (key, next) = parse_json_string(input, index)?;
        index = skip_whitespace(bytes, next);
        if bytes.get(index) != Some(&b':') {
            return Err("expected ':' in JSON object".into());
        }
        index = skip_whitespace(bytes, index + 1);
        if key == wanted_key {
            if bytes.get(index) != Some(&b'[') {
                return Ok(Vec::new());
            }
            index = skip_whitespace(bytes, index + 1);
            let mut values = Vec::new();
            if bytes.get(index) == Some(&b']') {
                return Ok(values);
            }
            loop {
                let (value, next) = parse_json_string(input, index)?;
                values.push(value);
                index = skip_whitespace(bytes, next);
                match bytes.get(index) {
                    Some(b',') => index = skip_whitespace(bytes, index + 1),
                    Some(b']') => return Ok(values),
                    _ => return Err("expected ',' or ']' in JSON array".into()),
                }
            }
        }
        index = skip_json_value(input, index)?;
        index = skip_whitespace(bytes, index);
        match bytes.get(index) {
            Some(b',') => index += 1,
            Some(b'}') => return Ok(Vec::new()),
            _ => return Err("expected ',' or '}' in JSON object".into()),
        }
    }
}

fn object_string_field(input: &str, wanted_key: &str) -> Result<Option<String>> {
    let bytes = input.as_bytes();
    let mut index = skip_whitespace(bytes, 0);
    if bytes.get(index) != Some(&b'{') {
        return Err("expected a JSON object".into());
    }
    index += 1;
    loop {
        index = skip_whitespace(bytes, index);
        if bytes.get(index) == Some(&b'}') {
            return Ok(None);
        }
        let (key, next) = parse_json_string(input, index)?;
        index = skip_whitespace(bytes, next);
        if bytes.get(index) != Some(&b':') {
            return Err("expected ':' in JSON object".into());
        }
        index = skip_whitespace(bytes, index + 1);
        if key == wanted_key {
            if bytes.get(index) == Some(&b'n') {
                return Ok(None);
            }
            let (value, _) = parse_json_string(input, index)?;
            return Ok(Some(value));
        }
        index = skip_json_value(input, index)?;
        index = skip_whitespace(bytes, index);
        match bytes.get(index) {
            Some(b',') => index += 1,
            Some(b'}') => return Ok(None),
            _ => return Err("expected ',' or '}' in JSON object".into()),
        }
    }
}

fn parse_json_string(input: &str, start: usize) -> Result<(String, usize)> {
    let bytes = input.as_bytes();
    if bytes.get(start) != Some(&b'"') {
        return Err("expected a JSON string".into());
    }
    let mut output = String::new();
    let mut index = start + 1;
    while let Some(&byte) = bytes.get(index) {
        match byte {
            b'"' => return Ok((output, index + 1)),
            b'\\' => {
                index += 1;
                match bytes
                    .get(index)
                    .copied()
                    .ok_or("unterminated JSON escape")?
                {
                    b'"' => output.push('"'),
                    b'\\' => output.push('\\'),
                    b'/' => output.push('/'),
                    b'b' => output.push('\u{0008}'),
                    b'f' => output.push('\u{000c}'),
                    b'n' => output.push('\n'),
                    b'r' => output.push('\r'),
                    b't' => output.push('\t'),
                    b'u' => {
                        let hex = input
                            .get(index + 1..index + 5)
                            .ok_or("short unicode escape")?;
                        let scalar = u32::from_str_radix(hex, 16)?;
                        output.push(char::from_u32(scalar).ok_or("invalid unicode escape")?);
                        index += 4;
                    }
                    _ => return Err("invalid JSON escape".into()),
                }
                index += 1;
            }
            byte if byte < 0x20 => return Err("control character in JSON string".into()),
            _ => {
                let character = input[index..].chars().next().ok_or("invalid UTF-8")?;
                output.push(character);
                index += character.len_utf8();
            }
        }
    }
    Err("unterminated JSON string".into())
}

fn skip_json_value(input: &str, start: usize) -> Result<usize> {
    let bytes = input.as_bytes();
    let start = skip_whitespace(bytes, start);
    match bytes.get(start) {
        Some(b'"') => parse_json_string(input, start).map(|(_, index)| index),
        Some(b'{') => skip_json_compound(input, start, b'{', b'}'),
        Some(b'[') => skip_json_compound(input, start, b'[', b']'),
        Some(_) => Ok(input[start..]
            .find(|character: char| {
                matches!(character, ',' | '}' | ']') || character.is_whitespace()
            })
            .map(|offset| start + offset)
            .unwrap_or(input.len())),
        None => Err("missing JSON value".into()),
    }
}

fn skip_json_compound(input: &str, start: usize, opening: u8, closing: u8) -> Result<usize> {
    let bytes = input.as_bytes();
    let mut index = start;
    let mut depth = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => index = parse_json_string(input, index)?.1,
            byte if byte == opening => {
                depth += 1;
                index += 1;
            }
            byte if byte == closing => {
                depth -= 1;
                index += 1;
                if depth == 0 {
                    return Ok(index);
                }
            }
            _ => index += 1,
        }
    }
    Err("unterminated JSON compound value".into())
}

fn skip_whitespace(bytes: &[u8], mut index: usize) -> usize {
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    index
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\\"'\\\"'"))
}

fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn tmux<I, S>(arguments: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new("tmux").args(arguments).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_owned()
            .into())
    }
}

fn tmux_output<I, S>(arguments: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new("tmux").args(arguments).output()?;
    if output.status.success() {
        Ok(String::from_utf8(output.stdout)?)
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_owned()
            .into())
    }
}

fn git<I, S>(repo: &Path, arguments: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(arguments)
        .stderr(Stdio::piped())
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_owned()
            .into())
    }
}

fn git_output<I, S>(repo: &Path, arguments: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(arguments)
        .output()?;
    if output.status.success() {
        Ok(String::from_utf8(output.stdout)?)
    } else {
        Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_owned()
            .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_routing_tag_and_removes_it() {
        let (to, text) =
            parse_routing("I finished the change.\n\n**[TO: Tester]**\n\nPlease verify it.");
        assert_eq!(to, "tester");
        assert_eq!(text, "I finished the change.\n\n\nPlease verify it.");
    }

    #[test]
    fn leaves_ambiguous_or_missing_routing_untouched() {
        let ambiguous = "[TO: builder]\n[TO: tester]";
        assert_eq!(
            parse_routing(ambiguous),
            (String::new(), ambiguous.to_owned())
        );
        let missing = "No routing line.";
        assert_eq!(parse_routing(missing), (String::new(), missing.to_owned()));
    }

    #[test]
    fn opens_terminal_views_unless_disabled() {
        let default_options = parse_launch_options(vec!["repo".to_owned()]).unwrap();
        assert!(default_options.open_views);

        let headless_options =
            parse_launch_options(vec!["repo".to_owned(), "--no-view".to_owned()]).unwrap();
        assert!(!headless_options.open_views);
    }

    #[test]
    fn uses_xterm_compatible_arguments_for_terminal_views() {
        assert_eq!(
            terminal_view_arguments("tmuxor-builder"),
            vec![
                "-e",
                "env",
                "-u",
                "TMUX",
                "tmux",
                "attach-session",
                "-t",
                "tmuxor-builder",
            ]
        );
    }

    #[test]
    fn reads_escaped_json_fields_without_matching_message_contents() {
        let payload = r#"{"last_assistant_message":"mentions \"agent_id\"", "agent_id":null}"#;
        assert_eq!(object_string_field(payload, "agent_id").unwrap(), None);
        assert_eq!(
            object_string_field(payload, "last_assistant_message").unwrap(),
            Some("mentions \"agent_id\"".to_owned())
        );
    }

    #[test]
    fn renders_json_strings_that_round_trip() {
        let value = "a quote: \"\nand a tab:\t";
        let record = format!("{{\"value\":{}}}", json_string(value));
        assert_eq!(
            object_string_field(&record, "value").unwrap(),
            Some(value.to_owned())
        );
    }

    #[test]
    fn writes_a_valid_multiline_claude_launcher() {
        let root = temporary_path("tmuxor-test");
        let runtime = Runtime {
            repo: root.join("repo"),
            home: root.join("home"),
            worktrees: root.join("worktrees"),
            executable: PathBuf::from("/tmp/tmuxor"),
        };
        fs::create_dir_all(&runtime.home).unwrap();
        let agents = vec![
            Agent {
                adapter: Adapter::Claude,
                role: "builder".to_owned(),
                prompt: "Build things.".to_owned(),
            },
            Agent {
                adapter: Adapter::Claude,
                role: "tester".to_owned(),
                prompt: "Test things.".to_owned(),
            },
        ];
        let rules = vec![
            Rule {
                name: "20-testing.md".to_owned(),
                contents: "Always add a regression test.".to_owned(),
            },
            Rule {
                name: "00-project.md".to_owned(),
                contents: "Keep changes focused.".to_owned(),
            },
        ];
        generate_agent_files(&runtime, &agents, &rules, "auto").unwrap();
        let launcher = fs::read_to_string(runtime.home.join("launch-builder.sh")).unwrap();
        assert!(!launcher.contains("\n+"));
        assert!(launcher.contains("exec claude \\\n  --settings"));
        assert!(launcher.contains("\n  --add-dir "));
        let prompt = fs::read_to_string(runtime.home.join("prompt-builder.txt")).unwrap();
        assert!(prompt.contains("# Project rules"));
        assert!(prompt.contains("Keep changes focused."));
        assert!(prompt.contains("Always add a regression test."));
        assert!(prompt.contains("Build things."));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn loads_markdown_rules_in_filename_order() {
        let root = temporary_path("tmuxor-test");
        let rules = root.join("rules");
        fs::create_dir_all(&rules).unwrap();
        fs::write(rules.join("20-testing.md"), "Test changes.").unwrap();
        fs::write(rules.join("00-project.md"), "Keep scope small.").unwrap();
        fs::write(rules.join("notes.txt"), "Ignored.").unwrap();

        let loaded = load_rules(&root, None).unwrap();

        assert_eq!(
            loaded
                .iter()
                .map(|rule| rule.name.as_str())
                .collect::<Vec<_>>(),
            vec!["00-project.md", "20-testing.md"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prefers_agora_rules_over_root_rules() {
        let root = temporary_path("tmuxor-test");
        let root_rules = root.join("rules");
        let agora_rules = root.join("agora/rules");
        fs::create_dir_all(&root_rules).unwrap();
        fs::create_dir_all(&agora_rules).unwrap();
        fs::write(root_rules.join("00-root.md"), "Root rule.").unwrap();
        fs::write(agora_rules.join("00-agora.md"), "Agora rule.").unwrap();

        let loaded = load_rules(&root, None).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].contents, "Agora rule.");
        fs::remove_dir_all(root).unwrap();
    }
}
