use clap::{Parser, Subcommand, ValueEnum};
use partial_compact_codex::prompts;
use partial_compact_codex::storage::{Role, Store};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "pcodx")]
#[command(about = "Codex-like partial-compaction wrapper skeleton backed by SQLite.")]
struct Cli {
    #[arg(long, global = true, value_name = "PATH")]
    db: Option<PathBuf>,
    #[arg(long, global = true, value_name = "SESSION")]
    session: Option<String>,
    #[arg(long, global = true, value_name = "DIR")]
    cwd: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init,
    Record {
        #[arg(long, value_enum)]
        role: CliRole,
        #[arg(long)]
        text: String,
        #[arg(long)]
        source: Option<String>,
    },
    Turn {
        #[arg(long)]
        text: String,
    },
    Resume {
        #[arg(long)]
        last: bool,
        #[arg(long)]
        text: Option<String>,
    },
    Ids,
    Show,
    Compact {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        summary: String,
    },
    Prompts {
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
        Command::Record { role, text, source } => {
            let session = session_or_create(&mut store, cli.session.as_deref(), &cwd)?;
            let message = store.record_message(&session, role.into(), &text, source.as_deref())?;
            println!("session_id={session}");
            println!("message_id={}", message.id);
            println!("visible_ids={}", store.visible_ids(&session)?.join(","));
        }
        Command::Turn { text } => {
            let session = session_or_create(&mut store, cli.session.as_deref(), &cwd)?;
            if text.is_empty() {
                return Err(partial_compact_codex::storage::Error::Invalid(
                    "turn prompt must be non-empty".to_owned(),
                ));
            }
            let message = store.record_message(&session, Role::User, &text, Some("cli-turn"))?;
            println!("session_id={session}");
            println!("prompt_message_id={}", message.id);
            println!("future_context_source=sqlite-ledger-render");
            println!("{}", store.render_visible_context(&session)?);
        }
        Command::Resume { last, text } => {
            let session = if last {
                store.last_session_id()?.ok_or_else(|| {
                    partial_compact_codex::storage::Error::Invalid("no prior session".to_owned())
                })?
            } else {
                session_or_existing(&store, cli.session.as_deref())?
            };
            if let Some(text) = text {
                if text.is_empty() {
                    return Err(partial_compact_codex::storage::Error::Invalid(
                        "resume prompt must be non-empty".to_owned(),
                    ));
                }
                let message =
                    store.record_message(&session, Role::User, &text, Some("cli-resume"))?;
                println!("prompt_message_id={}", message.id);
            }
            println!("session_id={session}");
            println!("visible_ids={}", store.visible_ids(&session)?.join(","));
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
