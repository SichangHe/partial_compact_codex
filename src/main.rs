use clap::{Parser, Subcommand, ValueEnum};
use partial_compact_codex::prompts;
use partial_compact_codex::storage::{Role, Store};
use std::io::Read;
use std::path::PathBuf;

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
    /// Print visible message and compaction ids usable as compaction range endpoints.
    Ids,
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
    /// List or print shared partial-compaction prompt fragments.
    Prompts {
        /// Prompt fragment name to print. Omit it to list names.
        name: Option<String>,
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
    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(Store::default_path);
    let cwd = cli.cwd.unwrap_or(std::env::current_dir()?);
    let mut store = Store::open(&db_path)?;
    match cli.command {
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
        Command::Ids => {
            let session = session_or_existing(&store, cli.session.as_deref())?;
            println!("session_id={session}");
            println!("{}", store.visible_ids(&session)?.join("\n"));
        }
        Command::Show => {
            let session = session_or_existing(&store, cli.session.as_deref())?;
            println!("{}", store.render_visible_context(&session)?);
        }
        Command::Compact { from, to, summary } => {
            let session = session_or_existing(&store, cli.session.as_deref())?;
            let compaction = store.compact(&session, &from, &to, &summary)?;
            println!("session_id={session}");
            println!("compaction_id={}", compaction.id);
            println!("n_messages_replaced={}", compaction.n_messages_replaced);
            if let Some(warning) = compaction.warning {
                println!("warning={warning}");
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
    }
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
