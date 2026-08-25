//! `hadal` — command-line client for the HadalOS capability broker.
//!
//! Deliberately has no privileges and no model. It opens a session, streams
//! the reply, and when the broker offers a proposal it shows the *broker's*
//! summary — never the model's prose — and asks. Confirming calls `Execute`,
//! at which point polkit decides.
//!
//! Nothing in this binary can bypass that. It holds no capability and knows no
//! token it was not handed.

mod key;

use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use futures_util::stream::StreamExt;
use zbus::{Connection, MatchRule, MessageStream};
use zvariant::{OwnedObjectPath, OwnedValue, Value};

const SERVICE: &str = "org.hadal.Broker1";
const OBJECT: &str = "/org/hadal/Broker1";
const SPOOL: &str = "/var/lib/hadalos/build-failures";

/// How many times the model may act on a result and be asked again.
/// Three is enough for propose -> read -> explain, and small enough that a
/// model looping on "read one more thing" stops on its own.
const MAX_GENERATIONS: usize = 3;


type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Subcommands that take no arguments.
///
/// Used to decide whether a bare word is a command or the start of a question:
/// `hadal status` is the command, `hadal status of my services` is a question.
/// A command is one word; a question has more.
const BARE_COMMANDS: &[&str] = &["status", "explain", "why"];

const SYNOPSIS: &str = "\
hadal — the assistant built into this system

  hadal <question>         ask anything, no subcommand needed
  hadal ask <question>     the same thing, said explicitly
  hadal explain            analyse the most recent Portage build failure
  hadal why                explain services that failed on this boot
  hadal status             broker readiness and what it is permitted to do
  hadal key                get an upstream API key and install it
  hadal -h, --help         this text, plus what Hadal is currently allowed to do

Proposed changes are always shown and confirmed before anything happens, and
the summary you confirm is built by the broker from the parsed action — never
from the model's prose.";

fn usage() -> ! {
    eprintln!("{SYNOPSIS}");
    std::process::exit(2)
}

/// `--help` also reports the live capability table, because "what can this
/// thing do" and "what is it permitted to do right now" are the same question
/// to anyone typing it. Degrades to the static text when the broker is down —
/// help must work when nothing else does.
async fn help() -> Res<()> {
    println!("{SYNOPSIS}");
    println!();

    let Ok(conn) = Connection::system().await else {
        println!("capabilities   (unavailable — cannot reach the system bus)");
        return Ok(());
    };
    let caps: HashMap<String, String> = match conn
        .call_method(Some(SERVICE), OBJECT, Some(SERVICE), "AvailableCapabilities", &())
        .await
    {
        Ok(reply) => reply.body().deserialize()?,
        Err(_) => {
            println!("capabilities   (unavailable — is hadal-brokerd running?)");
            return Ok(());
        }
    };

    println!("what Hadal may currently do, and what it costs to let it:");
    let mut rows: Vec<_> = caps.into_iter().collect();
    rows.sort();
    for (cap, disposition) in rows {
        let note = match disposition.as_str() {
            "allow" => "permitted",
            "auth" => "asks for authentication every time",
            "deny" => "off by default",
            other => other,
        };
        println!("  {cap:<20} {note}");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(first) = args.first().map(String::as_str) else { usage() };

    // Help is answered before touching the bus. A broker that will not start is
    // exactly when someone reaches for --help.
    if matches!(first, "-h" | "--help" | "help") {
        return help().await;
    }

    // Answered before the bus, like help, and for a sharper reason.
    // hadal-brokerd `Requires=hadald.service`, and hadald will not start
    // without the key this command installs — so the broker is guaranteed to be
    // down at the exact moment someone needs this. Dispatching it after
    // `Connection::system()` would make it unreachable in its only situation.
    if args.len() == 1 && first == "key" {
        return key::run();
    }

    // An unrecognised flag is a mistake, not a question. Without this,
    // `hadal --verbose` would be sent to a model as a prompt — a typo that
    // costs a round trip and returns something plausible.
    if first.starts_with('-') {
        eprintln!("hadal: unknown option {first}\n");
        usage()
    }

    let conn = Connection::system().await.map_err(|e| {
        format!("cannot reach the system bus: {e}\nis hadal-brokerd running?")
    })?;

    // A bare subcommand only wins when it stands alone. Anything longer is
    // prose that happens to start with a familiar word.
    if args.len() == 1 && BARE_COMMANDS.contains(&first) {
        return match first {
            "status" => status(&conn).await,
            "explain" => explain(&conn).await,
            "why" => {
                ask(
                    &conn,
                    "Which services failed on this boot, and why? Read the journal if you need it.",
                    HashMap::new(),
                    "cli-why",
                )
                .await
            }
            _ => unreachable!("BARE_COMMANDS and this match must agree"),
        };
    }

    if first == "ask" {
        if args.len() < 2 {
            eprintln!("hadal: ask needs a question\n");
            usage()
        }
        return ask(&conn, &args[1..].join(" "), HashMap::new(), "cli").await;
    }

    // Everything else is the question.
    ask(&conn, &args.join(" "), HashMap::new(), "cli").await
}

// ─────────────────────────────────────────────────────────────────────────

async fn status(conn: &Connection) -> Res<()> {
    let ready: bool = get_property(conn, OBJECT, SERVICE, "Ready").await?;
    let version: String = get_property(conn, OBJECT, SERVICE, "Version").await?;

    println!("broker   {version}");
    println!("model    {}", if ready { "ready" } else { "not reachable (is hadald running?)" });

    let caps: HashMap<String, String> = conn
        .call_method(Some(SERVICE), OBJECT, Some(SERVICE), "AvailableCapabilities", &())
        .await?
        .body()
        .deserialize()?;

    println!("\ncapabilities");
    let mut rows: Vec<_> = caps.into_iter().collect();
    rows.sort();
    for (cap, disposition) in rows {
        let note = match disposition.as_str() {
            "allow" => "permitted",
            "auth" => "asks for authentication every time",
            "deny" => "off by default",
            other => other,
        };
        println!("  {cap:<20} {note}");
    }
    Ok(())
}

/// Reads the newest record the Portage death hook left behind.
async fn explain(conn: &Connection) -> Res<()> {
    let dir = PathBuf::from(SPOOL);
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;

    let entries = std::fs::read_dir(&dir).map_err(|e| {
        format!("no build failures recorded ({}: {e})", dir.display())
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            if let Ok(t) = entry.metadata().and_then(|m| m.modified()) {
                if newest.as_ref().is_none_or(|(best, _)| t > *best) {
                    newest = Some((t, path));
                }
            }
        }
    }

    let Some((_, record_path)) = newest else {
        println!("No build failures recorded. Nothing to explain.");
        return Ok(());
    };

    let raw = std::fs::read_to_string(&record_path)?;
    let record: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("{}: malformed record: {e}", record_path.display()))?;

    let package = record["package"].as_str().unwrap_or("unknown");
    let phase = record["phase"].as_str().unwrap_or("unknown");
    let log = record["log"].as_str().unwrap_or("");

    println!("Most recent failure: \x1b[1m{package}\x1b[0m (phase: {phase})");
    if log.is_empty() {
        println!("No build log was captured, so this will be a guess.\n");
    } else {
        println!("Log: {log}\n");
    }

    // The log path goes in the context map, not spliced into the prompt. The
    // broker still validates it against read-path policy before anything opens
    // it — being named here grants nothing.
    let mut context: HashMap<String, OwnedValue> = HashMap::new();
    if !log.is_empty() {
        context.insert("portage_log".into(), Value::from(log).try_into()?);
    }

    let prompt = format!(
        "The Gentoo package {package} failed to build during the {phase} phase. \
         Read the build log and tell me what went wrong and how to fix it. \
         Be specific about which USE flags, versions or patches are involved."
    );

    ask(conn, &prompt, context, "portage").await
}

// ─────────────────────────────────────────────────────────────────────────

async fn ask(
    conn: &Connection,
    prompt: &str,
    context: HashMap<String, OwnedValue>,
    surface: &str,
) -> Res<()> {
    let mut options: HashMap<String, OwnedValue> = HashMap::new();
    options.insert("surface".into(), Value::from(surface).try_into()?);
    options.insert("tier".into(), Value::from("auto").try_into()?);

    let session: OwnedObjectPath = conn
        .call_method(Some(SERVICE), OBJECT, Some(SERVICE), "CreateSession", &(options,))
        .await?
        .body()
        .deserialize()?;

    // Subscribe before asking, or a fast reply can land before we are listening.
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(SERVICE)?
        .path(session.clone())?
        .build();
    let mut signals = MessageStream::for_match_rule(rule, conn, None).await?;

    // The conversation is a loop, not a single shot. A proposal exists so the
    // model can obtain something it said it needed; handing the result to the
    // user and not back to the model leaves it answering from the evidence it
    // had *before* asking. `explain` in particular then just reads a log aloud.
    //
    // Bounded, because "read, then propose another read" is a cycle a model
    // will happily sustain. Exceeding the bound is reported, never silent.
    let mut prompt = prompt.to_string();
    let mut context = context;

    for generation in 1..=MAX_GENERATIONS {
        let _request: u32 = conn
            .call_method(
                Some(SERVICE),
                &session,
                Some("org.hadal.Session1"),
                "Ask",
                &(prompt.as_str(), context),
            )
            .await?
            .body()
            .deserialize()?;

        let proposals = drain_until_finished(&mut signals).await?;
        if proposals.is_empty() {
            break;
        }

        let mut results: Vec<String> = Vec::new();
        for (capability, summary, token) in proposals {
            if let Some(text) =
                confirm_and_execute(conn, &session, &capability, &summary, &token).await?
            {
                results.push(text);
            }
        }

        // Nothing was gathered — declined, denied, or it failed. There is
        // nothing new to reason from, so stop rather than re-ask identically.
        if results.is_empty() {
            break;
        }

        if generation == MAX_GENERATIONS {
            eprintln!(
                "\x1b[33m[stopped after {MAX_GENERATIONS} rounds — \
                 the result above was gathered but not interpreted]\x1b[0m"
            );
            break;
        }

        let mut next: HashMap<String, OwnedValue> = HashMap::new();
        next.insert("result".into(), Value::from(results.join("\n\n")).try_into()?);
        context = next;
        prompt = format!(
            "The action you proposed has been run and its output is in the context above. \
             Using that output, answer the original question: {prompt}"
        );
    }

    let _ = conn
        .call_method(Some(SERVICE), &session, Some("org.hadal.Session1"), "Close", &())
        .await;
    Ok(())
}

/// Print deltas as they stream, collect proposals, return when the broker says
/// the generation is done.
async fn drain_until_finished(
    signals: &mut MessageStream,
) -> Res<Vec<(String, String, String)>> {
    let mut proposals: Vec<(String, String, String)> = Vec::new();

    while let Some(Ok(msg)) = signals.next().await {
        let header = msg.header();
        let Some(member) = header.member() else { continue };

        match member.as_str() {
            "Delta" => {
                let (_req, text): (u32, String) = msg.body().deserialize()?;
                print!("{text}");
                io::stdout().flush().ok();
            }
            "ActionProposed" => {
                let (_req, capability, _action, _params, rationale, token): (
                    u32,
                    String,
                    String,
                    HashMap<String, OwnedValue>,
                    String,
                    String,
                ) = msg.body().deserialize()?;
                proposals.push((capability, rationale, token));
            }
            "CapabilityDenied" => {
                let (_req, capability, detail): (u32, String, String) =
                    msg.body().deserialize()?;
                eprintln!("\n\x1b[33m[{capability} is not enabled]\x1b[0m {detail}");
            }
            "Finished" => {
                let (_req, reason): (u32, String) = msg.body().deserialize()?;
                println!();
                if reason != "complete" {
                    eprintln!("\x1b[31m[{reason}]\x1b[0m");
                }
                break;
            }
            _ => {}
        }
    }
    Ok(proposals)
}

/// Returns the executed action's output, so the caller can hand it back to the
/// model. `None` means nothing was gathered — declined, refused, or failed.
async fn confirm_and_execute(
    conn: &Connection,
    session: &OwnedObjectPath,
    capability: &str,
    summary: &str,
    token: &str,
) -> Res<Option<String>> {
    // `summary` came from the broker's Action::summary(), derived from the
    // parsed action — not from anything the model wrote. What is displayed is
    // what will run.
    println!("\n\x1b[1mHadal proposes:\x1b[0m {summary}");
    println!("  capability: {capability}");

    if !io::stdin().is_terminal() {
        println!("  \x1b[33mnot a terminal — declining\x1b[0m");
        let _ = conn
            .call_method(Some(SERVICE), session, Some("org.hadal.Session1"), "Discard", &(token,))
            .await;
        return Ok(None);
    }

    print!("  run it? [y/N] ");
    io::stdout().flush().ok();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;

    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        let _ = conn
            .call_method(Some(SERVICE), session, Some("org.hadal.Session1"), "Discard", &(token,))
            .await;
        println!("  declined");
        return Ok(None);
    }

    match conn
        .call_method(Some(SERVICE), session, Some("org.hadal.Session1"), "Execute", &(token,))
        .await
    {
        Ok(reply) => {
            let result: HashMap<String, OwnedValue> = reply.body().deserialize()?;
            match result.get("text").and_then(|v| <&str>::try_from(v).ok()) {
                Some(text) => {
                    // Report the size rather than the content. The user already
                    // saw and authorised the summary of exactly what would run,
                    // so printing a 20 KB build log here adds no oversight — it
                    // just buries the answer that follows. The full text goes to
                    // the model.
                    println!("  \x1b[2m← {} bytes\x1b[0m", text.len());
                    Ok(Some(text.to_string()))
                }
                None => {
                    println!("  done");
                    Ok(None)
                }
            }
        }
        // Includes the polkit refusal path: the user cancelled the auth
        // dialog, or policy forbids it outright.
        Err(e) => {
            eprintln!("  \x1b[31mnot permitted:\x1b[0m {e}");
            Ok(None)
        }
    }
}

async fn get_property<T>(conn: &Connection, path: &str, interface: &str, name: &str) -> Res<T>
where
    T: TryFrom<OwnedValue>,
    <T as TryFrom<OwnedValue>>::Error: std::fmt::Display,
{
    let reply = conn
        .call_method(
            Some(SERVICE),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(interface, name),
        )
        .await?;
    let value: OwnedValue = reply.body().deserialize()?;
    T::try_from(value).map_err(|e| format!("unexpected type for {name}: {e}").into())
}
