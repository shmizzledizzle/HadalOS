//! `hadal` — command-line client for the HadalOS capability broker.
//!
//! Deliberately has no privileges and no model. It opens a session, streams
//! the reply, and when the broker offers a proposal it shows the *broker's*
//! summary — never the model's prose — and asks. Confirming calls `Execute`,
//! at which point polkit decides.
//!
//! Nothing in this binary can bypass that. It holds no capability and knows no
//! token it was not handed.

use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use futures_util::stream::StreamExt;
use zbus::{Connection, MatchRule, MessageStream};
use zvariant::{OwnedObjectPath, OwnedValue, Value};

const SERVICE: &str = "org.hadal.Broker1";
const OBJECT: &str = "/org/hadal/Broker1";
const SPOOL: &str = "/var/lib/hadalos/build-failures";

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn usage() -> ! {
    eprintln!(
        "\
hadal — ask the assistant built into this system

  hadal ask <question>     ask anything
  hadal explain            analyse the most recent Portage build failure
  hadal why                explain services that failed on this boot
  hadal status             broker readiness and what it is permitted to do

Everything runs locally. Proposed changes are always shown and confirmed
before anything happens."
    );
    std::process::exit(2)
}

#[tokio::main]
async fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else { usage() };

    let conn = Connection::system().await.map_err(|e| {
        format!("cannot reach the system bus: {e}\nis hadal-brokerd running?")
    })?;

    match cmd {
        "status" => status(&conn).await,
        "ask" if args.len() > 1 => {
            ask(&conn, &args[1..].join(" "), HashMap::new(), "cli").await
        }
        "why" => {
            ask(
                &conn,
                "Which services failed on this boot, and why? Read the journal if you need it.",
                HashMap::new(),
                "cli-why",
            )
            .await
        }
        "explain" => explain(&conn).await,
        _ => usage(),
    }
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

    let _request: u32 = conn
        .call_method(
            Some(SERVICE),
            &session,
            Some("org.hadal.Session1"),
            "Ask",
            &(prompt, context),
        )
        .await?
        .body()
        .deserialize()?;

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

    for (capability, summary, token) in proposals {
        confirm_and_execute(conn, &session, &capability, &summary, &token).await?;
    }

    let _ = conn
        .call_method(Some(SERVICE), &session, Some("org.hadal.Session1"), "Close", &())
        .await;
    Ok(())
}

async fn confirm_and_execute(
    conn: &Connection,
    session: &OwnedObjectPath,
    capability: &str,
    summary: &str,
    token: &str,
) -> Res<()> {
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
        return Ok(());
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
        return Ok(());
    }

    match conn
        .call_method(Some(SERVICE), session, Some("org.hadal.Session1"), "Execute", &(token,))
        .await
    {
        Ok(reply) => {
            let result: HashMap<String, OwnedValue> = reply.body().deserialize()?;
            if let Some(text) = result.get("text").and_then(|v| <&str>::try_from(v).ok()) {
                println!("{text}");
            } else {
                println!("  done");
            }
        }
        // Includes the polkit refusal path: the user cancelled the auth
        // dialog, or policy forbids it outright.
        Err(e) => eprintln!("  \x1b[31mnot permitted:\x1b[0m {e}"),
    }
    Ok(())
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
