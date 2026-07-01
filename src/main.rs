use clap::{Parser, Subcommand, ValueEnum};
use partial_compact_codex::prompts;
use partial_compact_codex::proxy::{self, ProxyConfig, ProxyToolConfig};
use partial_compact_codex::storage::{CompactionInput, Error, Role, Store};
use partial_compact_codex::tool_endpoint;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Parser)]
#[command(name = "pcodx")]
#[command(about = "Codex, but with partial compaction.")]
#[command(
    long_about = "pcodx is the partial-compaction wrapper for Codex. The target shape is Codex TUI on the front, Codex app-server/model path on the back, and this wrapper in the middle. The current prototype records exact turns, appends minimal turn ids in rendered context, and replaces only compacted ranges with the agent's summary."
)]
struct Cli {
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "SQLite database path. Defaults to $PCODX_DB, then $XDG_DATA_HOME/pcodx/pcodx.sqlite3, then ~/.local/share/pcodx/pcodx.sqlite3."
    )]
    db: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        value_name = "SESSION",
        help = "pcodx wrapper session id to create, append to, inspect, compact, or resume."
    )]
    session: Option<String>,
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        help = "Working directory to store on newly created pcodx sessions."
    )]
    cwd: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create or refresh a pcodx wrapper session.
    Init,
    /// Append one exact completed turn to the wrapper state.
    Record {
        #[arg(long, value_enum, help = "Role for this completed turn.")]
        role: CliRole,
        #[arg(
            long,
            help = "Exact message text supplied as one shell argument. Use quotes when it contains spaces."
        )]
        text: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Read exact message text from PATH, or from stdin when PATH is `-`."
        )]
        text_file: Option<PathBuf>,
        #[arg(long, help = "Optional note about where this message came from.")]
        source: Option<String>,
    },
    /// Record one exact human prompt, then print the current future Codex context.
    Turn {
        #[arg(
            long,
            help = "Exact human prompt supplied as one shell argument. Use quotes when it contains spaces."
        )]
        text: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Read the exact human prompt from PATH, or from stdin when PATH is `-`."
        )]
        text_file: Option<PathBuf>,
    },
    /// Reopen an existing pcodx wrapper session and print its current future Codex context.
    Resume {
        #[arg(long, help = "Resume the most recently updated pcodx wrapper session.")]
        last: bool,
        #[arg(
            long,
            help = "Optional exact human prompt to append before rendering the resumed context."
        )]
        text: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Read the optional resume prompt from PATH, or from stdin when PATH is `-`."
        )]
        text_file: Option<PathBuf>,
    },
    /// Open a Codex-like line interface backed by the pcodx partial-compaction store.
    Interactive {
        #[arg(
            long,
            help = "Optional exact human prompt to append before reading interactive input."
        )]
        text: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Read the optional initial prompt from PATH. `-` is rejected because stdin is used for interactive input."
        )]
        text_file: Option<PathBuf>,
    },
    /// Print visible message and compaction ids usable as compaction range endpoints.
    Ids,
    /// Print the shared current-session message-id helper text for agents.
    CurrentSessionMessageIds,
    /// Print the current future Codex context for the session.
    Show,
    /// Replace a visible message/compaction range with one summary in future renders.
    Compact {
        #[arg(
            long,
            help = "First visible endpoint to replace, such as `msg1` or `cmp1`."
        )]
        from: String,
        #[arg(
            long,
            help = "Last visible endpoint to replace, such as `msg4` or `cmp2`."
        )]
        to: String,
        #[arg(
            long,
            help = "Replacement text that will stand in for the selected range in future context renders."
        )]
        summary: String,
    },
    /// Atomically replace multiple disjoint visible ranges with agent summaries.
    CompactMany {
        #[arg(
            long = "range",
            value_name = "FROM..TO=SUMMARY",
            required = true,
            help = "One range replacement. Repeat for disjoint ranges, for example `msg1..msg2=old setup`."
        )]
        ranges: Vec<String>,
    },
    /// List or print shared partial-compaction prompt fragments.
    Prompts {
        /// Prompt fragment name to print. Omit it to list names.
        name: Option<String>,
    },
    /// Run a transparent proxy between Codex TUI and Codex app-server.
    Serve {
        #[arg(
            long,
            value_name = "URL",
            default_value = "ws://127.0.0.1:48570",
            help = "Endpoint that real Codex frontend connects to with `codex --remote URL`."
        )]
        listen: String,
        #[arg(
            long,
            value_name = "URL",
            default_value = "ws://127.0.0.1:48571",
            help = "Endpoint used for the real upstream `codex app-server`."
        )]
        upstream: String,
        #[arg(
            long,
            value_name = "BIN",
            default_value = "codex",
            help = "Codex binary used to launch the upstream app-server."
        )]
        codex_bin: String,
        #[arg(
            long,
            help = "Do not launch upstream Codex app-server; connect to an already-running upstream endpoint."
        )]
        no_launch_upstream: bool,
        #[arg(
            long,
            help = "Inject PCODX dynamic tools into thread/start, thread/resume, and thread/fork. Fixture capture still works without this flag."
        )]
        enable_pcodx_tools: bool,
    },
}

#[derive(Clone, ValueEnum)]
enum CliRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

impl From<CliRole> for Role {
    fn from(role: CliRole) -> Self {
        match role {
            CliRole::System => Self::System,
            CliRole::Developer => Self::Developer,
            CliRole::User => Self::User,
            CliRole::Assistant => Self::Assistant,
            CliRole::Tool => Self::Tool,
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> partial_compact_codex::storage::Result<()> {
    run_cli(Cli::parse())
}

fn run_cli(cli: Cli) -> partial_compact_codex::storage::Result<()> {
    let db_path = cli.db.unwrap_or_else(Store::default_path);
    let cwd = cli.cwd.unwrap_or(std::env::current_dir()?);
    let command = match cli.command {
        Command::Serve {
            listen,
            upstream,
            codex_bin,
            no_launch_upstream,
            enable_pcodx_tools,
        } => {
            let tools = if enable_pcodx_tools {
                let mut store = Store::open(&db_path)?;
                let session = session_or_create(&mut store, cli.session.as_deref(), &cwd)?;
                Some(ProxyToolConfig {
                    db_path,
                    session_id: session,
                    enable_dynamic_tools: enable_pcodx_tools,
                })
            } else {
                None
            };
            return proxy::serve(ProxyConfig {
                listen,
                upstream,
                codex_bin,
                launch_upstream: !no_launch_upstream,
                tools,
            });
        }
        command => command,
    };
    let mut store = Store::open(&db_path)?;
    match command {
        Command::Init => {
            let session = store.create_session(cli.session.as_deref(), &cwd)?;
            println!("session_id={session}");
            println!("db_path={}", db_path.display());
        }
        Command::Record {
            role,
            text,
            text_file,
            source,
        } => {
            let session = session_or_create(&mut store, cli.session.as_deref(), &cwd)?;
            let text = read_text_arg(text, text_file, "record text")?;
            let message = store.record_message(&session, role.into(), &text, source.as_deref())?;
            println!("session_id={session}");
            println!("message_id={}", message.id);
            println!("visible_ids={}", store.visible_ids(&session)?.join(","));
        }
        Command::Turn { text, text_file } => {
            let session = session_or_create(&mut store, cli.session.as_deref(), &cwd)?;
            let text = read_text_arg(text, text_file, "turn prompt")?;
            let message = store.record_message(&session, Role::User, &text, Some("cli-turn"))?;
            eprintln!("session_id={session}");
            eprintln!("prompt_message_id={}", message.id);
            eprintln!("future_context_source=pcodx-render");
            println!("{}", store.render_visible_context(&session)?);
        }
        Command::Resume {
            last,
            text,
            text_file,
        } => {
            let session = if last {
                store.last_session_id()?.ok_or_else(|| {
                    partial_compact_codex::storage::Error::Invalid("no prior session".to_owned())
                })?
            } else {
                session_or_existing(&store, cli.session.as_deref())?
            };
            if text.is_some() || text_file.is_some() {
                let text = read_text_arg(text, text_file, "resume prompt")?;
                let message =
                    store.record_message(&session, Role::User, &text, Some("cli-resume"))?;
                eprintln!("prompt_message_id={}", message.id);
            }
            eprintln!("session_id={session}");
            eprintln!("visible_ids={}", store.visible_ids(&session)?.join(","));
            println!("{}", store.render_visible_context(&session)?);
        }
        Command::Interactive { text, text_file } => {
            let initial_text = if text.is_some() || text_file.is_some() {
                if text_file
                    .as_ref()
                    .is_some_and(|path| path.as_os_str() == "-")
                {
                    return Err(partial_compact_codex::storage::Error::Invalid(
                        "interactive initial prompt cannot use --text-file - because stdin is used for interactive input".to_owned(),
                    ));
                }
                Some(read_text_arg(
                    text,
                    text_file,
                    "interactive initial prompt",
                )?)
            } else {
                None
            };
            let session = session_or_create(&mut store, cli.session.as_deref(), &cwd)?;
            if let Some(text) = initial_text {
                let message =
                    store.record_message(&session, Role::User, &text, Some("cli-interactive"))?;
                println!("prompt_message_id={}", message.id);
            }
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            run_interactive(
                &mut store,
                &session,
                &cwd,
                stdin.lock(),
                stdout.lock(),
                std::io::stdout().is_terminal(),
            )?;
        }
        Command::Ids => {
            let session = session_or_existing(&store, cli.session.as_deref())?;
            println!("session_id={session}");
            println!("{}", store.visible_ids(&session)?.join("\n"));
        }
        Command::CurrentSessionMessageIds => {
            let session = session_or_existing(&store, cli.session.as_deref())?;
            print!(
                "{}",
                tool_endpoint::current_session_message_ids_tool(&store, &session)
            );
        }
        Command::Show => {
            let session = session_or_existing(&store, cli.session.as_deref())?;
            println!("{}", store.render_visible_context(&session)?);
        }
        Command::Compact { from, to, summary } => {
            let session = session_or_existing(&store, cli.session.as_deref())?;
            let compaction = store.compact(&session, &from, &to, &summary)?;
            print_compaction_result(&store, &session, compaction, &mut std::io::stdout())?;
        }
        Command::CompactMany { ranges } => {
            let session = session_or_existing(&store, cli.session.as_deref())?;
            let inputs = ranges
                .into_iter()
                .map(parse_compact_many_range)
                .collect::<partial_compact_codex::storage::Result<Vec<_>>>()?;
            let compactions = store.compact_ranges(&session, inputs)?;
            println!("session_id={session}");
            println!("n_ranges_compacted={}", compactions.len());
            let n_messages_replaced: i64 = compactions
                .iter()
                .map(|compaction| compaction.n_messages_replaced)
                .sum();
            println!("n_messages_replaced={n_messages_replaced}");
            for compaction in &compactions {
                println!(
                    "compaction={} {}..{}",
                    compaction.id, compaction.from_msg_id, compaction.to_msg_id
                );
                if let Some(warning) = &compaction.warning {
                    println!("warning[{}]={warning}", compaction.id);
                }
            }
            println!("visible_ids={}", store.visible_ids(&session)?.join(","));
        }
        Command::Prompts { name } => {
            if let Some(name) = name {
                let text = prompts::get(&name).ok_or_else(|| {
                    partial_compact_codex::storage::Error::Invalid(format!(
                        "unknown prompt `{name}`"
                    ))
                })?;
                print!("{text}");
            } else {
                for prompt in prompts::PROMPTS {
                    println!("{}", prompt.name);
                }
            }
        }
        Command::Serve { .. } => unreachable!("serve returns before opening storage"),
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
enum InteractiveAction {
    Help,
    Exit,
    Ids,
    Show,
    CurrentSessionMessageIds,
    Record {
        role: Role,
        text: String,
    },
    RecordFile {
        role: Role,
        path: PathBuf,
    },
    Compact {
        from: String,
        to: String,
        summary: String,
    },
    Turn {
        text: String,
    },
}

fn run_interactive<R, W>(
    store: &mut Store,
    session: &str,
    cwd: &std::path::Path,
    input: R,
    mut output: W,
    is_terminal: bool,
) -> partial_compact_codex::storage::Result<()>
where
    R: BufRead,
    W: Write,
{
    writeln!(output, "pcodx interactive")?;
    writeln!(output, "session_id={session}")?;
    writeln!(
        output,
        "commands: /ids /show /current-session-message-ids /record /record-file /compact /exit"
    )?;
    if is_terminal {
        write!(output, "pcodx> ")?;
        output.flush()?;
    }
    for line in input.lines() {
        let line = line?;
        let action = match parse_interactive_line(&line) {
            Ok(action) => action,
            Err(error) => {
                writeln!(output, "error: {error}")?;
                if is_terminal {
                    write!(output, "pcodx> ")?;
                    output.flush()?;
                }
                continue;
            }
        };
        match handle_interactive_action(store, session, cwd, action, &mut output) {
            Ok(true) => break,
            Ok(false) => {}
            Err(Error::Invalid(message)) => writeln!(output, "error: {message}")?,
            Err(error) => return Err(error),
        }
        if is_terminal {
            write!(output, "pcodx> ")?;
            output.flush()?;
        }
    }
    Ok(())
}

fn parse_interactive_line(line: &str) -> partial_compact_codex::storage::Result<InteractiveAction> {
    if line.is_empty() {
        return Ok(InteractiveAction::Help);
    }
    if !line.starts_with('/') {
        return Ok(InteractiveAction::Turn {
            text: line.to_owned(),
        });
    }
    let (command, rest) = split_first_token_preserve_rest(&line[1..]);
    match command {
        "help" => Ok(InteractiveAction::Help),
        "exit" | "quit" => Ok(InteractiveAction::Exit),
        "ids" => Ok(InteractiveAction::Ids),
        "show" => Ok(InteractiveAction::Show),
        "current-session-message-ids" | "message-ids" => {
            Ok(InteractiveAction::CurrentSessionMessageIds)
        }
        "record" => parse_interactive_record(rest),
        "record-file" => parse_interactive_record_file(rest),
        "compact" => parse_interactive_compact(rest),
        "turn" => {
            if rest.is_empty() {
                Err(partial_compact_codex::storage::Error::Invalid(
                    "usage: /turn <prompt>".to_owned(),
                ))
            } else {
                Ok(InteractiveAction::Turn {
                    text: rest.to_owned(),
                })
            }
        }
        "" => Ok(InteractiveAction::Help),
        other => Err(partial_compact_codex::storage::Error::Invalid(format!(
            "unknown command /{other}; type /help"
        ))),
    }
}

fn parse_interactive_record(
    rest: &str,
) -> partial_compact_codex::storage::Result<InteractiveAction> {
    let (role, text) = split_first_token_preserve_rest(rest);
    if role.is_empty() || text.is_empty() {
        return Err(partial_compact_codex::storage::Error::Invalid(
            "usage: /record <system|developer|user|assistant|tool> <text>".to_owned(),
        ));
    }
    Ok(InteractiveAction::Record {
        role: Role::from_str(role)?,
        text: text.to_owned(),
    })
}

fn parse_interactive_record_file(
    rest: &str,
) -> partial_compact_codex::storage::Result<InteractiveAction> {
    let (role, path) = split_first_token_preserve_rest(rest);
    if role.is_empty() || path.is_empty() {
        return Err(partial_compact_codex::storage::Error::Invalid(
            "usage: /record-file <system|developer|user|assistant|tool> <path>".to_owned(),
        ));
    }
    Ok(InteractiveAction::RecordFile {
        role: Role::from_str(role)?,
        path: PathBuf::from(path),
    })
}

fn parse_interactive_compact(
    rest: &str,
) -> partial_compact_codex::storage::Result<InteractiveAction> {
    let (range, summary) = split_first_token_preserve_rest(rest);
    if range.is_empty() || summary.is_empty() {
        return Err(partial_compact_codex::storage::Error::Invalid(
            "usage: /compact <from_msg_or_cmp>..<to_msg_or_cmp> <summary>".to_owned(),
        ));
    }
    let (from, to) = parse_range_bounds(range, "/compact range")?;
    Ok(InteractiveAction::Compact {
        from,
        to,
        summary: summary.to_owned(),
    })
}

fn handle_interactive_action<W: Write>(
    store: &mut Store,
    session: &str,
    cwd: &std::path::Path,
    action: InteractiveAction,
    output: &mut W,
) -> partial_compact_codex::storage::Result<bool> {
    match action {
        InteractiveAction::Help => {
            writeln!(output, "usage: plain text records a user turn")?;
            writeln!(
                output,
                "/record <system|developer|user|assistant|tool> <text>"
            )?;
            writeln!(
                output,
                "/record-file <system|developer|user|assistant|tool> <path>"
            )?;
            writeln!(output, "/compact <from>..<to> <summary>")?;
            writeln!(output, "/ids")?;
            writeln!(output, "/show")?;
            writeln!(output, "/current-session-message-ids")?;
            writeln!(output, "/exit")?;
        }
        InteractiveAction::Exit => {
            writeln!(output, "bye")?;
            return Ok(true);
        }
        InteractiveAction::Ids => {
            writeln!(output, "session_id={session}")?;
            for id in store.visible_ids(session)? {
                writeln!(output, "{id}")?;
            }
        }
        InteractiveAction::Show => {
            writeln!(output, "{}", store.render_visible_context(session)?)?;
        }
        InteractiveAction::CurrentSessionMessageIds => {
            write!(
                output,
                "{}",
                tool_endpoint::current_session_message_ids_tool(store, session)
            )?;
        }
        InteractiveAction::Record { role, text } => {
            let message = store.record_message(session, role, &text, Some("cli-interactive"))?;
            writeln!(output, "message_id={}", message.id)?;
            writeln!(
                output,
                "visible_ids={}",
                store.visible_ids(session)?.join(",")
            )?;
        }
        InteractiveAction::RecordFile { role, path } => {
            let path = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            let text = std::fs::read_to_string(&path)?;
            let message = store.record_message(
                session,
                role,
                &text,
                Some(&format!("cli-interactive-file:{}", path.display())),
            )?;
            writeln!(output, "message_id={}", message.id)?;
            writeln!(output, "text_file={}", path.display())?;
            writeln!(
                output,
                "visible_ids={}",
                store.visible_ids(session)?.join(",")
            )?;
        }
        InteractiveAction::Compact { from, to, summary } => {
            let compaction = store.compact(session, &from, &to, &summary)?;
            print_compaction_result(store, session, compaction, output)?;
        }
        InteractiveAction::Turn { text } => {
            let message =
                store.record_message(session, Role::User, &text, Some("cli-interactive"))?;
            writeln!(output, "prompt_message_id={}", message.id)?;
            writeln!(output, "future_context_source=pcodx-render")?;
            writeln!(output, "{}", store.render_visible_context(session)?)?;
        }
    }
    Ok(false)
}

fn print_compaction_result<W: Write>(
    store: &Store,
    session: &str,
    compaction: partial_compact_codex::storage::Compaction,
    output: &mut W,
) -> partial_compact_codex::storage::Result<()> {
    writeln!(output, "session_id={session}")?;
    writeln!(output, "compaction_id={}", compaction.id)?;
    writeln!(
        output,
        "n_messages_replaced={}",
        compaction.n_messages_replaced
    )?;
    if let Some(warning) = compaction.warning {
        writeln!(output, "warning={warning}")?;
    }
    writeln!(
        output,
        "visible_ids={}",
        store.visible_ids(session)?.join(",")
    )?;
    Ok(())
}

fn read_text_arg(
    text: Option<String>,
    text_file: Option<PathBuf>,
    label: &str,
) -> partial_compact_codex::storage::Result<String> {
    let value = match (text, text_file) {
        (Some(_), Some(_)) => {
            return Err(partial_compact_codex::storage::Error::Invalid(format!(
                "pass either --text or --text-file for {label}, not both"
            )))
        }
        (Some(text), None) => text,
        (None, Some(path)) if path.as_os_str() == "-" => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            text
        }
        (None, Some(path)) => std::fs::read_to_string(path)?,
        (None, None) => {
            return Err(partial_compact_codex::storage::Error::Invalid(format!(
                "missing --text or --text-file for {label}"
            )))
        }
    };
    if value.is_empty() {
        return Err(partial_compact_codex::storage::Error::Invalid(format!(
            "{label} must be non-empty"
        )));
    }
    Ok(value)
}

fn parse_compact_many_range(
    value: String,
) -> partial_compact_codex::storage::Result<CompactionInput> {
    let (bounds, summary) = value.split_once('=').ok_or_else(|| {
        partial_compact_codex::storage::Error::Invalid(
            "compact-many range must be FROM..TO=SUMMARY".to_owned(),
        )
    })?;
    let (from_msg_id, to_msg_id) = parse_range_bounds(bounds, "compact-many range")?;
    if summary.is_empty() {
        return Err(partial_compact_codex::storage::Error::Invalid(
            "compact-many range must include FROM, TO, and SUMMARY".to_owned(),
        ));
    }
    Ok(CompactionInput {
        from_msg_id,
        to_msg_id,
        summary: summary.to_owned(),
    })
}

fn parse_range_bounds(
    value: &str,
    label: &str,
) -> partial_compact_codex::storage::Result<(String, String)> {
    let (from_msg_id, to_msg_id) = value.split_once("..").ok_or_else(|| {
        partial_compact_codex::storage::Error::Invalid(format!("{label} must be FROM..TO"))
    })?;
    if from_msg_id.is_empty() || to_msg_id.is_empty() {
        return Err(partial_compact_codex::storage::Error::Invalid(format!(
            "{label} must include FROM and TO"
        )));
    }
    Ok((from_msg_id.to_owned(), to_msg_id.to_owned()))
}

fn split_first_token_preserve_rest(value: &str) -> (&str, &str) {
    let value = value.trim_start();
    let Some(idx) = value.find(char::is_whitespace) else {
        return (value, "");
    };
    let rest_idx = idx
        + value[idx..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or_default();
    (&value[..idx], &value[rest_idx..])
}

fn session_or_create(
    store: &mut Store,
    requested: Option<&str>,
    cwd: &std::path::Path,
) -> partial_compact_codex::storage::Result<String> {
    match requested {
        Some(session) => store.create_session(Some(session), cwd),
        None => match store.last_session_id()? {
            Some(session) => Ok(session),
            None => store.create_session(None, cwd),
        },
    }
}

fn session_or_existing(
    store: &Store,
    requested: Option<&str>,
) -> partial_compact_codex::storage::Result<String> {
    match requested {
        Some(session) if store.session_exists(session)? => Ok(session.to_owned()),
        Some(session) => Err(partial_compact_codex::storage::Error::Invalid(format!(
            "unknown session `{session}`"
        ))),
        None => store.last_session_id()?.ok_or_else(|| {
            partial_compact_codex::storage::Error::Invalid(
                "no session; run `pcodx init` first".to_owned(),
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_interactive_line, parse_range_bounds, run_cli, run_interactive, Cli,
        InteractiveAction, Role, Store,
    };
    use clap::Parser;
    use std::io::Cursor;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn interactive_parser_accepts_codex_like_commands() {
        assert_eq!(
            parse_interactive_line("/compact msg1..cmp2 faithful summary").unwrap(),
            InteractiveAction::Compact {
                from: "msg1".to_owned(),
                to: "cmp2".to_owned(),
                summary: "faithful summary".to_owned(),
            }
        );
        assert_eq!(
            parse_interactive_line("/record assistant exact assistant text").unwrap(),
            InteractiveAction::Record {
                role: Role::Assistant,
                text: "exact assistant text".to_owned(),
            }
        );
        assert_eq!(
            parse_interactive_line("/record-file assistant src/storage.rs").unwrap(),
            InteractiveAction::RecordFile {
                role: Role::Assistant,
                path: PathBuf::from("src/storage.rs"),
            }
        );
        assert_eq!(
            parse_interactive_line("continue this task").unwrap(),
            InteractiveAction::Turn {
                text: "continue this task".to_owned(),
            }
        );
        assert_eq!(
            parse_interactive_line("  exact user text  ").unwrap(),
            InteractiveAction::Turn {
                text: "  exact user text  ".to_owned(),
            }
        );
        assert_eq!(
            parse_interactive_line("/record assistant  exact assistant text  ").unwrap(),
            InteractiveAction::Record {
                role: Role::Assistant,
                text: " exact assistant text  ".to_owned(),
            }
        );
        assert_eq!(
            parse_interactive_line("/compact msg1..msg1  faithful summary  ").unwrap(),
            InteractiveAction::Compact {
                from: "msg1".to_owned(),
                to: "msg1".to_owned(),
                summary: " faithful summary  ".to_owned(),
            }
        );
    }

    #[test]
    fn interactive_parser_rejects_incomplete_compaction_before_storage() {
        let error = parse_interactive_line("/compact msg1..msg2").unwrap_err();
        assert!(error.to_string().contains("usage: /compact"));
        let error = parse_range_bounds("msg1", "/compact range").unwrap_err();
        assert!(error.to_string().contains("FROM..TO"));
    }

    #[test]
    fn interactive_loop_compacts_visible_range_without_rewriting_history() {
        let temp = tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("pcodx.sqlite3")).unwrap();
        let session = store
            .create_session(Some("ses-interactive"), temp.path())
            .unwrap();
        let script = Cursor::new(
            "/record assistant stale discovery with exact words  \n\
             /record assistant durable current result  \n\
             /compact msg999..msg999 missing endpoint summary\n\
             /compact msg1..msg1  stale discovery summary  \n\
             /ids\n\
             /show\n\
             /exit\n",
        );
        let mut output = Vec::new();

        run_interactive(
            &mut store,
            &session,
            temp.path(),
            script,
            &mut output,
            false,
        )
        .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("error: endpoint `msg999` is not visible"));
        assert!(output.contains("compaction_id=cmp1"));
        assert!(output.contains("visible_ids=cmp1,msg2"));
        assert!(output.contains(" stale discovery summary  \n<aboveturn id=\"cmp1\"/>"));
        assert!(output.contains("durable current result  \n<aboveturn id=\"msg2\"/>"));
        assert!(!output.contains("stale discovery with exact words\n<aboveturn id=\"msg1\"/>"));
        let messages = store.messages(&session).unwrap();
        assert_eq!(messages[0].text, "stale discovery with exact words  ");
    }

    #[test]
    fn interactive_record_file_reads_from_cwd_and_compacts_rendered_context() {
        let temp = tempdir().unwrap();
        let file_path = temp.path().join("kept.txt");
        std::fs::write(&file_path, "file detail alpha\nfile detail beta\n").unwrap();
        let db_path = temp.path().join("pcodx.sqlite3");
        let mut store = Store::open(&db_path).unwrap();
        let session = store
            .create_session(Some("ses-interactive-file"), temp.path())
            .unwrap();
        let script = Cursor::new(
            "/record assistant stale exact text\n\
             /record-file assistant kept.txt\n\
             /compact msg1..msg1 stale summary\n\
             /show\n\
             /exit\n",
        );
        let mut output = Vec::new();

        run_interactive(
            &mut store,
            &session,
            temp.path(),
            script,
            &mut output,
            false,
        )
        .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("text_file="));
        assert!(output.contains("kept.txt"));
        assert!(output.contains("stale summary\n<aboveturn id=\"cmp1\"/>"));
        assert!(output.contains("file detail alpha\nfile detail beta\n\n<aboveturn id=\"msg2\"/>"));
        assert!(!output.contains("stale exact text\n<aboveturn id=\"msg1\"/>"));
        let messages = store.messages(&session).unwrap();
        assert_eq!(messages[1].text, "file detail alpha\nfile detail beta\n");

        let resumed = Store::open(&db_path)
            .unwrap()
            .render_visible_context(&session)
            .unwrap();
        assert!(resumed.contains("file detail alpha\nfile detail beta\n\n<aboveturn id=\"msg2\"/>"));
        assert!(!resumed.contains("stale exact text\n<aboveturn id=\"msg1\"/>"));
    }

    #[test]
    fn interactive_text_file_stdin_rejects_before_session_creation() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("pcodx.sqlite3");
        let error = Cli::try_parse_from([
            "pcodx",
            "--db",
            db_path.to_str().unwrap(),
            "--session",
            "ses-invalid",
            "interactive",
            "--text-file",
            "-",
        ])
        .map_err(|error| partial_compact_codex::storage::Error::Invalid(error.to_string()))
        .and_then(run_cli)
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("stdin is used for interactive input"));
        let store = Store::open(&db_path).unwrap();
        assert!(!store.session_exists("ses-invalid").unwrap());
    }

    #[test]
    fn interactive_initial_prompt_conflict_rejects_before_session_creation() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("pcodx.sqlite3");
        let error = Cli::try_parse_from([
            "pcodx",
            "--db",
            db_path.to_str().unwrap(),
            "--session",
            "ses-conflict",
            "interactive",
            "--text",
            "prompt",
            "--text-file",
            "prompt.txt",
        ])
        .map_err(|error| partial_compact_codex::storage::Error::Invalid(error.to_string()))
        .and_then(run_cli)
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("pass either --text or --text-file"));
        let store = Store::open(&db_path).unwrap();
        assert!(!store.session_exists("ses-conflict").unwrap());
    }

    #[test]
    fn serve_rejects_removed_seed_context_flag() {
        let error = match Cli::try_parse_from([
            "pcodx",
            "serve",
            "--enable-pcodx-tools",
            "--seed-pcodx-context",
        ]) {
            Ok(_) => panic!("removed seed context flag must be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unexpected argument"));
    }
}
