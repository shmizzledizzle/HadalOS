//! `hadal-probe` — the phase 1 artifact.
//!
//! A standalone binary that carries the action grammar and nothing else. It is
//! cross-compiled to `aarch64-linux-android`, pushed to `/data/local/tmp`, and
//! driven over `adb shell`. No Binder, no SELinux, no ROM — the point is to
//! prove the grammar behaves identically on the device before any of that
//! exists.
//!
//! The desktop project validates itself by running its suite on real Linux
//! (`scripts/wsl-verify.sh`) rather than trusting the authoring machine. This
//! is the same move: `selftest` runs the validator corpus on the actual
//! phone, against bionic and aarch64, because a validator that behaves
//! differently there is not one to rest a security boundary on.
//!
//! Exits non-zero on any failure so it can be used as a build gate.

use std::io::Read;
use std::process::ExitCode;

use hadal_brokerd::capability::Tier;
use hadal_brokerd::{parse_proposal, Capability};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("capabilities") => {
            print_capabilities();
            ExitCode::SUCCESS
        }
        Some("validate") => validate_stdin(),
        Some("selftest") => selftest(),
        _ => {
            eprintln!(
                "usage: hadal-probe <capabilities|validate|selftest>\n\
                 \n\
                 capabilities   print the capability table\n\
                 validate       read one JSON proposal on stdin, validate it\n\
                 selftest       run the validator corpus, exit non-zero on failure"
            );
            ExitCode::from(2)
        }
    }
}

fn tier_name(t: Tier) -> &'static str {
    match t {
        Tier::Read => "Read",
        Tier::Inspect => "Inspect",
        Tier::Mutate => "Mutate",
        Tier::Egress => "Egress",
    }
}

fn print_capabilities() {
    println!("{:<24} {:<8} {:<7} {}", "CAPABILITY", "TIER", "PROMPT", "DESCRIPTION");
    for c in Capability::ALL {
        println!(
            "{:<24} {:<8} {:<7} {}",
            c.id(),
            tier_name(c.tier()),
            if c.requires_confirmation() { "yes" } else { "no" },
            c.describe()
        );
    }
}

fn validate_stdin() -> ExitCode {
    let mut buf = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
        eprintln!("cannot read stdin: {e}");
        return ExitCode::FAILURE;
    }
    match parse_proposal(buf.trim()) {
        Ok(action) => {
            let cap = action.capability();
            println!("accepted");
            println!("  action      {}", action.id());
            println!("  capability  {}", cap.id());
            println!("  tier        {}", tier_name(cap.tier()));
            println!("  confirm     {}", cap.requires_confirmation());
            println!("  summary     {}", action.summary());
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("rejected");
            println!("  reason      {e}");
            ExitCode::FAILURE
        }
    }
}

/// Every case states its expected outcome, so a validator that silently
/// loosens on another architecture fails loudly rather than passing.
struct Case {
    json: &'static str,
    accept: bool,
    why: &'static str,
}

const CORPUS: &[Case] = &[
    // ── must be accepted ────────────────────────────────────────────────
    Case { json: r#"{"action":"read-logcat"}"#, accept: true, why: "bare read with defaults" },
    Case {
        json: r#"{"action":"read-crash-report","tag":"data_app_anr","package":"com.example.app"}"#,
        accept: true,
        why: "ANR triage, the flagship read",
    },
    Case {
        json: r#"{"action":"read-network-activity","window":"last-day"}"#,
        accept: true,
        why: "per-app network activity",
    },
    Case {
        json: r#"{"action":"set-app-network-policy","package":"com.example.tracker","policy":"block-all"}"#,
        accept: true,
        why: "the remediation half",
    },
    Case {
        json: r#"{"action":"revoke-permission","package":"com.example.app","permission":"android.permission.ACCESS_FINE_LOCATION"}"#,
        accept: true,
        why: "runtime permission revoke",
    },
    Case {
        json: r#"{"action":"restart-service","service":"hadald"}"#,
        accept: true,
        why: "non-critical service restart",
    },
    Case { json: r#"{"action":"service-status","service":"zygote"}"#, accept: true, why: "critical services may be inspected" },
    // ── must be rejected ────────────────────────────────────────────────
    Case { json: r#"{"action":"exec","cmd":"sh -c id"}"#, accept: false, why: "there is no exec action" },
    Case { json: r#"{"action":"shell","command":"whoami"}"#, accept: false, why: "nor a shell action" },
    Case {
        json: r#"{"action":"restart-service","service":"zygote"}"#,
        accept: false,
        why: "restarting zygote takes the device down",
    },
    Case {
        json: r#"{"action":"restart-service","service":"system_server"}"#,
        accept: false,
        why: "same",
    },
    Case {
        json: r#"{"action":"query-package","package":"com.foo; rm -rf /data"}"#,
        accept: false,
        why: "command injection via package name",
    },
    Case {
        json: r#"{"action":"query-package","package":"$(id).pkg"}"#,
        accept: false,
        why: "substitution in package name",
    },
    Case { json: r#"{"action":"query-package","package":"--user"}"#, accept: false, why: "option smuggling" },
    Case {
        json: r#"{"action":"read-path","path":"/data/data/com.bank/databases/accounts.db"}"#,
        accept: false,
        why: "app private storage, refused at parse time (path denylist layer 1)",
    },
    Case {
        json: r#"{"action":"read-path","path":"/data/anr/../../data/data/com.bank/x"}"#,
        accept: false,
        why: "traversal out of an allowed root",
    },
    Case {
        json: r#"{"action":"revoke-permission","package":"com.foo.bar","permission":"android.permission.INTERNET"}"#,
        accept: false,
        why: "INTERNET is not a runtime permission",
    },
    Case {
        json: r#"{"action":"restart-service","service":"hadald","extra":"--now"}"#,
        accept: false,
        why: "unknown fields are not smuggled past the executor",
    },
    Case { json: r#"{"action":"read-logcat","lines":999999}"#, accept: false, why: "line count bound" },
    Case {
        json: r#"{"action":"write-setting","change":{"kind":"private-dns","mode":"hostname"}}"#,
        accept: false,
        why: "hostname mode without a hostname",
    },
];

fn selftest() -> ExitCode {
    let mut failed = 0usize;

    for case in CORPUS {
        let got = parse_proposal(case.json).is_ok();
        if got == case.accept {
            println!("ok    {} — {}", if case.accept { "accept" } else { "reject" }, case.why);
        } else {
            failed += 1;
            println!(
                "FAIL  expected {} but got {} — {}\n      {}",
                if case.accept { "accept" } else { "reject" },
                if got { "accept" } else { "reject" },
                case.why,
                case.json
            );
        }
    }

    // Structural invariants that must hold wherever this runs.
    let mut checks_failed = 0usize;
    let mut check = |name: &str, ok: bool| {
        if ok {
            println!("ok    invariant: {name}");
        } else {
            checks_failed += 1;
            println!("FAIL  invariant: {name}");
        }
    };
    check(
        "every Mutate capability prompts",
        Capability::ALL
            .iter()
            .filter(|c| c.tier() == Tier::Mutate)
            .all(|c| c.requires_confirmation()),
    );
    check(
        "no Read or Inspect capability prompts",
        Capability::ALL
            .iter()
            .filter(|c| matches!(c.tier(), Tier::Read | Tier::Inspect))
            .all(|c| !c.requires_confirmation()),
    );
    check("egress is denied by default", Capability::NetworkLookup.advisory_disposition() == "deny");

    let total = CORPUS.len() + 3;
    let bad = failed + checks_failed;
    println!("\n{}/{} passed", total - bad, total);

    if bad == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
