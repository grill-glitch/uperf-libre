//! uperf-cli — offline parity tool (AGENT.md §10.2).
//!
//! Subcommands:
//!   parse <config.json>                  — pretty-print the parsed structure.
//!   warn <config.json>                   — emit unknown-module / unknown-key lines.
//!   plan <config.json> <mode> <scene>    — emit the sysfs write sequence.
//!
//! Run on the host — no Android dependency. Used for parity checks against the
//! upstream binary's `attr` trace and against the real device.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod config;
mod plan;
mod warn;

use config::Config;

#[derive(Parser, Debug)]
#[command(name = "uperf-cli", version, about = "uperf v3 config parity tool")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Parse a JSON config and dump its structure (for parity inspection).
    Parse {
        config: PathBuf,
    },
    /// Emit warnings for unknown modules / keys (matches upstream `CfgMgr: Ignored ...`).
    Warn {
        config: PathBuf,
    },
    /// Plan the sysfs writes for a mode/scene pair (matches upstream `sysfs` writer
    /// sequence).
    Plan {
        config: PathBuf,
        mode: String,
        scene: String,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Parse { config } => parse_cmd(config),
        Cmd::Warn { config } => warn_cmd(config),
        Cmd::Plan { config, mode, scene } => plan_cmd(config, mode, scene),
    }
}

fn parse_cmd(path: PathBuf) -> std::process::ExitCode {
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("uperf-cli parse: cannot read {}: {e}", path.display());
            return std::process::ExitCode::from(2);
        }
    };
    let cfg = match Config::from_slice(&bytes) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("uperf-cli parse: invalid json: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    println!("{cfg}");
    std::process::ExitCode::SUCCESS
}

fn warn_cmd(path: PathBuf) -> std::process::ExitCode {
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("uperf-cli warn: cannot read {}: {e}", path.display());
            return std::process::ExitCode::from(2);
        }
    };
    let cfg = match Config::from_slice(&bytes) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("uperf-cli warn: invalid json: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    let warnings = warn::collect(&cfg);
    if warnings.is_empty() {
        println!("(no warnings)");
    } else {
        for w in warnings {
            println!("{w}");
        }
    }
    std::process::ExitCode::SUCCESS
}

fn plan_cmd(path: PathBuf, mode: String, scene: String) -> std::process::ExitCode {
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("uperf-cli plan: cannot read {}: {e}", path.display());
            return std::process::ExitCode::from(2);
        }
    };
    let cfg = match Config::from_slice(&bytes) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("uperf-cli plan: invalid json: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    plan::emit(&cfg, &mode, &scene);
    std::process::ExitCode::SUCCESS
}